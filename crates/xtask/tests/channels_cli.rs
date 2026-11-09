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
                "tierstore",
                "--arch",
                "amd64",
                "--version",
                "1.2.3",
                "--commit",
                "0123456789abcdef0123456789abcdef01234567",
            ],
            "--package \"tierstore\" is none of",
        ),
        (
            vec![
                "channels",
                "check-rpm",
                "--package",
                "tierstore",
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
            ],
            "--package \"tierstore\" is none of",
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
            vec!["channels", "decide", "--result", "farm=success"],
            "--result names \"farm\", which is no stage",
        ),
    ] {
        let o = run(&d, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", stderr(&o));
        assert!(stderr(&o).contains(why), "{args:?}: {}", stderr(&o));
    }
}
