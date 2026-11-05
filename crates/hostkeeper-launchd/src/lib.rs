//! The macOS side of the host daemon (design sections 4.2 and 5.9). A Mac runs the same daemon,
//! the same flow, the same config and package paths as a Linux host; what differs is behind
//! ports that already exist, and this crate implements them over launchd:
//!
//! - [`label`]: the reverse-DNS prefix and the labels of the daemon and of each step job.
//! - [`plist`]: the XML property lists of the daemon and of a step job.
//! - [`launchctl`]: the [`Launchctl`] port (one `launchctl` command), its real adapter
//!   [`LaunchctlProcess`], and the reading of `launchctl print system/<label>`.
//! - [`manager`]: [`LaunchdServiceManager`], the observer's
//!   [`ServiceManager`](hostkeeper_observe::ServiceManager) over `launchctl print`.
//! - [`procid`]: [`LaunchdProcessProbe`], the observer's
//!   [`ProcessProbe`](hostkeeper_observe::ProcessProbe) over `proc_pidpath` and a device and
//!   inode check.
//! - [`unit_control`]: [`LaunchdUnitControl`], the linking side's
//!   [`UnitControl`](hostkeeper_placed::unit_control::UnitControl) over `launchctl`.
//! - [`launcher`]: [`LaunchdLauncher`], the step runner's
//!   [`StepLauncher`](hostkeeper_steps::StepLauncher): each step of a hook as a transient launchd
//!   job, under the same contract as `hostkeeper_steps::SystemdLauncher`.
//! - [`platform`]: [`Platform`], the paths and unit directory that differ between Linux and
//!   macOS.
//! - [`fake`]: [`FakeLaunchctl`], the scripted `launchctl` every test drives.
//!
//! Nothing here is compiled for macOS only except the real `PidInfo` of [`procid`] and the host's
//! launcher (`LaunchdLauncher::system` and its clock, in [`launcher`]); the rest is built and
//! measured on Linux, against the fakes.

pub mod fake;
pub mod label;
pub mod launchctl;
pub mod launcher;
pub mod manager;
pub mod platform;
pub mod plist;
pub mod procid;
pub mod unit_control;

pub use fake::FakeLaunchctl;
pub use label::{Label, LabelError, REVERSE_DNS_PREFIX};
pub use launchctl::{
    JobInfo, JobState, JobStatus, LAUNCHCTL, Launchctl, LaunchctlError, LaunchctlOutput,
    LaunchctlProcess, ParseError, PrintError, parse_print,
};
pub use launcher::{LaunchdLauncher, Pace};
pub use manager::LaunchdServiceManager;
pub use platform::{Platform, PlatformKind};
pub use plist::{DaemonJob, PlistError, StepJob, daemon_plist, step_plist};
pub use procid::{FileIdentity, LaunchdProcessProbe, PidInfo, PidInfoError, StdFileIdentity};
pub use unit_control::LaunchdUnitControl;
