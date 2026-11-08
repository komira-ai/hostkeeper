//! `hostkeeper self-update-fallback --dir <dir>`: what the fallback of hostkeeper's own update
//! runs, with the *previous* binary (design section 6.6). On Linux the fallback unit runs it once
//! the daemon's unit has failed its start limit (`StartLimitBurst=` starts within
//! `StartLimitIntervalSec=`; the unit `hostkeeper_steps::selfupdate::fallback_unit`, which the
//! daemon writes at its start, `crate::selfunits`); on macOS the fallback timer job runs it every
//! interval (`hostkeeper_steps::selfupdate::launchd_fallback_job`), and the verb decides the burst
//! itself ([`fallback_after_burst`]). On OpenRC no unit or job runs the verb: the packaged binary,
//! which supervise-daemon starts at every start, runs [`fallback_after_burst`] itself before it
//! execs `current` ([`crate::packaged`]), and the verb, run by hand, judges as on macOS. The code
//! is the same on every system; only what makes it act differs.
//!
//! The fallback, in this order: `current` of the binary directory `<dir>` is pointed at
//! `previous` again ([`Layout::flip_back`], which refuses a `previous` that is not an executable
//! file); then the update that waits for the restart is settled with the version `current` now
//! names as the binary that runs ([`hostkeeper_steps::selfupdate::settle`]): its version goes on
//! the local list of bad versions and its record says `Failed` after the commit, of class
//! release, `self_update_fallback`. The flip comes first, so that a crash between the two leaves
//! a host that runs the previous binary, whose start settles the same verdict. Run again, it
//! changes nothing. The step record, the list and the start log are in the parent of `<dir>`,
//! the state directory of the service `hostkeeper` ([`crate::selfupdate::binary_dir`]).
//!
//! On both systems the fallback puts nothing back when no update waits for its restart and
//! `current` is not `previous` already ([`FallbackError::NothingWaits`]): the binary that runs was
//! not switched in by an update, so it is not the one that failed. A new binary that confirmed its
//! update at its start ([`crate::selfupdate`]) has run, and is not put back either, even when that
//! start fails later. On Linux systemd runs the
//! fallback unit whenever the daemon's unit fails its start limit, for any reason (a config that
//! does not load, three restarts within the window); the verb then exits [`EXIT_FAILED`], which
//! the unit ignores (`ExecStart=-`), and its `ExecStartPost=` lines still reset the daemon's unit
//! and start it again: the daemon is retried for good, three starts at a time.
//!
//! On macOS and OpenRC the fallback runs only when all of these hold, and otherwise nothing is
//! written:
//!
//! - the record waits for the restart into a version;
//! - no daemon holds the state directory's lock (a daemon that runs is not failing to start), and
//!   the fallback holds it while it runs;
//! - no other service's step record holds an unfinished update (the rule of the self-update
//!   order itself, `crate::selfupdate`);
//! - the start log ([`hostkeeper_steps::StartLog`], which each daemon start on macOS and OpenRC
//!   adds to, [`record_start`]) holds [`START_LIMIT_BURST`] starts of that version within
//!   [`START_LIMIT_WINDOW_SECS`] ([`should_fall_back`]), the latest of them less than
//!   [`START_LIMIT_WINDOW_SECS`] ago: a burst long past is not a binary failing now.
//!
//! A start is written by the daemon itself, first thing in `hostkeeper daemon`; a binary that
//! the system kills before it runs any of its code (a refused signature, a binary for another
//! architecture) writes none, and is not fallen back from by this rule.
//!
//! A wait for a hand-over (docs/design/two-programs.md, 4.3, [`crate::handover`]) moved no
//! link: the fallback puts nothing back and writes nothing for it, on Linux exiting
//! [`EXIT_FAILED`] ([`FallbackError::HandOver`]) and on macOS judging nothing
//! ([`Judged::NotPending`]).
//!
//! It exits 0 when the previous binary is back or, on macOS, when there was nothing to do yet,
//! and [`EXIT_FAILED`] with a message naming the cause when there is no `previous`, when no update
//! waits for its restart, when `previous` is not an executable file, or when the record, the
//! list, the log or the lock cannot be used; with no `previous`, or nothing waiting, nothing is
//! written.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hostkeeper_config::names::ServiceName;
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_steps::record::{AwaitingRestart, Loaded};
use hostkeeper_steps::selfupdate::{START_LIMIT_BURST, START_LIMIT_WINDOW_SECS, settle};
use hostkeeper_steps::{
    Clock, DaemonStart, Layout, RecordStore, SelfUpdateError, Settled, Stamp, StartLog,
    StartLogError, should_fall_back,
};

