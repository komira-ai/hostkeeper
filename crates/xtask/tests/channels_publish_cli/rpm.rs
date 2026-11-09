use super::*;

// ---------------------------------------------------------------------------------------------
// rpm
// ---------------------------------------------------------------------------------------------

fn rpm_read(d: &Path, fake: &Fake, suite: &str, out: &Path) -> String {
    ok(
        d,
        &[
            "channels",
            "rpm",
            "repo",
            "read",
            "--suite",
            suite,
            "--bucket",
            BUCKET,
            "--out",
            text(out),
            "--aws",
            text(&fake.program()),
        ],
    )
}

fn rpm_write(
    d: &Path,
    fake: &Fake,
    suite: &str,
    next: &Path,
    pool: Option<(&Path, &str)>,
) -> Output {
    let (aws, curl) = (fake.program(), fake.program());
    let mut args = vec![
        "channels",
        "rpm",
        "repo",
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
        text(next),
        "--aws",
        text(&aws),
        "--curl",
        text(&curl),
    ];
    match pool {
        Some((rpms, version)) => args.extend(["--rpms", text(rpms), "--version", version]),
        None => args.push("--no-pool"),
    }
    xtask(d, &args)
}

/// `rpm repo read`, `index` and `write` of the release in `root/rpms` into `suite`; what `write`
/// printed.
fn rpm_publish(root: &Path, fake: &Fake, suite: &str, version: &str) -> String {
    let (current, next) = (
        root.join(format!("rpm-current-{suite}")),
        root.join(format!("rpm-next-{suite}")),
    );
    rpm_read(root, fake, suite, &current);
    ok(
        root,
        &[
            "channels",
            "rpm",
            "repo",
            "index",
            "--suite",
            suite,
            "--current",
            text(&current),
            "--out",
            text(&next),
            "--rpms",
            text(&root.join("rpms")),
            "--version",
            version,
            "--date",
            DATE,
        ],
    );
    sign_rpm(&next, suite);
    let o = rpm_write(
        root,
        fake,
        suite,
        &next,
        Some((&root.join("rpms"), version)),
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    stdout(&o)
}

#[test]
fn rpm_repo_read_index_and_write_put_a_release_into_the_pool_and_the_repodata() {
    let d = dir("rpm-publish");
    let fake = Fake::new(&d);
    let files = release(&d, "rpms", "1.2.3", "rpm");
    let current = d.join("rpm-current-testing");
    let o = rpm_read(&d, &fake, "testing", &current);
    assert!(
        o.contains("read x86_64/repodata/repomd.xml: does not exist yet\n"),
        "{o}"
    );
    assert_eq!(
        fake.calls()[0],
        [
            "s3api",
            "get-object",
            "--bucket",
            BUCKET,
            "--key",
            "rpm/testing/aarch64/repodata/repomd.xml",
            text(&current.join("testing/aarch64/repodata/repomd.xml")),
        ]
    );
    fake.clear_calls();
    let o = rpm_publish(&d, &fake, "testing", "1.2.3");
    let next = d.join("rpm-next-testing/testing");
    for (_, arch) in ARCHES {
        assert!(next.join(arch).join("repodata/repomd.xml").is_file());
    }
    assert!(d.join("rpm-next-testing/etags.txt").is_file());
    for key in pool_keys("rpm", &files) {
        assert!(o.contains(&format!("uploaded {key}\n")), "{o}");
        assert!(fake.has(&key));
    }
    assert!(o.contains(&format!(
        "invalidated /rpm/testing/aarch64/repodata/* /rpm/testing/x86_64/repodata/*; the repository is at https://{HOST}/rpm/testing/<arch>/repodata/repomd.xml\n"
    )), "{o}");
    let puts = fake.puts();
    let none = ["--if-none-match".to_owned(), "*".to_owned()];
    assert!(puts.iter().all(|(_, cond)| cond == &none), "{puts:?}");
    for (_, arch) in ARCHES {
        for file in ["repomd.xml", "repomd.xml.asc"] {
            let key = format!("rpm/testing/{arch}/repodata/{file}");
            assert!(puts.iter().any(|(k, _)| *k == key), "{key}");
        }
    }
    assert_eq!(
        fake.calls().last().expect("a call"),
        &[
            "cloudfront",
            "create-invalidation",
            "--distribution-id",
            "E1ABC",
            "--paths",
            "/rpm/testing/aarch64/repodata/*",
            "/rpm/testing/x86_64/repodata/*"
        ]
    );
}

#[test]
fn rpm_repo_write_replaces_the_repomd_it_read_with_if_match() {
    let d = dir("rpm-if-match");
    let fake = Fake::new(&d);
    release(&d, "rpms", "1.2.3", "rpm");
    rpm_publish(&d, &fake, "testing", "1.2.3");
    let current = d.join("rpm-current-testing");
    std::fs::remove_dir_all(&current).expect("remove");
    let o = rpm_read(&d, &fake, "testing", &current);
    let said = o
        .lines()
        .find_map(|l| l.strip_prefix("read x86_64/repodata/repomd.xml: "))
        .expect("repomd.xml");
    assert_eq!(
        said,
        etag(&fake.object("rpm/testing/x86_64/repodata/repomd.xml"))
    );
    let next = d.join("next-again");
    ok(
        &d,
        &[
            "channels",
            "rpm",
            "repo",
            "index",
            "--suite",
            "testing",
            "--current",
            text(&current),
            "--out",
            text(&next),
            "--rpms",
            text(&d.join("rpms")),
            "--version",
            "1.2.3",
            "--date",
            DATE,
        ],
    );
    sign_rpm(&next, "testing");
    fake.clear_calls();
    let o = rpm_write(&d, &fake, "testing", &next, None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let puts = fake.puts();
    let repomd = puts
        .iter()
        .find(|(k, _)| k == "rpm/testing/x86_64/repodata/repomd.xml")
        .expect("repomd.xml");
    assert_eq!(repomd.1, ["--if-match".to_owned(), said.to_owned()]);
    assert!(puts.iter().all(|(k, _)| !k.contains("/pool/")), "{puts:?}");
    // Another writer replaced it after the read: a conflict, and with --last a failure.
    fake.put(
        "rpm/testing/x86_64/repodata/repomd.xml",
        b"written by another writer",
    );
    let o = rpm_write(&d, &fake, "testing", &next, None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("::notice title=rpm repodata::rpm/testing/x86_64/repodata/repomd.xml changed while the repodata was built"), "{}", stdout(&o));
    let last = xtask(
        &d,
        &[
            "channels",
            "rpm",
            "repo",
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
            "--last",
            "--aws",
            text(&fake.program()),
        ],
    );
    assert_eq!(last.status.code(), Some(1));
    assert!(
        stderr(&last).contains("this was the last attempt: nothing more is written"),
        "{}",
        stderr(&last)
    );
}

#[test]
fn rpm_repo_write_keeps_a_package_in_the_pool_only_when_the_public_copy_has_the_same_bytes() {
    let d = dir("rpm-pool-compare");
    let fake = Fake::new(&d);
    let files = release(&d, "rpms", "1.2.3", "rpm");
    rpm_publish(&d, &fake, "testing", "1.2.3");
    let next = d.join("rpm-next-testing");
    let rpms = d.join("rpms");
    let url = |name: &str| format!("https://{HOST}/rpm/pool/hostkeeper/{name}");
    // The public host serves what the pool holds.
    for key in pool_keys("rpm", &files) {
        fake.serve(&key, &fake.object(&key));
    }
    let o = rpm_write(&d, &fake, "testing", &next, Some((&rpms, "1.2.3")));
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).contains("rpm/pool/hostkeeper/hostkeeper-1.2.3-1.aarch64.rpm exists already, with the same bytes; it is kept as it is\n"), "{}", stdout(&o));
    let fetched = fake
        .calls()
        .into_iter()
        .find(|c| c.contains(&"--output".to_owned()))
        .expect("a fetch");
    assert_eq!(
        fetched.last().expect("a url"),
        &url("hostkeeper-1.2.3-1.aarch64.rpm")
    );
    assert_eq!(fetched[..4], ["--proto", "=https", "--tlsv1.2", "--fail"]);
    // Other bytes there.
    fake.serve(
        "rpm/pool/hostkeeper/hostkeeper-1.2.3-1.aarch64.rpm",
        b"other bytes",
    );
    let o = rpm_write(&d, &fake, "testing", &next, Some((&rpms, "1.2.3")));
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("exists already with different bytes"),
        "{}",
        stderr(&o)
    );
    // Not served at all.
    std::fs::remove_file(
        fake.bin
            .join("origin/rpm/pool/hostkeeper/hostkeeper-1.2.3-1.aarch64.rpm"),
    )
    .expect("remove");
    let o = rpm_write(&d, &fake, "testing", &next, Some((&rpms, "1.2.3")));
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(&format!(
            "exists already and could not be fetched from {} to compare it",
            url("hostkeeper-1.2.3-1.aarch64.rpm")
        )),
        "{}",
        stderr(&o)
    );
}

