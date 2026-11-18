//! The tests of `mac_ssh`: the three programs as scripted ports, the exact arguments each step runs
//! them with, and the lines it logs.

use std::path::PathBuf;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError};
use hostkeeper_link::testing::RecordingLog;

use super::*;

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The three ports of a Mac, and the fakes behind them.
struct Rig {
    steps: MacSsh,
    launchctl: FakeLaunchctl,
    dscl: FakeLaunchctl,
    dseditgroup: FakeLaunchctl,
    log: RecordingLog,
}

fn fresh() -> Rig {
    let launchctl = FakeLaunchctl::new();
    let dscl = FakeLaunchctl::new();
    let dseditgroup = FakeLaunchctl::new();
    Rig {
        steps: MacSsh::new(
            Box::new(launchctl.clone()),
            Box::new(dscl.clone()),
            Box::new(dseditgroup.clone()),
        ),
        launchctl,
        dscl,
        dseditgroup,
        log: RecordingLog::default(),
    }
}

fn account() -> Account {
    Account {
        name: "alice".to_owned(),
        uid: 501,
        gid: 20,
        home: PathBuf::from(concat!("/Users", "/alice")),
    }
}

fn print() -> Vec<String> {
    words(&["print", "system/com.openssh.sshd"])
}

fn enable() -> Vec<String> {
    words(&["enable", "system/com.openssh.sshd"])
}

fn bootstrap() -> Vec<String> {
    words(&[
        "bootstrap",
        "system",
        "/System/Library/LaunchDaemons/ssh.plist",
    ])
}

fn group_read() -> Vec<String> {
    words(&[
        ".",
        "-read",
        "/Groups/com.apple.access_ssh",
        "GroupMembership",
    ])
}

fn add() -> Vec<String> {
    words(&[
        "-o",
        "edit",
        "-a",
        "alice",
        "-t",
        "user",
        "com.apple.access_ssh",
    ])
}

const LOADED: &str = "system/com.openssh.sshd = {\n\tstate = running\n}\n";

/// Proves a loaded sshd job leaves Remote Login alone: only `launchctl print` runs, nothing is
/// logged.
#[test]
fn a_loaded_sshd_is_left_alone() {
    let rig = fresh();
    rig.launchctl
        .always(&print(), Ok(LaunchctlOutput::success(LOADED)));
    rig.steps.ensure_remote_login(&rig.log).expect("on");
    assert_eq!(rig.launchctl.calls(), [print()]);
    assert!(rig.log.lines().is_empty());
}

/// Proves sshd's job that is not loaded is enabled and bootstrapped, then looked at again, with
/// exactly these arguments and in this order, and the change is logged; `systemsetup` is never run.
#[test]
fn an_sshd_that_is_not_loaded_is_enabled_and_bootstrapped() {
    let rig = fresh();
    rig.launchctl
        .then(&print(), Ok(LaunchctlOutput::not_loaded()));
    rig.launchctl
        .then(&print(), Ok(LaunchctlOutput::success(LOADED)));
    rig.launchctl
        .always(&enable(), Ok(LaunchctlOutput::success("")));
    rig.launchctl
        .always(&bootstrap(), Ok(LaunchctlOutput::success("")));
    rig.steps.ensure_remote_login(&rig.log).expect("on");
    assert_eq!(
        rig.launchctl.calls(),
        [print(), enable(), bootstrap(), print()]
    );
    assert_eq!(
        rig.log.lines(),
        ["ssh: Remote Login turned on (launchctl enable and bootstrap of system/com.openssh.sshd)"]
    );
}

/// Proves an sshd that is still not loaded after its enable and bootstrap is an error that names
/// the step and its exit code, and nothing is logged as done.
#[test]
fn an_sshd_that_is_still_not_loaded_is_an_error() {
    let rig = fresh();
    rig.launchctl
        .always(&print(), Ok(LaunchctlOutput::not_loaded()));
    rig.launchctl
        .always(&enable(), Ok(LaunchctlOutput::success("")));
    rig.launchctl
        .always(&bootstrap(), Ok(LaunchctlOutput::success("")));
    let error = rig.steps.ensure_remote_login(&rig.log).expect_err("not on");
    assert_eq!(
        error,
        MacSshError::RemoteLogin(
            "launchctl print of sshd after its enable and bootstrap failed with exit code 113: \
             Could not find service: system/x"
                .to_owned()
        )
    );
    assert_eq!(
        error.to_string(),
        "Remote Login is not on: launchctl print of sshd after its enable and bootstrap failed \
         with exit code 113: Could not find service: system/x"
    );
    assert!(rig.log.lines().is_empty());
}

