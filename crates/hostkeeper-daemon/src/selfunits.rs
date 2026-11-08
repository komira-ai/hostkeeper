//! The units of hostkeeper's own update that the daemon writes itself (design section 6.6), each
//! the text of its generator in `hostkeeper_steps::selfupdate`, in the host's unit directory:
//!
//! - Linux: the fallback unit [`FALLBACK_UNIT`] ([`fallback_unit`]: the previous binary's
//!   `self-update-fallback` verb, then the daemon's unit started again), and then the drop-in
//!   [`DROPIN_FILE`] in [`DROPIN_DIR`] of the daemon's unit [`MAIN_UNIT`] ([`main_dropin`]:
//!   `OnFailure=` [`FALLBACK_UNIT`], the start limit, `Restart=always`), so that `OnFailure=` never
//!   names a unit that is not there;
//! - macOS: the fallback timer job ([`launchd_fallback_job`], labelled [`fallback_label`], every
//!   [`FALLBACK_INTERVAL_SECS`] seconds), whose plist is [`fallback_label`]`.plist`;
//! - OpenRC: none. The init script of hostkeeper's Alpine package starts the packaged binary,
//!   which judges the burst from the start log and puts the previous binary back itself before
//!   it execs `current` ([`crate::packaged`]), so no file of the daemon's is needed and nothing
//!   is written: [`install`] answers [`Installed::Unchanged`].
//!
//! The paths they name are those of the binary directory ([`binary_dir`]) under the daemon's
//! state directory, the directory its unit starts `current` of.
//!
//! The daemon installs them at every start, once it is ready ([`Daemon::install_self_units`],
//! [`install`]): each file is compared with what is on disk, and only one that differs is
//! written, durably (the placer's temporary name for it beside it, `fsync`, rename, `fsync` of the
//! directory; neither systemd nor launchd reads that name, the next write goes over one a crash
//! left, and the start's reconciliation removes those of the unit directory). When any was
//! written, the service manager is told once: `daemon-reload` on Linux; on macOS the fallback job
//! is booted out and bootstrapped again from its plist ([`UnitControl::stop_unit`], then
//! [`UnitControl::start_unit`]), because the launchd unit control's reload looks only at the
//! links hostkeeper placed, never at a plain file. A start that finds every file as it should be
//! writes nothing and tells the service manager nothing.
//!
//! A file that cannot be written, or a service manager that cannot be told, is an error the
//! caller logs as an alert ([`ALERT_SELF_UNITS`]); it never stops the start. What was written
//! stays: the files are the same at every start, and systemd reads them at its next
//! `daemon-reload` (the daemon runs one after every placement) or at the next boot, as launchd
//! loads the plists of its directory at boot.

use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_launchd::{Label, PlatformKind};
use hostkeeper_placed::placer::temporary_name;
use hostkeeper_placed::{Fs, UnitControl, UnitControlError};
use hostkeeper_steps::Layout;
use hostkeeper_steps::selfupdate::{
    START_LIMIT_BURST, fallback_unit, launchd_fallback_job, main_dropin,
};

use crate::executor::Daemon;
use crate::selfupdate::binary_dir;

/// The daemon's own unit on Linux.
pub const MAIN_UNIT: &str = "hostkeeper.service";
/// The unit the drop-in names in `OnFailure=`: it puts the previous binary back.
pub const FALLBACK_UNIT: &str = "hostkeeper-fallback.service";
/// The directory of [`MAIN_UNIT`]'s drop-ins, in the unit directory.
pub const DROPIN_DIR: &str = "hostkeeper.service.d";
/// The drop-in of [`MAIN_UNIT`], in [`DROPIN_DIR`].
pub const DROPIN_FILE: &str = "self-update.conf";
/// How often, in seconds, the fallback job on macOS runs the verb, which judges the burst itself.
pub const FALLBACK_INTERVAL_SECS: u32 = 60;
/// The alert of a start that could not install these units.
pub const ALERT_SELF_UNITS: &str = "self_update_units";

/// One file of these units: the directory it goes in and its name there, as the host names
/// them, and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitFile {
    pub dir: PathBuf,
    pub name: String,
    pub text: String,
}

impl UnitFile {
    /// Where the file goes.
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    /// The temporary file it is written through, beside it: the placer's temporary name for it
    /// ([`temporary_name`]), which no unit or plist has and which the start sweeps.
    pub fn tmp(&self) -> PathBuf {
        self.dir.join(temporary_name(&self.name))
    }
}

/// What [`install`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    /// Every file was as it should be: nothing was written, and the service manager was not told.
    Unchanged,
    /// These files were written, in this order, and the service manager was told once.
    Written(Vec<PathBuf>),
}

