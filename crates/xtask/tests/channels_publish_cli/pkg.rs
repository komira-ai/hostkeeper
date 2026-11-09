use super::*;

// ---------------------------------------------------------------------------------------------
// pkg
// ---------------------------------------------------------------------------------------------

fn pkg(d: &Path, fake: &Fake, verb: &str, dir: &Path, tag: &str, extra: &[&str]) -> Output {
    let (work, aws) = (d.join(format!("work-{verb}")), fake.program());
    let mut args = vec![
        "channels",
        "pkg",
        verb,
        "--dir",
        text(dir),
        "--tag",
        tag,
        "--commit",
        COMMIT,
        "--bucket",
        BUCKET,
        "--work",
        text(&work),
        "--aws",
        text(&aws),
    ];
    args.extend_from_slice(extra);
    xtask(d, &args)
}

#[test]
fn pkg_upload_puts_the_installer_its_bundle_and_the_sums_once_each_below_the_version() {
    let d = dir("pkg-upload");
    let fake = Fake::new(&d);
    let release = pkg_release(&d, "release", "1.2.3");
    let o = pkg(&d, &fake, "upload", &release, "v1.2.3", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let prefix = "pkg/hostkeeper/1.2.3/";
    let said = stdout(&o);
    let keys = [
        format!("{prefix}{INSTALLER}"),
        format!("{prefix}{INSTALLER}.sigstore.json"),
        format!("{prefix}SHA256SUMS"),
    ];
    for key in &keys {
        assert!(said.contains(&format!("{key}: written\n")), "{said}");
        assert!(fake.has(key));
    }
    assert_eq!(
        fake.object(&keys[0]),
        std::fs::read(release.join(INSTALLER)).expect("read")
    );
    let none = ["--if-none-match".to_owned(), "*".to_owned()];
    let puts = fake.puts();
    assert_eq!(puts.len(), 3);
    assert!(
        puts.iter()
            .all(|(k, cond)| keys.contains(k) && cond == &none),
        "{puts:?}"
    );
    let sums = String::from_utf8(fake.object(&keys[2])).expect("utf8");
    assert!(
        sums.contains(&format!("  {INSTALLER}\n"))
            && sums.contains(&format!("  {INSTALLER}.sigstore.json\n")),
        "{sums}"
    );
    assert!(!fake.has("pkg/hostkeeper/stable.json"));
    // Again: the files are there with the same bytes, and kept.
    let o = pkg(&d, &fake, "upload", &release, "v1.2.3", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o).contains(&format!("{}: already there with the same bytes\n", keys[0])),
        "{}",
        stdout(&o)
    );
    // Another installer under the same version is refused, and the first one stays.
    let changed = pkg_release(&d, "changed", "1.2.3");
    std::fs::write(changed.join(INSTALLER), b"other installer").expect("write");
    std::fs::write(
        changed.join("SHA256SUMS"),
        format!("{}  {INSTALLER}\n", sha256(b"other installer")),
    )
    .expect("write");
    let o = pkg(&d, &fake, "upload", &changed, "v1.2.3", &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(&format!("{} exists with other bytes", keys[0])),
        "{}",
        stderr(&o)
    );
    assert_eq!(
        fake.object(&keys[0]),
        std::fs::read(release.join(INSTALLER)).expect("read")
    );
}

