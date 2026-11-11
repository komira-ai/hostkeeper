//! The self-update tests' daemon program and main unit, with no systemd and no root: the start its
//! `--fixture-fail-start` refuses and the ones it does not, the command lines it refuses, and the
//! main units made from the real unit files. Part of the pull-request set, so that a change to either is caught before the privileged run.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use hostkeeper_testkit::Scratch;
use support::self_update::{BOOTSTRAP_UNIT, DAEMON, Paths, REAL_UNIT, refused_line, test_unit};

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

/// Catches `--fixture-link` read as another link, or a link it does not name let through: `files`
/// and `coordinator` are taken (the start goes on, here to the refusal of a `daemon` with no
/// `--fixture-dir`), and any other value is refused naming it, before the daemon starts.
#[test]
fn the_link_flag_takes_files_or_coordinator_only() {
    let s = scratch("su_link_flag");
    let program = staged(s.path(), "1.0.0");
    for link in ["files", "coordinator"] {
        let out = run(&program, &["--fixture-link", link, "daemon"]);
        assert_eq!(out.status.code(), Some(2), "{link}: {out:?}");
        assert_eq!(
            text(&out.stderr),
            "hostkeeper-selfupdate-daemon: daemon needs --fixture-dir\n",
            "{link}"
        );
    }
    let other = run(&program, &["--fixture-link", "http", "daemon"]);
    assert_eq!(other.status.code(), Some(2), "{other:?}");
    assert_eq!(
        text(&other.stderr),
        "hostkeeper-selfupdate-daemon: --fixture-link takes files or coordinator, not http\n"
    );
}

fn paths() -> Paths {
    Paths::under(Path::new("/run/hostkeeper-ci/t"))
}

/// The `ExecStart=` line of the test's main unit made from `real`, with the fixture flag
/// `--fixture-dir /f` and the test's own `--facts-file`, when `program` is its program.
fn exec_line(program: &str) -> String {
    format!(
        "ExecStart={program} --fixture-dir /f daemon --config /run/hostkeeper-ci/t/hostkeeper.toml \
         --state-dir /run/hostkeeper-ci/t/state --packages-dir /run/hostkeeper-ci/t/packages \
         --notify-systemd --facts-file /run/hostkeeper-ci/t/facts.json --heartbeat-secs 1"
    )
}

/// The main unit made from `real`, checked for what holds of both real units: its `ExecStart=`
/// is `expected` (and the only one), every other line of `real` is kept in order but the
/// description, and the keys of the restart policy are there.
fn assert_test_unit_of(real: &str, expected: &str) {
    let unit = test_unit(real, &paths(), &["--fixture-dir".into(), "/f".into()]);
    let exec: Vec<&str> = unit
        .lines()
        .filter(|line| line.starts_with("ExecStart"))
        .collect();
    assert_eq!(exec, [expected]);
    let kept: Vec<&str> = real
        .lines()
        .filter(|line| !line.starts_with("ExecStart") && !line.starts_with("Description="))
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
    assert!(
        !unit
            .lines()
            .any(|line| line.starts_with("RuntimeDirectory")),
        "{unit}"
    );
}

/// Catches a main unit that is not the package unit: every line of
/// `units/hostkeeper.service` is kept, in order, but `ExecStart=`, which starts the same `current`
/// and flags with the test's directories, the fixture's flags before the verb and a heartbeat a
/// second.
#[test]
fn the_main_unit_is_the_package_unit_with_the_tests_directories() {
    assert_test_unit_of(
        REAL_UNIT,
        &exec_line("/run/hostkeeper-ci/t/state/hostkeeper/bin/current"),
    );
}

/// Catches a main unit that is not the bootstrap unit: every line of `hostkeeper.service` is
/// kept, in order, but `ExecStart=`, which starts the packaged binary of the test (a file named
/// by the first version, in the host's directory) with the same flags as the package unit's.
#[test]
fn the_main_unit_of_a_deb_is_the_bootstrap_unit_over_the_packaged_binary() {
    assert_test_unit_of(
        BOOTSTRAP_UNIT,
        &exec_line("/run/hostkeeper-ci/t/packaged/1.0.0"),
    );
    assert_eq!(
        paths().packaged(),
        Path::new("/run/hostkeeper-ci/t/packaged/1.0.0")
    );
}

/// Catches a unit whose `ExecStart=` starts a program the test does not know being made into a
/// main unit that runs it.
#[test]
#[should_panic(expected = "starts neither `current` nor the packaged binary")]
fn a_real_unit_program_the_test_does_not_know_stops_it() {
    let real = BOOTSTRAP_UNIT.replace("ExecStart=/usr/bin/hostkeeper", "ExecStart=/usr/bin/other");
    test_unit(&real, &paths(), &[]);
}

/// Catches a `daemon` run from the file `--fixture-packaged` names that does not start as the
/// packaged binary: with an empty binary directory it seeds it with itself as the version in its
/// file name (`current` names it, nothing is `previous`) and runs `current` with its own
/// arguments, in place of itself: the program that answers is `versions/<version>`, which, with no
/// `--fixture-dir`, refuses the command line.
#[test]
fn a_daemon_run_from_the_packaged_path_seeds_the_binary_directory_and_runs_current() {
    let s = scratch("su_packaged");
    let packaged = s.path().join("packaged");
    fs::create_dir_all(&packaged).expect("packaged/");
    let image = packaged.join("1.0.0");
    fs::copy(DAEMON, &image).expect("copy the program");
    let state = s.path().join("state");
    fs::create_dir_all(&state).expect("state/");
    let (image_arg, state_arg) = (
        image.to_str().expect("UTF-8"),
        state.to_str().expect("UTF-8"),
    );

    let out = run(
        &image,
        &[
            "--fixture-packaged",
            image_arg,
            "daemon",
            "--state-dir",
            state_arg,
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "hostkeeper-selfupdate-daemon: daemon needs --fixture-dir\n"
    );
    let bin = state.join("hostkeeper").join("bin");
    assert_eq!(
        fs::read_link(bin.join("current")).ok(),
        Some(PathBuf::from("versions/1.0.0"))
    );
    assert!(bin.join("versions/1.0.0").is_file());
    assert!(fs::symlink_metadata(bin.join("previous")).is_err());
}

/// Catches a `daemon` that is not the file `--fixture-packaged` names being started as the
/// packaged binary: the same program run from `versions/<version>` with the flag runs as the
/// daemon itself (here the refusal of a `daemon` with no `--fixture-dir`) and writes nothing into
/// the binary directory.
#[test]
fn a_daemon_not_run_from_the_packaged_path_is_not_seeded() {
    let s = scratch("su_not_packaged");
    let state = s.path().join("state");
    fs::create_dir_all(&state).expect("state/");
    let state_arg = state.to_str().expect("UTF-8");

    let out = run(
        &staged(s.path(), "1.0.0"),
        &[
            "--fixture-packaged",
            "/nowhere/1.0.0",
            "daemon",
            "--state-dir",
            state_arg,
        ],
    );
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "hostkeeper-selfupdate-daemon: daemon needs --fixture-dir\n"
    );
    assert!(!state.join("hostkeeper").exists());
}

/// Catches the real unit's `ExecStart=` gaining a flag the privileged test would pass on with a
/// host path: the main unit is not made.
#[test]
#[should_panic(expected = "a flag this test does not place: --new-flag")]
fn a_real_unit_flag_the_test_does_not_place_stops_it() {
    let real = REAL_UNIT.replace("--notify-systemd", "--notify-systemd --new-flag /etc/x");
    test_unit(&real, &paths(), &[]);
}
