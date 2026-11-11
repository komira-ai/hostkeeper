//! `xtask channels decide|check-apt|check-rpm|pkg` end to end: each verb is run as a process and
//! its exit status and words are read. The logic of each verb is tested beside it, in the
//! library; these tests hold the dispatch of the flags and the exit statuses (0, 1 for a failure,
//! 2 for a usage error).

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::Path;
use std::process::{Command, Output};

use testdir::TestDir;

fn dir(name: &str) -> TestDir {
    TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("channels-cli-{name}"),
    )
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run xtask")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn channels_decide_writes_promote_true_when_every_stage_allows_it_and_fails_with_status_1_otherwise()
 {
    let d = dir("channels-decide");
    let out = d.join("out");
    let stages = [
        "testing-apt-write",
        "testing-apt-write-2",
        "testing-apt-write-3",
        "testing-rpm-write",
        "testing-rpm-write-2",
        "testing-rpm-write-3",
        "testing-pkg-upload",
        "check-apt",
        "check-rpm",
        "check-pkg",
    ];
    let decide = |changed: &str| {
        let mut args = vec!["channels".to_owned(), "decide".to_owned()];
        for stage in stages {
            args.push("--result".to_owned());
            args.push(if stage == changed {
                format!("{stage}=failure")
            } else {
                format!("{stage}=success")
            });
        }
        args.push("--output".to_owned());
        args.push(out.to_str().expect("utf8").to_owned());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(&d, &args)
    };
    let o = decide("check-pkg");
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("check-pkg is failure: the release does not go on"),
        "{}",
        stderr(&o)
    );
    assert!(!out.exists());
    let o = decide("");
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "promote: the release is promoted from testing\n"
    );
    assert_eq!(
        std::fs::read_to_string(&out).expect("out"),
        "promote=true\n"
    );
}

#[test]
fn channels_install_checks_and_the_installer_verbs_with_flags_that_do_not_fit_are_status_2() {
    // A package of another product, a tag without its v, and a result that names no stage.
    let d = dir("channels-usage");
    for (args, why) in [
        (
            vec![
                "channels",
                "check-apt",
                "--package",
                "other-product",
                "--arch",
                "amd64",
                "--version",
                "1.2.3",
                "--commit",
                "0123456789abcdef0123456789abcdef01234567",
            ],
            "--package \"other-product\" is none of",
        ),
        (
            vec![
                "channels",
                "check-rpm",
                "--package",
                "other-product",
                "--container",
                "c",
                "--arch",
                "x86_64",
                "--version",
                "1.2.3",
                "--commit",
                "0123456789abcdef0123456789abcdef01234567",
                "--work",
                "/work",
                "--scratch",
                "scratch",
            ],
            "--package \"other-product\" is none of",
        ),
        (
            vec![
                "channels",
                "pkg",
                "upload",
                "--dir",
                "release",
                "--tag",
                "1.2.3",
                "--commit",
                "0123456789abcdef0123456789abcdef01234567",
                "--bucket",
                "b",
                "--work",
                "work",
            ],
            "--tag \"1.2.3\" is not v<major>.<minor>.<patch>",
        ),
        (
            vec!["channels", "decide", "--result", "no-such-stage=success"],
            "--result names \"no-such-stage\", which is no stage",
        ),
    ] {
        let o = run(&d, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(stderr(&o).contains(why), "{args:?}: {}", stderr(&o));
    }
}

const FINGERPRINT: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

fn run_env(cwd: &Path, args: &[&str], set: &[(&str, &str)], unset: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command.current_dir(cwd).args(args);
    for name in unset {
        command.env_remove(name);
    }
    for (name, value) in set {
        command.env(name, value);
    }
    command.output().expect("run xtask")
}

#[test]
fn the_sign_verbs_take_the_key_and_passphrase_from_the_environment_and_leave_no_file() {
    // The key and the passphrase are written below --work (so gpg is reached, here a program
    // that is not there) and are gone when the verb ends, whatever it came to.
    let d = dir("channels-sign-env");
    std::fs::create_dir_all(d.join("next/dists/testing")).expect("dir");
    std::fs::write(d.join("next/dists/testing/Release"), "Suite: testing\n").expect("release");
    let gpg = d.join("no-such-gpg").display().to_string();
    let work = d.join("work");
    let work_arg = work.display().to_string();
    let set = [
        ("APT_GPG_PRIVATE_KEY", "PRIVATE-KEY-TEXT"),
        ("APT_GPG_PASSPHRASE", "PASSPHRASE-TEXT"),
    ];
    let o = run_env(
        &d,
        &[
            "channels",
            "apt",
            "sign",
            "--dir",
            "next/dists/testing",
            "--work",
            &work_arg,
            "--fingerprint",
            FINGERPRINT,
            "--gpg",
            &gpg,
        ],
        &set,
        &[],
    );
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains(&gpg), "{}", stderr(&o));
    assert!(
        !stderr(&o).contains("PRIVATE-KEY-TEXT") && !stderr(&o).contains("PASSPHRASE-TEXT"),
        "{}",
        stderr(&o)
    );
    let left: Vec<_> = std::fs::read_dir(&work)
        .expect("work")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

#[test]
fn a_sign_verb_without_its_key_variable_is_status_2_naming_the_variable() {
    let d = dir("channels-sign-no-env");
    let work = d.join("work").display().to_string();
    let fpr = FINGERPRINT;
    let apt = [
        "channels",
        "apt",
        "sign",
        "--dir",
        "d",
        "--work",
        &work,
        "--fingerprint",
        fpr,
    ];
    let rpm = [
        "channels",
        "rpm",
        "sign",
        "--rpms",
        "r",
        "--out",
        "o",
        "--version",
        "1.2.3",
        "--sums",
        "s",
        "--work",
        &work,
        "--fingerprint",
        fpr,
        "--signed-at",
        "1",
    ];
    let repo = [
        "channels",
        "rpm",
        "repo",
        "sign",
        "--dir",
        "d",
        "--work",
        &work,
        "--fingerprint",
        fpr,
    ];
    for args in [&apt[..], &rpm[..], &repo[..]] {
        let o = run_env(
            &d,
            args,
            &[("APT_GPG_PASSPHRASE", "p")],
            &["APT_GPG_PRIVATE_KEY"],
        );
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("APT_GPG_PRIVATE_KEY is not set"),
            "{args:?}: {}",
            stderr(&o)
        );
        let o = run_env(
            &d,
            args,
            &[("APT_GPG_PRIVATE_KEY", "k")],
            &["APT_GPG_PASSPHRASE"],
        );
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("APT_GPG_PASSPHRASE is not set"),
            "{args:?}: {}",
            stderr(&o)
        );
    }
    assert!(!d.join("work").exists(), "nothing is written without both");
}

