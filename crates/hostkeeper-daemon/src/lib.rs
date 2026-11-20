//! The hostkeeper daemon: the one root process of a host (docs/design/hostkeeper-design.md,
//! sections 4.1, 5.1, 5.2, 6.2, 6.3, 8.1 and 8.3).
//!
//! - [`start`]: start-up, in order: the daemon config, the lock on the state directory, the
//!   registration socket of kbf's agent unlinked, the removal of the dangling unit links an
//!   earlier move left ([`unit_links`]), the sweep of package leftovers, the boot work,
//!   the deletion of the archive version a native service left as `previous`
//!   ([`archive_versions`]); then serving: the enrollment and heartbeat loops started
//!   ([`links`]). The daemon serves nothing on the host.
//! - [`boot`]: the boot work: the step records read against the boot id, each unfinished update
//!   run to its verdict, the post-success watchers restarted ([`watchers`]), the manifest's
//!   `boot` steps, and the readiness notice ([`notify`]).
//! - [`executor`]: [`Daemon::order`], one order of the coordinator, decoded into a [`Verb`]
//!   ([`verb`]) and handed to its module under [`verbs`]. `update`, `force_rollback` and
//!   `install` are checked by the daemon itself ([`checks`]) and run through the step runner of
//!   `hostkeeper-steps` under the service's lock and the host's step lock ([`locks`]); each is
//!   switched by the runner's switch ([`place`]). `force` is checked the same way but for `busy`
//!   and the standing halt, runs no hook, and switches the service under its lock alone, through
//!   the installer of its version ([`verbs::force`]).
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
//! - [`mac_config`]: the daemon config written from the managed preferences the device management installed,
//!   which the start of a Mac's daemon does when its config is not there.
//! - [`keep_awake`]: a Mac's power settings, made at the start of its daemon so that the Mac never
//!   sleeps (`pmset`), unless the daemon config's `keep_awake` is off.
//! - [`remote_login_probe`]: a Mac's report of what its daemon can do of Remote Login without
//!   Full Disk Access, and of why the Mac may sleep, run once at its start and sent in every
//!   heartbeat after ([`line_process`] reads `pmset -g log` for it).
//! - [`adapters`]: the step launcher, service manager, unit directory, observer ports and package
//!   check of the host's platform ([`adapters::Adapters::for_platform`]: systemd on Linux,
//!   launchd on macOS), and the claims of its enrollment.
//! - [`ipmi`]: the BMC a Linux host's enrollment claims ask for its MAC, over `/dev/ipmi0`.
//! - [`bmc`]: the BMC's address and MAC a Linux host's heartbeats report, read over the same
//!   device every ten minutes.
//! - [`mac`]: the real adapters of a Mac's package check (`getxattr`, `codesign`) and claims
//!   (`ioreg`), over the process port of the launchd adapters.
//! - [`ssh_host_mac`]: the Mac's host of the sshd setup that trusts the coordinator's SSH user CA:
//!   the `Include` of `sshd_config`, the login user, the `com.apple.access_ssh` group and
//!   Remote Login by `launchctl` (docs/design/ssh-through-coordinator.md, section 3).
//! - [`host_identity`]: the host's certificate and key, kept after every step of the enrollment
//!   loop at `/var/lib/hostkeeper/identity` for the group `hostkeeper-identity`
//!   ([`identity_group`]), for the services of the host (docs/design/host-identity.md).
//! - [`prune`]: after a confirmed switch (an update that ends `Succeeded`, a force that moved
//!   `current`), the service's version directories but `current` and `previous` are deleted,
//!   under the lock the order already holds.
//! - [`halt`]: the generation a halt of each service is held under, persisted while the halt stands.
//! - [`hook_log`]: what the host's step launcher kept of a hook's standard error, as lines of the
//!   daemon's log under the step's unit.
//! - [`packages`]: the port that fetches a `macos-pkg` entry's `.pkg` by its pin,
//!   [`real_packages`] its real implementation (the host's stores, the one table that maps the
//!   fetch's failures onto [`refusal`]), and [`installed`] what the host runs.
//! - [`installers`]: the installer each installed service is recorded with, whose backend
//!   (`hostkeeper_install`) the uninstall and the prune read.
//! - [`repository`]: the repository installer (`apt`) with the daemon's ports: the stage of an
//!   order's repository entry, `activate` in the switch, the rollback, the purge at uninstall and
//!   the drift of each heartbeat; [`target`] the sealed `target.json` of such a version and the
//!   one reader of a version directory's manifest; [`builtin`] the built-in steps of a version
//!   with no hook program; [`mirror`] the files of a private repository, read from the coordinator into a local copy apt reads; [`repository_push`] the repository sets the coordinator pushes, written
//!   as the host's repository configuration and taken into use without a restart.
//! - [`facts_file`]: the host's facts file, written from the facts the coordinator sends.
//! - [`maintenance_state`]: the run state of the maintenance report, read from the host's
//!   maintenance run record (`hostkeeper_steps::maintenance`).
//! - [`operator_keys`]: the operator keys the coordinator sends (`operator-keys-v1`), put in the
//!   managed block of the login account's `authorized_keys` ([`authorized_keys`], the block and the
//!   file) and reported by digest in every heartbeat; [`login_account`] finds the account (root on
//!   Linux, a Mac's one local administrator) and, on a Mac, [`mac_ssh`] makes sure Remote Login is
//!   on and the account may use ssh.
//! - [`ssh_setup`]: the host's sshd, set up to trust the coordinator's SSH user CA from the access
//!   the coordinator sends (`ssh-v1`), with [`ssh_files`] (the files and the record's checks),
//!   [`ssh_effective`] (what `sshd -T` says and the lockout rules judge of it) and [`ssh_host`] (the
//!   host's port and its real adapters, Linux's and [`ssh_host_mac`]'s).
//! - [`prerequisites`]: the host prerequisites the coordinator sends (`prerequisites-v1`), made sure
//!   of on the host and reported by state in every heartbeat; [`xcode_clt`] is the one there is,
//!   Apple's Command Line Tools, looked for and installed headless on a Mac.
//! - [`maintenance_facts`]: what the host last found of its maintenance (`maintenance-v1`), looked
//!   for on a worker once an hour and at once after a maintenance step, and reported in every
//!   heartbeat to a coordinator that asks; the look reads its [`maintenance_collectors`] (each
//!   platform's list), which are [`maintenance_collect`]'s `Collector`s: [`maintenance_host`] (the
//!   OS, the kernels, the boot), [`maintenance_reboot`] (a reboot the host asks for) and the
//!   package managers [`maintenance_apt`], [`maintenance_dnf`] and [`maintenance_apk`], with
//!   [`maintenance_own`] naming the packages hostkeeper itself installs, which are never due.
//! - [`update_account`]: a Mac's update account, read from its root-only file, and the update
//!   call, which gives its password to the program on its standard input alone.
//! - [`observer`]: what [`Daemon::status`] reports of a running service.
//! - [`node`]: the host's enrollment and the heartbeat loop's link, as the loops last wrote them,
//!   which [`Daemon::status`] reports besides the services.
//! - [`cli`]: `hostkeeper daemon` and its flags; the binary only forwards to it.
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
//! manager (`hostkeeper_placed::UnitControl`), the filesystem the daemon's links are made through
//! (`hostkeeper_placed::Fs`), the repository installer's tools ([`repository::RepositoryPorts`])
//! and the service observer (`hostkeeper_observe::Observer`). Time is the
//! `hostkeeper_steps::Clock`; nothing here reads the wall clock.

