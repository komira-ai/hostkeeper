//! The updater's own update (docs/design/host-updates.md, 3.5, "Updating the updater itself").
//!
//! The updater is updated like any service, but never inside the trial it guards. After the
//! update of the release has finished and `verify` has succeeded, a follow-on step
//! ([`follow_on`]) stages the new updater binary beside the old, points `current` at it and tells
//! the caller to reply and exit cleanly; the
//! unit's `Restart=always` starts the new binary. If the new binary cannot start, systemd gives
//! up after `StartLimitBurst` starts, the unit fails, and its `OnFailure=` unit runs the
//! *previous* binary's `self-update-fallback` verb, which calls [`Layout::flip_back`]: `current`
//! points at the previous binary again and the unit is started.
//!
//! Two kinds of [`Layout`] share the switch:
//!
//! The updater's binary directory ([`Layout::new`]):
//!
//! ```text
//! <dir>/versions/<version>    one executable per staged version
//! <dir>/versions/.<version>.stage   a version being staged (no version starts with `.`)
//! <dir>/current  -> versions/<version>   what the unit runs
//! <dir>/previous -> versions/<version>   what it ran before the last switch
//! ```
//!
//! A package directory, `/var/lib/hostkeeper/packages/<service>` ([`Layout::package`]; the
//! format is public, docs/package-format.md):
//!
//! ```text
//! <dir>/<version>/        the unpacked tree of a version, root-owned, read-only
//! <dir>/<version>.seal    its seal (crate::seal), written before the tree is renamed in
//! <dir>/.stage-<random>/  a tree being unpacked (no version starts with `.`)
//! <dir>/current  -> <version>   the version the host runs
//! <dir>/previous -> <version>   the version it ran before the last switch
//! ```
//!
//! Link targets are relative. Every move is a rename of a link made beside the old one, then a
//! directory fsync, so a crash leaves either link whole.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{MAX_NAME, is_version};
use hostkeeper_hook::Verdict;

use crate::artifact::{ArtifactError, Staged, host_owner, trusted_owners, walk};
use crate::runner::Finished;
use crate::seal::{Seal, read_seal};

/// The failed starts after which the fallback runs.
pub const START_LIMIT_BURST: u32 = 3;

/// A directory of versions with a `current` and a `previous` link: the updater's own binaries
/// ([`Layout::new`]) or the unpacked versions of one package ([`Layout::package`]).
///
/// **The caller gives a directory that is root-owned and closed to group and others, in
/// root-owned parents**: this type does not check it, and the fallback unit runs what `previous`
/// points at as root.
#[derive(Clone, Debug)]
pub struct Layout {
    dir: PathBuf,
    kind: Kind,
}

/// What a version of a layout is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// One executable, `versions/<version>`.
    Binary,
    /// A sealed tree, `<version>/` with `<version>.seal`.
    Package,
}

/// What the caller does after [`follow_on`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowOn {
    /// The new binary is staged and `current` points at it: reply, then exit with status 0.
    Restart,
    /// This updater already is that version, or the update did not succeed: carry on.
    NotNeeded,
}

impl Layout {
    /// The updater's binary directory: each version one executable in `versions/`.
    pub fn new(dir: &Path) -> Layout {
        Layout {
            dir: dir.to_path_buf(),
            kind: Kind::Binary,
        }
    }

