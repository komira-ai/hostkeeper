//! The verified artifact: a staged release tree and the digest it had when the updater verified
//! it. Nothing from the tree runs unless it still has that digest, and the check is made again
//! before every step, resumed ones included (docs/design/hostkeeper-design.md, 5.5 and 6.2).
//!
//! The staged tree is root-owned and read-only in root-owned parents (design section 4.7), so it cannot be
//! swapped between this check and the exec; the check is what notices a tree that was changed
//! anyway (a bad restore, a disk fault, a root that was tricked), and the runner treats it as a
//! `halt`.
//!
//! A staged tree is named by its real path: [`Staged::seal`] resolves the links above the tree
//! once, so the check, the hook it returns and every reader of [`Staged::dir`] use one path that
//! holds no link. On macOS `/var` is a link to `/private/var`, so a version under
//! `/var/lib/hostkeeper/packages` is `/private/var/lib/hostkeeper/packages/...` from its seal on.
//! The tree itself is never a link.
//!
//! # The tree digest
//!
//! [`tree_digest`] is a public format (docs/package-format.md): the seal of every unpacked
//! package holds it, and a later release recomputes it. It is the lower-case hex SHA-256 of one
//! record per entry below the tree (the tree itself is not an entry), in the byte order of the
//! entries' full relative names (`bin`, `bin/x`, `manifest.toml`, ...; `/` separates names):
//!
//! ```text
//! record  = length header [bytes]
//! length  = u64, little-endian: the length of header plus, for a file, its size
//! header  = kind NUL name NUL mode NUL size NUL
//! kind    = "f" (a regular file), "d" (a directory) or "l" (a symbolic link)
//! name    = the full relative name, as bytes
//! mode    = the permission bits (mode & 0o7777), four octal digits ("0555", "0444"); "0000"
//!           for a link, whose own bits mean nothing and differ between systems
//! size    = the file's size in bytes, in decimal; "0" for a directory; the length of the
//!           target text for a link
//! bytes   = the file's contents (files only); the link's target text, as `readlink` gives it,
//!           never followed or resolved (links only)
//! ```
//!
//! Nothing else is hashed: not the owner, not the times. A link is an entry only inside the
//! `tree/` directory of a version (its full relative name has `tree` as its first component and
//! at least one more: `tree/usr/lib/x`, never `tree` itself, `apt/x` or a top-level `x`). A tree
//! holding a link anywhere else, or anything other than regular files, directories and those
//! links (a device, a pipe), has no digest ([`ArtifactError::Unsupported`]). A tree with no link
//! has the digest it had before links were allowed.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// Why a staged release cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("cannot read the staged release: {0}")]
    Io(#[from] io::Error),
    #[error(
        "the staged release holds {0:?}, which is neither a regular file nor a directory (a link is taken only inside tree/)"
    )]
    Unsupported(PathBuf),
    #[error("the staged release changed after it was verified: it was {expected}, it is {found}")]
    Changed { expected: String, found: String },
    #[error(
        "{0:?} is not trusted: it must be an absolute plain path of real directories owned by the updater or root and closed to group and others, and nothing in the tree may be a link, owned by another user, setuid, setgid or writable by group or others"
    )]
    Untrusted(PathBuf),
    #[error("the hook path {0:?} is not a relative path of plain names")]
    BadHookPath(PathBuf),
    #[error("the hook {0:?} is not a regular file of the staged release")]
    HookMissing(PathBuf),
}

/// What an entry of a tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    /// A symbolic link inside `tree/`, hashed by its target text.
    Link,
}

/// One entry of a tree as the digest read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// The full relative name, `/`-separated.
    pub name: Vec<u8>,
    pub kind: EntryKind,
    /// The permission bits (`mode & 0o7777`); 0 for a link.
    pub mode: u32,
    /// The size in bytes; 0 for a directory; the length of the target text for a link.
    pub size: u64,
    /// The SHA-256 of a file's contents or of a link's target text; `None` for a directory.
    pub content: Option<[u8; 32]>,
}

