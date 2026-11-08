use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use super::*;
use crate::release::fakes::{FakeExec, FakeRunVersion, nth_commit};

const STAMP: &str = "hostkeeper 0.1.5+abcdef012345";

fn version() -> Version {
    "0.1.5".parse().expect("a version")
}

/// A commit whose first 12 digits are `abcdef012345`.
fn pinned() -> Commit {
    format!("abcdef012345{}", "0".repeat(28))
        .parse()
        .expect("a commit")
}

fn printing(success: bool, stdout: &str, stderr: &str) -> FakeRunVersion {
    FakeRunVersion {
        said: Said {
            success,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        },
    }
}

fn check(run: &FakeRunVersion) -> Result<Vec<String>, Error> {
    check_binary(run, Path::new("/opt/bin/hostkeeper"), &version(), &pinned())
}

#[test]
fn check_binary_requires_version_and_commit_in_the_output() {
    // Catches: a check that accepts a binary printing the version without its commit, or the
    // stamp of another commit.
    assert_eq!(
        check(&printing(true, "hostkeeper 0.1.5\n", "")),
        Err(Error::Failed(
            "/opt/bin/hostkeeper --version printed \"hostkeeper 0.1.5\\n\", not \
             \"hostkeeper 0.1.5+abcdef012345\""
                .to_owned()
        ))
    );
    assert_eq!(
        check(&printing(true, "hostkeeper 0.1.5+abcdef012346\n", "")),
        Err(Error::Failed(
            "/opt/bin/hostkeeper --version printed \"hostkeeper 0.1.5+abcdef012346\\n\", not \
             \"hostkeeper 0.1.5+abcdef012345\""
                .to_owned()
        ))
    );
    assert_eq!(
        check(&printing(true, &format!("{STAMP}\n"), "")),
        Ok(vec![format!("/opt/bin/hostkeeper: {STAMP}")])
    );
}

#[test]
fn the_coordinator_is_checked_against_its_own_program_name() {
    // Catches: a check that holds the coordinator to the daemon's stamp, or accepts the daemon's
    // stamp from the coordinator.
    let binary = Path::new("/opt/bin/hostkeeper-coordinator");
    let own = "hostkeeper-coordinator 0.1.5+abcdef012345";
    assert_eq!(
        check_binary(
            &printing(true, &format!("{own}\n"), ""),
            binary,
            &version(),
            &pinned()
        ),
        Ok(vec![format!("/opt/bin/hostkeeper-coordinator: {own}")])
    );
    assert_eq!(
        check_binary(
            &printing(true, &format!("{STAMP}\n"), ""),
            binary,
            &version(),
            &pinned()
        ),
        Err(Error::Failed(format!(
            "/opt/bin/hostkeeper-coordinator --version printed {:?}, not {own:?}",
            format!("{STAMP}\n")
        )))
    );
}

#[test]
fn the_mdm_kit_binary_is_checked_against_its_own_program_name() {
    // Catches: the MDM kit's binary refused as not shipped, held to the daemon's or the
    // coordinator's stamp, or accepted printing either of theirs, or its developer stamp (no
    // commit).
    let binary = Path::new("/opt/bin/hostkeeper-mdm");
    let own = "hostkeeper-mdm 0.1.5+abcdef012345";
    let run =
        |printed: &str| check_binary(&printing(true, printed, ""), binary, &version(), &pinned());
    assert_eq!(
        run(&format!("{own}\n")),
        Ok(vec![format!("/opt/bin/hostkeeper-mdm: {own}")])
    );
    for other in [
        STAMP,
        "hostkeeper-coordinator 0.1.5+abcdef012345",
        "hostkeeper-mdm 0.1.5",
    ] {
        let printed = format!("{other}\n");
        assert_eq!(
            run(&printed),
            Err(Error::Failed(format!(
                "/opt/bin/hostkeeper-mdm --version printed {printed:?}, not {own:?}"
            ))),
            "{other}"
        );
    }
}

#[test]
fn the_mdm_packages_program_is_checked_against_its_own_program_name() {
    // Catches: `bin/mdm`, the mdm package's program, refused as not shipped, or held to the old
    // kit's `hostkeeper-mdm` stamp, or accepted printing that stamp or its developer stamp.
    let binary = Path::new("/build/target/release/mdm");
    let own = "mdm 0.1.5+abcdef012345";
    let run =
        |printed: &str| check_binary(&printing(true, printed, ""), binary, &version(), &pinned());
    assert_eq!(
        run(&format!("{own}\n")),
        Ok(vec![format!("/build/target/release/mdm: {own}")])
    );
    for other in ["hostkeeper-mdm 0.1.5+abcdef012345", "mdm 0.1.5"] {
        let printed = format!("{other}\n");
        assert_eq!(
            run(&printed),
            Err(Error::Failed(format!(
                "/build/target/release/mdm --version printed {printed:?}, not {own:?}"
            ))),
            "{other}"
        );
    }
}

