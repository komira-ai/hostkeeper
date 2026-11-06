//! What the privileged tests share, and the part of them that can be tested without root.
//!
//! The privileged tests (`tests/transient_unit.rs`, `tests/observer.rs`, `tests/unit_control.rs`,
//! `tests/self_update.rs`) run as root against the
//! real systemd of a host, in `.github/workflows/privileged.yml` only, after a merge to main.
//! Each file starts with `#![cfg(feature = "privileged")]`; the feature is off by default, so the
//! pull-request set compiles none of them ([`PRIVILEGED`]). They drive the production adapters
//! (`hostkeeper_steps::SystemdLauncher`, `hostkeeper_steps::DeadMan`, the observer's
//! `SystemdManager` and `ProcProbe`, the Linux unit control, and the daemon's own update under
//! hostkeeper's unit file, drop-in and fallback unit), never a copy of them. The macOS device tests
//! (`tests/device_mac.rs`, first line `#![cfg(all(target_os = "macos", feature = "privileged"))]`)
//! do the same on a Mac, in privileged.yml's job `macos-device`, against launchd: the daemon
//! (the program `hostkeeper-device-daemon`, which forwards to `hostkeeper_daemon::cli::main`),
//! `hostkeeper_launchd`'s launcher, service manager and process probe, and the daemon's package
//! check. `tests/tierstore_shaped.rs` runs the whole update path end to end: the real
//! coordinator and the real daemon (that same program, under a `Type=notify` unit) update a
//! tierstore-shaped package (the program `tierstore-like` and `fixtures/tierstore-shaped/`),
//! roll it back on a failed `verify` and force it back with no fetch; its rig is `tests/e2e/`,
//! test code outside the measured set, whose root-free parts `tests/tierstore_rig.rs` runs in
//! the pull-request set.
//!
//! - [`fixture`]: the workload fixture, the program `hostkeeper-fixture-hook` that speaks the hook
//!   contract in the modes a test picks, and `serve`, a long-running main process for a unit. Its
//!   interface is documented and kept stable for any test that drives it.
//! - [`host`]: the check a privileged test makes before it acts: root, and systemd as the
//!   service manager.
//! - [`rig`]: the names and files of one test: a unique unit name per unit, the params files of
//!   the fixture, the unit file of `serve`, and the `systemctl` lines that tear every unit down.

pub mod fixture;
pub mod host;
pub mod rig;

pub use fixture::{Ending, FixtureError, Me, Mode, PidFile, Run, StepRun};
pub use host::{HostError, SYSTEMD_RUNTIME_DIR, check_host};
pub use rig::{Rig, RigError, SERVICE, UNIT_PREFIX};

/// Whether this build has the feature `privileged`, which compiles the privileged tests. False in
/// every build of the workspace's own test run; true only in privileged.yml's.
pub const PRIVILEGED: bool = cfg!(feature = "privileged");

#[cfg(test)]
mod tests;