/// A tree read from disk: its digest and its entries in digest order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    /// The lower-case hex [`tree_digest`].
    pub digest: String,
    pub entries: Vec<TreeEntry>,
}

/// A staged release and its digest.
///
/// Its fields are private: a `Staged` is made only by [`Staged::seal`], so every one names a
/// tree as it was when it was sealed, and what runs from it is reached through
/// [`Staged::check`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    /// The absolute path of the tree.
    dir: PathBuf,
    /// The lower-case hex sha256 of the tree ([`tree_digest`]) at the time it was verified.
    digest: String,
}

impl Staged {
    /// The tree at `dir` as it is now. Called by the step that verified the release, once, right
    /// after the verification and before anything else touches the tree.
    ///
    /// `dir` must be absolute, made of plain names and not the root ([`ArtifactError::Untrusted`]
    /// otherwise): the checks of its ancestors would else be made on something else than the
    /// tree that runs. The links above the tree are resolved here, once: the `Staged` names the
    /// real path of its parent joined with the tree's own name, which is not resolved, so a tree
    /// that is itself a link stays one and [`Staged::check`] refuses it. A parent that cannot be
    /// resolved is [`ArtifactError::Io`]. resolved. A tree holding a link outside `tree/` or anything else that is neither a
    /// regular file, a directory nor a link inside `tree/` is [`ArtifactError::Unsupported`].
    pub fn seal(dir: &Path) -> Result<Staged, ArtifactError> {
        let untrusted = || ArtifactError::Untrusted(dir.to_path_buf());
        if !dir.is_absolute() || !dir.components().skip(1).all(is_plain) {
            return Err(untrusted());
        }
        let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
            return Err(untrusted());
        };
        let dir = fs::canonicalize(parent)?.join(name);
        Ok(Staged {
            digest: tree_digest(&dir)?,
            dir,
        })
    }

    /// The absolute path of the tree: its real path, as [`Staged::seal`] resolved it.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The lower-case hex sha256 of the tree at the time it was sealed.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Checks the tree still has its digest and returns the hook executable inside it.
    ///
    /// **What the launcher may rely on:** the returned path is a regular file of a tree that was
    /// not changed since it was sealed, and the tree and every directory above it were, at this
    /// check, real directories (no link) owned by the running user or root and closed to group
    /// and others. The updater's staged trees are root-owned, read-only and in root-owned
    /// parents, so the digest and the place together mean the file cannot be swapped before the
    /// exec. A launcher that runs the path must run it from here, not from a path it built.
    ///
    /// `hook` is the program of the manifest's `[hook] command`: relative, plain names only, a
    /// regular file of the tree reached without passing a link at any component (each is looked at
    /// without following it), so it cannot lead out of the tree.
    pub fn check(&self, hook: &Path) -> Result<PathBuf, ArtifactError> {
        self.check_as(hook, &trusted_owners(), &host_owner)
    }

    /// [`Staged::check`] with the trusted owners and the way an owner is read given, so that a
    /// test can refuse a foreign owner without a second user.
    ///
    /// Besides the digest: the tree and every directory above it must be real directories owned
    /// by a trusted user and not writable by group or others, and every entry inside the tree must
    /// be a regular file or a directory, or a link inside `tree/`, owned by a trusted user; a
    /// file or directory is also not setuid or setgid and not writable by group or others (a
    /// link's own mode is not looked at). Any other link is [`ArtifactError::Untrusted`] naming
    /// the entry, as is any entry that breaks a rule.
    pub fn check_as(
        &self,
        hook: &Path,
        trusted: &[u32],
        owner: &dyn Fn(&fs::Metadata) -> (u32, u32),
    ) -> Result<PathBuf, ArtifactError> {
        self.check_tree_as(trusted, owner)?;
        if hook.as_os_str().is_empty() || !hook.components().all(is_plain) {
            return Err(ArtifactError::BadHookPath(hook.to_path_buf()));
        }
        // `hook` is plain names, and each component is looked at without following it: every
        // one but the last a real directory, the last a regular file, so a link inside `tree/`
        // is never on the way.
        let parts: Vec<Component<'_>> = hook.components().collect();
        let mut at = self.dir.clone();
        for (i, part) in parts.iter().enumerate() {
            at.push(part);
            let last = i + 1 == parts.len();
            let sound = fs::symlink_metadata(&at)
                .is_ok_and(|meta| if last { meta.is_file() } else { meta.is_dir() });
            if !sound {
                return Err(ArtifactError::HookMissing(hook.to_path_buf()));
            }
        }
        Ok(at)
    }

    /// [`Staged::check_as`] without a hook: the place, every entry's trust and the digest, with
    /// the host's trusted owners. For a release whose steps are built in and run nothing from
    /// the tree.
    pub fn check_tree(&self) -> Result<(), ArtifactError> {
        self.check_tree_as(&trusted_owners(), &host_owner)
    }

    /// [`Staged::check_tree`] with the trusted owners and the way an owner is read given.
    pub fn check_tree_as(
        &self,
        trusted: &[u32],
        owner: &dyn Fn(&fs::Metadata) -> (u32, u32),
    ) -> Result<(), ArtifactError> {
        check_place(&self.dir, trusted, owner)?;
        let found = walk(&self.dir, Some((trusted, owner)))?.digest;
        if found != self.digest {
            return Err(ArtifactError::Changed {
                expected: self.digest.clone(),
                found,
            });
        }
        Ok(())
    }
}