    /// A package directory, `packages/<service>`: each version a sealed tree `<version>/` with
    /// its seal `<version>.seal` beside it.
    pub fn package(dir: &Path) -> Layout {
        Layout {
            dir: dir.to_path_buf(),
            kind: Kind::Package,
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The link the unit runs.
    pub fn current_path(&self) -> PathBuf {
        self.dir.join("current")
    }

    /// The link to the binary run before the last switch.
    pub fn previous_path(&self) -> PathBuf {
        self.dir.join("previous")
    }

    /// Where version `version` is: `versions/<version>` for the binary directory,
    /// `<version>` for a package directory. `version` is not checked here.
    pub fn version_path(&self, version: &str) -> PathBuf {
        self.dir.join(self.link_target(version))
    }

    /// Where the seal of version `version` of a package directory is: `<version>.seal`.
    /// `version` is not checked here.
    pub fn seal_path(&self, version: &str) -> PathBuf {
        self.dir.join(format!("{version}.seal"))
    }

    /// What `current` and `previous` hold when they name `version`: relative to the directory.
    fn link_target(&self, version: &str) -> PathBuf {
        match self.kind {
            Kind::Binary => Path::new("versions").join(version),
            Kind::Package => PathBuf::from(version),
        }
    }

    /// The version `current` points at, `None` before the first switch.
    pub fn current(&self) -> io::Result<Option<String>> {
        link_version(&self.current_path())
    }

    /// The version `previous` points at.
    pub fn previous(&self) -> io::Result<Option<String>> {
        link_version(&self.previous_path())
    }

    /// Puts `binary` beside the others as version `version`, mode 0755, durably. Changes no link.
    /// For the binary directory only ([`io::ErrorKind::InvalidInput`] for a package directory,
    /// whose versions are trees: [`Layout::stage_tree`]).
    ///
    /// The version `current` or `previous` points at is refused
    /// ([`io::ErrorKind::AlreadyExists`]) and left as it is: a crash between staging over it and
    /// the switch would leave the unit, or its fallback, running a binary that was never the one
    /// it switched to. The copy is made under a name no version can have (`.<version>.stage`),
    /// created afresh, so that staging one version never writes over the file of another.
    pub fn stage(&self, version: &str, binary: &Path) -> io::Result<()> {
        self.expect_kind(Kind::Binary)?;
        self.check_stageable(version)?;
        let versions = self.dir.join("versions");
        fs::create_dir_all(&versions)?;
        let tmp = versions.join(format!(".{version}.stage"));
        // What an earlier staging of this version left when it crashed; a directory there is
        // not one, and is an error.
        match fs::remove_file(&tmp) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&tmp)?;
        io::copy(&mut File::open(binary)?, &mut out)?;
        out.set_permissions(Permissions::from_mode(0o755))?;
        out.sync_all()?;
        fs::rename(&tmp, self.version_path(version))?;
        File::open(&versions)?.sync_all()
    }

    /// Moves the directory `tree` in as version `version` of a package directory, durably, and
    /// changes no link. For a package directory only ([`io::ErrorKind::InvalidInput`] for the
    /// binary directory). `tree` must be a real directory named `.stage-*` directly in the
    /// package directory ([`io::ErrorKind::InvalidInput`] otherwise, before anything is
    /// touched): so it is never a version, never the destination and never inside one. The
    /// caller writes `<version>.seal` first, so a version directory never exists without its
    /// seal.
    ///
    /// The refusals of [`Layout::stage`] hold: the version `current` or `previous` points at is
    /// refused ([`io::ErrorKind::AlreadyExists`]) and left as it is, and only `<version>` is
    /// written: a `<version>` directory already there, staged before and never switched to or
    /// left by a crash, is removed before the rename, and no other version is touched.
    pub fn stage_tree(&self, version: &str, tree: &Path) -> io::Result<()> {
        self.expect_kind(Kind::Package)?;
        self.check_stageable(version)?;
        let staging = tree
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().starts_with(b".stage-"));
        let beside = tree.parent() == Some(self.dir.as_path());
        let dir = fs::symlink_metadata(tree).is_ok_and(|meta| meta.is_dir());
        if !(staging & beside & dir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a package version is staged from a .stage-* directory of the package directory",
            ));
        }
        let at = self.version_path(version);
        remove_tree(&at)?;
        fs::rename(tree, &at)?;
        File::open(&self.dir)?.sync_all()
    }

    /// The version is a plain name that `current` and `previous` do not point at.
    fn check_stageable(&self, version: &str) -> io::Result<()> {
        self.check_name(version)?;
        if self.in_place(version)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the version `current` or `previous` points at is not staged over",
            ));
        }
        Ok(())
    }

    /// A version keeps [`is_version`] ([`io::ErrorKind::InvalidInput`] otherwise), and is short
    /// enough that every name the layout derives from it, the longest being
    /// `.<version>.seal.tmp`, fits in [`MAX_NAME`] bytes ([`io::ErrorKind::InvalidFilename`]
    /// otherwise). In a package directory it is also none of
    /// the directory's own names: not `current` or `previous`, and not ending in `.seal` or
    /// `.tmp`, so that a version can never be a link, a seal or a temporary file.
    pub fn check_name(&self, version: &str) -> io::Result<()> {
        check_version(version)?;
        if version.len() + LONGEST_AFFIX > MAX_NAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidFilename,
                format!(
                    "a version of {} bytes is too long for the names the layout puts beside it: \
                     `.<version>.seal.tmp` would pass {MAX_NAME} bytes",
                    version.len()
                ),
            ));
        }
        let own = (version == "current")
            | (version == "previous")
            | version.ends_with(".seal")
            | version.ends_with(".tmp");
        if (self.kind == Kind::Package) & own {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a version may not be a name the package directory holds for itself",
            ));
        }
        Ok(())
    }

    fn expect_kind(&self, kind: Kind) -> io::Result<()> {
        if self.kind != kind {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a binary directory stages files and a package directory stages trees",
            ));
        }
        Ok(())
    }

    /// Points `current` at the staged `version` and `previous` at what `current` was. Running it
    /// again changes nothing. The version must be an executable file (binary directory) or a
    /// sealed tree that still has its seal's digest ([`Layout::check_tree`]).
    pub fn switch(&self, version: &str) -> io::Result<()> {
        self.check(version)?;
        let before = self.current()?;
        if before.as_deref() == Some(version) {
            return Ok(());
        }
        if let Some(before) = before {
            self.set_link("previous", &before)?;
        }
        self.set_link("current", version)
    }

    /// Points `current` at `previous` again. `false` when there is no `previous` or `current`
    /// already points at it; an error, with `current` unchanged, when what `previous` names is
    /// not an executable file or a sealed tree with its seal's digest, whichever `current`
    /// points at: the fallback reports a fallback it cannot run.
    pub fn flip_back(&self) -> io::Result<bool> {
        let Some(previous) = self.previous()? else {
            return Ok(false);
        };
        self.check(&previous)?;
        if self.current()?.as_deref() == Some(previous.as_str()) {
            return Ok(false);
        }
        self.set_link("current", &previous)?;
        Ok(true)
    }

    /// What a switch to `version` requires, by the kind of layout.
    fn check(&self, version: &str) -> io::Result<()> {
        match self.kind {
            Kind::Binary => self.check_binary(version),
            Kind::Package => self.check_tree(version),
        }
    }

    /// `versions/<version>` is a regular file with an execute bit, not a link (dangling or not)
    /// and not a directory: what staging makes. [`io::ErrorKind::NotFound`] when there is
    /// nothing, [`io::ErrorKind::InvalidData`] when it is something else.
    fn check_binary(&self, version: &str) -> io::Result<()> {
        check_version(version)?;
        let meta = fs::symlink_metadata(self.version_path(version))?;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("versions/{version} is not an executable file"),
            ));
        }
        Ok(())
    }

    /// Version `version` of a package directory is whole: `<version>` is a real directory (not
    /// a link, dangling or not) owned by root or the running user and closed to group and
    /// others, every entry in it is a regular file or a directory under the trust rules of
    /// [`Staged::check_as`], its seal `<version>.seal` exists and parses, and the tree's digest
    /// equals the seal's `tree_digest`. [`io::ErrorKind::NotFound`] when there is no
    /// `<version>`, [`io::ErrorKind::InvalidData`] when it is anything else, and an I/O error as
    /// it came (it says nothing about the tree: the caller tries again). For a package directory
    /// only ([`io::ErrorKind::InvalidInput`] otherwise).
    pub fn check_tree(&self, version: &str) -> io::Result<()> {
        self.check_tree_as(version, &trusted_owners(), &host_owner)
    }

    /// [`Layout::check_tree`] with the trusted owners and the way an owner is read given, so
    /// that a test can refuse a foreign owner without a second user.
    pub fn check_tree_as(
        &self,
        version: &str,
        trusted: &[u32],
        owner: &dyn Fn(&fs::Metadata) -> (u32, u32),
    ) -> io::Result<()> {
        self.expect_kind(Kind::Package)?;
        self.check_name(version)?;
        let bad = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
        let dir = self.version_path(version);
        let meta = fs::symlink_metadata(&dir)?;
        // One comparison: a real directory, a trusted owner, closed to group and others.
        let sound = (
            meta.is_dir(),
            trusted.contains(&owner(&meta).0),
            meta.permissions().mode() & 0o022 == 0,
        );
        if sound != (true, true, true) {
            return Err(bad(format!(
                "{version} is not a directory owned by root or the updater and closed to group and others"
            )));
        }
        let seal = match read_seal(&self.seal_path(version)) {
            Ok(bytes) => Seal::parse(&bytes).map_err(|e| bad(format!("{version}.seal: {e}")))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(bad(format!("{version} has no seal")));
            }
            Err(e) => return Err(e),
        };
        let tree = match walk(&dir, Some((trusted, owner))) {
            Ok(tree) => tree,
            Err(ArtifactError::Io(e)) => return Err(e),
            Err(e) => return Err(bad(format!("{version}: {e}"))),
        };
        if tree.digest != seal.tree_digest() {
            return Err(bad(format!(
                "{version} changed after it was sealed: it was {}, it is {}",
                seal.tree_digest(),
                tree.digest
            )));
        }
        Ok(())
    }

    /// Whether `current` or `previous` points at `version`.
    fn in_place(&self, version: &str) -> io::Result<bool> {
        let named = [self.current()?, self.previous()?];
        Ok(named.iter().any(|v| v.as_deref() == Some(version)))
    }

    fn set_link(&self, name: &str, version: &str) -> io::Result<()> {
        let tmp = self.dir.join(format!("{name}.tmp"));
        match fs::remove_file(&tmp) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        symlink(self.link_target(version), &tmp)?;
        fs::rename(&tmp, self.dir.join(name))?;
        File::open(&self.dir)?.sync_all()
    }
}

