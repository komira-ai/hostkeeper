//! The updater's own update (docs/design/host-updates.md, 3.5, "Updating the updater itself").
//!
//! The updater is updated like any service, but never inside the trial it guards. After the
//! update of the release has run its steps and `verify` has succeeded, a follow-on step
//! ([`follow_on`]) stages the new updater binary beside the old, writes in the step record that
//! the update waits for the restart ([`AwaitingRestart`]), points `current` at it and tells the
//! caller to reply and exit cleanly; the unit's `Restart=always` starts the new binary. If the
//! new binary cannot start, systemd gives up after `StartLimitBurst` starts, the unit fails, and
//! its `OnFailure=` unit runs the *previous* binary's `self-update-fallback` verb, which calls
//! [`Layout::flip_back`]: `current` points at the previous binary again and the unit is started.
//!
//! The record says `Succeeded` only once the new binary has run (design section 6.6): the step
//! runner of the update holds its success back ([`crate::StepRunner::with_held_success`]), the
//! record waits for the restart (`done` at `cleanup`, with [`AwaitingRestart`]), and the binary
//! that starts next settles it ([`settle`]): the new one confirms it `Succeeded`, and any other
//! (the fallback, or a hand switch, put the previous binary back) closes it `Failed` after the
//! commit, of class release, [`REASON_SELF_UPDATE_FALLBACK`], and adds the version to the local
//! list of bad versions ([`crate::bad`]).
//!
//! On macOS the daemon runs from its LaunchDaemons plist, which `hostkeeper-launchd` renders
//! (`hostkeeper_launchd::plist::daemon_plist`: `KeepAlive` plain `true`, the counterpart of
//! `Restart=always`, so the clean exit that [`follow_on`] asks for starts the new binary), and
//! the fallback is a timer job ([`launchd_fallback_job`]) that runs the previous binary's
//! `self-update-fallback` verb every interval, since launchd has neither a start limit nor
//! `OnFailure=`.
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
use crate::bad::{BadVersions, BadVersionsError};
pub use crate::record::{AwaitingRestart, REASON_SELF_UPDATE_FALLBACK};
use crate::record::{Loaded, Record, RecordError, RecordStore, Settlement, State};
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
    /// The new binary is staged, the record waits for the restart and `current` points at the
    /// new binary: reply, then exit with status 0.
    Restart,
    /// This updater already is that version, or the update did not succeed: carry on.
    NotNeeded,
}

/// What [`settle`] found at the start of a binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// The new binary runs: the update is recorded `Succeeded`.
    Confirmed,
    /// Another binary runs: the update is recorded `Failed` after the commit, and its version is
    /// bad here.
    FellBack,
    /// No update waited for the restart; nothing was written.
    NotPending,
}