/// The owners a check trusts: the running user and root.
pub(crate) fn trusted_owners() -> [u32; 2] {
    [rustix::process::geteuid().as_raw(), 0]
}

pub(crate) fn host_owner(meta: &fs::Metadata) -> (u32, u32) {
    (meta.uid(), meta.gid())
}

/// The tree and every directory above it are real directories (not links) owned by a trusted
/// user and not writable by group or others. The path is absolute and made of plain names:
/// [`Staged::seal`] makes no other.
fn check_place(
    dir: &Path,
    trusted: &[u32],
    owner: &dyn Fn(&fs::Metadata) -> (u32, u32),
) -> Result<(), ArtifactError> {
    let mut at = PathBuf::new();
    for part in dir.components() {
        at.push(part);
        let meta = fs::symlink_metadata(&at)?;
        // One comparison; a link is not a directory here, so a linked component fails it.
        let sound = (
            meta.is_dir(),
            trusted.contains(&owner(&meta).0),
            meta.mode() & 0o022 == 0,
        );
        if sound != (true, true, true) {
            return Err(ArtifactError::Untrusted(at));
        }
    }
    Ok(())
}

fn is_plain(component: Component<'_>) -> bool {
    matches!(component, Component::Normal(_))
}

/// The digest of the tree below `dir` (the format is in the module documentation). Lower-case
/// hex. Links are never followed: a link inside `tree/` is hashed by its target text, and a tree
/// holding one anywhere else has no digest.
pub fn tree_digest(dir: &Path) -> Result<String, ArtifactError> {
    Ok(read_tree(dir)?.digest)
}

/// The tree below `dir`: its digest and every entry with the SHA-256 of each file's contents,
/// read in one pass.
pub fn read_tree(dir: &Path) -> Result<Tree, ArtifactError> {
    walk(dir, None)
}

/// The trust rules of [`Staged::check_as`], applied to each entry while the tree is read.
pub(crate) type Trust<'a> = (&'a [u32], &'a dyn Fn(&fs::Metadata) -> (u32, u32));

