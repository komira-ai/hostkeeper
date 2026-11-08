//! What the privileged tests share, and the part of them that can be tested without root.
//!
//! The privileged tests (`tests/transient_unit.rs`, `tests/observer.rs`, `tests/unit_control.rs`,
//! `tests/self_update.rs`) run as root against the real systemd of a host, in
//! `.github/workflows/privileged.yml`, after a merge to main, and in `pr-device.yml`'s copies of
//! its jobs, on every pull request. Each of these files starts with
//! `#![cfg(feature = "privileged")]` (the first-boot and macOS files below with lines of their
//! own); the feature is off by default, so the pull-request set compiles none of them
//! ([`PRIVILEGED`]). They drive the production adapters
//! (`hostkeeper_steps::SystemdLauncher`, `hostkeeper_steps::DeadMan`, the observer's
//! `SystemdManager` and `ProcProbe`, the Linux unit control, and the daemon's own update under
//! hostkeeper's unit file, drop-in and fallback unit), never a copy of them. The macOS device tests
//! (`tests/device_mac.rs`, first line `#![cfg(all(target_os = "macos", feature = "privileged"))]`)
//! do the same on a Mac, in privileged.yml's job `macos-device`, against launchd: the daemon
//! (the program `hostkeeper-device-daemon`, which forwards to `hostkeeper_daemon::cli::main`),
//! `hostkeeper_launchd`'s launcher, service manager and process probe, and the daemon's package
//! check. The first-boot tests (`tests/first_boot.rs`, `tests/install_uninstall.rs`, first line
//! `#![cfg(all(target_os = "linux", feature = "privileged"))]`, privileged.yml's job `first-boot`)
//! start the programs themselves as child processes: the coordinator (the program
//! `hostkeeper-privileged-coordinator`, which forwards to
//! `hostkeeper_coordinator::config::main`) and the daemon (`hostkeeper-device-daemon`), and drive
//! them as an operator and a host do.
//! `tests/tierstore_shaped.rs` runs the whole update path end to end: the real coordinator (in
//! process) and the real daemon (`hostkeeper-device-daemon`, under a `Type=notify` unit) update a
//! tierstore-shaped package (the program `tierstore-like` and `fixtures/tierstore-shaped/`),
//! roll it back on a failed `verify` and force it back with no fetch; its rig is `tests/e2e/`,
//! test code outside the measured set, whose root-free parts `tests/tierstore_rig.rs` runs in
//! the pull-request set. `tests/device_mac_e2e.rs` (a macOS file, in the job `macos-device`)
//! runs the same path on a Mac with hello's macOS package (the program `hello-e2e` as its
//! `bin/hello`): install, update, a failed `verify` rolled back and uninstall under launchd,
//! with the rig's macOS host (`tests/e2e/mac_host.rs`); `tests/hello_rig.rs` checks its packages
//! in the pull-request set. `tests/device_mac_selfupdate.rs` (a macOS file, in the job
//! `macos-device`) is the Mac twin of `tests/self_update.rs`: the program
//! `hostkeeper-selfupdate-daemon` as a launchd job from `current` of its binary directory,
//! updated and installed to the next version and restarted into it by launchd, with its rig in
//! `tests/mac_self_update/`. The network-install tests (`tests/net_install.rs`, first line
//! `#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "privileged"))]`, job
//! `net-install`, and `tests/net_install_arm64.rs`, job `net-install-arm64`, both in
//! `.github/workflows/net-install.yml`) install a QEMU guest
//! by network with the real `hostkeeper-pxe` (the program `hostkeeper-privileged-pxe`, which
//! forwards to `hostkeeper_pxe::cli::main`) and the real coordinator's LAN listener, on a bridge
//! of the test's with no route out; their rig is `tests/netinstall/`, whose root-free parts
//! `tests/net_install_rig.rs` runs in the pull-request set.
//! `tests/self_update_across_releases.rs` (first line
//! `#![cfg(all(any(target_os = "linux", target_os = "macos"), feature = "privileged"))]`, in both
//! the job `privileged` and the job `macos-device`) is the cross-release rig: the daemon binary of
//! the published release `previous-release.toml` pins, under the host's own unit or launchd job,
//! updates to this build (the programs `hostkeeper-named-daemon`, versioned by the file it runs
//! from, and `hostkeeper-failing-daemon`, which cannot start) through the end-to-end rig; its rig
//! is `tests/across/`.
//!
//! - [`across`]: the cross-release rig's pin of the previous release, the check of its
//!   downloaded tarball against the pin, and the version a program of this build runs as.
//! - [`fixture`]: the workload fixture, the program `hostkeeper-fixture-hook` that speaks the hook
//!   contract in the modes a test picks, and `serve`, a long-running main process for a unit. Its
//!   interface is documented and kept stable for any test that drives it.
//! - [`host`]: the check a privileged test makes before it acts: root, and systemd as the
//!   service manager.
//! - [`rig`]: the names and files of one test: a unique unit name per unit, the params files of
//!   the fixture, the unit file of `serve`, and the `systemctl` lines that tear every unit down.

pub mod across;
pub mod fixture;
pub mod host;
pub mod rig;

pub use fixture::{Ending, FixtureError, Me, Mode, PidFile, Run, StepRun};
pub use host::{HostError, SYSTEMD_RUNTIME_DIR, check_host};
pub use rig::{Rig, RigError, SERVICE, UNIT_PREFIX};

/// Whether this build has the feature `privileged`, which compiles the privileged tests. False in
/// every build of the workspace's own test run; true only in the builds of privileged.yml's jobs
/// and of pr-device.yml's copies of them.
pub const PRIVILEGED: bool = cfg!(feature = "privileged");

#[cfg(test)]
mod tests;