#[test]
fn pkg_promote_moves_stable_json_with_if_none_match_then_if_match_and_never_to_an_older_version() {
    let d = dir("pkg-promote");
    let fake = Fake::new(&d);
    let (one, two) = (
        pkg_release(&d, "one", "1.2.3"),
        pkg_release(&d, "two", "1.2.4"),
    );
    // Nothing uploaded yet: the bucket does not hold the files the pointer would name.
    let o = pkg(&d, &fake, "promote", &one, "v1.2.3", &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(&format!(
            "pkg/hostkeeper/1.2.3/{INSTALLER} could not be read back"
        )),
        "{}",
        stderr(&o)
    );
    assert!(!fake.has("pkg/hostkeeper/stable.json"));

    for (release, tag) in [(&one, "v1.2.3"), (&two, "v1.2.4")] {
        let o = pkg(&d, &fake, "upload", release, tag, &[]);
        assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    }
    fake.clear_calls();
    let o = pkg(
        &d,
        &fake,
        "promote",
        &one,
        "v1.2.3",
        &["--distribution-id", "E1ABC"],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("pkg/hostkeeper/stable.json: now 1.2.3\n"),
        "{}",
        stdout(&o)
    );
    assert!(
        stdout(&o).contains("/pkg/hostkeeper/stable.json: invalidated\n"),
        "{}",
        stdout(&o)
    );
    let first = fake.object("pkg/hostkeeper/stable.json");
    let pointer: serde_json::Value = serde_json::from_slice(&first).expect("json");
    assert_eq!(pointer["version"], "1.2.3");
    assert_eq!(pointer["tag"], "v1.2.3");
    assert_eq!(pointer["commit"], COMMIT);
    assert!(pointer["files"][INSTALLER]["sha256"].is_string());
    let puts = fake.puts();
    assert_eq!(
        puts,
        [(
            "pkg/hostkeeper/stable.json".to_owned(),
            vec!["--if-none-match".to_owned(), "*".to_owned()]
        )]
    );
    assert!(fake.calls().iter().any(|c| c.join(" ") == "cloudfront create-invalidation --distribution-id E1ABC --paths /pkg/hostkeeper/stable.json"));

    // The same release again changes nothing; a higher one replaces the pointer it read.
    let o = pkg(&d, &fake, "promote", &one, "v1.2.3", &[]);
    assert!(
        stdout(&o).contains("pkg/hostkeeper/stable.json: already at 1.2.3\n"),
        "{}",
        stdout(&o)
    );
    fake.clear_calls();
    let o = pkg(&d, &fake, "promote", &two, "v1.2.4", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        fake.puts(),
        [(
            "pkg/hostkeeper/stable.json".to_owned(),
            vec!["--if-match".to_owned(), etag(&first)]
        )]
    );
    // An older one is left alone.
    fake.clear_calls();
    let o = pkg(&d, &fake, "promote", &one, "v1.2.3", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o)
            .contains("pkg/hostkeeper/stable.json: at 1.2.4, newer than 1.2.3: left as it is\n"),
        "{}",
        stdout(&o)
    );
    assert!(fake.puts().is_empty());
}

#[test]
fn pkg_check_holds_the_files_the_public_host_serves_to_the_releases_sums() {
    let d = dir("pkg-check");
    let fake = Fake::new(&d);
    let release = pkg_release(&d, "release", "1.2.3");
    let o = pkg(&d, &fake, "upload", &release, "v1.2.3", &[]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let prefix = "pkg/hostkeeper/1.2.3/";
    let sums = release.join("SHA256SUMS");
    let work = d.join("work-check");
    let url = format!("https://{HOST}");
    let check = |version: &str, url: &str| {
        xtask(
            &d,
            &[
                "channels",
                "pkg",
                "check",
                "--sums",
                text(&sums),
                "--version",
                version,
                "--url",
                url,
                "--work",
                text(&work),
                "--curl",
                text(&fake.program()),
            ],
        )
    };
    // Nothing served yet.
    let o = check("1.2.3", &url);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(&format!(
            "{url}/{prefix}{INSTALLER} could not be downloaded"
        )),
        "{}",
        stderr(&o)
    );
    for name in [
        INSTALLER.to_owned(),
        format!("{INSTALLER}.sigstore.json"),
        "SHA256SUMS".to_owned(),
    ] {
        fake.serve(
            &format!("{prefix}{name}"),
            &fake.object(&format!("{prefix}{name}")),
        );
    }
    let o = check("1.2.3", &url);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o).contains("as the release's SHA256SUMS lists; its bundle and the directory's SHA256SUMS agree; these are the release's signed bytes"),
        "{}",
        stdout(&o)
    );
    assert_eq!(
        std::fs::read(work.join(INSTALLER)).expect("read"),
        std::fs::read(release.join(INSTALLER)).expect("read")
    );
    // The host serves another installer.
    fake.serve(&format!("{prefix}{INSTALLER}"), b"another installer");
    let o = check("1.2.3", &url);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(", but the release's SHA256SUMS lists "),
        "{}",
        stderr(&o)
    );
    // Flags out of their form.
    let o = check("1.2", &url);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("--version \"1.2\" is not <major>.<minor>.<patch>"),
        "{}",
        stderr(&o)
    );
    let o = check("1.2.3", "http://packages.example.com");
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains(
            "--url \"http://packages.example.com\" is not an https URL without a trailing /"
        ),
        "{}",
        stderr(&o)
    );
}

