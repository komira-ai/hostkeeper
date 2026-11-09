//! The units of hostkeeper's own update that the releases before the hand-over wrote
//! (docs/design/two-programs.md, section 4.3), which the daemon now removes at start:
//!
//! - Linux: the drop-in [`DROPIN_FILE`] in [`DROPIN_DIR`] of the daemon's unit [`MAIN_UNIT`]
//!   (`OnFailure=` [`FALLBACK_UNIT`], a start limit of three starts in 300 seconds), and the
//!   fallback unit [`FALLBACK_UNIT`], in the host's unit directory;
//! - macOS: the fallback timer job, whose plist is [`fallback_label`]`.plist` in the unit
//!   directory, loaded under the label [`fallback_label`];
//! - OpenRC: none. No release before the hand-over ran on an OpenRC host, so there is nothing to
//!   remove, and nothing is looked at or told: [`remove`] answers [`Removed::Unchanged`].
//!
//! A release before the hand-over armed them when it updated hostkeeper the old way, and they put
//! the previous binary back when the new one could not start. The hand-over has no unit and no
//! job: the old binary execs the new one, three tries ([`crate::handover`]), so the two must
//! never be armed at once. The daemon removes what it finds at every start, once it is ready
//! ([`Daemon::remove_self_units`], [`remove`]). On Linux the drop-in goes first, so that
//! `OnFailure=` never names a unit that is not there; then systemd is told once
//! (`daemon-reload`: without the drop-in the unit has no start limit again, and a daemon that
//! cannot start is restarted every second); and only then the fallback unit goes. So a start
//! whose reload fails, or that fails before it, leaves the fallback unit on disk, and the next
//! start, finding it, reloads again before it removes it: a reload is never lost. On macOS the job
//! is booted out ([`UnitControl::stop_unit`]) before its plist is removed, since the launchd unit
//! control's reload looks only at the links hostkeeper placed, never at a plain file. A start
//! that finds none of them removes nothing and tells the service manager nothing.
//!
//! A file that cannot be removed, or a service manager that cannot be told, is an error the
//! caller logs as an alert ([`ALERT_SELF_UNITS`]) and answers every self-update `busy` for
//! ([`crate::executor::Ctx::hold_self_updates`]) until the next start, which tries again. What
//! was removed stays removed.
//!
//! The verb `self-update-fallback`, which the unit and the job of those releases run, is still
//! answered ([`crate::fallback`]); so is the start log a macOS daemon writes at each start, which
//! the job of the release that armed it reads.

use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_launchd::{Label, PlatformKind};
use hostkeeper_placed::{FileKind, Fs, UnitControl, UnitControlError};

use crate::executor::Daemon;

/// The daemon's own unit on Linux.
pub const MAIN_UNIT: &str = "hostkeeper.service";
/// The unit the drop-in names in `OnFailure=`: it put the previous binary back.
pub const FALLBACK_UNIT: &str = "hostkeeper-fallback.service";
/// The directory of [`MAIN_UNIT`]'s drop-ins, in the unit directory.
pub const DROPIN_DIR: &str = "hostkeeper.service.d";
/// The drop-in of [`MAIN_UNIT`], in [`DROPIN_DIR`].
pub const DROPIN_FILE: &str = "self-update.conf";
/// The alert of a start that could not remove these units.
pub const ALERT_SELF_UNITS: &str = "self_update_units";

/// What [`remove`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Removed {
    /// None of the files was there: nothing was removed, and the service manager was not told.
    Unchanged,
    /// These paths were removed, in this order, and the service manager was told once (on Linux
    /// before the fallback unit was removed).
    Gone(Vec<PathBuf>),
}

/// Why [`remove`] could not remove the units. Whatever was removed before it stays removed.
#[derive(Debug, thiserror::Error)]
pub enum SelfUnitsError {
    /// A file, or its directory, could not be read or removed.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The service manager could not be told: on Linux after `removed` (the drop-in and its
    /// directory, never the fallback unit) were removed, on macOS (`removed` empty) before the
    /// job's plist was.
    #[error("{removed:?} were removed, and the service manager could not be told: {source}")]
    NotTold {
        removed: Vec<PathBuf>,
        #[source]
        source: UnitControlError,
    },
}

/// The label of the fallback job on macOS: the daemon's label ([`Label::daemon`]) and
/// `.fallback`, the label the job of the releases before the hand-over carried.
pub fn fallback_label() -> String {
    format!("{}.fallback", Label::daemon())
}