/// Removes `path` and everything below it, never following a link; nothing there is fine. A
/// directory is opened to its owner first, so that a read-only tree (0555) can be emptied.
fn remove_tree(path: &Path) -> io::Result<()> {
    // What cannot be looked at is left to the rename that follows, which then reports it.
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if !meta.is_dir() {
        return fs::remove_file(path);
    }
    fs::set_permissions(path, Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(path)? {
        remove_tree(&entry?.path())?;
    }
    fs::remove_dir(path)
}

fn link_version(link: &Path) -> io::Result<Option<String>> {
    match fs::read_link(link) {
        Ok(target) => Ok(target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// What the longest name the layout derives from a version adds to it: `.<version>.seal.tmp`,
/// the seal `hostkeeper-unpack` writes beside a package version before renaming it into place
/// (`<version>.seal` and the binary directory's `.<version>.stage` are shorter).
const LONGEST_AFFIX: usize = ".".len() + ".seal.tmp".len();

/// A version names a file: it keeps [`is_version`].
fn check_version(version: &str) -> io::Result<()> {
    if !is_version(version) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a version that is not a plain file name",
        ));
    }
    Ok(())
}

/// The follow-on step of an update that finished: when it succeeded and this updater (version
/// `running`) is not `version`, stages the updater binary of the verified release `release` as
/// `version` and switches to it.
///
/// `binary` is the binary's path inside the release, relative and of plain names; it is taken
/// only through [`Staged::check`], so a release that changed since it was sealed, or a path that
/// leaves it, stages nothing and moves no link. There is no way to give this a file of no
/// verified release.
///
/// A version `current` or `previous` already names (a repeat after a crash between the switch
/// and the restart, or a return to the binary before) is not staged again: it is switched to
/// when its file holds the bytes of the verified binary, and refused, with nothing changed,
/// when it does not.
///
/// The record of the update is finished before this is called ([`crate::StepRunner::run`]
/// returned), so the restart interrupts nothing. It is safe to call again after a crash, and
/// the new binary, started, finds `running == version` and does nothing.
pub fn follow_on(
    finished: &Finished,
    layout: &Layout,
    running: &str,
    version: &str,
    release: &Staged,
    binary: &Path,
) -> Result<FollowOn, ArtifactError> {
    if !matches!(finished.verdict, Verdict::Succeeded { .. }) || running == version {
        return Ok(FollowOn::NotNeeded);
    }
    let new_binary = release.check(binary)?;
    if layout.in_place(version)? {
        let same = fs::read(layout.version_path(version))? == fs::read(&new_binary)?;
        if !same {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the version in place holds another binary than the verified release",
            )
            .into());
        }
    } else {
        layout.stage(version, &new_binary)?;
    }
    layout.switch(version)?;
    Ok(FollowOn::Restart)
}

/// The drop-in for the updater's own unit: restart when it exits, and run the fallback unit
/// once it has failed `burst` starts.
pub fn main_dropin(fallback_unit: &str, burst: u32) -> String {
    format!(
        "[Unit]\nOnFailure={fallback_unit}\nStartLimitIntervalSec=300\nStartLimitBurst={burst}\n\n[Service]\nRestart=always\nRestartSec=1\n"
    )
}

/// The fallback unit: the previous binary flips `current` back and the main unit is started.
pub fn fallback_unit(layout: &Layout, main_unit: &str) -> String {
    let dir = layout.dir().display();
    format!(
        "[Unit]\nDescription=Put the previous updater binary back after the new one failed to start\n\n[Service]\nType=oneshot\nExecStart={dir}/previous self-update-fallback --dir {dir}\nExecStartPost=/usr/bin/systemctl reset-failed {main_unit}\nExecStartPost=/usr/bin/systemctl start --no-block {main_unit}\n"
    )
}