#[test]
fn the_sign_verbs_no_longer_take_key_files_as_flags() {
    let d = dir("channels-sign-no-files");
    let o = run(
        &d,
        &[
            "channels",
            "apt",
            "sign",
            "--dir",
            "d",
            "--work",
            "w",
            "--fingerprint",
            FINGERPRINT,
            "--key-file",
            "k",
            "--passphrase-file",
            "p",
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("--key-file"), "{}", stderr(&o));
}

#[test]
fn channels_assume_takes_the_token_url_as_a_flag_that_must_be_https() {
    let d = dir("channels-assume-url");
    let base = [
        "channels",
        "assume",
        "--role-arn",
        "arn:aws:iam::123456789012:role/release",
        "--session-name",
        "hostkeeper-release-1",
        "--region",
        "us-east-1",
        "--env-file",
        "env",
    ];
    // The variable the runner sets is not the source: without the flag the verb is a usage
    // error of clap, and the variable does not stand in for it.
    let o = run_env(
        &d,
        &base,
        &[(
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "https://token.example/request",
        )],
        &[],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("--token-url"), "{}", stderr(&o));
    for url in ["", "http://token.example/request"] {
        let mut args = base.to_vec();
        args.extend(["--token-url", url]);
        let o = run(&d, &args);
        assert_eq!(o.status.code(), Some(2), "{url:?}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("--token-url is not an https URL"),
            "{url:?}: {}",
            stderr(&o)
        );
    }
    assert!(!d.join("env").exists());
}

/// Proves the suite `testing-fail`, hello's failing build alone, is an apt suite only: every rpm
/// verb that takes a suite refuses it as a usage error, while `apt read` takes it.
///
/// Catches: the rpm tree gaining a `testing-fail` directory the failing build (which has no rpm)
/// never needs.
#[test]
fn testing_fail_is_a_suite_of_apt_and_no_rpm_verb_takes_it() {
    let d = dir("channels-rpm-testing-fail");
    for verb in ["read", "index", "write"] {
        let o = run(
            &d,
            &["channels", "rpm", "repo", verb, "--suite", "testing-fail"],
        );
        assert_eq!(o.status.code(), Some(2), "rpm repo {verb}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("invalid value 'testing-fail' for '--suite <SUITE>'"),
            "rpm repo {verb}: {}",
            stderr(&o)
        );
    }
    let o = run(&d, &["channels", "apt", "read", "--suite", "testing-fail"]);
    assert!(
        !stderr(&o).contains("invalid value 'testing-fail'"),
        "{}",
        stderr(&o)
    );
}
