use super::*;

// ---------------------------------------------------------------------------------------------
// apt
// ---------------------------------------------------------------------------------------------

/// `apt read`, `apt index` and `apt write` of the release in `root/<debs>` into `suite`; what
/// `write` printed. `sign` is not run: the files it writes are put there as they are.
fn apt_publish(root: &Path, fake: &Fake, suite: &str, debs: &str, version: &str) -> String {
    let aws = fake.program();
    let current = root.join(format!("apt-current-{suite}"));
    let next = root.join(format!("apt-next-{suite}"));
    let debs = root.join(debs);
    ok(
        root,
        &[
            "channels",
            "apt",
            "read",
            "--suite",
            suite,
            "--bucket",
            BUCKET,
            "--out",
            text(&current),
            "--aws",
            text(&aws),
        ],
    );
    ok(
        root,
        &[
            "channels",
            "apt",
            "index",
            "--suite",
            suite,
            "--current",
            text(&current),
            "--out",
            text(&next),
            "--debs",
            text(&debs),
            "--version",
            version,
            "--origin",
            "Komira",
            "--label",
            "Komira",
            "--description",
            "Packages",
            "--date",
            DATE,
        ],
    );
    sign_apt(&next, suite);
    ok(
        root,
        &[
            "channels",
            "apt",
            "write",
            "--suite",
            suite,
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1ABC",
            "--hostname",
            HOST,
            "--next",
            text(&next),
            "--debs",
            text(&debs),
            "--version",
            version,
            "--aws",
            text(&aws),
        ],
    )
}

/// The files `apt sign` adds to a new index (which this test does not run).
fn sign_apt(next: &Path, suite: &str) {
    for file in ["InRelease", "Release.gpg"] {
        std::fs::write(next.join("dists").join(suite).join(file), b"signed").expect("write");
    }
}

#[test]
fn apt_read_index_and_write_put_a_release_into_the_pool_and_the_index_with_if_none_match() {
    let d = dir("apt-publish");
    let fake = Fake::new(&d);
    let files = release(&d, "debs", "1.2.3", "deb");
    let current = d.join("apt-current-testing");
    let o = ok(
        &d,
        &[
            "channels",
            "apt",
            "read",
            "--suite",
            "testing",
            "--bucket",
            BUCKET,
            "--out",
            text(&current),
            "--aws",
            text(&fake.program()),
        ],
    );
    assert!(o.contains("read Release: does not exist yet\n"), "{o}");
    assert_eq!(
        fake.calls()[0],
        [
            "s3api",
            "get-object",
            "--bucket",
            BUCKET,
            "--key",
            "apt/dists/testing/Release",
            text(&current.join("dists/testing/Release")),
        ]
    );
    assert!(current.join("etags.txt").is_file());

    fake.clear_calls();
    let o = apt_publish(&d, &fake, "testing", "debs", "1.2.3");
    let next = d.join("apt-next-testing/dists/testing");
    let release_file = std::fs::read_to_string(next.join("Release")).expect("Release");
    assert!(release_file.contains("Origin: Komira\n"), "{release_file}");
    let packages =
        std::fs::read_to_string(next.join("main/binary-amd64/Packages")).expect("Packages");
    for name in [
        "hostkeeper_1.2.3_amd64.deb",
        "hostkeeper-coordinator_1.2.3_amd64.deb",
    ] {
        assert!(
            packages.contains(&format!("Filename: pool/hostkeeper/{name}\n")),
            "{packages}"
        );
    }
    assert!(next.join("main/binary-arm64/Packages.gz").is_file());

    for key in pool_keys("apt", &files) {
        assert!(o.contains(&format!("uploaded {key}\n")), "{o}");
        assert!(fake.has(&key));
    }
    assert!(o.contains(&format!(
        "invalidated /apt/dists/*; the index is at https://{HOST}/apt/dists/testing/InRelease\n"
    )));
    let puts = fake.puts();
    let none = ["--if-none-match".to_owned(), "*".to_owned()];
    for (key, cond) in &puts {
        assert!(
            key.starts_with("apt/pool/hostkeeper/") || key.starts_with("apt/dists/testing/"),
            "{key}"
        );
        assert_eq!(cond, &none, "{key}");
    }
    assert!(puts.iter().any(|(k, _)| k == "apt/dists/testing/InRelease"));
    assert_eq!(
        fake.calls().last().expect("a call"),
        &[
            "cloudfront",
            "create-invalidation",
            "--distribution-id",
            "E1ABC",
            "--paths",
            "/apt/dists/*"
        ]
    );
}

