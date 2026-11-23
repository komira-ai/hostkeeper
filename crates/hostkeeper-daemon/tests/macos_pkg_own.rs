//! The launchd job of hostkeeper's own `.pkg` on a Mac (packages-repositories.md section 4.7):
//! its manifest names no `run`, yet its payload ships the daemon's job plist. An `activate` of
//! the package enables the daemon's job and never disables it, over a kept previous version and
//! on a rollback alike; an `activate` of any other service's package leaves the daemon's job
//! alone. The Mac is the installer's fake, over a scratch root.

mod support;
mod support_macos_pkg;

use std::fs;
use std::time::Duration;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::repository::Native;
use hostkeeper_daemon::{Daemon, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_install::repository::Reason;
use hostkeeper_install::{Tool, Tools};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use support_macos_pkg::{FakePkg, MacRig, entry, install_of, manifest, package, update_of};

/// The service name of hostkeeper's own package, its identifier and its job's label.
const OWN: &str = "hostkeeper";
const OWN_IDENTIFIER: &str = "dev.hostkeeper.hostkeeper";
const OWN_LABEL: &str = "dev.hostkeeper.hostkeeper";
const OWN_JOB: &str = "system/dev.hostkeeper.hostkeeper";

/// Hostkeeper's own package at `version`: the daemon's job plist in the payload.
fn own_package(version: &str) -> FakePkg {
    FakePkg::standard(OWN, OWN_IDENTIFIER, version)
        .without(&format!("/Library/LaunchDaemons/{OWN}.plist"))
        .label(OWN_LABEL)
}

/// Teaches the Mac `pkg`, the package of hostkeeper at `version`, served by its pin; its entry,
/// whose manifest names no `run`.
fn publish_own(mac: &MacRig, version: &str, pkg: FakePkg) -> Target {
    let bytes = format!("fake-pkg:{OWN}-{version}");
    let digest = support_macos_pkg::sha256_hex(bytes.as_bytes());
    mac.rig.pins.add_pinned(OWN, &digest, bytes.as_bytes());
    mac.mac.add(pkg);
    let mut own = entry(&digest, bytes.len() as u64);
    let mut named = manifest();
    named.package = Some(OWN_IDENTIFIER.to_owned());
    named.run.clear();
    own.manifest = Some(named);
    own
}

/// Stages `version` of hostkeeper's package from `target` and activates it as the daemon's
/// repository does at a switch.
fn activate_own(
    daemon: &Daemon,
    version: &str,
    target: &Target,
) -> Result<(), hostkeeper_install::repository::Failure> {
    let repository = daemon.ctx().repository();
    let name = ServiceName::new(OWN).expect("a name");
    let at = Version::new(version).expect("a version");
    let staged = repository
        .stage(Native::MacosPkg, &name, &at, target)
        .unwrap_or_else(|reply| panic!("{version} stages: {reply:?}"));
    let dir = staged.dir().to_path_buf();
    repository.activate_installed(&name, &at, &dir, target)
}

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// What launchd was asked, as the verb and the job.
fn asked(mac: &MacRig) -> Vec<Vec<String>> {
    mac.mac.calls_of(Tool::Launchctl)
}

fn current(mac: &MacRig) -> Option<String> {
    fs::read_link(mac.rig.packages.join(OWN).join("current"))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

/// Leaves a `disable` override on the daemon's job, as an earlier release's `activate` did.
fn disable_own_job(mac: &MacRig) {
    mac.mac
        .run(
            Tool::Launchctl,
            &["disable".to_owned(), OWN_JOB.to_owned()],
            Duration::from_secs(1),
        )
        .expect("the fake runs");
}

/// Proves that an update of hostkeeper's own package over a kept `.pkg` version, whose
/// manifests name no `run`, enables the daemon's job (clearing an override an earlier release
/// left) and runs no `launchctl disable`; the same holds for the rollback to the kept version.
/// Catches: an `activate` that disables the daemon's job because its plist is in the kept tree,
/// or one that leaves the override standing.
#[test]
fn an_update_and_a_rollback_of_the_own_package_enable_the_daemons_job_and_disable_none() {
    let mac = MacRig::new("pkg-own-update");
    let daemon = mac.daemon();
    let one = publish_own(&mac, "1.0.0", own_package("1.0.0"));
    let two = publish_own(&mac, "1.1.0", own_package("1.1.0"));

    activate_own(&daemon, "1.0.0", &one).expect("a first install");
    assert_eq!(current(&mac).as_deref(), Some("1.0.0"));
    assert_eq!(mac.mac.enabled(OWN_LABEL), Some(true));

    disable_own_job(&mac);
    assert_eq!(mac.mac.enabled(OWN_LABEL), Some(false));
    mac.mac.clear_calls();
    activate_own(&daemon, "1.1.0", &two).expect("the update");
    assert_eq!(current(&mac).as_deref(), Some("1.1.0"));
    assert_eq!(asked(&mac), [["enable", OWN_JOB]]);
    assert_eq!(mac.mac.enabled(OWN_LABEL), Some(true));

    disable_own_job(&mac);
    mac.mac.clear_calls();
    activate_own(&daemon, "1.0.0", &one).expect("the rollback");
    assert_eq!(current(&mac).as_deref(), Some("1.0.0"));
    assert_eq!(asked(&mac), [["enable", OWN_JOB]]);
    assert_eq!(mac.mac.enabled(OWN_LABEL), Some(true));
}

/// Proves that an `activate` of another service's package does not enable the daemon's job:
/// its install and its update run `launchctl` for its own `run` labels alone.
#[test]
fn an_update_of_another_service_does_not_touch_the_daemons_job() {
    let mac = MacRig::new("pkg-other-update");
    let daemon = mac.daemon();
    let one = mac.publish("1.0.0", package("1.0.0"));
    let two = mac.publish("1.1.0", package("1.1.0"));
    assert_eq!(daemon.order(&install_of("1.0.0", &one, 1)), succeeded());
    assert_eq!(daemon.order(&update_of("1.1.0", &two, 2)), succeeded());
    let calls = asked(&mac);
    assert_eq!(
        calls,
        [
            ["enable", "system/hello.service"],
            ["enable", "system/hello.service"]
        ]
    );
    assert_eq!(mac.mac.enabled(OWN_LABEL), None);
}

/// Proves that a `launchctl enable` of the daemon's job that fails fails the `activate` as
/// `installer_error` with `current` still the kept version, and that the `activate` run again
/// ends enabled with `current` switched.
/// Catches: a failed enable ignored, or `current` switched over a job left disabled.
#[test]
fn a_failing_enable_of_the_daemons_job_fails_the_activate_and_a_second_try_succeeds() {
    let mac = MacRig::new("pkg-own-enable-fails");
    let daemon = mac.daemon();
    let one = publish_own(&mac, "1.0.0", own_package("1.0.0"));
    let two = publish_own(&mac, "1.1.0", own_package("1.1.0"));
    activate_own(&daemon, "1.0.0", &one).expect("a first install");
    disable_own_job(&mac);

    mac.mac
        .exit(Tool::Launchctl, &["enable", OWN_JOB], 1, "no such service");
    let err = activate_own(&daemon, "1.1.0", &two).expect_err("the enable fails");
    assert_eq!(err.reason(), Reason::InstallerError);
    assert_eq!(current(&mac).as_deref(), Some("1.0.0"), "not switched");
    assert_eq!(mac.mac.enabled(OWN_LABEL), Some(false));

    mac.mac.clear_faults();
    activate_own(&daemon, "1.1.0", &two).expect("the second try");
    assert_eq!(current(&mac).as_deref(), Some("1.1.0"));
    assert_eq!(mac.mac.enabled(OWN_LABEL), Some(true));
}
