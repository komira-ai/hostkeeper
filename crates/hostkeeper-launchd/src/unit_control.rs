//! The linking side's [`UnitControl`] over `launchctl`.
//!
//! The plists hostkeeper placed are the entries of the unit directory named `<label>.plist` that
//! are symbolic links (placed paths are links into `current`); `daemon_reload` never reads or
//! touches a regular file there. `stop_unit` and `start_unit` act on the label they are given,
//! whatever its plist is: the daemon's own fallback job, a regular file it writes itself, is
//! loaded again that way. `daemon_reload` has no launchd counterpart, so
//! the control keeps a snapshot of those links, each with the device and inode of the file it
//! resolves to, and acts on what differs from the snapshot of the last successful reload: a job
//! whose link is new or re-pointed is booted out when it is loaded and then bootstrapped from its
//! link, and a loaded job whose link is gone is booted out. After a reload launchd holds the job
//! of every link that changed, as it holds every plist of the directory after a boot; loading a
//! job is not starting it (a plist's `RunAtLoad` and `KeepAlive` say whether launchd runs it), so
//! the hook that `kickstart`s a job finds it loaded, as a systemd hook finds a unit file known
//! after `daemon-reload`.
//!
//! The first snapshot is taken at construction: a link that was there before the daemon started
//! is taken to have been loaded by launchd at boot.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use hostkeeper_placed::unit_control::{UnitControl, UnitControlError};

use crate::label::Label;
use crate::launchctl::{
    JobState, JobStatus, Launchctl, bootout, bootout_args, bootstrap_args, kickstart_args, print,
    print_args,
};

/// The suffix of a plist's file name.
const PLIST_SUFFIX: &str = ".plist";

/// The device and inode of the file a link resolves to, per label.
type Snapshot = BTreeMap<Label, (u64, u64)>;

/// [`UnitControl`] by `launchctl`.
#[derive(Debug)]
pub struct LaunchdUnitControl<L> {
    launchctl: L,
    unit_dir: PathBuf,
    seen: Mutex<Snapshot>,
}

/// The links of `unit_dir` named `<label>.plist` and what each resolves to. A link that does not
/// resolve is left out, as if it were gone; a name that is not a label is an error.
fn snapshot(unit_dir: &Path) -> io::Result<Snapshot> {
    let mut found = Snapshot::new();
    for entry in std::fs::read_dir(unit_dir)?.flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(PLIST_SUFFIX)) else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            continue;
        }
        let label = Label::parse(stem).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", entry.path().display()),
            )
        })?;
        if let Ok(meta) = std::fs::metadata(entry.path()) {
            found.insert(label, (meta.dev(), meta.ino()));
        }
    }
    Ok(found)
}

/// The error of the command `launchctl <args>`.
fn failure(args: &[String], why: String) -> UnitControlError {
    UnitControlError {
        action: format!("launchctl {}", args.join(" ")),
        why,
    }
}

impl<L: Launchctl> LaunchdUnitControl<L> {
    /// A control over `launchctl` for the plists in `unit_dir`, with the links there now as its
    /// first snapshot. An unreadable directory, or a link whose name is not a label, is an error.
    pub fn new(launchctl: L, unit_dir: &Path) -> io::Result<LaunchdUnitControl<L>> {
        Ok(LaunchdUnitControl {
            launchctl,
            unit_dir: unit_dir.to_path_buf(),
            seen: Mutex::new(snapshot(unit_dir)?),
        })
    }

    fn label(unit: &str, verb: &str) -> Result<Label, UnitControlError> {
        Label::parse(unit)
            .map_err(|e| failure(&[verb.to_owned(), format!("system/{unit}")], e.to_string()))
    }

    fn plist_path(&self, label: &Label) -> PathBuf {
        self.unit_dir.join(format!("{label}{PLIST_SUFFIX}"))
    }

    fn status(&self, label: &Label) -> Result<JobStatus, UnitControlError> {
        print(&self.launchctl, label).map_err(|e| failure(&print_args(label), e.to_string()))
    }

    /// Runs one command that must exit 0.
    fn run(&self, args: &[String]) -> Result<(), UnitControlError> {
        let output = self
            .launchctl
            .run(args)
            .map_err(|e| failure(args, e.to_string()))?;
        match output.code {
            Some(0) => Ok(()),
            code => Err(failure(
                args,
                format!(
                    "{}: {}",
                    code.map_or_else(|| "ended by a signal".to_owned(), |c| format!("exit {c}")),
                    output.stderr_text()
                ),
            )),
        }
    }

    fn bootstrap(&self, label: &Label) -> Result<(), UnitControlError> {
        self.run(&bootstrap_args(&self.plist_path(label)))
    }

    fn stop(&self, label: &Label) -> Result<(), UnitControlError> {
        bootout(&self.launchctl, label).map_err(|e| failure(&bootout_args(label), e.to_string()))
    }

    /// Loads `label` from its link when `relink` is set, booting it out first when it is
    /// loaded; when `relink` is not set (its link is gone) only boots it out, if it is loaded.
    fn reload_one(&self, label: &Label, relink: bool) -> Result<(), UnitControlError> {
        if let JobStatus::Loaded(_) = self.status(label)? {
            self.stop(label)?;
        }
        match relink {
            true => self.bootstrap(label),
            false => Ok(()),
        }
    }
}

impl<L: Launchctl> UnitControl for LaunchdUnitControl<L> {
    /// Acts on the links that are new, re-pointed or gone since the last successful reload; the
    /// snapshot is replaced only when every action succeeded, so a failed reload is tried again.
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        let now = snapshot(&self.unit_dir).map_err(|e| UnitControlError {
            action: format!("read {}", self.unit_dir.display()),
            why: e.to_string(),
        })?;
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let labels: BTreeSet<&Label> = seen.keys().chain(now.keys()).collect();
        for label in labels {
            let identity = now.get(label);
            if seen.get(label) != identity {
                self.reload_one(label, identity.is_some())?;
            }
        }
        *seen = now;
        Ok(())
    }

    /// `launchctl bootout system/<unit>`; a job that is not loaded is stopped already.
    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        self.stop(&Self::label(unit, "bootout")?)
    }

    /// Bootstraps the plist when the job is not loaded, kickstarts it when it is loaded and not
    /// running, and does nothing when it is running.
    fn start_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        let label = Self::label(unit, "kickstart")?;
        match self.status(&label)? {
            JobStatus::NotLoaded => self.bootstrap(&label),
            JobStatus::Loaded(info) if info.state == JobState::Running => Ok(()),
            JobStatus::Loaded(_) => self.run(&kickstart_args(&label)),
        }
    }

    /// Whether the job is loaded and running; a job that is not loaded is not active.
    fn is_active(&self, unit: &str) -> Result<bool, UnitControlError> {
        let label = Self::label(unit, "print")?;
        Ok(matches!(
            self.status(&label)?,
            JobStatus::Loaded(info) if info.state == JobState::Running
        ))
    }
}