/// Removes what the releases before the hand-over wrote on a host of `kind` whose unit directory
/// is `unit_dir`, through `fs`, in the module's order, telling the service manager through `units`
/// once when anything was there.
pub fn remove(
    kind: PlatformKind,
    unit_dir: &Path,
    fs: &dyn Fs,
    units: &dyn UnitControl,
) -> Result<Removed, SelfUnitsError> {
    match kind {
        PlatformKind::Linux => remove_linux(unit_dir, fs, units),
        PlatformKind::Macos => remove_macos(unit_dir, fs, units),
        PlatformKind::Openrc => Ok(Removed::Unchanged),
    }
}

fn remove_linux(
    unit_dir: &Path,
    fs: &dyn Fs,
    units: &dyn UnitControl,
) -> Result<Removed, SelfUnitsError> {
    let dropin_dir = unit_dir.join(DROPIN_DIR);
    let mut gone = Vec::new();
    // The drop-in first: `OnFailure=` never names a unit that is not there.
    take(fs, &dropin_dir.join(DROPIN_FILE), unit_dir, &mut gone)?;
    let at = |source| SelfUnitsError::Io {
        path: dropin_dir.clone(),
        source,
    };
    // Its directory goes once it is empty; one that holds another drop-in stays.
    if !gone.is_empty() && fs.list(&dropin_dir).map_err(at)?.is_empty() {
        fs.remove_dir(&dropin_dir).map_err(at)?;
        fs.sync_dir(unit_dir).map_err(at)?;
        gone.push(dropin_dir.clone());
    }
    let unit = unit_dir.join(FALLBACK_UNIT);
    let unit_there = fs
        .kind(&unit)
        .map_err(|source| SelfUnitsError::Io {
            path: unit.clone(),
            source,
        })?
        .is_some();
    if gone.is_empty() & !unit_there {
        return Ok(Removed::Unchanged);
    }
    // Told before the fallback unit goes: a reload that fails leaves the unit for the next start,
    // which reloads again.
    if let Err(source) = units.daemon_reload() {
        return Err(SelfUnitsError::NotTold {
            removed: gone,
            source,
        });
    }
    take(fs, &unit, unit_dir, &mut gone)?;
    Ok(Removed::Gone(gone))
}

fn remove_macos(
    unit_dir: &Path,
    fs: &dyn Fs,
    units: &dyn UnitControl,
) -> Result<Removed, SelfUnitsError> {
    let plist = unit_dir.join(format!("{}.plist", fallback_label()));
    let io = |source| SelfUnitsError::Io {
        path: plist.clone(),
        source,
    };
    if fs.kind(&plist).map_err(io)?.is_none() {
        return Ok(Removed::Unchanged);
    }
    // Booted out before the plist goes, so that no timer outlives its file.
    units
        .stop_unit(&fallback_label())
        .map_err(|source| SelfUnitsError::NotTold {
            removed: Vec::new(),
            source,
        })?;
    let mut gone = Vec::new();
    take(fs, &plist, unit_dir, &mut gone)?;
    Ok(Removed::Gone(gone))
}

/// Removes the file at `path` when it is there, and syncs `dir`; its path is added to `gone`.
fn take(
    fs: &dyn Fs,
    path: &Path,
    dir: &Path,
    gone: &mut Vec<PathBuf>,
) -> Result<(), SelfUnitsError> {
    let io = |source| SelfUnitsError::Io {
        path: path.to_path_buf(),
        source,
    };
    match fs.kind(path).map_err(io)? {
        None => Ok(()),
        Some(FileKind::Dir) => Err(io(io::Error::other("a directory, not a file"))),
        Some(_) => {
            fs.remove_file(path).map_err(io)?;
            fs.sync_dir(path.parent().unwrap_or(dir)).map_err(io)?;
            gone.push(path.to_path_buf());
            Ok(())
        }
    }
}

impl Daemon {
    /// Removes the units of the releases before the hand-over from a host of `kind`
    /// ([`remove`]): from the unit directory of the daemon's placement, through its filesystem
    /// and its service manager.
    pub fn remove_self_units(&self, kind: PlatformKind) -> Result<Removed, SelfUnitsError> {
        let ctx = self.ctx();
        remove(
            kind,
            ctx.placement.roots.unit_dir(),
            &*ctx.placement.fs,
            &*ctx.units,
        )
    }
}