pub mod adapters;
pub mod archive_versions;
pub mod authorized_keys;
pub mod bmc;
pub mod boot;
pub mod builtin;
pub mod checks;
pub mod cli;
pub mod console;
pub mod coordinator_store;
pub mod entry;
pub mod executor;
pub mod facts_file;
mod files;
pub mod halt;
pub mod handover;
pub mod hook_log;
pub mod host_identity;
pub mod identity_group;
pub mod installed;
pub mod installers;
pub mod ipmi;
pub mod keep_awake;
#[cfg(test)]
mod key_fixture_tests;
mod launchd_plist;
pub mod layout;
pub mod line_process;
pub mod link_adapters;
pub mod links;
pub mod locks;
pub mod login_account;
pub mod mac;
pub mod mac_config;
pub mod mac_files;
pub mod mac_ssh;
pub mod macos_pkg;
pub mod maintenance_apk;
pub mod maintenance_apt;
pub mod maintenance_collect;
pub mod maintenance_collectors;
pub mod maintenance_dnf;
pub mod maintenance_facts;
pub mod maintenance_host;
pub mod maintenance_own;
pub mod maintenance_reboot;
pub mod maintenance_state;
pub mod mirror;
pub mod node;
pub mod notify;
pub mod observer;
pub mod operator_keys;
pub mod own_package;
pub mod packaged;
pub mod packages;
pub mod place;
pub mod prerequisites;
pub mod prune;
pub mod real_packages;
pub mod refusal;
pub mod remote_login_probe;
pub mod repositories;
pub mod repository;
pub mod repository_push;
pub mod rollback_floor;
pub mod run_units;
pub mod seed;
pub mod selfupdate;
pub mod ssh_effective;
pub mod ssh_files;
pub mod ssh_host;
pub mod ssh_host_mac;
pub mod ssh_setup;
pub mod stamp;
pub mod start;
pub mod target;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod uninstall;
pub mod unit_links;
pub mod unix_clock;
pub mod update_account;
pub mod verb;
pub mod verbs;
pub mod watchers;
pub mod xcode_clt;

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