#[test]
fn apt_write_replaces_the_index_it_read_with_if_match_on_the_etags_read() {
    let d = dir("apt-if-match");
    let fake = Fake::new(&d);
    release(&d, "debs", "1.2.3", "deb");
    apt_publish(&d, &fake, "testing", "debs", "1.2.3");
    fake.clear_calls();
    let read = ok(
        &d,
        &[
            "channels",
            "apt",
            "read",
            "--suite",
            "testing",
            "--bucket",
            BUCKET,
            "--out",
            text(&d.join("apt-current-testing")),
            "--aws",
            text(&fake.program()),
        ],
    );
    let said = read
        .lines()
        .find_map(|l| l.strip_prefix("read InRelease: "))
        .expect("InRelease");
    assert_eq!(said, etag(&fake.object("apt/dists/testing/InRelease")));

    fake.clear_calls();
    let next = d.join("apt-next-testing");
    let current = d.join("apt-current-testing");
    ok(
        &d,
        &[
            "channels",
            "apt",
            "index",
            "--suite",
            "testing",
            "--current",
            text(&current),
            "--out",
            text(&next),
            "--debs",
            text(&d.join("debs")),
            "--version",
            "1.2.3",
            "--origin",
            "Other",
            "--label",
            "Other",
            "--description",
            "Other",
            "--date",
            DATE,
        ],
    );
    sign_apt(&next, "testing");
    let o = ok(
        &d,
        &[
            "channels",
            "apt",
            "write",
            "--suite",
            "testing",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1ABC",
            "--hostname",
            HOST,
            "--next",
            text(&next),
            "--no-pool",
            "--aws",
            text(&fake.program()),
        ],
    );
    assert!(o.contains("wrote apt/dists/testing/InRelease\n"), "{o}");
    let puts = fake.puts();
    assert!(puts.iter().all(|(k, _)| !k.contains("/pool/")), "{puts:?}");
    let commit = puts
        .iter()
        .find(|(k, _)| k == "apt/dists/testing/InRelease")
        .expect("InRelease");
    assert_eq!(commit.1, ["--if-match".to_owned(), said.to_owned()]);
    // The Release that exists keeps its own Origin.
    let release = std::fs::read_to_string(next.join("dists/testing/Release")).expect("Release");
    assert!(release.contains("Origin: Komira\n"), "{release}");
}