#[test]
fn a_binary_that_is_not_shipped_is_refused_before_it_runs() {
    // Catches: a check that runs any file it is given, or that takes the program name from the
    // output instead of from the file name the binary ships under.
    struct Unrun;
    impl RunVersion for Unrun {
        fn version_of(&self, binary: &Path) -> Result<Said, PortError> {
            panic!("{} was run", binary.display())
        }
    }
    let refused = |path: &Path| -> Result<Vec<String>, Error> {
        Err(Error::Failed(format!(
            "{} is not a shipped binary: its file name is none of \
             [\"hostkeeper\", \"hostkeeper-coordinator\", \"hostkeeper-mdm\", \"mdm\"]",
            path.display()
        )))
    };
    for path in [
        Path::new("/opt/bin/hostkeeperd"),
        Path::new("/opt/bin/hostkeeper.old"),
        Path::new("/opt/bin/hostkeeper-mdm-core"),
        Path::new("/"),
        Path::new(OsStr::from_bytes(b"/opt/bin/\xff")),
    ] {
        assert_eq!(
            check_binary(&Unrun, path, &version(), &pinned()),
            refused(path)
        );
    }
}

#[test]
fn the_stamp_passes_with_or_without_its_one_line_break() {
    // Catches: a check that demands the line break, or that strips more than one.
    let expected = Ok(vec![format!("/opt/bin/hostkeeper: {STAMP}")]);
    assert_eq!(check(&printing(true, STAMP, "")), expected);
    assert_eq!(check(&printing(true, &format!("{STAMP}\n"), "")), expected);
    assert_eq!(
        check(&printing(true, &format!("{STAMP}\n\n"), "")),
        Err(Error::Failed(format!(
            "/opt/bin/hostkeeper --version printed {:?}, not {STAMP:?}",
            format!("{STAMP}\n\n")
        )))
    );
}

#[test]
fn text_around_the_stamp_fails() {
    // Catches: a substring match that accepts a prefix or a suffix around the stamp.
    for printed in [
        format!("v {STAMP}"),
        format!("{STAMP} extra"),
        String::new(),
    ] {
        assert_eq!(
            check(&printing(true, &printed, "")),
            Err(Error::Failed(format!(
                "/opt/bin/hostkeeper --version printed {printed:?}, not {STAMP:?}"
            )))
        );
    }
}

#[test]
fn a_failing_exit_is_reported_with_its_trimmed_stderr() {
    // Catches: a check that reads the stamp from a binary that exited with failure.
    assert_eq!(
        check(&printing(false, &format!("{STAMP}\n"), "  boom\n")),
        Err(Error::Failed(
            "/opt/bin/hostkeeper --version failed: boom".to_owned()
        ))
    );
}

#[test]
fn a_port_error_is_passed_on() {
    // Catches: a check that swallows the port's error.
    struct Broken;
    impl RunVersion for Broken {
        fn version_of(&self, _binary: &Path) -> Result<Said, PortError> {
            Err(PortError::new("run", "no such file"))
        }
    }
    assert_eq!(
        check_binary(
            &Broken,
            Path::new("/x/hostkeeper"),
            &version(),
            &nth_commit(1)
        ),
        Err(Error::Port(PortError::new("run", "no such file")))
    );
}

#[test]
fn the_adapter_runs_the_binary_with_version_and_reports_what_it_printed() {
    // Catches: an adapter that passes other arguments, or drops the exit status or an output.
    let exec = FakeExec::default().on(
        &["/opt/bin/hostkeeper", "--version"],
        false,
        "out\n",
        "err\n",
    );
    let of = VersionOf::new(exec);
    assert_eq!(
        of.version_of(Path::new("/opt/bin/hostkeeper")),
        Ok(Said {
            success: false,
            stdout: "out\n".to_owned(),
            stderr: "err\n".to_owned(),
        })
    );
    assert_eq!(
        *of.exec.calls.borrow(),
        vec![vec![
            "/opt/bin/hostkeeper".to_owned(),
            "--version".to_owned()
        ]]
    );
}

#[test]
fn the_adapter_names_the_binary_when_it_cannot_start() {
    // Catches: a spawn error reported without the command that failed.
    let of = VersionOf::new(FakeExec::default());
    assert_eq!(
        of.version_of(Path::new("/opt/bin/other")),
        Err(PortError::new(
            "/opt/bin/other --version",
            "not scripted: /opt/bin/other --version"
        ))
    );
}

#[test]
fn the_adapter_refuses_a_path_that_is_not_utf8() {
    // Catches: a lossy conversion that runs a different program than the one named.
    let of = VersionOf::new(FakeExec::default());
    let path = Path::new(OsStr::from_bytes(b"/opt/bin/\xff"));
    assert_eq!(
        of.version_of(path),
        Err(PortError::new(
            "run /opt/bin/\u{fffd} --version",
            "the path is not UTF-8"
        ))
    );
    assert!(of.exec.calls.borrow().is_empty());
}
