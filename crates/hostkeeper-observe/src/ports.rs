//! The ports the observer reads the host through, and the values they answer with. Each has a
//! real adapter in this crate (`systemd`, `proc`, `health`, `command`) and a fake in
//! [`crate::fake`]; the observer itself knows only these traits.

use std::time::Duration;

use crate::error::{ObserveError, RunError};
use crate::url::HealthUrl;

/// What the service manager says of one unit's activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveState {
    /// Running (systemd `active`, `reloading` or `refreshing`).
    Active,
    /// Not running, and not failed.
    Inactive,
    /// Not running, after a failure.
    Failed,
    /// Starting.
    Activating,
    /// Stopping.
    Deactivating,
    /// The manager knows no such unit (systemd `LoadState=not-found`).
    NotFound,
}

/// One unit as the service manager reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitState {
    pub active: ActiveState,
    /// The manager's finer state (systemd `SubState`: `running`, `exited`, `dead`, ...), for the
    /// operator only; no decision reads it.
    pub sub: String,
    /// The unit's main process, `None` when it has none (systemd `MainPID=0`, such as a
    /// `oneshot` unit with `RemainAfterExit`).
    pub main_pid: Option<u32>,
}

/// The service manager: systemd on Linux (`crate::systemd`), launchd later.
pub trait ServiceManager: Send + Sync {
    /// The state of `unit`. A unit the manager does not know is `Ok` with
    /// [`ActiveState::NotFound`]; `Err` means the manager could not be asked or its answer could
    /// not be read, and is never a state.
    fn unit_state(&self, unit: &str) -> Result<UnitState, ObserveError>;
}

/// A process told apart from any later one that reuses its pid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// When it started, in clock ticks since boot (field 22 of `/proc/<pid>/stat`).
    pub start_time: u64,
}

/// The size and modification time of the file a process runs, the part of the digest cache's
/// key that changes when the executable is rewritten in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExeStat {
    pub size: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: i64,
}

/// The running processes: `/proc` on Linux (`crate::proc`).
pub trait ProcessProbe: Send + Sync {
    /// Who `pid` is now.
    fn identity(&self, pid: u32) -> Result<ProcessIdentity, ObserveError>;
    /// The size and modification time of the file `pid` runs (the inode behind
    /// `/proc/<pid>/exe`, even when its path was replaced).
    fn exe_stat(&self, pid: u32) -> Result<ExeStat, ObserveError>;
    /// The SHA-256 of the file `pid` runs, read through `/proc/<pid>/exe`.
    fn exe_digest(&self, pid: u32) -> Result<[u8; 32], ObserveError>;
}

/// What a health URL answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealthReply {
    /// The HTTP status code.
    pub status: u16,
    /// How many bytes after the header block were read (never more than the read cap allows).
    pub body_bytes_read: usize,
}

/// How long a probe may wait: to connect, and for each read or write after that. The caller
/// derives it from its own clock and interval; the observer never sleeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deadline {
    pub connect: Duration,
    pub read: Duration,
}

/// The health probe: one HTTP GET (`crate::health`).
pub trait HealthClient: Send + Sync {
    /// GETs `url` once, following no redirect. `Err` is [`ObserveError::Unreachable`] when the
    /// transport failed (refused, reset, timed out, no HTTP answer) and
    /// [`ObserveError::Probe`] when the probe could not be made at all.
    fn get(&self, url: &HealthUrl, deadline: Deadline) -> Result<HealthReply, ObserveError>;
}

/// The digest the executable of a version must have, from what was sealed when the version was
/// verified. The daemon implements it over the sealed package tree.
pub trait ExpectedExe: Send + Sync {
    /// The SHA-256 the main executable of `version` of `service` has, or `None` when it is not
    /// known (no seal, or no executable recorded): the version is then reported unconfirmed,
    /// never guessed.
    fn digest(&self, service: &str, version: &str) -> Option<[u8; 32]>;
}

/// What a finished command printed and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    /// The exit code; `None` when a signal ended it.
    pub exit_code: Option<i32>,
    /// Standard output, at most the runner's cap.
    pub stdout: Vec<u8>,
    /// Whether the command printed more than the cap (the rest was not read).
    pub truncated: bool,
}

/// Runs a program: argv in, bounded standard output and the exit code out. The systemd adapter
/// runs `systemctl show` through it (`crate::command` is the real one).
pub trait CommandRunner: Send + Sync {
    /// Runs `argv` (`argv[0]` is the program) and waits at most `deadline` for it.
    fn run(&self, argv: &[String], deadline: Duration) -> Result<CommandOutput, RunError>;
}