#[test]
fn rpm_promote_lists_the_tested_release_in_stable_and_write_no_pool_puts_the_repodata_only() {
    let d = dir("rpm-promote");
    let fake = Fake::new(&d);
    release(&d, "rpms", "1.2.3", "rpm");
    rpm_publish(&d, &fake, "testing", "1.2.3");
    let (testing, stable, out) = (
        d.join("rpm-current-testing"),
        d.join("rpm-current-stable"),
        d.join("rpm-next-stable"),
    );
    std::fs::remove_dir_all(&testing).expect("remove");
    rpm_read(&d, &fake, "testing", &testing);
    rpm_read(&d, &fake, "stable", &stable);
    let sums = d.join("rpms/SHA256SUMS");
    // Nothing is signed here, so the signed sums list the files as they are.
    let promote = |signed: &Path, version: &str| {
        xtask(
            &d,
            &[
                "channels",
                "rpm",
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
                text(&sums),
                "--signed-sums",
                text(signed),
                "--date",
                DATE,
            ],
        )
    };
    let o = promote(&sums, "1.2.3");
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "rpm promote: added 4, dropped 0, listed 4\n");
    for (_, arch) in ARCHES {
        assert!(
            out.join("stable")
                .join(arch)
                .join("repodata/repomd.xml")
                .is_file()
        );
    }
    sign_rpm(&out, "stable");
    fake.clear_calls();
    let o = rpm_write(&d, &fake, "stable", &out, None);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        fake.puts()
            .iter()
            .all(|(k, _)| k.starts_with("rpm/stable/")),
        "{:?}",
        fake.puts()
    );

    let wrong = d.join("wrong-sums");
    let first = std::fs::read_to_string(&sums).expect("sums");
    let changed = format!("{}{}", "0".repeat(64), &first[64..]);
    std::fs::write(&wrong, changed).expect("write");
    let o = promote(&wrong, "1.2.3");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("the bytes that were checked are not the ones in testing"),
        "{}",
        stderr(&o)
    );
    let o = promote(&sums, "9.9.9");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o)
            .contains("SHA256SUMS lists no hostkeeper-9.9.9-1.aarch64.rpm: nothing to promote"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn rpm_check_signed_refuses_a_package_that_is_not_signed_and_a_package_that_is_not_there() {
    let d = dir("rpm-check-signed");
    release(&d, "rpms", "1.2.3", "rpm");
    let (rpms, sums) = (d.join("rpms"), d.join("rpms/SHA256SUMS"));
    let check = |dir: &Path, version: &str, sums: &Path| {
        xtask(
            &d,
            &[
                "channels",
                "rpm",
                "check-signed",
                "--dir",
                text(dir),
                "--version",
                version,
                "--sums",
                text(sums),
            ],
        )
    };
    let o = check(&rpms, "1.2.3", &sums);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("hostkeeper-1.2.3-1.aarch64.rpm: it is not signed"),
        "{}",
        stderr(&o)
    );
    // Another product's package where ours should be.
    let theirs = d.join("theirs");
    std::fs::create_dir_all(&theirs).expect("create");
    std::fs::write(
        theirs.join("other-1.2.3-1.x86_64.rpm"),
        rpm("other", "1.2.3", "x86_64"),
    )
    .expect("write");
    let o = check(&theirs, "1.2.3", &sums);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("hostkeeper-1.2.3-1.aarch64.rpm is not in"),
        "{}",
        stderr(&o)
    );
    let o = check(&rpms, "1.2", &sums);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("--version \"1.2\" is not <major>.<minor>.<patch>"),
        "{}",
        stderr(&o)
    );
    let o = check(&rpms, "1.2.3", &d.join("no-sums"));
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(&format!("read {}", d.join("no-sums").display())),
        "{}",
        stderr(&o)
    );
}