/// Reads the tree below `dir`. With `trust`, an entry that breaks a trust rule is
/// [`ArtifactError::Untrusted`] (a link outside `tree/` included); without, such a link is
/// [`ArtifactError::Unsupported`].
pub(crate) fn walk(dir: &Path, trust: Option<Trust<'_>>) -> Result<Tree, ArtifactError> {
    let mut found = Vec::new();
    list(dir, Path::new(""), trust, &mut found)?;
    found.sort_by(|a, b| a.0.as_os_str().as_bytes().cmp(b.0.as_os_str().as_bytes()));
    let mut hasher = Sha256::new();
    let mut entries = Vec::with_capacity(found.len());
    for (rel, meta) in found {
        let name = rel.as_os_str().as_bytes().to_vec();
        let mode = match meta.is_symlink() {
            true => 0,
            false => meta.permissions().mode() & 0o7777,
        };
        let (kind, size, content) = if meta.is_symlink() {
            let target = fs::read_link(dir.join(&rel))?.into_os_string().into_vec();
            let size = target.len() as u64;
            put_header(&mut hasher, b'l', &name, mode, size);
            hasher.update(&target);
            (EntryKind::Link, size, Some(Sha256::digest(&target).into()))
        } else if meta.is_dir() {
            put_header(&mut hasher, b'd', &name, mode, 0);
            (EntryKind::Dir, 0, None)
        } else {
            let size = meta.size();
            put_header(&mut hasher, b'f', &name, mode, size);
            let content = hash_file(&mut hasher, &dir.join(&rel))?;
            (EntryKind::File, size, Some(content))
        };
        entries.push(TreeEntry {
            name,
            kind,
            mode,
            size,
            content,
        });
    }
    Ok(Tree {
        digest: hex(&hasher.finalize()),
        entries,
    })
}

/// Whether the full relative name `rel` is inside `tree/`: `tree` is its first component and it
/// has at least one more.
fn inside_tree(rel: &Path) -> bool {
    let mut parts = rel.components();
    parts.next() == Some(Component::Normal("tree".as_ref())) && parts.next().is_some()
}

/// Every entry below `dir` (relative to the tree root `rel`), each looked at without following
/// it. Only regular files, directories and links inside `tree/` are taken.
fn list(
    dir: &Path,
    rel: &Path,
    trust: Option<Trust<'_>>,
    found: &mut Vec<(PathBuf, fs::Metadata)>,
) -> Result<(), ArtifactError> {
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let path = dir.join(&name);
        let rel = rel.join(&name);
        let meta = fs::symlink_metadata(&path)?;
        let kind = meta.file_type();
        if let Some((trusted, owner)) = trust {
            // A link's own mode means nothing: it is read as 0 (no bit to refuse).
            let allowed = kind.is_symlink() && inside_tree(&rel);
            let mode = match allowed {
                true => 0,
                false => meta.mode() & 0o7777,
            };
            // One comparison: a link outside tree/, a foreign owner, setuid or setgid, writable
            // by others.
            let sound = (
                allowed || !kind.is_symlink(),
                trusted.contains(&owner(&meta).0),
                mode & 0o6000 == 0,
                mode & 0o022 == 0,
            );
            if sound != (true, true, true, true) {
                return Err(ArtifactError::Untrusted(rel));
            }
        }
        if kind.is_dir() {
            list(&path, &rel, trust, found)?;
            found.push((rel, meta));
        } else if kind.is_file() || (kind.is_symlink() && inside_tree(&rel)) {
            found.push((rel, meta));
        } else {
            return Err(ArtifactError::Unsupported(rel));
        }
    }
    Ok(())
}

/// The header of a record, with the length that covers it and the `size` bytes that follow.
fn put_header(hasher: &mut Sha256, kind: u8, name: &[u8], mode: u32, size: u64) {
    let mut header = vec![kind, 0];
    header.extend_from_slice(name);
    header.push(0);
    header.extend_from_slice(format!("{mode:04o}\0{size}\0").as_bytes());
    hasher.update((header.len() as u64 + size).to_le_bytes());
    hasher.update(&header);
}

/// Feeds the file's bytes to the tree hasher and returns their own SHA-256. The file is opened
/// without following a link.
fn hash_file(tree: &mut Sha256, path: &Path) -> Result<[u8; 32], ArtifactError> {
    use rustix::fs::{Mode, OFlags};
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let mut file = File::from(fd);
    let mut own = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            let mut content = [0u8; 32];
            content.copy_from_slice(&own.finalize());
            return Ok(content);
        }
        tree.update(&buf[..n]);
        own.update(&buf[..n]);
    }
}

/// Lower-case hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}
