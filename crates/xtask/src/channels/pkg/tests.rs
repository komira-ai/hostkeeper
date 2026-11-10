use super::testing::*;
use super::*;

/// A change to a request, to put one flag out of its form.
type Change = fn(&mut Request);

#[test]
fn the_files_of_a_version_are_each_installer_and_its_bundle_held_to_the_release_sums() {
    // Catches: hello's installer left out of the version's directory, or its sums.
    let f = Fixture::new("collect");
    let entries = collect(&f.release, "1.2.3").expect("collected");
    let listed: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        listed,
        [
            "hello-1.2.3-macos-arm64.pkg",
            "hello-1.2.3-macos-arm64.pkg.sigstore.json",
            "hostkeeper-1.2.3-macos-arm64.pkg",
            "hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json",
        ]
    );
    assert_eq!(listed, names());
    assert_eq!(
        entries[0].size,
        "bytes of hello-1.2.3-macos-arm64.pkg".len() as u64
    );
    assert_eq!(
        entries[2].size,
        "bytes of hostkeeper-1.2.3-macos-arm64.pkg".len() as u64
    );
    assert_eq!(
        sums_text(&entries),
        format!(
            "{}  hello-1.2.3-macos-arm64.pkg\n{}  hello-1.2.3-macos-arm64.pkg.sigstore.json\n{}  hostkeeper-1.2.3-macos-arm64.pkg\n{}  hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json\n",
            entries[0].sha256, entries[1].sha256, entries[2].sha256, entries[3].sha256
        )
    );
}

#[test]
fn a_file_that_is_not_what_the_release_sums_list_is_refused() {
    // The installer changed after the sums were made, the bundle missing, and an installer the
    // sums do not list.
    let f = Fixture::new("collect-bad");
    let installer = f.release.join("hostkeeper-1.2.3-macos-arm64.pkg");
    std::fs::write(&installer, "changed").expect("change");
    std::fs::remove_file(
        f.release
            .join("hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json"),
    )
    .expect("remove");
    let Error::Refused(problems) = collect(&f.release, "1.2.3").unwrap_err() else {
        panic!("refused")
    };
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
        problems[0].starts_with("hostkeeper-1.2.3-macos-arm64.pkg is ")
            && problems[0].contains(", but SHA256SUMS lists "),
        "{problems:?}"
    );
    assert!(
        problems[1].ends_with("hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json is not a file"),
        "{problems:?}"
    );

    let g = Fixture::new("collect-unlisted");
    std::fs::write(
        g.release.join(SUMS),
        format!("{}  other\n", Sha256::of(b"x")),
    )
    .expect("sums");
    let Error::Refused(problems) = collect(&g.release, "1.2.3").unwrap_err() else {
        panic!("refused")
    };
    assert_eq!(
        problems,
        [
            "SHA256SUMS does not list hello-1.2.3-macos-arm64.pkg",
            "SHA256SUMS does not list hostkeeper-1.2.3-macos-arm64.pkg"
        ]
    );

    // A bundle the sums do list is held to its digest too.
    let h = Fixture::new("collect-listed-bundle");
    let sums_path = h.release.join(SUMS);
    let mut text = std::fs::read_to_string(&sums_path).expect("sums");
    text.push_str(&format!(
        "{}  hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json\n",
        Sha256::of(b"other")
    ));
    std::fs::write(&sums_path, text).expect("sums");
    assert!(matches!(
        collect(&h.release, "1.2.3"),
        Err(Error::Refused(_))
    ));
}

