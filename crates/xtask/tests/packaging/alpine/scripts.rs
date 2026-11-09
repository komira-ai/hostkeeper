//! The apk's two scripts, `scripts/postinstall.sh` and `scripts/preremove.sh`: what they may
//! run, and what they do when run as apk runs them.

use super::{APK_POSTINSTALL, APK_PREREMOVE, read_alpine};
use crate::script;
use crate::{altered, refuses};

/// The commands the apk's scripts may run.
const ADD: [&str; 4] = ["rc-update", "add", "hostkeeper", "default"];
const DEL: [&str; 4] = ["rc-update", "del", "hostkeeper", "default"];
const ALLOWED: [&[&str]; 5] = [&["set", "-e"], &ADD, &DEL, &["true"], &["exit", "0"]];
/// Words that would start or stop the service, or reach another program for that.
const STARTING: [&str; 4] = ["start", "restart", "stop", "rc-service"];

fn check_scripts(post: &str, pre: &str) -> Result<(), String> {
    let post = script::parse(post)?;
    let pre = script::parse(pre)?;
    for command in post.iter().chain(&pre) {
        if let Some(word) = command
            .words
            .iter()
            .find(|w| STARTING.contains(&w.as_str()))
        {
            return Err(format!(
                "line {}: `{}` would start or stop the service ({word})",
                command.line,
                command.words.join(" ")
            ));
        }
        if !ALLOWED.iter().any(|allowed| command.words == **allowed) {
            return Err(format!(
                "line {}: `{}` is not a command the apk's scripts may run",
                command.line,
                command.words.join(" ")
            ));
        }
    }
    let count = |commands: &[script::Command], words: [&str; 4]| {
        commands.iter().filter(|c| c.words == words).count()
    };
    if count(&post, ADD) != 1 || count(&post, DEL) != 0 {
        return Err(
            "the post-install script must add the service to default once, and delete nothing"
                .to_owned(),
        );
    }
    if count(&pre, DEL) != 1 || count(&pre, ADD) != 0 {
        return Err(
            "the pre-remove script must delete the service from default once, and add nothing"
                .to_owned(),
        );
    }
    Ok(())
}

/// Catches a script that starts or stops the service (a host with no config yet would start a
/// daemon that refuses it), runs another command than adding and deleting the service in the
/// default runlevel, adds it more than once or in the removal, or deletes it in the install;
/// and a construct of a shell other than POSIX sh.
#[test]
fn apk_scripts_are_posix_sh_and_only_add_and_delete_the_service() {
    let post = read_alpine(APK_POSTINSTALL);
    let pre = read_alpine(APK_PREREMOVE);
    assert_eq!(check_scripts(&post, &pre), Ok(()));
    let post_before_exit = |line: &str| altered(&post, "exit 0", &format!("{line}\nexit 0"));
    let pre_before_exit = |line: &str| altered(&pre, "exit 0", &format!("{line}\nexit 0"));
    let post_cases = [
        (
            post_before_exit("rc-service hostkeeper start"),
            "would start or stop the service (rc-service)",
        ),
        (
            post_before_exit("rc-update add hostkeeper default --start"),
            "is not a command",
        ),
        (
            post_before_exit("rc-update add hostkeeper boot"),
            "is not a command",
        ),
        (
            post_before_exit("rc-update add hostkeeper default"),
            "must add the service to default once",
        ),
        (
            post_before_exit("rc-update del hostkeeper default"),
            "must add the service to default once",
        ),
        (
            post_before_exit("rm -rf /var/lib/updater"),
            "`rm -rf /var/lib/updater` is not",
        ),
        (
            post_before_exit("chown -R root /etc/hostkeeper"),
            "`chown -R root /etc/hostkeeper` is not",
        ),
        (post_before_exit("echo $(id -u)"), "the word \"$(id\""),
        (
            post_before_exit("true > /etc/hostkeeper/hostkeeper.toml"),
            "the word \">\"",
        ),
        (
            post_before_exit("rc-update add hostkeeper default | true"),
            "the word \"|\"",
        ),
        (
            altered(&post, "#!/bin/sh\n", "#!/bin/bash\n"),
            "does not start with `#!/bin/sh`",
        ),
        (
            altered(&post, "rc-update add hostkeeper default\n", ""),
            "must add the service to default once",
        ),
    ];
    refuses(|t: &str| check_scripts(t, &pre), &post_cases);
    let pre_cases = [
        (
            pre_before_exit("rc-service hostkeeper stop"),
            "would start or stop the service (rc-service)",
        ),
        (
            pre_before_exit("rc-update del hostkeeper boot"),
            "is not a command",
        ),
        (
            pre_before_exit("rc-update add hostkeeper default"),
            "must delete the service from default once",
        ),
        (
            pre_before_exit("rc-update del hostkeeper default"),
            "must delete the service from default once",
        ),
        (
            pre_before_exit("apk del hostkeeper"),
            "`apk del hostkeeper` is not",
        ),
        (
            altered(&pre, "rc-update del hostkeeper default || true\n", ""),
            "must delete the service from default once",
        ),
    ];
    refuses(|t: &str| check_scripts(&post, t), &pre_cases);
}

