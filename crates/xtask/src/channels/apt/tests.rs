use super::testing::*;
use super::*;
use crate::channels::fixtures::FakeExec;

#[test]
fn a_failed_aws_call_is_told_by_what_it_says() {
    // Catches: a lost race read as a failure (or the reverse), and a missing key read as a denial.
    assert_eq!(classify(PRECONDITION), Failure::Precondition);
    assert_eq!(
        classify("An error occurred (ConditionalRequestConflict) when calling"),
        Failure::Precondition
    );
    assert_eq!(
        classify("a response (412) came back"),
        Failure::Precondition
    );
    assert_eq!(classify(NO_SUCH_KEY), Failure::NotFound);
    assert_eq!(classify("Not Found"), Failure::NotFound);
    assert_eq!(classify(DENIED), Failure::Denied);
    assert_eq!(classify("Forbidden"), Failure::Denied);
    assert_eq!(classify("could not connect"), Failure::Other);
}

#[test]
fn the_package_files_of_a_version_are_every_package_for_every_architecture_and_must_all_be_there() {
    // Catches: an index or upload for one architecture or one package only, or for another
    // version's files.
    let dir = scratch("package-files");
    release_files(&dir, "1.2.3");
    let files = package_files(&dir, "1.2.3").expect("files");
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "hostkeeper-coordinator_1.2.3_amd64.deb",
            "hostkeeper-coordinator_1.2.3_arm64.deb",
            "hostkeeper_1.2.3_amd64.deb",
            "hostkeeper_1.2.3_arm64.deb"
        ]
    );
    assert_eq!(
        files[0].1,
        dir.join("hostkeeper-coordinator_1.2.3_amd64.deb")
    );
    std::fs::remove_file(dir.join("hostkeeper_1.2.3_arm64.deb")).expect("remove");
    let e = package_files(&dir, "1.2.3").unwrap_err().to_string();
    assert!(e.contains("hostkeeper_1.2.3_arm64.deb is not in"), "{e}");
    // The coordinator's package is as much part of the release as the daemon's.
    release_files(&dir, "1.2.3");
    std::fs::remove_file(dir.join("hostkeeper-coordinator_1.2.3_amd64.deb")).expect("remove");
    let e = package_files(&dir, "1.2.3").unwrap_err().to_string();
    assert!(
        e.contains("hostkeeper-coordinator_1.2.3_amd64.deb is not in"),
        "{e}"
    );
    assert!(package_files(&dir, "1.2.4").is_err());
    assert!(matches!(
        package_files(&dir, "v1.2.3"),
        Err(Error::Usage(_))
    ));
}

#[test]
fn a_write_verb_with_a_pool_half_given_is_a_usage_error() {
    // Catches: a pool of one half (debs without a version) read as a pool or as none; clap holds
    // the flags together on the command line, and the verb holds them again.
    let verb = |debs: Option<&str>, version: Option<&str>, no_pool: bool| Verb::Write {
        suite: Suite::Stable,
        bucket: "apt-bucket".to_owned(),
        distribution_id: "E1".to_owned(),
        hostname: "packages.example".to_owned(),
        next: PathBuf::from("next"),
        debs: debs.map(PathBuf::from),
        version: version.map(str::to_owned),
        no_pool,
        last: true,
        output: None,
        aws: PathBuf::from("aws"),
    };
    for v in [
        verb(Some("d"), None, false),
        verb(None, None, false),
        verb(Some("d"), Some("1.2.3"), true),
    ] {
        let e = run(v, &FakeExec::missing()).unwrap_err();
        assert!(matches!(e, Error::Usage(_)), "{e}");
    }
}

#[test]
fn every_verb_is_wired_to_its_implementation_and_reports_what_it_meets_first() {
    // Catches: a verb that is not dispatched, or dispatched to another one. The programs are not
    // there, so each ends where it first needs one; the others end at the first file they read.
    const NOWHERE: &str = "/nonexistent/program";
    let dir = scratch("dispatch");
    let p = |name: &str| dir.join(name);
    let exec = FakeExec::missing();
    let fails = |v: Verb| run(v, &exec).unwrap_err().to_string();
    let e = fails(Verb::Read {
        suite: Suite::Testing,
        bucket: "apt-bucket".to_owned(),
        out: p("read"),
        aws: PathBuf::from(NOWHERE),
    });
    assert!(e.contains(NOWHERE), "{e}");
    let e = fails(Verb::Index {
        suite: Suite::Testing,
        current: p("read"),
        out: p("next"),
        debs: p("dist"),
        version: "1.2.3".to_owned(),
        origin: "komira".to_owned(),
        label: "komira".to_owned(),
        description: "komira packages".to_owned(),
        date: 1,
        keep: 25,
    });
    assert!(e.contains("run `apt read` first"), "{e}");
    std::fs::write(p("key"), "k").expect("key");
    std::fs::write(p("pass"), "p").expect("pass");
    std::fs::create_dir_all(p("next/dists/testing")).expect("dir");
    std::fs::write(p("next/dists/testing/Release"), "Suite: testing\n").expect("release");
    let e = fails(Verb::Sign {
        dir: p("next/dists/testing"),
        work: p("work"),
        fingerprint: "F767235D83B0410CFAF4779E8738AD1BE061EEDD".to_owned(),
        key_file: p("key"),
        passphrase_file: p("pass"),
        gpg: PathBuf::from(NOWHERE),
        output: None,
    });
    assert!(e.contains(NOWHERE), "{e}");
    assert!(!p("key").exists(), "the key is removed whatever came of it");
    let e = fails(Verb::Promote {
        testing: p("t"),
        stable: p("s"),
        out: p("o"),
        version: "1.2.3".to_owned(),
        sums: p("SHA256SUMS"),
        origin: "komira".to_owned(),
        label: "komira".to_owned(),
        description: "komira packages".to_owned(),
        date: 1,
        keep: 25,
    });
    assert!(e.contains("run `apt read --suite stable` first"), "{e}");
    let e = fails(Verb::Write {
        suite: Suite::Testing,
        bucket: "apt-bucket".to_owned(),
        distribution_id: "E1ABC".to_owned(),
        hostname: "packages-test.example".to_owned(),
        next: p("next"),
        debs: Some(p("dist")),
        version: Some("1.2.3".to_owned()),
        no_pool: false,
        last: false,
        output: None,
        aws: PathBuf::from(NOWHERE),
    });
    assert!(e.contains("is not in"), "{e}");
    // Without a pool, the verb goes to the index it is to write and finds none.
    let e = fails(Verb::Write {
        suite: Suite::Testing,
        bucket: "apt-bucket".to_owned(),
        distribution_id: "E1ABC".to_owned(),
        hostname: "packages-test.example".to_owned(),
        next: p("next"),
        debs: None,
        version: None,
        no_pool: true,
        last: false,
        output: None,
        aws: PathBuf::from(NOWHERE),
    });
    assert!(e.contains("is missing from the new index"), "{e}");
}