#[test]
fn a_release_whose_sums_do_not_list_hellos_installer_is_refused() {
    // Catches: hello's installer published from a release whose SHA256SUMS does not vouch for
    // it, while hostkeeper's is listed.
    let f = Fixture::new("collect-no-hello");
    let sums_path = f.release.join(SUMS);
    let text: String = std::fs::read_to_string(&sums_path)
        .expect("sums")
        .lines()
        .filter(|l| !l.ends_with("  hello-1.2.3-macos-arm64.pkg"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&sums_path, text).expect("sums");
    let Error::Refused(problems) = collect(&f.release, "1.2.3").unwrap_err() else {
        panic!("refused")
    };
    assert_eq!(
        problems,
        ["SHA256SUMS does not list hello-1.2.3-macos-arm64.pkg"]
    );
}

#[test]
fn the_installer_is_served_as_binary_data_its_bundle_as_json_and_the_sums_as_text() {
    assert_eq!(
        content_type("hostkeeper-1.2.3-macos-arm64.pkg"),
        "application/octet-stream"
    );
    assert_eq!(
        content_type("hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json"),
        "application/json"
    );
    assert_eq!(content_type(SUMS), "text/plain; charset=utf-8");
}

#[test]
fn flags_out_of_their_form_are_usage_errors_before_anything_is_read() {
    let f = Fixture::new("usage");
    let cases: [(Change, &str); 8] = [
        (
            |r| r.tag = "1.2.3",
            "--tag \"1.2.3\" is not v<major>.<minor>.<patch>",
        ),
        (
            |r| r.tag = "v01.2.3",
            "--tag \"v01.2.3\" is not v<major>.<minor>.<patch>",
        ),
        (
            |r| r.tag = "v99999999999999999999.0.0",
            "--tag \"v99999999999999999999.0.0\" holds a number too large",
        ),
        (
            |r| r.commit = "abc",
            "--commit \"abc\": not a commit: \"abc\" (expected 40 lowercase hex digits)",
        ),
        (
            |r| r.bucket = "Bucket/x",
            "--bucket \"Bucket/x\" is not a bucket name",
        ),
        (
            |r| r.distribution_id = "e1",
            "--distribution-id \"e1\" is not a distribution id",
        ),
        (|r| r.attempts = 0, "--attempts is 0; it must be 1 or more"),
        (|r| r.bucket = "", "--bucket \"\" is not a bucket name"),
    ];
    for (change, want) in cases {
        let mut request = f.request("");
        change(&mut request);
        assert_eq!(
            check_request(&request).unwrap_err(),
            Error::Usage(want.to_owned())
        );
    }
    assert_eq!(
        check_request(&f.request("EDIST123")).expect("fits"),
        ("1.2.3".to_owned(), (1, 2, 3))
    );
}

#[test]
fn the_verbs_run_through_the_port_and_name_the_programs_they_use() {
    use crate::channels::fixtures::FakeExec;
    let f = Fixture::new("run");
    let exec = FakeExec::says("{}");
    let work = f.work.clone();
    let release = f.release.clone();
    let said = run(
        Verb::Upload {
            dir: release.clone(),
            tag: "v1.2.3".to_owned(),
            commit: COMMIT.to_owned(),
            bucket: BUCKET.to_owned(),
            work: work.clone(),
            aws: PathBuf::from("aws"),
        },
        &exec,
    )
    .expect("uploaded");
    // Two installers and their bundles, then the directory's sums.
    assert_eq!(said.len(), 5);
    assert!(
        exec.calls
            .borrow()
            .iter()
            .all(|(program, _)| program == "aws")
    );
    assert_eq!(exec.calls.borrow().len(), 5);

    // The promotion reads the bucket back through the same port; the stand-in's answers are
    // not the files, so it refuses at the first one.
    let refused = run(
        Verb::Promote {
            dir: release,
            tag: "v1.2.3".to_owned(),
            commit: COMMIT.to_owned(),
            bucket: BUCKET.to_owned(),
            distribution_id: String::new(),
            work: work.clone(),
            attempts: 1,
            aws: PathBuf::from("aws"),
        },
        &FakeExec::fails("boom"),
    )
    .unwrap_err();
    assert_eq!(
        refused,
        Error::Failed(
            "pkg/hostkeeper/1.2.3/hello-1.2.3-macos-arm64.pkg could not be read back: boom"
                .to_owned()
        )
    );

    let checked = run(
        Verb::Check {
            sums: f.release.join(SUMS),
            version: "1.2.3".to_owned(),
            url: "http://packages.komira.ai".to_owned(),
            work,
            curl: PathBuf::from("curl"),
        },
        &exec,
    )
    .unwrap_err();
    assert!(matches!(checked, Error::Usage(_)));
}