#[test]
fn rpm_verbs_with_flags_that_do_not_fit_are_status_2() {
    let d = dir("rpm-usage");
    let current = empty_current(&d, "current");
    release(&d, "rpms", "1.2.3", "rpm");
    let (out_dir, rpms_dir) = (d.join("out"), d.join("rpms"));
    let (cur, out, rpms) = (text(&current), text(&out_dir), text(&rpms_dir));
    let why = exits(
        &d,
        2,
        &[
            "channels", "rpm", "repo", "read", "--suite", "beta", "--bucket", BUCKET, "--out", out,
        ],
    );
    assert!(why.contains("invalid value 'beta'"), "{why}");
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "rpm",
            "repo",
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
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "rpm",
            "repo",
            "index",
            "--current",
            cur,
            "--out",
            out,
            "--rpms",
            rpms,
            "--version",
            "1.2",
            "--date",
            DATE,
        ],
    );
    assert!(
        why.contains("--version \"1.2\" is not <major>.<minor>.<patch>"),
        "{why}"
    );
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "rpm",
            "repo",
            "write",
            "--bucket",
            BUCKET,
            "--distribution-id",
            "E1",
            "--hostname",
            HOST,
            "--next",
            out,
            "--rpms",
            rpms,
        ],
    );
    assert!(why.contains("--version"), "{why}");
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "rpm",
            "repo",
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
            "--rpms",
            rpms,
        ],
    );
    assert!(why.contains("cannot be used with"), "{why}");
    let why = exits(
        &d,
        2,
        &[
            "channels",
            "rpm",
            "repo",
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
            "rpm",
            "promote",
            "--testing",
            cur,
            "--stable",
            cur,
            "--out",
            out,
            "--version",
            "x",
            "--sums",
            "s",
            "--signed-sums",
            "s",
            "--date",
            DATE,
        ],
    );
    assert!(
        why.contains("--version \"x\" is not <major>.<minor>.<patch>"),
        "{why}"
    );
}

#[test]
fn rpm_index_of_a_package_that_is_not_the_product_s_is_a_failure_naming_it() {
    let d = dir("rpm-other-product");
    let current = empty_current(&d, "current");
    let run = |rpms: &Path| {
        xtask(
            &d,
            &[
                "channels",
                "rpm",
                "repo",
                "index",
                "--current",
                text(&current),
                "--out",
                text(&d.join("out")),
                "--rpms",
                text(rpms),
                "--version",
                "1.2.3",
                "--date",
                DATE,
            ],
        )
    };
    let mixed = d.join("rpms");
    release(&d, "rpms", "1.2.3", "rpm");
    std::fs::write(
        mixed.join("hostkeeper-1.2.3-1.x86_64.rpm"),
        rpm("other", "1.2.3", "x86_64"),
    )
    .expect("write");
    let o = run(&mixed);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("hostkeeper-1.2.3-1.x86_64.rpm is the package other-1.2.3-1.x86_64.rpm: its header does not say the name it has"),
        "{}",
        stderr(&o)
    );
    assert!(!d.join("out").exists());
}
