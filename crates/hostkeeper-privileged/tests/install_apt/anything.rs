//! A package is any package: what hostkeeper does with the ones that carry maintainer scripts,
//! files outside `/usr`, a hook program of their own, or a run unit nothing enables, as root on
//! the host's real dpkg and systemd (the rig and the fixture repository of this test, and for the
//! orders the daemon's own order path).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_install::repository::{Reason, policy};
use hostkeeper_proto::hostkeeper::release::v1::Target;

use super::daemon::{self, HOOK_COMMAND};
use super::repo::PACKAGE;
use super::rig::{LINK, Rig, Trust, dpkg_version, print};

/// The policy file the package manager asks before it starts a service.
const POLICY: &str = "/usr/sbin/policy-rc.d";
/// What the fixture's `postinst` copied of the policy file it found.
const SEEN: &str = "/run/hkaptfix-policy-seen";
/// What the policy file answered when the `postinst` ran it.
const SEEN_EXIT: &str = "/run/hkaptfix-policy-exit";
/// The steps the fixture hook was given, one line each.
const HOOK_LOG: &str = "/run/hkaptfix-hook.log";
/// While it exists the fixture hook fails `verify`.
const FAIL_VERIFY: &str = "/run/hkaptfix-hook.fail-verify";

/// Whether `hkaptfix.service` is active.
fn active() -> String {
    print("systemctl", &["is-active", "hkaptfix.service"])
        .text
        .trim()
        .to_owned()
}

/// The policy file of the host as a test sets it, put back as it was when the test ends.
struct HostPolicy {
    saved: Option<Vec<u8>>,
}

impl HostPolicy {
    /// Writes `text` as the policy file, mode 0755, keeping what was there.
    fn put(text: &str) -> HostPolicy {
        let saved = fs::read(POLICY).ok();
        fs::create_dir_all("/usr/sbin").expect("the directory of the policy file");
        fs::write(POLICY, text).expect("write the policy file");
        fs::set_permissions(POLICY, fs::Permissions::from_mode(0o755)).expect("its mode");
        HostPolicy { saved }
    }
}

impl Drop for HostPolicy {
    fn drop(&mut self) {
        match &self.saved {
            Some(bytes) => {
                let _ = fs::write(POLICY, bytes);
                let _ = fs::set_permissions(POLICY, fs::Permissions::from_mode(0o755));
            }
            None => {
                let _ = fs::remove_file(POLICY);
            }
        }
    }
}

/// The verdict and reason an order finished with.
fn finished(reply: hostkeeper_daemon::Reply) -> (hostkeeper_hook::Verdict, Option<String>) {
    match reply {
        hostkeeper_daemon::Reply::Finished {
            verdict, reason, ..
        } => (verdict, reason),
        other => panic!("the order did not finish: {other:?}"),
    }
}

/// The success a finished order reports.
fn succeeded() -> (hostkeeper_hook::Verdict, Option<String>) {
    (
        hostkeeper_hook::Verdict::Succeeded {
            promotion_pending: false,
        },
        None,
    )
}

/// The target of `package_version` whose manifest names the fixture hook.
fn hooked(rig: &Rig, package_version: &str) -> Target {
    daemon::entry_running(package_version, &rig.pin(package_version), &HOOK_COMMAND)
}

/// Proves: while dpkg runs the maintainer script of a package that starts its unit with
/// `deb-systemd-invoke`, the host's policy file is hostkeeper's (byte for byte, answering 101),
/// so the unit is not started by `activate`; the file is gone when `activate` ends; and the
/// daemon's install order (the built-in `cutover`) then starts the unit.
///
/// Catches: a script that starts a service under hostkeeper's feet during `activate`, and a
/// policy file left on the host.
#[test]
fn a_postinst_that_starts_its_unit_is_held_back_by_the_policy_and_cutover_starts_it() {
    let rig = Rig::new("apt-policy", Trust::Entry);
    let tools = rig.tools();
    let installer = rig.installer(&tools);
    let (order, dir) = (rig.order("0.4-start"), rig.staging("start"));
    installer.stage(&order, &dir).expect("stage 0.4-start");
    installer
        .activate(&order, &dir)
        .expect("activate 0.4-start");

    assert_eq!(dpkg_version().as_deref(), Some("0.4-start"));
    assert_eq!(
        fs::read_to_string(SEEN).expect("the script's copy"),
        policy::TEXT,
        "dpkg ran the script with hostkeeper's policy in place"
    );
    assert_eq!(
        fs::read_to_string(SEEN_EXIT).expect("its answer").trim(),
        "101"
    );
    assert_ne!(active(), "active", "the script did not start the unit");
    assert!(
        fs::symlink_metadata(POLICY).is_err(),
        "the policy file is gone once activate ends"
    );
    installer
        .remove(&rig.service, 120)
        .expect("remove 0.4-start");
    assert_eq!(dpkg_version(), None);
    assert!(fs::symlink_metadata(POLICY).is_err());

    let (daemon, _, _scratch) = daemon::daemon(&rig);
    let pin = rig.pin("0.4-start");
    assert_eq!(
        finished(daemon.order(&daemon::install("0.4-start", &pin, 1))),
        succeeded()
    );
    assert_eq!(active(), "active", "cutover starts the unit");
    assert!(fs::symlink_metadata(POLICY).is_err());
}