use crate::adapters::host_clock;
use crate::selfupdate::unfinished_other;
use crate::start::{EXIT_FAILED, StartError, lock_state_dir};
use crate::verbs::force::SELF_SERVICE;

/// Why the fallback could not put the previous binary back, or record it, or judge whether to.
#[derive(Debug, thiserror::Error)]
pub enum FallbackError {
    /// The binary directory has no `previous`: there is nothing to fall back to.
    #[error("{} has no previous binary to fall back to", .0.display())]
    NoPrevious(PathBuf),
    /// No update waits for its restart, and `current` of the binary directory is not `previous`:
    /// the binary that runs was not switched in by an update, and is not put back.
    #[error(
        "no update of hostkeeper waits for its restart, so {} is left as it is: the binary that runs was not switched in by an update",
        .0.display()
    )]
    NothingWaits(PathBuf),
    /// The update waits for a hand-over (docs/design/two-programs.md, 4.3): `current` was not
    /// moved, and the start of the binary it names settles the update, so nothing is put back.
    #[error(
        "the update of hostkeeper waits for a hand-over, so {} is left as it is: the start of the binary `current` names settles it",
        .0.display()
    )]
    HandOver(PathBuf),
    /// `previous` could not be read, or names what is not an executable file, or `current`
    /// could not be moved.
    #[error("the previous binary cannot be put back: {0}")]
    Flip(#[from] io::Error),
    /// The previous binary is back, and the update could not be recorded failed.
    #[error("the previous binary is back, but the update could not be recorded: {0}")]
    Settle(#[from] SelfUpdateError),
    /// On macOS: the start log could not be read.
    #[error("the burst cannot be judged: {0}")]
    Starts(#[from] StartLogError),
    /// On macOS: the state directory or its lock could not be used.
    #[error("the state directory cannot be locked: {0}")]
    Lock(String),
    /// On macOS: the step records could not be listed.
    #[error("the step records cannot be listed: {0}")]
    Records(io::Error),
}

/// What the fallback on macOS found when it ran.
#[derive(Debug, PartialEq, Eq)]
pub enum Judged {
    /// No update waits for the restart, or one waits for a hand-over, which the start of the
    /// binary `current` names settles: nothing to judge.
    NotPending,
    /// A daemon holds the state directory, or another service's update is unfinished: the
    /// words say which. Nothing was changed.
    Busy(String),
    /// The version waited for has `starts` starts in the log, not yet its burst. Nothing was
    /// changed.
    NotYet { starts: usize },
    /// The previous binary is back, and the update settled so.
    FellBack(Settled),
}

/// The fallback for the binary directory `dir`, the step record and the list of bad versions in
/// its parent.
pub fn fallback(dir: &Path) -> Result<Settled, FallbackError> {
    let state = dir.parent().unwrap_or(dir);
    fallback_with(&Layout::new(dir), &RecordStore::new(state))
}

/// The fallback for the binary directory `layout`, the update in the record of `store`: the seam
/// of the tests that stop it at a write of the record.
pub fn fallback_with(layout: &Layout, store: &RecordStore) -> Result<Settled, FallbackError> {
    let previous = layout.previous()?;
    if previous.is_none() {
        return Err(FallbackError::NoPrevious(layout.dir().to_path_buf()));
    }
    let awaiting = awaited(store)?;
    if awaiting.as_ref().is_some_and(|a| a.next.is_some()) {
        return Err(FallbackError::HandOver(layout.dir().to_path_buf()));
    }
    if awaiting.is_none() & (layout.current()? != previous) {
        return Err(FallbackError::NothingWaits(layout.dir().to_path_buf()));
    }
    layout.flip_back()?;
    let running = layout.current()?.unwrap_or_default();
    Ok(settle(layout, store, &running)?)
}

/// The update the record of `store` waits for the restart into, if any; a record of a newer
/// daemon, which this binary cannot read, waits for nothing it knows of.
fn awaited(store: &RecordStore) -> Result<Option<AwaitingRestart>, SelfUpdateError> {
    Ok(match store.load()? {
        Loaded::Current(rec) => rec.progress.awaiting_restart,
        Loaded::Absent | Loaded::Newer { .. } => None,
    })
}

/// The fallback on macOS for the binary directory `dir`: [`fallback_with`] once the version the
/// record waits for has failed its burst, under the state directory's lock, while no other update
/// is unfinished (the module's list); otherwise what kept it from running.
/// Time is read on `clock`.
pub fn fallback_after_burst(dir: &Path, clock: &dyn Clock) -> Result<Judged, FallbackError> {
    let hk_state = dir.parent().unwrap_or(dir);
    let state = hk_state.parent().unwrap_or(hk_state);
    let awaiting = awaited(&RecordStore::new(hk_state))?;
    let Some(awaiting) = awaiting.filter(|a| a.next.is_none()) else {
        return Ok(Judged::NotPending);
    };
    let _lock = match lock_state_dir(state) {
        Ok(lock) => lock,
        Err(StartError::Locked { path, source }) => {
            return Ok(Judged::Busy(format!(
                "a daemon holds {}: {source}",
                path.display()
            )));
        }
        Err(other) => return Err(FallbackError::Lock(other.to_string())),
    };
    let load = |service: &ServiceName| RecordStore::new(&state.join(service.as_str())).load();
    if let Some(busy) =
        unfinished_other(state, SELF_SERVICE, &load).map_err(FallbackError::Records)?
    {
        return Ok(Judged::Busy(busy));
    }
    let starts = StartLog::load(hk_state)?.of(&awaiting.version);
    let recent = starts
        .last()
        // A start of another boot, or of an unknown one, is read as just made, as a stamp of
        // another boot has spent no time.
        .is_some_and(|latest| latest.elapsed_secs(clock).unwrap_or(0) < START_LIMIT_WINDOW_SECS);
    if !(recent & should_fall_back(&starts, START_LIMIT_BURST, START_LIMIT_WINDOW_SECS)) {
        return Ok(Judged::NotYet {
            starts: starts.len(),
        });
    }
    Ok(Judged::FellBack(fallback_with(
        &Layout::new(dir),
        &RecordStore::new(hk_state),
    )?))
}

/// On macOS and OpenRC, adds this start of the daemon of version `own` to the start log of the
/// service `hostkeeper` under `state_dir` (stamped on `clock`), when that service has a state
/// directory: `Ok(true)` when it was written. Nothing on Linux under systemd, which counts the
/// starts.
pub fn record_start(
    state_dir: &Path,
    kind: PlatformKind,
    own: &str,
    clock: &dyn Clock,
) -> Result<bool, StartLogError> {
    let dir = state_dir.join(SELF_SERVICE);
    let counted = match kind {
        PlatformKind::Linux => false,
        PlatformKind::Macos | PlatformKind::Openrc => dir.is_dir(),
    };
    if counted {
        StartLog::load(&dir)?.record(DaemonStart {
            version: own.to_owned(),
            at: Stamp::now(clock),
        })?;
    }
    Ok(counted)
}

/// The verb on the host's platform.
pub fn main(dir: &Path) -> ExitCode {
    main_for(dir, Platform::host().kind, &host_clock())
}

/// The verb on a host of `kind`: [`fallback`] on Linux under systemd, [`fallback_after_burst`] on
/// `clock` on macOS and OpenRC, its error printed.
pub fn main_for(dir: &Path, kind: PlatformKind, clock: &dyn Clock) -> ExitCode {
    let done = match kind {
        PlatformKind::Linux => fallback(dir).map(|_| ()),
        PlatformKind::Macos | PlatformKind::Openrc => fallback_after_burst(dir, clock).map(|_| ()),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hostkeeper self-update-fallback: {error}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}
