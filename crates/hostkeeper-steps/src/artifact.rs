//! The verified artifact: a staged release tree and the digest it had when the updater verified
//! it. Nothing from the tree runs unless it still has that digest, and the check is made again
//! before every step, resumed ones included (docs/design/host-updates.md, 3.3 and 3.5).
//!
//! The staged tree is root-owned and read-only in root-owned parents (3.4.1), so it cannot be
//! swapped between this check and the exec; the check is what notices a tree that was changed
//! anyway (a bad restore, a disk fault, a root that was tricked), and the runner treats it as a
//! `halt`.
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
//! kind    = "f" (a regular file) or "d" (a directory)
//! name    = the full relative name, as bytes
//! mode    = the permission bits (mode & 0o7777), four octal digits ("0555", "0444")
//! size    = the file's size in bytes, in decimal; "0" for a directory
//! bytes   = the file's contents (files only)
//! ```
//!
//! Nothing else is hashed: not the owner, not the times. A tree holding anything other than
//! regular files and directories (a link, a device, a pipe) has no digest
//! ([`ArtifactError::Unsupported`]).

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// Why a staged release cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("cannot read the staged release: {0}")]
    Io(#[from] io::Error),
    #[error("the staged release holds {0:?}, which is neither a regular file nor a directory")]
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
}

/// One entry of a tree as the digest read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// The full relative name, `/`-separated.
    pub name: Vec<u8>,
    pub kind: EntryKind,
    /// The permission bits (`mode & 0o7777`).
    pub mode: u32,
    /// The size in bytes; 0 for a directory.
    pub size: u64,
    /// The SHA-256 of a file's contents; `None` for a directory.
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
    /// `dir` must be absolute and made of plain names ([`ArtifactError::Untrusted`] otherwise):
    /// the checks of its ancestors would else be made on something else than the tree that runs.
    /// A tree holding a link or anything else that is neither a regular file nor a directory is
    /// [`ArtifactError::Unsupported`].
    pub fn seal(dir: &Path) -> Result<Staged, ArtifactError> {
        if !dir.is_absolute() || !dir.components().skip(1).all(is_plain) {
            return Err(ArtifactError::Untrusted(dir.to_path_buf()));
        }
        Ok(Staged {
            dir: dir.to_path_buf(),
            digest: tree_digest(dir)?,
        })
    }

    /// The absolute path of the tree.
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
    /// `hook` is the declaration's `binary`: relative, plain names only, a regular file of the
    /// tree reached without passing a link at any component, so it cannot lead out of the tree.
    pub fn check(&self, hook: &Path) -> Result<PathBuf, ArtifactError> {
        self.check_as(hook, &trusted_owners(), &host_owner)
    }

    /// [`Staged::check`] with the trusted owners and the way an owner is read given, so that a
    /// test can refuse a foreign owner without a second user.
    ///
    /// Besides the digest: the tree and every directory above it must be real directories owned
    /// by a trusted user and not writable by group or others, and every entry inside the tree must
    /// be a regular file or a directory (no link), owned by a trusted user, not setuid or setgid,
    /// and not writable by group or others ([`ArtifactError::Untrusted`] naming the entry).
    pub fn check_as(
        &self,
        hook: &Path,
        trusted: &[u32],
        owner: &dyn Fn(&fs::Metadata) -> (u32, u32),
    ) -> Result<PathBuf, ArtifactError> {
        check_place(&self.dir, trusted, owner)?;
        let found = walk(&self.dir, Some((trusted, owner)))?.digest;
        if found != self.digest {
            return Err(ArtifactError::Changed {
                expected: self.digest.clone(),
                found,
            });
        }
        if hook.as_os_str().is_empty() || !hook.components().all(is_plain) {
            return Err(ArtifactError::BadHookPath(hook.to_path_buf()));
        }
        // The walk above refused every link in the tree and `hook` is plain names, so no
        // component on the way is a link; the last is looked at without following it.
        let at = self.dir.join(hook);
        if !fs::symlink_metadata(&at).is_ok_and(|meta| meta.is_file()) {
            return Err(ArtifactError::HookMissing(hook.to_path_buf()));
        }
        Ok(at)
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
/// hex. Links are never followed: a tree holding one has no digest.
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
/// [`ArtifactError::Untrusted`] (a link included); without, a link is
/// [`ArtifactError::Unsupported`].
pub(crate) fn walk(dir: &Path, trust: Option<Trust<'_>>) -> Result<Tree, ArtifactError> {
    let mut found = Vec::new();
    list(dir, Path::new(""), trust, &mut found)?;
    found.sort_by(|a, b| a.0.as_os_str().as_bytes().cmp(b.0.as_os_str().as_bytes()));
    let mut hasher = Sha256::new();
    let mut entries = Vec::with_capacity(found.len());
    for (rel, meta) in found {
        let name = rel.as_os_str().as_bytes().to_vec();
        let mode = meta.permissions().mode() & 0o7777;
        let (kind, size, content) = if meta.is_dir() {
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

/// Every entry below `dir` (relative to the tree root `rel`), each looked at without following
/// it. Only regular files and directories are taken.
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
            let mode = meta.mode() & 0o7777;
            // One comparison: a link, a foreign owner, setuid or setgid, writable by others.
            let sound = (
                !kind.is_symlink(),
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
        } else if kind.is_file() {
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