/// Proves: a package with its files under `/opt/<package>/` and a link `/usr/bin/<x>` to a
/// program there installs, and the manifest's hook command `/usr/bin/<x> update-hook` stages
/// (the link is followed inside the extracted tree); after the purge the files and the link are
/// gone.
///
/// Catches: a stage that refuses files outside `/usr` or does not follow a link in the tree to
/// the hook program.
#[test]
fn files_under_opt_and_a_hook_program_reached_through_a_link_stage_and_install() {
    let rig = Rig::new("apt-opt", Trust::Entry);
    let tools = rig.tools();
    let installer = rig.installer(&tools);
    let mut order = rig.order("0.5-opt");
    order.hook = Some("/usr/bin/hkaptfix-opt".to_owned());
    let dir = rig.staging("opt");
    let staged = installer.stage(&order, &dir).expect("stage 0.5-opt");
    assert!(staged.hook, "the manifest names a hook program");
    assert!(dir.join("tree/opt/hkaptfix/bin/hkaptfix-opt").is_file());

    installer.activate(&order, &dir).expect("activate 0.5-opt");
    assert_eq!(
        fs::read_link("/usr/bin/hkaptfix-opt").expect("the link"),
        Path::new("/opt/hkaptfix/bin/hkaptfix-opt")
    );
    assert_eq!(print("/usr/bin/hkaptfix-opt", &[]).text.trim(), "opt");

    installer.remove(&rig.service, 120).expect("remove 0.5-opt");
    assert!(fs::symlink_metadata("/usr/bin/hkaptfix-opt").is_err());
    assert!(fs::symlink_metadata("/opt/hkaptfix/bin/hkaptfix-opt").is_err());
}

/// Proves: a run unit with no `[Install]` section gets no enablement link and is active after
/// the install order (the built-in `cutover` starts it), and after `systemctl stop` the daemon's
/// start starts it again.
///
/// Catches: a unit that runs only until its first stop because nothing enables it and nothing
/// starts it at boot.
#[test]
fn a_run_unit_without_install_is_started_by_cutover_and_again_by_the_daemons_start() {
    let rig = Rig::new("apt-noinstall", Trust::Entry);
    let (daemon, _, _scratch) = daemon::daemon(&rig);
    let pin = rig.pin("0.6-noinstall");
    assert_eq!(
        finished(daemon.order(&daemon::install("0.6-noinstall", &pin, 1))),
        succeeded()
    );
    assert!(
        fs::symlink_metadata(LINK).is_err(),
        "nothing enables the unit"
    );
    assert_eq!(active(), "active");

    print("systemctl", &["stop", "hkaptfix.service"]);
    assert_ne!(active(), "active");
    daemon::start(&daemon);
    assert_eq!(active(), "active", "the start pass starts the unit");
}

