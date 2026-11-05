//! The hostkeeper daemon: the one root process of a host (docs/design/hostkeeper-design.md,
//! sections 4.1, 5.1, 5.2, 6.2, 6.3, 8.1 and 8.3).
//!
//! - [`start`]: start-up, in order: the daemon config, the lock on the state directory, the
//!   registration socket of kbf's agent unlinked, the sweep of package leftovers and the
//!   reconciliation of the placed paths, the boot work, the admin socket; then serving the admin
//!   socket.
//! - [`boot`]: the boot work: the switch each step record says was begun
//!   ([`boot::switch_intent`]), the step records read against the boot id, each unfinished
//!   update run to its verdict, the post-success watchers restarted ([`watchers`]), the
//!   manifest's `boot` steps, and the readiness notice ([`notify`]).
//! - [`executor`]: [`Daemon::order`], one order of the coordinator, decoded into a [`Verb`]
//!   ([`verb`]) and handed to its module under [`verbs`]. `update`, `force_rollback` and
//!   `install` are checked by the daemon itself ([`checks`]) and run through the step runner of
//!   `hostkeeper-steps` under the service's lock and the host's step lock ([`locks`]); each is
//!   placed, switched and confirmed by the runner's switch ([`place`]). `force` is checked the
//!   same way but for `busy` and the standing halt, runs no hook, and switches the service under
//!   its lock alone, through the placed-path linker of `hostkeeper-placed` ([`verbs::force`]);
//!   [`prune`] is its last step. `uninstall` runs the sequence of design section 4.7
//!   ([`uninstall`]), resumed from its tombstone in `placed.json`.
//! - [`halt`]: the held halt generation of each service, persisted, never lowered.
//! - [`packages`]: the port of the package store, [`real_packages`] its real implementation
//!   (fetch, prove, unpack and seal, with the one table that maps their failures onto
//!   [`refusal`]), and [`installed`] what the host runs.
//! - [`observer`]: what the admin `Status` reports of a running service.
//! - [`admin`] and [`socket`]: the root-only admin socket (`hostkeeper.local.v1.Admin`).
//! - [`cli`]: `hostkeeper daemon` and its flags; the binary only forwards to it.
//! - [`stamp`]: the version `hostkeeper --version` prints, with the commit of a release build.
//! - [`refusal`]: the closed set of reason tokens.
//!
//! Everything privileged is behind a port the daemon is given: the step launcher
//! (`hostkeeper_steps::StepLauncher`), the package store ([`packages::Packages`]), the service
//! manager (`hostkeeper_placed::UnitControl`), the filesystem placed paths are linked through
//! (`hostkeeper_placed::Fs`) and the service observer (`hostkeeper_observe::Observer`). Time is the
//! `hostkeeper_steps::Clock`; nothing here reads the wall clock.

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
pub mod observer;
pub mod packages;
pub mod place;
pub mod prune;
pub mod real_packages;
pub mod refusal;
pub mod socket;
pub mod stamp;
pub mod start;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod uninstall;
pub mod verb;
pub mod verbs;
pub mod watchers;

pub use boot::{BootError, BootReport, BootStep, Outcome, ServiceBoot, Stage};
pub use executor::{
    Ctx, Daemon, DaemonStatus, Dirs, ForcedResult, LastUninstall, Placement, Ports, Reply,
    ServiceStatus, StoreSeam,
};
pub use files::WriteStep;
pub use notify::{NoNotifier, ReadyNotifier, SystemdNotifier};
pub use refusal::Refusal;
pub use verb::Verb;
