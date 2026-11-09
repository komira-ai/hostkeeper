//! The OpenRC side of the host daemon (docs/design/two-programs.md, section 4.6). An Alpine host
//! runs the same daemon, the same flow, the same config and package paths as a Linux host under
//! systemd; what differs is behind ports that already exist, and this crate implements them over
//! OpenRC:
//!
//! - [`rc_service`]: [`RcService`], the linking side's
//!   [`UnitControl`](hostkeeper_placed::UnitControl) over `rc-service`.
//! - [`manager`]: [`OpenrcServiceManager`], the observer's
//!   [`ServiceManager`](hostkeeper_observe::ServiceManager) over `rc-service <service> status`
//!   and the child pid supervise-daemon keeps.
//! - [`launcher`]: [`OpenrcLauncher`], the step runner's
//!   [`StepLauncher`](hostkeeper_steps::StepLauncher): each step of a hook as a child process of
//!   the daemon in a process group of its own, under the same contract as
//!   `hostkeeper_steps::SystemdLauncher`. OpenRC has no transient unit and no control group of a
//!   unit to kill, so the launcher bounds, stops and cleans up each step's process group itself.
//!
//! The process probe of an OpenRC host is the Linux one (`/proc`), in `hostkeeper-observe`.

pub mod launcher;
pub mod manager;
pub mod rc_service;

pub use launcher::OpenrcLauncher;
pub use manager::OpenrcServiceManager;
pub use rc_service::RcService;