/// One run of a script: what apk passes (the version), the program that fails (if any), and
/// the programs it must run and the status it must end with.
struct Scenario {
    name: &'static str,
    args: &'static [&'static str],
    failing: Option<&'static [&'static str]>,
    ran: &'static [&'static [&'static str]],
    status: i32,
}

/// The post-install script's runs: a first install adds the service and nothing else; a failed
/// add fails the install.
const POSTINSTALL_RUNS: [Scenario; 2] = [
    Scenario {
        name: "first install",
        args: &["1.2.3"],
        failing: None,
        ran: &[&ADD],
        status: 0,
    },
    Scenario {
        name: "add fails",
        args: &["1.2.3"],
        failing: Some(&ADD),
        ran: &[&ADD],
        status: 1,
    },
];

/// The pre-remove script's runs: a removal deletes the service, and a service that is not in
/// the runlevel (the operator took it out) does not stop the removal.
const PREREMOVE_RUNS: [Scenario; 2] = [
    Scenario {
        name: "removal",
        args: &["1.2.3"],
        failing: None,
        ran: &[&DEL],
        status: 0,
    },
    Scenario {
        name: "delete fails",
        args: &["1.2.3"],
        failing: Some(&DEL),
        ran: &[&DEL],
        status: 0,
    },
];

/// Runs `text` through every scenario of `runs`, with each program a fake that fails only for
/// the scenario's failing command.
fn check_runs(text: &str, runs: &[Scenario]) -> Result<(), String> {
    let commands = script::parse(text)?;
    for scenario in runs {
        let done = script::run(&commands, scenario.args, |words| {
            i32::from(scenario.failing.is_some_and(|failing| words == failing))
        });
        let wanted = script::Run {
            ran: scenario
                .ran
                .iter()
                .map(|words| words.iter().map(|w| (*w).to_owned()).collect())
                .collect(),
            status: scenario.status,
        };
        if done != wanted {
            return Err(format!(
                "{}: the script ran {:?} and ended {}, not {:?} and {}",
                scenario.name, done.ran, done.status, wanted.ran, wanted.status
            ));
        }
    }
    Ok(())
}

/// Catches an apk script that skips its step, hides a failed add that must fail the install (no
/// `set -e`, or `|| true`), fails a removal over a service that is not in the runlevel, or
/// ends with another status, by running each script as sh runs it, with apk's argument and a
/// fake for every program.
#[test]
fn apk_scripts_run_their_step_and_fail_as_apk_expects() {
    let post = read_alpine(APK_POSTINSTALL);
    let pre = read_alpine(APK_PREREMOVE);
    assert_eq!(check_runs(&post, &POSTINSTALL_RUNS), Ok(()));
    assert_eq!(check_runs(&pre, &PREREMOVE_RUNS), Ok(()));
    let post_cases = [
        (
            altered(&post, "rc-update add hostkeeper default\n", ""),
            "first install: the script ran",
        ),
        (altered(&post, "set -e\n", ""), "add fails: the script ran"),
        (
            altered(
                &post,
                "rc-update add hostkeeper default",
                "rc-update add hostkeeper default || true",
            ),
            "add fails: the script ran",
        ),
        (
            altered(&post, "exit 0", "exit 1"),
            "first install: the script ran",
        ),
    ];
    refuses(|t: &str| check_runs(t, &POSTINSTALL_RUNS), &post_cases);
    let pre_cases = [
        (
            altered(&pre, "rc-update del hostkeeper default || true\n", ""),
            "removal: the script ran",
        ),
        (
            altered(
                &pre,
                "rc-update del hostkeeper default || true",
                "rc-update del hostkeeper default",
            ),
            "delete fails: the script ran",
        ),
        (altered(&pre, "exit 0", "exit 1"), "removal: the script ran"),
    ];
    refuses(|t: &str| check_runs(t, &PREREMOVE_RUNS), &pre_cases);
}
