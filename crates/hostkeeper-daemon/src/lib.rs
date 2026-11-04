//! The hostkeeper daemon: the one root process of a host (docs/design/hostkeeper-design.md,
//! sections 4.1, 5.1, 5.2, 6.2, 6.3, 8.1 and 8.3).
//!
//! - [`start`]: start-up, in order: the daemon config, the lock on the state directory, the boot
//!   work, the admin socket; then serving the admin socket.
//! - [`boot`]: the boot work: the step records read against the boot id, each unfinished update
//!   run to its verdict, the post-success watchers restarted ([`watchers`]), the manifest's
//!   `boot` steps, and the readiness notice ([`notify`]).
//! - [`executor`]: [`Daemon::order`], one order of the coordinator, decoded into a [`Verb`]
//!   ([`verb`]) and handed to its module under [`verbs`]. `update` and `force_rollback` are
//!   checked by the daemon itself ([`checks`]) and run through the step runner of
//!   `hostkeeper-steps` under the service's lock and the host's step lock ([`locks`]).
//! - [`halt`]: the held halt generation of each service, persisted, never lowered.
//! - [`packages`]: the port of the package store, and [`installed`] what the host runs.
//! - [`admin`] and [`socket`]: the root-only admin socket (`hostkeeper.local.v1.Admin`).
//! - [`cli`]: `hostkeeper daemon` and its flags; the binary only forwards to it.
//! - [`refusal`]: the closed set of reason tokens.
//!
//! Everything privileged is behind a port the daemon is given: the step launcher
//! (`hostkeeper_steps::StepLauncher`) and the package store ([`packages::Packages`]). Time is
//! the `hostkeeper_steps::Clock`; nothing here reads the wall clock.

pub mod admin;
pub mod boot;
pub mod checks;
pub mod cli;
pub mod executor;
mod files;
pub mod halt;
pub mod installed;
pub mod locks;
pub mod notify;
pub mod packages;
pub mod refusal;
pub mod socket;
pub mod start;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod verb;
pub mod verbs;
pub mod watchers;

pub use boot::{BootError, BootReport, BootStep, Outcome, ServiceBoot, Stage};
pub use executor::{Ctx, Daemon, DaemonStatus, Dirs, Ports, Reply, ServiceStatus, StoreSeam};
pub use files::WriteStep;
pub use notify::{NoNotifier, ReadyNotifier, SystemdNotifier};
pub use refusal::Refusal;
pub use verb::Verb;
