//! The service observer of the host daemon (design section 5.8). A service does not say it is
//! alive: the daemon looks, for every installed service, at three things and reports what it saw
//! in the heartbeat.
//!
//! - Liveness, from the service manager: the state of each unit of the manifest's `[units] run`
//!   ([`ServiceManager`]; systemd in [`systemd`]).
//! - The version, from the step record, confirmed by the digest of the running executable against
//!   the one sealed for that version ([`ProcessProbe`], [`ExpectedExe`]; `/proc` in [`proc`]).
//! - Health, from the manifest's `[health] url`: one HTTP GET per active instance, to a loopback
//!   literal, no redirect followed, 4 KiB read at most, only 2xx healthy ([`HealthClient`];
//!   the real probe in [`health`]).
//!
//! [`Observer::observe`] makes one consistent [`Observation`] of these; [`beat_health`] maps it to
//! the heartbeat's `Health` and version. An observation that fails is `unobserved` and is never
//! read as absent or stopped.
//!
//! At daemon start, before any unit is touched, [`unlink_stale_registration_socket`] removes the
//! registration socket kbf's agent served, which hostkeeper does not carry.
//!
//! The real adapters are thin: [`systemd`] (through a [`CommandRunner`], [`command`] the real
//! one), [`proc`] and the TCP transport of [`health`]. Everything else runs against the fakes of
//! [`fake`] in the tests.

pub mod beat;
pub mod command;
pub mod error;
pub mod fake;
pub mod health;
pub mod observe;
pub mod ports;
pub mod proc;
pub mod socket;
pub mod systemd;
pub mod url;

pub use beat::beat_health;
pub use command::ProcessRunner;
pub use error::{ObserveError, RunError};
pub use health::{Connect, HttpHealthClient, TcpConnect};
pub use observe::{
    Current, HealthView, Observation, ObserveOptions, Observer, Ports, ServiceInput, ServiceState,
    UnhealthyReason, UnobservedReason, VersionView,
};
pub use ports::{
    ActiveState, CommandOutput, CommandRunner, Deadline, ExeStat, ExpectedExe, HealthClient,
    HealthReply, ProcessIdentity, ProcessProbe, ServiceManager, UnitState,
};
pub use proc::ProcProbe;
pub use socket::unlink_stale_registration_socket;
pub use systemd::SystemdManager;
pub use url::{HealthUrl, UrlRefusal};