/// Why the updater's own update could not go on.
#[derive(Debug, thiserror::Error)]
pub enum SelfUpdateError {
    /// The new binary could not be taken from its verified release.
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error(transparent)]
    BadVersions(#[from] BadVersionsError),
    /// The binary directory could not be read or changed.
    #[error("cannot use the binary directory: {0}")]
    Io(#[from] io::Error),
    /// The step record holds no unfinished update to the version: there is nothing to wait for
    /// the restart.
    #[error("the step record holds no unfinished update to {version}")]
    NoUpdate { version: String },
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
    /// enough that every name the layout derives from it fits in [`MAX_NAME`] bytes
    /// ([`io::ErrorKind::InvalidFilename`] otherwise): the longest is `.<version>.seal.tmp` in a
    /// package directory and `.<version>.stage` in the binary directory. In a package directory
    /// it is also none of
    /// the directory's own names: not `current` or `previous`, and not ending in `.seal` or
    /// `.tmp`, so that a version can never be a link, a seal or a temporary file.
    pub fn check_name(&self, version: &str) -> io::Result<()> {
        check_version(version)?;
        let (affix, longest) = match self.kind {
            Kind::Package => (PACKAGE_AFFIX, "`.<version>.seal.tmp`"),
            Kind::Binary => (BINARY_AFFIX, "`.<version>.stage`"),
        };
        if version.len() + affix > MAX_NAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidFilename,
                format!(
                    "a version of {} bytes is too long for the names the layout puts beside it: \
                     {longest} would pass {MAX_NAME} bytes",
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

    /// [`io::ErrorKind::InvalidInput`] unless this is a package directory.
    pub(crate) fn expect_package(&self) -> io::Result<()> {
        self.expect_kind(Kind::Package)
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

/// What the longest name a package directory derives from a version adds to it:
/// `.<version>.seal.tmp`, the seal `hostkeeper-unpack` writes beside a package version before
/// renaming it into place (`<version>.seal` is shorter).
const PACKAGE_AFFIX: usize = ".".len() + ".seal.tmp".len();
/// What the longest name the binary directory derives from a version adds to it:
/// `.<version>.stage`, the copy [`Layout::stage`] renames into `versions/<version>`.
const BINARY_AFFIX: usize = ".".len() + ".stage".len();

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

/// The follow-on step of an update whose steps ended: when it succeeded and this updater
/// (version `running`) is not `version`, stages the updater binary of the verified release
/// `release` as `version`, writes in the record of `store` that the update waits for the restart
/// ([`AwaitingRestart`]), and only then switches to it. When this updater already is `version`,
/// the binary that runs is the new one, and an unfinished record of the update to it is finished
/// `Succeeded` here.
///
/// `binary` is the binary's path inside the release, relative and of plain names; it is taken
/// only through [`Staged::check`], so a release that changed since it was sealed, or a path that
/// leaves it, stages nothing, writes nothing and moves no link. There is no way to give this a
/// file of no verified release.
///
/// A version `current` or `previous` already names (a repeat after a crash between the switch
/// and the restart, or a return to the binary before) is not staged again: it is switched to
/// when its file holds the bytes of the verified binary, and refused, with nothing changed,
/// when it does not.
///
/// The record must hold the update to `version` unfinished: the step runner held its success
/// back ([`crate::StepRunner::with_held_success`]), so the restart interrupts nothing and no
/// binary reads the update as succeeded before the new one has run
/// ([`SelfUpdateError::NoUpdate`] otherwise, with nothing switched). The record is written before
/// the switch, so that a crash between the two leaves a record the next start settles
/// ([`settle`]); a switch that fails takes the wait back. It is safe to call again after a crash.
pub fn follow_on(
    finished: &Finished,
    store: &RecordStore,
    layout: &Layout,
    running: &str,
    version: &str,
    release: &Staged,
    binary: &Path,
) -> Result<FollowOn, SelfUpdateError> {
    let Verdict::Succeeded { promotion_pending } = finished.verdict else {
        return Ok(FollowOn::NotNeeded);
    };
    if running == version {
        finish_running(store, version, finished)?;
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
    let awaiting = AwaitingRestart {
        version: version.to_owned(),
        verdict_promotion_pending: promotion_pending,
    };
    await_restart(store, awaiting.clone())?;
    if let Err(error) = layout.switch(version) {
        // The switch did not happen: the record waits for nothing, so that a start of this
        // binary does not read it as a fallback, and a resend runs the follow-on again. Should
        // that write fail too, the record waits, and the next start settles it as a fallback.
        drop(stop_waiting(store, &awaiting));
        return Err(error.into());
    }
    Ok(FollowOn::Restart)
}

/// Under the record lock, takes back the wait for the restart [`await_restart`] wrote.
fn stop_waiting(store: &RecordStore, awaiting: &AwaitingRestart) -> Result<(), SelfUpdateError> {
    let _held = store.hold();
    let mut rec = unfinished_update(store, &awaiting.version)?;
    rec.progress.awaiting_restart = None;
    store.save(&rec)?;
    Ok(())
}

/// Under the record lock, marks the unfinished update to `awaiting.version` as waiting for the
/// restart; a record that already says so is not written again.
fn await_restart(store: &RecordStore, awaiting: AwaitingRestart) -> Result<(), SelfUpdateError> {
    let _held = store.hold();
    let mut rec = unfinished_update(store, &awaiting.version)?;
    if rec.progress.awaiting_restart.as_ref() == Some(&awaiting) {
        return Ok(());
    }
    rec.progress.awaiting_restart = Some(awaiting);
    store.save(&rec)?;
    Ok(())
}

/// Under the record lock, finishes the unfinished update to `version`, which this binary is, as
/// `finished` says; a record of anything else is left as it is.
fn finish_running(
    store: &RecordStore,
    version: &str,
    finished: &Finished,
) -> Result<(), SelfUpdateError> {
    let _held = store.hold();
    let mut rec = match unfinished_update(store, version) {
        Ok(rec) => rec,
        Err(SelfUpdateError::NoUpdate { .. }) => return Ok(()),
        Err(error) => return Err(error),
    };
    rec.progress.awaiting_restart = None;
    rec.state = State::Finished {
        verdict: finished.verdict.clone(),
        reason: finished.reason.clone(),
    };
    store.save(&rec)?;
    Ok(())
}

/// The record of `store` when it holds an unfinished update to `version`.
fn unfinished_update(store: &RecordStore, version: &str) -> Result<Box<Record>, SelfUpdateError> {
    let no_update = || SelfUpdateError::NoUpdate {
        version: version.to_owned(),
    };
    let Loaded::Current(rec) = store.load()? else {
        return Err(no_update());
    };
    let unfinished = !matches!(rec.state, State::Finished { .. });
    match unfinished & (rec.target_version == version) {
        true => Ok(rec),
        false => Err(no_update()),
    }
}

/// At the start of a binary of version `running`: settles the update of the record of `store`
/// that waits for the restart, if there is one ([`settle_with`] with nothing to do before a
/// confirmation).
pub fn settle(
    layout: &Layout,
    store: &RecordStore,
    running: &str,
) -> Result<Settled, SelfUpdateError> {
    settle_with(layout, store, running, &mut || Ok(()))
}

/// At the start of a binary of version `running`: settles the update of the record of `store`
/// that waits for the restart (design section 6.6).
///
/// - No update waits ([`Settled::NotPending`]): nothing is written.
/// - The version it waits for is `running`, and `current` of `layout` names it: the new binary
///   runs. `before_confirm` runs first, then the record is rewritten `Succeeded` in one write
///   ([`Settled::Confirmed`]). A crash between the two leaves the record waiting, and the next
///   start confirms again, so `before_confirm` must be safe to run again.
/// - Otherwise the binary that runs is not the new one: the version is added to the list of bad
///   versions in the store's directory ([`BadVersions`]), then the record is rewritten `Failed`
///   after the commit, of class release, [`REASON_SELF_UPDATE_FALLBACK`] ([`Settled::FellBack`]).
///   The list is written first, so that a crash between the two never loses the mark.
///
/// Once settled the record waits for nothing, so a second call writes nothing.
pub fn settle_with(
    layout: &Layout,
    store: &RecordStore,
    running: &str,
    before_confirm: &mut dyn FnMut() -> Result<(), SelfUpdateError>,
) -> Result<Settled, SelfUpdateError> {
    let awaiting = match store.load()? {
        Loaded::Current(rec) => rec.progress.awaiting_restart,
        Loaded::Absent | Loaded::Newer { .. } => None,
    };
    let Some(awaiting) = awaiting else {
        return Ok(Settled::NotPending);
    };
    let current = layout.current()?;
    let confirmed = (awaiting.version == running) & (current.as_deref() == Some(running));
    if confirmed {
        before_confirm()?;
        store.settle_awaiting_restart(Settlement::Confirmed)?;
        return Ok(Settled::Confirmed);
    }
    BadVersions::load(store.dir())?.add(&awaiting.version)?;
    store.settle_awaiting_restart(Settlement::FellBack)?;
    Ok(Settled::FellBack)
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

/// The plist of the fallback timer job on macOS, labelled `<label>.fallback`: every
/// `interval_secs` it runs the previous binary's `self-update-fallback` verb, the command
/// [`fallback_unit`] runs, so the verb is the same code on both systems.
///
/// launchd has no start limit and no `OnFailure=` to run the job when the daemon fails its
/// burst, so the job runs on a timer. What the verb does when it runs, such as flipping
/// `current` back ([`Layout::flip_back`]), is the verb's; this function only produces the text.
/// The job does not run at load.
pub fn launchd_fallback_job(layout: &Layout, label: &str, interval_secs: u32) -> String {
    let dir = xml_escape(&layout.dir().display().to_string());
    let label = xml_escape(label);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key>\n\
         \x20 <string>{label}.fallback</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>{dir}/previous</string>\n\
         \x20   <string>self-update-fallback</string>\n\
         \x20   <string>--dir</string>\n\
         \x20   <string>{dir}</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key>\n\
         \x20 <false/>\n\
         \x20 <key>StartInterval</key>\n\
         \x20 <integer>{interval_secs}</integer>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// `text` for the inside of an XML element: `&`, `<`, `>`, `"` and `'` become entities.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}