/// Proves an enable or a bootstrap that exits non-zero stops the steps with its name and exit
/// code (the bootstrap is not tried after a failed enable), and a launchctl that cannot be run
/// fails the step it was asked for.
#[test]
fn a_failing_enable_or_bootstrap_stops_the_steps() {
    let rig = fresh();
    rig.launchctl
        .always(&print(), Ok(LaunchctlOutput::not_loaded()));
    rig.launchctl
        .always(&enable(), Ok(LaunchctlOutput::exit(1, "denied\n")));
    assert_eq!(
        rig.steps.ensure_remote_login(&rig.log),
        Err(MacSshError::RemoteLogin(
            "launchctl enable of sshd failed with exit code 1: denied".to_owned()
        ))
    );
    assert_eq!(rig.launchctl.calls(), [print(), enable()]);

    let rig = enabled();
    rig.launchctl.always(
        &bootstrap(),
        Ok(LaunchctlOutput::exit(5, "Input/output error\n")),
    );
    assert_eq!(
        rig.steps.ensure_remote_login(&rig.log),
        Err(MacSshError::RemoteLogin(
            "launchctl bootstrap of sshd failed with exit code 5: Input/output error".to_owned()
        ))
    );

    let rig = enabled();
    assert_eq!(
        rig.steps.ensure_remote_login(&rig.log),
        Err(MacSshError::RemoteLogin(
            "launchctl bootstrap of sshd could not be run: the fake has no answer".to_owned()
        ))
    );
}

/// A rig whose sshd is not loaded and whose enable succeeds.
fn enabled() -> Rig {
    let rig = fresh();
    rig.launchctl
        .always(&print(), Ok(LaunchctlOutput::not_loaded()));
    rig.launchctl
        .always(&enable(), Ok(LaunchctlOutput::success("")));
    rig
}

/// Proves the first look at sshd that cannot be run is an error naming the step.
#[test]
fn a_launchctl_that_cannot_be_run_is_an_error() {
    let rig = fresh();
    rig.launchctl.always(
        &print(),
        Err(LaunchctlError::TimedOut {
            args: "print".to_owned(),
        }),
    );
    assert_eq!(
        rig.steps.ensure_remote_login(&rig.log),
        Err(MacSshError::RemoteLogin(
            "launchctl print of sshd did not finish within its deadline".to_owned()
        ))
    );
}

/// Proves a Mac with no `com.apple.access_ssh` group (dscl says eDSRecordNotFound, or -14136)
/// needs nothing: every account may use ssh, and no group is edited.
#[test]
fn a_mac_with_no_access_group_needs_nothing() {
    for said in [
        "<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)\n",
        "something eDSRecordNotFound\n",
        "DS Error: -14136\n",
    ] {
        let rig = fresh();
        rig.dscl
            .always(&group_read(), Ok(LaunchctlOutput::exit(56, said)));
        rig.steps
            .ensure_access(&account(), &rig.log)
            .expect("nothing");
        assert_eq!(rig.dscl.calls(), [group_read()]);
        assert!(rig.dseditgroup.calls().is_empty());
        assert!(rig.log.lines().is_empty());
    }
    let rig = fresh();
    rig.dscl.always(
        &group_read(),
        Ok(LaunchctlOutput {
            code: Some(56),
            stdout: b"-14136 on standard output\n".to_vec(),
            stderr: Vec::new(),
        }),
    );
    rig.steps
        .ensure_access(&account(), &rig.log)
        .expect("nothing");
}

