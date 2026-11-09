use super::*;
use crate::channels::fixtures::FakeExec;
use crate::testdir::TestDir;

fn scratch(name: &str) -> TestDir {
    let beside = std::env::current_exe().expect("test binary");
    TestDir::under(
        &beside.parent().expect("deps").join("xtask-channels-unit"),
        name,
    )
}

#[test]
fn a_plain_version_is_three_whole_numbers_without_leading_zeros() {
    for ok in ["0.0.0", "1.2.3", "10.20.30"] {
        assert!(is_plain_version(ok), "{ok}");
    }
    let four_numbers = ["1", "2", "3", "4"].join(".");
    for bad in [
        "1.2",
        four_numbers.as_str(),
        "1.2.x",
        "01.2.3",
        "1.2.3+abc",
        "",
        "1..3",
        "-1.2.3",
    ] {
        assert!(!is_plain_version(bad), "{bad}");
    }
}

#[test]
fn numbers_reads_a_plain_version_and_nothing_else() {
    assert_eq!(numbers("1.20.3"), Some((1, 20, 3)));
    assert_eq!(numbers("1.2"), None);
    assert_eq!(numbers("1.2.3+x"), None);
    // Too large for a u64.
    assert_eq!(numbers("99999999999999999999.1.1"), None);
}

#[test]
fn append_output_appends_lines_and_names_the_file_it_could_not_open() {
    let dir = scratch("append-output");
    let file = dir.join("out");
    append_output(&file, &["a=1".to_owned()]).expect("first");
    append_output(&file, &["b=2".to_owned(), "c=3".to_owned()]).expect("second");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read"),
        "a=1\nb=2\nc=3\n"
    );
    let e = append_output(&dir, &["x".to_owned()]).unwrap_err();
    assert!(
        e.to_string()
            .starts_with(&format!("append to {}: ", dir.display())),
        "{e}"
    );
}

#[test]
fn the_errors_print_as_the_verbs_say_them() {
    assert_eq!(Error::Failed("one".to_owned()).to_string(), "one");
    assert_eq!(Error::Usage("two".to_owned()).to_string(), "two");
    assert_eq!(
        Error::Refused(vec!["a".to_owned(), "b".to_owned()]).to_string(),
        "a\nb"
    );
    let io = std::io::Error::other("boom");
    assert_eq!(Error::spawn("read x")(io).to_string(), "read x: boom");
}

#[test]
fn gate_runs_through_the_verb_table() {
    let dir = scratch("gate-verb");
    let out = dir.join("out");
    let exec = FakeExec::says("");
    let lines = run(
        Verb::Gate {
            label: "testing-apt-read".to_owned(),
            settings: vec!["A=set".to_owned()],
            requires: vec![],
            output: Some(out.clone()),
        },
        &exec,
    )
    .expect("gate");
    assert_eq!(lines, ["testing-apt-read: all its settings are set"]);
    assert_eq!(std::fs::read_to_string(out).expect("out"), "enabled=true\n");
    assert!(exec.calls.borrow().is_empty());
}

#[test]
fn assume_is_dispatched_and_judges_its_flags_before_it_reads_the_runner() {
    // Catches: a verb dispatched to another one, and a credential request made for a role that
    // is empty. No token is asked for and no program is run.
    let exec = FakeExec::says("");
    let e = run(
        Verb::Assume {
            role_arn: String::new(),
            session_name: "hostkeeper-apt-1".to_owned(),
            region: "us-east-1".to_owned(),
            token_url: "https://token.example/request".to_owned(),
            env_file: PathBuf::from("env"),
            aws: PathBuf::from("aws"),
        },
        &exec,
    )
    .unwrap_err();
    assert_eq!(e, Error::Usage("--role-arn is empty".to_owned()));
    assert!(exec.calls.borrow().is_empty());
}

#[test]
fn the_apt_and_rpm_verbs_are_dispatched_to_their_channels() {
    // Catches: a channel verb dispatched to the other channel. Each ends where it first reads
    // a file that is not there.
    let exec = FakeExec::says("");
    let dir = scratch("dispatch-channels");
    let e = run(
        Verb::Apt {
            verb: apt::Verb::Promote {
                testing: dir.join("t"),
                stable: dir.join("s"),
                out: dir.join("o"),
                version: "1.2.3".to_owned(),
                sums: dir.join("SHA256SUMS"),
                origin: "komira".to_owned(),
                label: "komira".to_owned(),
                description: "komira packages".to_owned(),
                date: 1,
                keep: 25,
            },
        },
        &exec,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("run `apt read --suite stable` first"), "{e}");
    let e = run(
        Verb::Rpm {
            verb: rpm::Verb::CheckSigned {
                dir: dir.join("signed"),
                version: "1.2.3".to_owned(),
                sums: dir.join("SHA256SUMS"),
                signed_sums: None,
                fingerprint: None,
            },
        },
        &exec,
    )
    .unwrap_err()
    .to_string();
    assert!(e.starts_with("read "), "{e}");
    assert!(exec.calls.borrow().is_empty());
}

#[test]
fn check_apt_is_dispatched_to_the_machine_it_runs_on_through_env() {
    // The first thing the check asks the machine is its architecture; the stand-in refuses, and
    // the failure names the program and the frontend it ran with.
    let exec = FakeExec::fails("boom");
    let e = run(
        Verb::CheckApt {
            package: "hostkeeper-coordinator".to_owned(),
            arch: "amd64".to_owned(),
            version: "1.2.3".to_owned(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            apt_url: "https://packages.komira.ai/apt".to_owned(),
            root: PathBuf::from("/nowhere"),
        },
        &exec,
    )
    .unwrap_err();
    assert_eq!(
        e,
        Error::Failed("dpkg --print-architecture failed: boom".to_owned())
    );
    assert_eq!(
        *exec.calls.borrow(),
        [(
            "env".to_owned(),
            vec![
                "DEBIAN_FRONTEND=noninteractive".to_owned(),
                "dpkg".to_owned(),
                "--print-architecture".to_owned()
            ]
        )]
    );
}

#[test]
fn check_rpm_is_dispatched_to_the_container_through_docker_exec() {
    let exec = FakeExec::fails("boom");
    let e = run(
        Verb::CheckRpm {
            package: "hostkeeper".to_owned(),
            container: "check-rpm".to_owned(),
            arch: "x86_64".to_owned(),
            version: "1.2.3".to_owned(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            url: "https://packages.komira.ai".to_owned(),
            work: "/work".to_owned(),
            scratch: PathBuf::from("/nowhere"),
            docker: PathBuf::from("docker"),
        },
        &exec,
    )
    .unwrap_err();
    assert_eq!(e, Error::Failed("uname -m failed: boom".to_owned()));
    assert_eq!(
        *exec.calls.borrow(),
        [(
            "docker".to_owned(),
            vec![
                "exec".to_owned(),
                "check-rpm".to_owned(),
                "uname".to_owned(),
                "-m".to_owned()
            ]
        )]
    );
}

#[test]
fn decide_is_dispatched_with_the_products_stages() {
    let e = run(
        Verb::Decide {
            results: vec!["check-apt=success".to_owned()],
            output: None,
        },
        &FakeExec::says(""),
    )
    .unwrap_err();
    assert_eq!(
        e,
        Error::Usage("--result testing-apt-write is not given".to_owned())
    );
}