#[test]
fn pkg_verbs_with_flags_that_do_not_fit_are_status_2_and_a_release_without_the_installer_is_status_1()
 {
    let d = dir("pkg-usage");
    let fake = Fake::new(&d);
    let release = pkg_release(&d, "release", "1.2.3");
    for (verb, extra) in [("upload", &[][..]), ("promote", &[][..])] {
        let o = pkg(&d, &fake, verb, &release, "1.2.3", extra);
        assert_eq!(o.status.code(), Some(2), "{verb}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("--tag \"1.2.3\" is not v<major>.<minor>.<patch>"),
            "{}",
            stderr(&o)
        );
        let o = xtask(
            &d,
            &[
                "channels",
                "pkg",
                verb,
                "--dir",
                text(&release),
                "--tag",
                "v1.2.3",
                "--commit",
                "abc",
                "--bucket",
                BUCKET,
                "--work",
                text(&d.join("w")),
            ],
        );
        assert_eq!(o.status.code(), Some(2), "{verb}: {}", stderr(&o));
        assert!(stderr(&o).contains("--commit \"abc\""), "{}", stderr(&o));
        let o = xtask(
            &d,
            &[
                "channels",
                "pkg",
                verb,
                "--dir",
                text(&release),
                "--tag",
                "v1.2.3",
                "--commit",
                COMMIT,
                "--bucket",
                "Bad_Bucket",
                "--work",
                text(&d.join("w")),
            ],
        );
        assert_eq!(o.status.code(), Some(2), "{verb}: {}", stderr(&o));
        assert!(
            stderr(&o).contains("--bucket \"Bad_Bucket\" is not a bucket name"),
            "{}",
            stderr(&o)
        );
    }
    let o = pkg(
        &d,
        &fake,
        "promote",
        &release,
        "v1.2.3",
        &["--attempts", "0"],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("--attempts is 0; it must be 1 or more"),
        "{}",
        stderr(&o)
    );
    let o = pkg(
        &d,
        &fake,
        "promote",
        &release,
        "v1.2.3",
        &["--distribution-id", "bad id"],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("--distribution-id \"bad id\" is not a distribution id"),
        "{}",
        stderr(&o)
    );
    // Another product's installer where ours should be, and sums that do not list ours.
    let theirs = d.join("theirs");
    std::fs::create_dir_all(&theirs).expect("create");
    std::fs::write(theirs.join("other-1.2.3-macos-arm64.pkg"), b"other").expect("write");
    std::fs::write(
        theirs.join("SHA256SUMS"),
        format!("{}  other-1.2.3-macos-arm64.pkg\n", sha256(b"other")),
    )
    .expect("write");
    let o = pkg(&d, &fake, "upload", &theirs, "v1.2.3", &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains(&format!("{INSTALLER} is not a file")),
        "{}",
        stderr(&o)
    );
    assert!(fake.calls().is_empty());
}
