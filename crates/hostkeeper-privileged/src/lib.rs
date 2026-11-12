//! What the privileged tests share, and the part of them that can be tested without root.
//!
//! The privileged tests (`tests/transient_unit.rs`, `tests/observer.rs`, `tests/unit_control.rs`,
//! `tests/self_update.rs`) run as root against the real systemd of a host, in
//! `.github/workflows/privileged.yml`, after a merge to main, and in `pr-device.yml`'s copies of
//! its jobs, on a pull request (the macOS one when it changes a path the macOS jobs build or
//! read). Each of these files starts with
//! `#![cfg(feature = "privileged")]` (the first-boot and macOS files below with lines of their
//! own); the feature is off by default, so the pull-request set compiles none of them
//! ([`PRIVILEGED`]). They drive the production adapters
//! (`hostkeeper_steps::SystemdLauncher`, `hostkeeper_steps::DeadMan`, the observer's
//! `SystemdManager` and `ProcProbe`, the Linux unit control, and the daemon's own update under
//! hostkeeper's unit file), never a copy of them. The macOS device tests
//! (`tests/device_mac.rs`, first line `#![cfg(all(target_os = "macos", feature = "privileged"))]`)
//! do the same on a Mac, in release.yml's job `macos-device`, against launchd: the daemon
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
//! tierstore-shaped package (the program `tierstore-like` and `fixtures/tierstore-shaped/`, built
//! into a `.deb` in a signed apt repository the test makes) through its `apt` entry, roll it back
//! on a failed `verify` and force it back with the repository gone; its rig is `tests/e2e/`, test
//! code outside the measured set, whose root-free parts `tests/tierstore_rig.rs` runs in the
//! pull-request set. On a Mac the daemon announces `macos-pkg` (its signers file, `installer` and
//! `pkgutil`), and the same two end-to-end paths run through it: `tests/device_mac_e2e.rs` (a
//! macOS file, in the job `macos-device`) has the real coordinator (in process) install, update,
//! roll back on a failed `verify` and uninstall hello's `.pkg` (the program `hello-e2e`, whose
//! rig is `tests/e2e/hello.rs` and `tests/e2e/mac_host.rs`), and
//! `tests/device_mac_selfupdate.rs` (a macOS file, in the job `macos-device`) has hostkeeper
//! update itself through its `.pkg` under launchd (the program `hostkeeper-selfupdate-daemon`
//! on the Mac's adapters, `tests/mac_self_update/`); no package is signed on a runner, so the
//! program's `pkgutil --check-signature` of a package the test serves is answered as a signed
//! package's (`tests/shared/macos_pkg.rs`).
//! `tests/install_macos_pkg.rs` (a macOS file, in the job `macos-device`) drives
//! `hostkeeper_install`'s `MacosPkg` as root with the Mac's real `pkgutil`, `installer`, `lsbom`
//! and `launchctl`, against M11's unsigned fixture forms built at test time: nothing is signed and
//! no trust setting or keychain is changed, and only `pkgutil --check-signature` of a fixture the
//! test names is answered by the test. The network-install tests (`tests/net_install.rs`, first line
//! `#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "privileged"))]`, job
//! `net-install`, and `tests/net_install_arm64.rs`, job `net-install-arm64`, both in
//! `.github/workflows/net-install.yml`) install a QEMU guest
//! by network with the real `pxe` (the program `hostkeeper-privileged-pxe`, which
//! forwards to `hostkeeper_pxe::cli::main`) and the real coordinator's LAN listener, on a bridge
//! of the test's with no route out, and, in `tests/net_install.rs`, install the `pxe` .deb under
//! systemd through the coordinator and the daemon; their rig is `tests/netinstall/`, whose root-free parts
//! `tests/net_install_rig.rs` runs in the pull-request set.
//! `tests/self_update_across_releases.rs` (first line `#![cfg(feature = "privileged")]`, in the
//! job `privileged`) is the cross-release rig: the daemon binary of the published release
//! `previous-release.toml` pins, under the host's own unit, updates to this build (the programs
//! `hostkeeper-named-daemon`, versioned by the file it runs from, and `hostkeeper-failing-daemon`,
//! which cannot start) through hostkeeper's `.deb` and the end-to-end rig; its rig is
//! `tests/across/`.
//! `tests/openrc_device.rs` (first line
//! `#![cfg(all(target_os = "linux", target_env = "gnu", feature = "privileged"))]`, privileged.yml's
//! job `openrc-device`) runs the daemon on Alpine under OpenRC, in a container of the image
//! `openrc/Dockerfile` whose init is OpenRC: hostkeeper's Alpine package installed and run under
//! supervise-daemon; its rig is `tests/openrc/`.
//! `tests/install_apt.rs` (first line `#![cfg(feature = "privileged")]`, the job `privileged`)
//! runs the `apt` backend of the repository installer (`hostkeeper_install::repository`) as root
//! against the runner's own apt, dpkg and systemd, with a signed fixture repository the test
//! builds on disk with `dpkg-deb` and `gpg`: install, update, rollback with the repository
//! removed, an upgrade that leaves the held version, drift put back, the refusals of `stage`,
//! and uninstall; the daemon's own order path over the same host (`Daemon::order`: `apt`
//! announced, then an install, an update, a `force_rollback` with the repository removed and an
//! uninstall, `tests/install_apt/daemon.rs`); and the backend's `apt-get update` and simulation
//! in a container of each deb release of `linux-releases.toml`. Its rig is `tests/install_apt/`.
//!
//! - [`across`]: the cross-release rig's pin of the previous release, the check of its
//!   downloaded tarball against the pin, and the version a program of this build runs as.
//! - [`linux_packages`]: the list of Linux releases the packages are installed on
//!   (`linux-releases.toml`), the finding of the packages the job built, and the script and
//!   arguments of the container each release runs (`tests/linux_packages.rs`, job `privileged`).
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
pub mod linux_packages;
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