#[test]
fn apt_write_after_another_writer_changed_the_index_is_a_conflict_and_with_last_a_failure() {
    let d = dir("apt-conflict");
    let fake = Fake::new(&d);
    release(&d, "debs", "1.2.3", "deb");
    apt_publish(&d, &fake, "testing", "debs", "1.2.3");
    let (current, next) = (d.join("apt-current-testing"), d.join("apt-next-testing"));
    let aws = fake.program();
    ok(
        &d,
        &[
            "channels",
            "apt",
            "read",
            "--suite",
            "testing",
            "--bucket",
            BUCKET,
            "--out",
            text(&current),
            "--aws",
            text(&aws),
        ],
    );
    std::fs::remove_dir_all(&next).expect("remove");
    ok(
        &d,
        &[
            "channels",
            "apt",
            "index",
            "--suite",
            "testing",
            "--current",
            text(&current),
            "--out",
            text(&next),
            "--debs",
            text(&d.join("debs")),
            "--version",
            "1.2.3",
            "--origin",
            "Komira",
            "--label",
            "Komira",
            "--description",
            "Packages",
            "--date",
            DATE,
        ],
    );
    sign_apt(&next, "testing");
    // Someone else writes the Release after it was read.
    fake.put("apt/dists/testing/Release", b"written by another writer");
    let output = d.join("github-output");
    let write = |extra: &[&str]| {
        let mut args = vec![
            "channels",
            "apt",
            "write",
            "--suite",
            "testing",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1ABC",
            "--hostname",
            HOST,
            "--next",
            text(&next),
            "--no-pool",
            "--aws",
            text(&aws),
        ];
        args.extend_from_slice(extra);
        xtask(&d, &args)
    };
    let o = write(&["--output", text(&output)]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o).contains(
            "::notice title=apt index::apt/dists/testing/Release changed while the index was built"
        ),
        "{}",
        stdout(&o)
    );
    assert_eq!(
        std::fs::read_to_string(&output).expect("output"),
        "conflict=true\n"
    );
    let o = write(&["--last"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("this was the last attempt: nothing more is written"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn apt_promote_lists_the_tested_release_in_stable_and_write_no_pool_puts_the_index_only() {
    let d = dir("apt-promote");
    let fake = Fake::new(&d);
    release(&d, "debs", "1.2.3", "deb");
    apt_publish(&d, &fake, "testing", "debs", "1.2.3");
    let aws = fake.program();
    let (testing, stable, out) = (
        d.join("apt-current-testing"),
        d.join("apt-current-stable"),
        d.join("apt-next-stable"),
    );
    for (suite, dest) in [("testing", &testing), ("stable", &stable)] {
        ok(
            &d,
            &[
                "channels",
                "apt",
                "read",
                "--suite",
                suite,
                "--bucket",
                BUCKET,
                "--out",
                text(dest),
                "--aws",
                text(&aws),
            ],
        );
    }
    let sums = d.join("debs/SHA256SUMS");
    let promote = |sums: &Path, version: &str| {
        xtask(
            &d,
            &[
                "channels",
                "apt",
                "promote",
                "--testing",
                text(&testing),
                "--stable",
                text(&stable),
                "--out",
                text(&out),
                "--version",
                version,
                "--sums",
                text(sums),
                "--origin",
                "Komira",
                "--label",
                "Komira",
                "--description",
                "Packages",
                "--date",
                DATE,
            ],
        )
    };
    let o = promote(&sums, "1.2.3");
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "apt promote: added 4, dropped 0, listed 4\n");
    let packages = std::fs::read_to_string(out.join("dists/stable/main/binary-arm64/Packages"))
        .expect("Packages");
    assert!(
        packages.contains("Filename: pool/hostkeeper/hostkeeper_1.2.3_arm64.deb\n"),
        "{packages}"
    );
    assert!(out.join("etags.txt").is_file());

    sign_apt(&out, "stable");
    fake.clear_calls();
    let o = ok(
        &d,
        &[
            "channels",
            "apt",
            "write",
            "--suite",
            "stable",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1ABC",
            "--hostname",
            HOST,
            "--next",
            text(&out),
            "--no-pool",
            "--aws",
            text(&aws),
        ],
    );
    assert!(o.contains("wrote apt/dists/stable/InRelease\n"), "{o}");
    assert!(
        fake.puts()
            .iter()
            .all(|(k, _)| k.starts_with("apt/dists/stable/")),
        "{:?}",
        fake.puts()
    );

    // A SHA256SUMS that lists another file than the one testing holds refuses the promotion.
    let other = d.join("other-sums");
    let wrong = std::fs::read_to_string(&sums).expect("sums").replacen(
        &sha256(&deb("hostkeeper", "1.2.3", "amd64")),
        &"0".repeat(64),
        1,
    );
    std::fs::write(&other, wrong).expect("write");
    let o = promote(&other, "1.2.3");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("the bytes that were checked are not the ones in testing"),
        "{}",
        stderr(&o)
    );
    // A version no release has is not in the sums.
    let o = promote(&sums, "9.9.9");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("SHA256SUMS lists no hostkeeper-coordinator_9.9.9_amd64.deb"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn apt_verbs_with_flags_that_do_not_fit_are_status_2() {
    let d = dir("apt-usage");
    let current = empty_current(&d, "current");
    release(&d, "debs", "1.2.3", "deb");
    let debs = d.join("debs");
    let out_dir = d.join("out");
    let (cur, out, debs) = (text(&current), text(&out_dir), text(&debs));
    let index = |version: &'static str| {
        vec![
            "channels",
            "apt",
            "index",
            "--current",
            cur,
            "--out",
            out,
            "--debs",
            debs,
            "--version",
            version,
            "--origin",
            "O",
            "--label",
            "L",
            "--description",
            "D",
            "--date",
            DATE,
        ]
    };
    let why = exits(
        &d,
        2,
        &[
            "channels", "apt", "read", "--suite", "beta", "--bucket", BUCKET, "--out", out,
        ],
    );
    assert!(why.contains("invalid value 'beta'"), "{why}");
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "apt",
            "read",
            "--bucket",
            "Not_A_Bucket",
            "--out",
            out,
        ],
    );
    assert!(
        why.contains("--bucket \"Not_A_Bucket\" is not a bucket name"),
        "{why}"
    );
    let why = exits(&d, 2, &index("1.2"));
    assert!(
        why.contains("--version \"1.2\" is not <major>.<minor>.<patch>"),
        "{why}"
    );
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "apt",
            "write",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1",
            "--hostname",
            HOST,
            "--next",
            out,
            "--debs",
            debs,
        ],
    );
    assert!(why.contains("--version"), "{why}");
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "apt",
            "write",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1",
            "--hostname",
            HOST,
            "--next",
            out,
            "--no-pool",
            "--debs",
            debs,
        ],
    );
    assert!(why.contains("cannot be used with"), "{why}");
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "apt",
            "write",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "bad id",
            "--hostname",
            HOST,
            "--next",
            out,
            "--no-pool",
        ],
    );
    assert!(
        why.contains("--distribution-id \"bad id\" is not a distribution id"),
        "{why}"
    );
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "apt",
            "write",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1",
            "--hostname",
            "bad host",
            "--next",
            out,
            "--no-pool",
        ],
    );
    assert!(
        why.contains("--hostname \"bad host\" is not a host name"),
        "{why}"
    );
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "apt",
            "promote",
            "--testing",
            cur,
            "--stable",
            cur,
            "--out",
            out,
            "--version",
            "1.2.x",
            "--sums",
            "s",
            "--origin",
            "O",
            "--label",
            "L",
            "--description",
            "D",
            "--date",
            DATE,
        ],
    );
    assert!(
        why.contains("--version \"1.2.x\" is not <major>.<minor>.<patch>"),
        "{why}"
    );
}