/// Proves an account that is among the members of the group needs nothing: no group is edited
/// and nothing is logged.
#[test]
fn a_member_of_the_access_group_needs_nothing() {
    let rig = fresh();
    rig.dscl.always(
        &group_read(),
        Ok(LaunchctlOutput::success("GroupMembership: bob alice\n")),
    );
    rig.steps
        .ensure_access(&account(), &rig.log)
        .expect("member");
    assert!(rig.dseditgroup.calls().is_empty());
    assert!(rig.log.lines().is_empty());
}

/// Proves an account that is not a member is added with exactly `dseditgroup -o edit -a <name> -t
/// user com.apple.access_ssh` and the change is logged without the name; a name that only starts
/// like a member's is not a member.
#[test]
fn an_account_that_is_not_a_member_is_added_to_the_access_group() {
    for record in [
        "GroupMembership: bob aliceb\n",
        "No such key: GroupMembership\n",
    ] {
        let rig = fresh();
        rig.dscl
            .always(&group_read(), Ok(LaunchctlOutput::success(record)));
        rig.dseditgroup
            .always(&add(), Ok(LaunchctlOutput::success("")));
        rig.steps
            .ensure_access(&account(), &rig.log)
            .expect("added");
        assert_eq!(rig.dseditgroup.calls(), [add()]);
        assert_eq!(
            rig.log.lines(),
            ["operator keys: the login account added to com.apple.access_ssh"]
        );
    }
}

/// Proves a `dseditgroup` that fails, a group read that fails for another reason, and one that
/// cannot be run are errors naming the step and the exit code, never the account: the group read
/// keeps the first line of the standard error, `dseditgroup` (whose arguments name the account)
/// does not.
///
/// Catches: a `dseditgroup` reason that carried the tool's standard error, which may name the
/// account.
#[test]
fn a_failing_access_group_step_is_an_error_without_the_account() {
    let rig = fresh();
    rig.dscl.always(
        &group_read(),
        Ok(LaunchctlOutput::success("GroupMembership: bob\n")),
    );
    rig.dseditgroup
        .always(&add(), Ok(LaunchctlOutput::exit(66, "not authorized\n")));
    let error = rig
        .steps
        .ensure_access(&account(), &rig.log)
        .expect_err("fails");
    assert_eq!(
        error,
        MacSshError::AccessGroup(
            "dseditgroup add to the ssh access group failed with exit code 66".to_owned()
        )
    );
    assert_eq!(
        error.to_string(),
        "the login account is not allowed to use ssh: dseditgroup add to the ssh access group \
         failed with exit code 66"
    );
    assert!(rig.log.lines().is_empty());

    let rig = fresh();
    rig.dscl
        .always(&group_read(), Ok(LaunchctlOutput::exit(70, "dscl broke\n")));
    assert_eq!(
        rig.steps.ensure_access(&account(), &rig.log),
        Err(MacSshError::AccessGroup(
            "dscl read of the ssh access group failed with exit code 70: dscl broke".to_owned()
        ))
    );

    let rig = fresh();
    assert_eq!(
        rig.steps.ensure_access(&account(), &rig.log),
        Err(MacSshError::AccessGroup(
            "dscl read of the ssh access group could not be run: the fake has no answer".to_owned()
        ))
    );
}

/// Proves `ensure` makes sure of Remote Login first and then of the account, and stops at the
/// first error without running the second step.
#[test]
fn ensure_runs_remote_login_then_the_access_group() {
    let rig = fresh();
    rig.launchctl
        .always(&print(), Ok(LaunchctlOutput::success(LOADED)));
    rig.dscl.always(
        &group_read(),
        Ok(LaunchctlOutput::success("GroupMembership: alice\n")),
    );
    rig.steps.ensure(&account(), &rig.log).expect("ready");
    assert_eq!(rig.launchctl.calls(), [print()]);
    assert_eq!(rig.dscl.calls(), [group_read()]);

    let rig = fresh();
    rig.launchctl
        .always(&print(), Ok(LaunchctlOutput::exit(1, "")));
    rig.launchctl
        .always(&enable(), Ok(LaunchctlOutput::exit(1, "")));
    assert!(matches!(
        rig.steps.ensure(&account(), &rig.log),
        Err(MacSshError::RemoteLogin(_))
    ));
    assert!(rig.dscl.calls().is_empty());
}