/// Proves: a policy file with hostkeeper's text (left by a killed run) is removed by
/// `clear_stale`, which the daemon calls at its start, and an operator's own policy file is kept
/// byte for byte; and `activate` over a stale file of ours installs and leaves none, while over an
/// operator's it installs and leaves it untouched.
///
/// Catches: a stale policy file that silences every later service start on the host, and an
/// installer that deletes or replaces the operator's policy.
#[test]
fn a_stale_policy_file_of_ours_is_removed_and_an_operators_is_kept() {
    let rig = Rig::new("apt-stale-policy", Trust::Entry);
    let root = Path::new("/");

    let stale = HostPolicy::put(policy::TEXT);
    assert!(policy::clear_stale(root).expect("clear"), "ours is removed");
    assert!(fs::symlink_metadata(POLICY).is_err());
    drop(stale);

    let own = "#!/bin/sh\n# the operator's\nexit 0\n";
    let operator = HostPolicy::put(own);
    assert!(!policy::clear_stale(root).expect("clear"), "theirs is kept");
    assert_eq!(fs::read_to_string(POLICY).expect("kept"), own);

    let tools = rig.tools();
    let installer = rig.installer(&tools);
    let (order, dir) = (rig.order("1.0-1"), rig.staging("operator"));
    installer.stage(&order, &dir).expect("stage 1.0-1");
    installer.activate(&order, &dir).expect("activate 1.0-1");
    assert_eq!(dpkg_version().as_deref(), Some("1.0-1"));
    assert_eq!(fs::read_to_string(POLICY).expect("kept"), own);
    installer.remove(&rig.service, 120).expect("remove 1.0-1");
    assert_eq!(fs::read_to_string(POLICY).expect("kept"), own);
    drop(operator);

    let _stale = HostPolicy::put(policy::TEXT);
    installer
        .activate(&order, &dir)
        .expect("activate over a stale file");
    assert_eq!(dpkg_version().as_deref(), Some("1.0-1"));
    assert!(
        fs::symlink_metadata(POLICY).is_err(),
        "ours is removed after"
    );
}

/// Proves: a manifest whose hook command names a program the package does not install fails
/// `stage` with `manifest_invalid`, the failure text names the command, and nothing is
/// installed.
///
/// Catches: a hook named in the manifest that is found missing only when a step runs.
#[test]
fn a_hook_command_missing_from_the_package_is_manifest_invalid_at_stage() {
    let rig = Rig::new("apt-missing-hook", Trust::Entry);
    let tools = rig.tools();
    let installer = rig.installer(&tools);
    let mut order = rig.order("1.0-1");
    order.hook = Some("/usr/bin/hkaptfix-missing".to_owned());
    let failure = installer
        .stage(&order, &rig.staging("missing"))
        .expect_err("a hook program the package does not install");
    assert_eq!(failure.reason(), Reason::ManifestInvalid, "{failure}");
    assert!(
        failure.to_string().contains("/usr/bin/hkaptfix-missing"),
        "the failure names the command: {failure}"
    );
    assert_eq!(dpkg_version(), None);
}

/// Proves: the manifest's hook command runs the package's own program for the steps, with its
/// leading argument before the step's name, and when `verify` fails on the update the program is
/// given `rollback` after it, and the update finishes failed.
///
/// Catches: a hook command that is ignored for the built-in steps, leading arguments that are
/// dropped, and a failed `verify` that does not reach the program's `rollback`.
#[test]
fn the_manifest_hook_runs_the_steps_and_its_rollback_runs_when_verify_fails() {
    let rig = Rig::new("apt-hook", Trust::Entry);
    let (daemon, _, _scratch) = daemon::daemon(&rig);
    let (one, two) = (rig.pin("4.0-hook"), rig.pin("4.1-hook"));

    let install = daemon::install_of(hooked(&rig, "4.0-hook"), "4.0-hook", &one, 1);
    assert_eq!(finished(daemon.order(&install)), succeeded());
    let log = fs::read_to_string(HOOK_LOG).expect("the hook's log");
    for step in ["prepare", "cutover", "verify"] {
        assert!(
            log.lines().any(|l| l == format!("update-hook {step}")),
            "the program was given `update-hook {step}`:\n{log}"
        );
    }
    assert_eq!(active(), "active");

    fs::write(FAIL_VERIFY, "").expect("make verify fail");
    fs::remove_file(HOOK_LOG).expect("start a new log");
    let update = daemon::update_of(hooked(&rig, "4.1-hook"), "4.1-hook", &two, 2);
    let (verdict, _) = finished(daemon.order(&update));
    assert!(
        matches!(verdict, hostkeeper_hook::Verdict::Failed { .. }),
        "the update fails: {verdict:?}"
    );
    let log = fs::read_to_string(HOOK_LOG).expect("the hook's log");
    let steps: Vec<&str> = log.lines().collect();
    let verify = steps.iter().position(|l| *l == "update-hook verify");
    let rollback = steps.iter().position(|l| *l == "update-hook rollback");
    assert!(
        verify.is_some() && rollback > verify,
        "rollback follows the failed verify:\n{log}"
    );
    // The switch back reinstalled the previous package before `rollback` ran.
    assert_eq!(
        dpkg_version().as_deref(),
        Some("4.0-hook"),
        "{PACKAGE} is back at the previous version"
    );
}
