//! The hostkeeper daemon: the one root process of a host (docs/design/hostkeeper-design.md,
//! sections 4.1, 5.1, 5.2, 6.2, 6.3, 8.1 and 8.3).
//!
//! - [`start`]: start-up, in order: the daemon config, the lock on the state directory, the
//!   registration socket of kbf's agent unlinked, the sweep of package leftovers and the
//!   reconciliation of the placed paths, the boot work; then serving: the enrollment and
//!   heartbeat loops started ([`links`]). The daemon serves nothing on the host.
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
//!   its lock alone, through the placed-path linker of `hostkeeper-placed` ([`verbs::force`]).
//!   `uninstall` runs the sequence of design section 4.7 ([`uninstall`]), resumed from its
//!   tombstone in `placed.json`.
//! - [`selfupdate`]: the update of hostkeeper itself (design section 6.6): an `update` of the
//!   service `hostkeeper` refused while another service's update is unfinished, its new binary's
//!   release judged against the daemon's oldest reader, its steps run under the step lock (built
//!   in for a floor above [`selfupdate::LEGACY_FLOOR`]), then the follow-on that hands over to
//!   the new binary and asks the daemon to exit; at start, the update settled by the binary that
//!   runs. [`seed`] gives the binary directory its first binary, for the packaged start.
//! - [`packaged`]: `hostkeeper daemon` started from the packaged binary seeds the binary
//!   directory ([`seed`]) and execs its `current`.
//! - [`handover`]: the first thing `hostkeeper daemon` does, before it reads its config (after
//!   the packaged start, which execs `current` first): a
//!   hand-over of hostkeeper's own update into another binary (docs/design/two-programs.md, 4.3)
//!   execs that binary while its start count lasts, and is settled failed once it cannot.
//! - [`mac_config`]: the daemon config written from the managed preferences the MDM installed,
//!   which the start of a Mac's daemon does when its config is not there.
//! - [`adapters`]: the step launcher, service manager, unit directory, observer ports and package
//!   check of the host's platform ([`adapters::Adapters::for_platform`]: systemd on Linux,
//!   launchd on macOS), and the claims of its enrollment.
//! - [`ipmi`]: the BMC a Linux host's enrollment claims ask for its MAC, over `/dev/ipmi0`.
//! - [`mac`]: the real adapters of a Mac's package check (`getxattr`, `codesign`) and claims
//!   (`ioreg`), over the process port of the launchd adapters.
//! - [`prune`]: after a confirmed switch (an update that ends `Succeeded`, a force that moved
//!   `current`), the service's version directories but `current` and `previous` are deleted,
//!   under the lock the order already holds.
//! - [`halt`]: the held halt generation of each service, persisted, never lowered.
//! - [`hook_log`]: what the host's step launcher kept of a hook's standard error, as lines of the
//!   daemon's log under the step's unit.
//! - [`packages`]: the port of the package store, [`real_packages`] its real implementation
//!   (fetch, prove, the stage of the entry's installer and seal, with the one table that maps
//!   their failures onto [`refusal`]), and [`installed`] what the host runs.
//! - [`installers`]: the installer each installed service is recorded with, whose steps
//!   (`hostkeeper_install`) the switch, the prune and the uninstall run, the placer doing what
//!   they ask of the host.
//! - [`repository`]: the repository installer (`apt`) with the daemon's ports: the stage of an
//!   order's repository entry, `activate` in the switch, the rollback, the purge at uninstall and
//!   the drift of each heartbeat; [`target`] the sealed `target.json` of such a version and the
//!   one reader of a version directory's manifest; [`builtin`] the built-in steps of a version
//!   with no hook program; [`repository_push`] the repository sets the coordinator pushes, written
//!   as the host's repository configuration and taken into use without a restart.
//! - [`facts_file`]: the host's facts file, written from the facts the coordinator sends.
//! - [`observer`]: what [`Daemon::status`] reports of a running service.
//! - [`trusted_root`]: the Sigstore trusted root package checks use, and the age and reason
//!   [`Daemon::status`] reports of it.
//! - [`node`]: the host's enrollment and the heartbeat loop's link, as the loops last wrote them,
//!   which [`Daemon::status`] reports besides the services.
//! - [`cli`]: `hostkeeper daemon` and its flags, and `hostkeeper check-repositories`
//!   ([`repositories`]); the binary only forwards to it.
//! - [`stamp`]: the version `hostkeeper --version` prints, with the commit of a release build.
//! - [`refusal`]: the closed set of reason tokens.
//! - [`link_adapters`]: the daemon as the executor, the held halts, the installed services and
//!   the service observer of the heartbeat loop (`hostkeeper_link`): its order becomes a protobuf
//!   `Order` for [`Daemon::order`], and the daemon's reply the verdict the loop reports.
//! - [`links`]: the enrollment loop (`hostkeeper_enroll`) and the heartbeat loop, which
//!   [`start::serve`] starts, in that order, once the daemon is ready, and which sends the
//!   watchdog notice ([`notify::Watchdog`]) after each heartbeat; [`unix_clock`] is the wall
//!   clock of the loop's deferrals, the daemon's one read of it.
//!
//! Everything privileged is behind a port the daemon is given: the step launcher
//! (`hostkeeper_steps::StepLauncher`), the package store ([`packages::Packages`]), the service
//! manager (`hostkeeper_placed::UnitControl`), the filesystem placed paths are linked through
//! (`hostkeeper_placed::Fs`) and the service observer (`hostkeeper_observe::Observer`). Time is the
//! `hostkeeper_steps::Clock`; nothing here reads the wall clock.

pub mod adapters;
pub mod boot;
pub mod builtin;
pub mod checks;
pub mod cli;
pub mod console;
pub mod entry;
pub mod executor;
pub mod facts_file;
mod files;
pub mod halt;
pub mod handover;
pub mod hook_log;
pub mod installed;
pub mod installers;
pub mod ipmi;
pub mod layout;
pub mod link_adapters;
pub mod links;
pub mod locks;
pub mod mac;
pub mod mac_config;
pub mod mac_files;
pub mod macos_pkg;
pub mod node;
pub mod notify;
pub mod observer;
pub mod own_package;
pub mod packaged;
pub mod packages;
pub mod place;
pub mod prune;
pub mod real_packages;
pub mod refusal;
pub mod repositories;
pub mod repository;
pub mod repository_push;
pub mod rollback_floor;
pub mod run_units;
pub mod seed;
pub mod selfupdate;
pub mod stamp;
pub mod start;
pub mod target;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod trusted_root;
pub mod uninstall;
pub mod unix_clock;
pub mod verb;
pub mod verbs;
pub mod watchers;

pub use boot::{BootError, BootReport, BootStep, Outcome, ServiceBoot, Stage};
pub use executor::{
    Ctx, Daemon, DaemonStatus, Dirs, ForcedResult, LastUninstall, Placement, Ports, Refused, Reply,
    ServiceStatus, StoreSeam,
};
pub use files::WriteStep;
pub use notify::{
    NoNotifier, NoWatchdog, ReadyFile, ReadyNotifier, SystemdNotifier, SystemdWatchdog, Watchdog,
};
pub use refusal::Refusal;
pub use verb::Verb;