#[test]
fn apt_index_of_a_package_that_is_not_the_product_s_is_a_failure_naming_it() {
    let d = dir("apt-other-product");
    let current = empty_current(&d, "current");
    let run = |debs: &Path| {
        xtask(
            &d,
            &[
                "channels",
                "apt",
                "index",
                "--current",
                text(&current),
                "--out",
                text(&d.join("out")),
                "--debs",
                text(debs),
                "--version",
                "1.2.3",
                "--origin",
                "O",
                "--label",
                "L",
                "--description",
                "D",
                "--date",
                DATE,
            ],
        )
    };
    // Only another product's files in the directory.
    let theirs = d.join("theirs");
    std::fs::create_dir_all(&theirs).expect("create");
    std::fs::write(
        theirs.join("other_1.2.3_amd64.deb"),
        deb("other", "1.2.3", "amd64"),
    )
    .expect("write");
    let o = run(&theirs);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("hostkeeper-coordinator_1.2.3_amd64.deb is not in"),
        "{}",
        stderr(&o)
    );
    // A file named as ours whose control file says another package.
    let mixed = d.join("mixed");
    release(&d, "mixed", "1.2.3", "deb");
    std::fs::write(
        mixed.join("hostkeeper_1.2.3_amd64.deb"),
        deb("other", "1.2.3", "amd64"),
    )
    .expect("write");
    let o = run(&mixed);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("hostkeeper_1.2.3_amd64.deb is the package pool/hostkeeper/other_1.2.3_amd64.deb: its control file does not say the name it has"),
        "{}",
        stderr(&o)
    );
    assert!(!d.join("out").exists());
}
