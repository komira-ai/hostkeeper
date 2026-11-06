//! The self-update tests' daemon program and main unit, with no systemd and no root: the verbs
//! it forwards, the start its `--fixture-fail-start` refuses and the ones it does not, the
//! command lines it refuses, and the main unit made from the real unit file. Part of the
//! pull-request set, so that a change to either is caught before the privileged run.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use hostkeeper_testkit::Scratch;
use support::self_update::{DAEMON, Paths, REAL_UNIT, refused_line, test_unit};

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

/// The program at `path`, run with `args` and nothing in its environment.
fn run(path: &Path, args: &[&str]) -> Output {
    Command::new(path)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .expect("run the program")
}

/// The program as `versions/<version>` of a binary directory in `dir`, as staging leaves it.
fn staged(dir: &Path, version: &str) -> PathBuf {
    let versions = dir.join("versions");
    fs::create_dir_all(&versions).expect("versions/");
    let at = versions.join(version);
    if fs::hard_link(DAEMON, &at).is_err() {
        fs::copy(DAEMON, &at).expect("copy the program");
    }
    at
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("UTF-8")
}

/// Catches a program that does not forward the probe's verb to the shipped command line: a new
/// binary whose protocol the daemon cannot read is refused before anything is staged.
#[test]
fn the_protocol_probe_is_answered_by_the_shipped_verb() {
    let out = run(Path::new(DAEMON), &["self-update-protocol"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(text(&out.stdout), "1\n");
}

/// Catches `--fixture-fail-start` refusing the wrong start: the version it names exits 1 with
/// its line before anything else, and any other version goes on (here to the refusal of a
/// `daemon` with no `--fixture-dir`), so the old binary still starts on the same command line.
#[test]
fn fail_start_refuses_the_start_of_its_version_only() {
    let s = scratch("su_fail_start");
    let flags = ["--fixture-fail-start", "1.0.1", "daemon"];

    let refused = run(&staged(s.path(), "1.0.1"), &flags);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert_eq!(
        text(&refused.stderr),
        format!("{}\n", refused_line("1.0.1"))
    );

    let other = run(&staged(s.path(), "1.0.0"), &flags);
    assert_eq!(other.status.code(), Some(2), "{other:?}");
    assert_eq!(
        text(&other.stderr),
        "hostkeeper-selfupdate-daemon: daemon needs --fixture-dir\n"
    );
}

/// Catches a `daemon` that runs as a version it was not staged as.
#[test]
fn a_daemon_not_run_from_a_version_is_refused() {
    let out = run(Path::new(DAEMON), &["--fixture-dir", "/nowhere", "daemon"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        format!(
            "hostkeeper-selfupdate-daemon: daemon runs from versions/<major.minor.patch> of a \
             binary directory, not from {DAEMON}\n"
        )
    );
}

/// Catches a fixture flag the program does not know, or one with no value, being read as
/// something else.
#[test]
fn an_unknown_or_unfinished_fixture_flag_is_refused() {
    let unknown = run(Path::new(DAEMON), &["--fixture-other", "x", "daemon"]);
    assert_eq!(unknown.status.code(), Some(2), "{unknown:?}");
    assert_eq!(
        text(&unknown.stderr),
        "hostkeeper-selfupdate-daemon: unknown flag --fixture-other\n"
    );
    let unfinished = run(Path::new(DAEMON), &["--fixture-dir"]);
    assert_eq!(unfinished.status.code(), Some(2), "{unfinished:?}");
    assert_eq!(
        text(&unfinished.stderr),
        "hostkeeper-selfupdate-daemon: --fixture-dir takes a value\n"
    );
}

fn paths() -> Paths {
    Paths::under(Path::new("/run/hostkeeper-ci/t"))
}

/// Catches a main unit that is not the real one: every line of `hostkeeper.service` is kept,
/// in order, but the seed, the runtime directory and `ExecStart=`, which starts the same
/// `current` and flags with the test's directories, the fixture's flags before the verb and a
/// heartbeat a second.
#[test]
fn the_main_unit_is_the_real_unit_with_the_tests_directories() {
    let unit = test_unit(REAL_UNIT, &paths(), &["--fixture-dir".into(), "/f".into()]);
    let exec: Vec<&str> = unit
        .lines()
        .filter(|line| line.starts_with("ExecStart"))
        .collect();
    assert_eq!(
        exec,
        [
            "ExecStart=/run/hostkeeper-ci/t/state/hostkeeper/bin/current --fixture-dir /f \
             daemon --config /run/hostkeeper-ci/t/hostkeeper.toml --state-dir \
             /run/hostkeeper-ci/t/state --packages-dir /run/hostkeeper-ci/t/packages \
             --admin-socket /run/hostkeeper-ci/t/admin.sock --notify-systemd --heartbeat-secs 1"
        ]
    );
    let kept: Vec<&str> = REAL_UNIT
        .lines()
        .filter(|line| {
            !["ExecStart", "RuntimeDirectory", "Description="]
                .iter()
                .any(|key| line.starts_with(key))
        })
        .collect();
    let ours: Vec<&str> = unit
        .lines()
        .filter(|line| !line.starts_with("ExecStart") && !line.starts_with("Description="))
        .collect();
    assert_eq!(ours, kept);
    for line in [
        "Type=notify",
        "Restart=always",
        "RestartSec=1",
        "RestartMode=direct",
        "StartLimitIntervalSec=0",
        "WatchdogSec=60",
    ] {
        assert!(unit.lines().any(|l| l == line), "{line} is kept");
    }
    assert!(unit.contains("\nDescription=hostkeeper privileged test: "));
}

/// Catches the real unit's `ExecStart=` gaining a flag the privileged test would pass on with a
/// host path: the main unit is not made.
#[test]
#[should_panic(expected = "a flag this test does not place: --new-flag")]
fn a_real_unit_flag_the_test_does_not_place_stops_it() {
    let real = REAL_UNIT.replace("--notify-systemd", "--notify-systemd --new-flag /etc/x");
    test_unit(&real, &paths(), &[]);
}