/// Why [`install`] could not install the units. Whatever was written before it stays.
#[derive(Debug, thiserror::Error)]
pub enum SelfUnitsError {
    /// A file, or its directory, could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The files were written, and the service manager could not be told of them.
    #[error("{written:?} were written, and the service manager could not be told: {source}")]
    NotTold {
        written: Vec<PathBuf>,
        #[source]
        source: UnitControlError,
    },
}

/// The label of the fallback job on macOS: the daemon's label ([`Label::daemon`]) and
/// `.fallback`, the label [`launchd_fallback_job`] gives it.
pub fn fallback_label() -> String {
    format!("{}.fallback", Label::daemon())
}

/// The files of a host of `kind` whose unit directory is `unit_dir` and whose daemon keeps its
/// state in `state_dir`, in the order they are written.
pub fn unit_files(kind: PlatformKind, unit_dir: &Path, state_dir: &Path) -> Vec<UnitFile> {
    let layout = Layout::new(&binary_dir(state_dir));
    match kind {
        PlatformKind::Linux => vec![
            UnitFile {
                dir: unit_dir.to_path_buf(),
                name: FALLBACK_UNIT.to_owned(),
                text: fallback_unit(&layout, MAIN_UNIT),
            },
            UnitFile {
                dir: unit_dir.join(DROPIN_DIR),
                name: DROPIN_FILE.to_owned(),
                text: main_dropin(FALLBACK_UNIT, START_LIMIT_BURST),
            },
        ],
        PlatformKind::Macos => vec![UnitFile {
            dir: unit_dir.to_path_buf(),
            name: format!("{}.plist", fallback_label()),
            text: launchd_fallback_job(&layout, Label::daemon().as_str(), FALLBACK_INTERVAL_SECS),
        }],
        PlatformKind::Openrc => Vec::new(),
    }
}

/// Writes each of `files` that differs from what is on disk through `fs`, in order, then, when
/// any was written, tells the service manager of a host of `kind` once through `units` (the
/// module's rules).
pub fn install(
    kind: PlatformKind,
    files: &[UnitFile],
    fs: &dyn Fs,
    units: &dyn UnitControl,
) -> Result<Installed, SelfUnitsError> {
    let mut written = Vec::new();
    for file in files {
        let io = |source| SelfUnitsError::Io {
            path: file.path(),
            source,
        };
        if !same_on_disk(fs, file).map_err(io)? {
            put(fs, file).map_err(io)?;
            written.push(file.path());
        }
    }
    if written.is_empty() {
        return Ok(Installed::Unchanged);
    }
    match tell(kind, units) {
        Ok(()) => Ok(Installed::Written(written)),
        Err(source) => Err(SelfUnitsError::NotTold { written, source }),
    }
}

/// Whether the file at `file`'s path holds its text; a missing file does not.
fn same_on_disk(fs: &dyn Fs, file: &UnitFile) -> io::Result<bool> {
    match fs.read(&file.path()) {
        Ok(bytes) => Ok(bytes == file.text.as_bytes()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Writes `file` durably: its directory made when missing, the temporary file beside it written
/// and synced, renamed over it, the directory synced.
fn put(fs: &dyn Fs, file: &UnitFile) -> io::Result<()> {
    let tmp = file.tmp();
    fs.create_dirs(&file.dir)?;
    fs.write(&tmp, file.text.as_bytes())?;
    fs.sync_file(&tmp)?;
    fs.rename(&tmp, &file.path())?;
    fs.sync_dir(&file.dir)
}

/// Tells the service manager of a host of `kind` that the files changed: `daemon-reload` on
/// Linux; on macOS the fallback job booted out and bootstrapped from its plist again; nothing on
/// OpenRC, which has no files to tell of.
fn tell(kind: PlatformKind, units: &dyn UnitControl) -> Result<(), UnitControlError> {
    match kind {
        PlatformKind::Linux => units.daemon_reload(),
        PlatformKind::Macos => {
            let label = fallback_label();
            units.stop_unit(&label)?;
            units.start_unit(&label)
        }
        PlatformKind::Openrc => Ok(()),
    }
}

impl Daemon {
    /// Installs the units of hostkeeper's own update of a host of `kind` ([`install`]): in the
    /// unit directory of the daemon's placement, through its filesystem and its service manager,
    /// naming the binary directory under its state directory.
    pub fn install_self_units(&self, kind: PlatformKind) -> Result<Installed, SelfUnitsError> {
        let ctx = self.ctx();
        let files = unit_files(kind, ctx.placement.roots.unit_dir(), &ctx.dirs.state);
        install(kind, &files, &*ctx.placement.fs, &*ctx.units)
    }
}
