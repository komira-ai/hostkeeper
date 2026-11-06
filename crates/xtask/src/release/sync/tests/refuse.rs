//! What is refused: a release that fails its check, a name or commit that is not safe to use,
//! an index that cannot be placed, and what in the channel is not a version.

use super::*;

/// Syncs a tierstore release whose assets `edit` changed after they were built, with
/// `verifier`; the lines of the refusal, after checking that nothing reached the channel.
fn refused_release(
    name: &str,
    edit: impl FnOnce(&mut Assets),
    verifier: &FakeVerifier,
) -> Vec<String> {
    let c = channel(name);
    let source = FakeSource::default();
    let mut assets = release_of("tierstore", TS_V, "a");
    edit(&mut assets);
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    let lines = refused_lines(sync(&source, verifier, &request(&c, &[TIERSTORE])));
    assert!(snapshot(&c.dir).is_empty(), "{:?}", snapshot(&c.dir));
    lines
}

#[test]
fn a_release_with_a_bad_sum_is_not_staged_and_is_reported() {
    // Catches: a package whose SHA-256 is not the one SHA256SUMS lists staged anyway.
    let package = format!("tierstore-{TS_V}-linux-arm64.tar.zst");
    let lines = refused_release(
        "sync-bad-sum",
        |assets| {
            let sums = String::from_utf8(assets[SUMS].clone()).expect("text");
            let digest = sha256(&assets[&package]);
            let bad = sums.replace(&digest, &"0".repeat(64));
            assets.insert(SUMS.to_owned(), bad.into_bytes());
        },
        &FakeVerifier::attests_as_asked(),
    );
    assert_eq!(
        lines[0],
        format!(
            "refused {TIERSTORE} v0.3.53: {package}: its SHA-256 is {}, SHA256SUMS says {}",
            sha256(release_of("tierstore", TS_V, "a")[&package].as_slice()),
            "0".repeat(64)
        )
    );
}

#[test]
fn a_release_with_a_missing_attestation_is_not_staged_and_is_reported() {
    // Catches: a package that has no verified attestation (online or in its bundle) staged.
    let commit = nth_commit(1);
    let verifier = FakeVerifier::with(Box::new(move |file, bundle| {
        if name_of(file).contains("macos-arm64.tar.zst") & bundle.is_some() {
            return Ok(Vec::new());
        }
        let mut good = good_attestation(file, commit.as_str());
        good.signer_workflow = TIERSTORE_SIGNER.to_owned();
        Ok(vec![good])
    }));
    let lines = refused_release("sync-no-attestation", |_| {}, &verifier);
    assert_eq!(
        lines[0],
        format!(
            "refused {TIERSTORE} v0.3.53: tierstore-{TS_V}-macos-arm64.tar.zst (offline): no \
             attestation"
        )
    );
}

#[test]
fn a_release_signed_by_another_workflow_is_not_staged() {
    // Catches: another repository's release taken on hostkeeper's (or any other) signature.
    let commit = nth_commit(1);
    let verifier = FakeVerifier::with(Box::new(move |file, _| {
        Ok(vec![good_attestation(file, commit.as_str())])
    }));
    let lines = refused_release("sync-other-signer", |_| {}, &verifier);
    assert!(
        lines[0].contains(&format!(
            "signed by {SIGNER_WORKFLOW}, not {TIERSTORE_SIGNER}"
        )),
        "{lines:?}"
    );
}

#[test]
fn the_sums_must_list_every_asset_and_only_the_assets_in_their_form() {
    // Catches: an asset missing from SHA256SUMS, a listed name the release lacks, or a malformed
    // line, taken as checked.
    let lines = refused_release(
        "sync-sums-form",
        |assets| {
            let sums = String::from_utf8(assets[SUMS].clone()).expect("text");
            let kept: String = sums
                .lines()
                .filter(|line| !line.ends_with(".tar.gz"))
                .map(|line| format!("{line}\n"))
                .collect();
            let text = format!("{kept}{}  ghost.bin\nnot a sums line\n", "1".repeat(64));
            assets.insert(SUMS.to_owned(), text.into_bytes());
        },
        &FakeVerifier::attests_as_asked(),
    );
    let tarball = format!("tierstore-{TS_V}-linux-x86_64.tar.gz");
    assert_eq!(
        lines[0],
        format!(
            "refused {TIERSTORE} v0.3.53: SHA256SUMS line 6 is not '<sha256>  <name>'\nSHA256SUMS \
             lists ghost.bin, which is not an asset of the release\nSHA256SUMS does not list \
             {tarball}"
        )
    );
}

#[test]
fn a_release_without_sums_is_not_staged() {
    // Catches: a release with no SHA256SUMS staged on its attestations alone.
    let lines = refused_release(
        "sync-no-sums",
        |assets| {
            assets.remove(SUMS);
        },
        &FakeVerifier::attests_as_asked(),
    );
    assert_eq!(
        lines[0],
        format!("refused {TIERSTORE} v0.3.53: missing SHA256SUMS")
    );
}

#[test]
fn a_release_channel_stage_refuses_is_not_staged() {
    // Catches: a version staged whose index lacks a platform the daemons need.
    let lines = refused_release(
        "sync-stage-refused",
        |assets| {
            let name = format!("tierstore-{TS_V}-macos-arm64.tar.zst");
            assets.remove(&name);
            assets.remove(&format!("{name}.sigstore.json"));
            resum(assets);
        },
        &FakeVerifier::attests_as_asked(),
    );
    assert!(
        lines[0].starts_with(&format!("refused {TIERSTORE} v0.3.53: tierstore {TS_V}: ")),
        "{lines:?}"
    );
    assert!(
        lines[0].contains("missing, but the index lists it"),
        "{lines:?}"
    );
}

#[test]
fn a_release_with_an_unsafe_asset_name_or_commit_is_refused_before_any_download() {
    // Catches: an asset name with a path separator or a glob character passed to the download,
    // or a release whose commit is a branch name checked against it.
    for (name, asset, commit, why) in [
        (
            "sync-slash",
            "../index.json",
            nth_commit(1).to_string(),
            "the asset name \"../index.json\" is not a plain file name",
        ),
        (
            "sync-glob",
            "index*.json",
            nth_commit(1).to_string(),
            "the asset name \"index*.json\" is not a plain file name",
        ),
        (
            "sync-branch",
            "x-index.json",
            "main".to_owned(),
            "its commit \"main\" is not a full commit id",
        ),
    ] {
        let c = channel(name);
        let source = FakeSource::default();
        let mut assets: Vec<(String, Vec<u8>)> =
            release_of("tierstore", TS_V, "a").into_iter().collect();
        assets.push((asset.to_owned(), b"{}".to_vec()));
        source.publish(TIERSTORE, "v0.3.53", &commit, assets);
        let lines = refused_lines(sync(
            &source,
            &FakeVerifier::attests_as_asked(),
            &request(&c, &[TIERSTORE]),
        ));
        assert_eq!(
            lines[0],
            format!("refused {TIERSTORE} v0.3.53: {why}"),
            "{name}"
        );
        assert!(source.downloads.borrow().is_empty(), "{name}");
        assert!(snapshot(&c.dir).is_empty(), "{name}");
    }
}

#[test]
fn an_index_that_names_no_service_and_version_is_refused() {
    // Catches: an index the sync cannot place staged at a guessed path, or a panic on it.
    let cases: [(&str, &[u8], &str); 4] = [
        ("sync-index-utf8", b"\xff\xfe", "not UTF-8"),
        (
            "sync-index-json",
            b"[1, 2]",
            "not JSON with a service and a version",
        ),
        (
            "sync-index-service",
            br#"{"service":".x","version":"1"}"#,
            "is not a service name",
        ),
        (
            "sync-index-version",
            br#"{"service":"x","version":"-1"}"#,
            "is not a version",
        ),
    ];
    for (name, index, why) in cases {
        let c = channel(name);
        let source = FakeSource::default();
        let mut assets = release_of("tierstore", TS_V, "a");
        assets.insert("index.json".to_owned(), index.to_vec());
        publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
        let lines = refused_lines(sync(
            &source,
            &FakeVerifier::attests_as_asked(),
            &request(&c, &[TIERSTORE]),
        ));
        let start = format!("refused {TIERSTORE} v0.3.53: index.json: index_invalid: ");
        assert!(
            lines[0].starts_with(&start) & lines[0].contains(why),
            "{name}: {lines:?}"
        );
        assert!(snapshot(&c.dir).is_empty(), "{name}");
    }
}

#[test]
fn what_is_not_a_version_directory_or_index_file_in_the_channel_is_refused_and_left() {
    // Catches: a file where a version's directory belongs, or a directory where its index
    // belongs, replaced or read as a version.
    for (name, make) in [
        ("sync-version-file", "file"),
        ("sync-index-dir", "index-dir"),
        ("sync-service-file", "service-file"),
    ] {
        let c = channel(name);
        let source = FakeSource::default();
        publish(
            &source,
            TIERSTORE,
            "v0.3.53",
            1,
            &release_of("tierstore", TS_V, "a"),
        );
        let dir = c.dir.join("tierstore").join(TS_V);
        let why = match make {
            "file" => {
                std::fs::create_dir(c.dir.join("tierstore")).expect("make");
                std::fs::write(&dir, b"x").expect("write");
                format!("tierstore {TS_V}: {} is not a directory", dir.display())
            }
            "index-dir" => {
                std::fs::create_dir_all(dir.join("index.json")).expect("make");
                format!(
                    "tierstore {TS_V}: {} is not a file",
                    dir.join("index.json").display()
                )
            }
            _ => {
                std::fs::write(c.dir.join("tierstore"), b"x").expect("write");
                format!("read {}: ", dir.display())
            }
        };
        let before = snapshot(&c.dir);
        let lines = refused_lines(sync(
            &source,
            &FakeVerifier::attests_as_asked(),
            &request(&c, &[TIERSTORE]),
        ));
        assert!(lines[0].contains(&why), "{name}: {lines:?}");
        assert_eq!(snapshot(&c.dir), before, "{name}");
    }
}

#[test]
fn two_indexes_of_one_version_in_a_release_stage_it_once_and_refuse_the_second() {
    // Catches: a release that names one version twice staged twice, or which index wins left
    // to the order of a rename.
    let c = channel("sync-two-indexes");
    let source = FakeSource::default();
    let mut assets = release_of("tierstore", TS_V, "a");
    let index = assets["index.json"].clone();
    assets.insert("tierstore-index.json".to_owned(), index);
    resum(&mut assets);
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    ));

    assert_eq!(
        snapshot(&c.dir),
        staged("tierstore", TS_V, &assets, "index.json")
    );
    assert_eq!(
        lines[..2],
        [
            format!(
                "refused {TIERSTORE} v0.3.53: tierstore {TS_V}: tierstore-index.json is a second \
                 index of it in the release, after index.json"
            ),
            format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}"),
        ]
    );
}

/// A source that makes `<channel>/tierstore/<TS_V>/` with a file in it when the first asset that
/// is not an index is downloaded: a version that appears after it was compared with the channel.
struct Racing<'a> {
    inner: &'a FakeSource,
    channel: PathBuf,
}

impl ReleaseSource for Racing<'_> {
    fn list(&self, repo: &Repo) -> Result<Vec<Release>, PortError> {
        self.inner.list(repo)
    }

    fn download(&self, repo: &Repo, tag: &str, name: &str, dir: &Path) -> Result<(), PortError> {
        let version = self.channel.join("tierstore").join(TS_V);
        if !is_index(name) & !version.exists() {
            std::fs::create_dir_all(&version).expect("make the version");
            std::fs::write(version.join("other"), b"another run's").expect("write");
        }
        self.inner.download(repo, tag, name, dir)
    }
}

#[test]
fn a_version_that_appears_while_the_release_is_staged_is_left_as_it_is() {
    // Catches: a staged version renamed over (or into) one made since it was compared with the
    // channel.
    let c = channel("sync-race");
    let source = FakeSource::default();
    publish(
        &source,
        TIERSTORE,
        "v0.3.53",
        1,
        &release_of("tierstore", TS_V, "a"),
    );
    let racing = Racing {
        inner: &source,
        channel: c.dir.clone(),
    };

    let lines = refused_lines(sync(
        &racing,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    ));

    let dir = c.dir.join("tierstore").join(TS_V);
    assert_eq!(
        lines[0],
        format!(
            "refused {TIERSTORE} v0.3.53: tierstore {TS_V}: {} appeared while the release was \
             staged; it is left as it is",
            dir.display()
        )
    );
    assert_eq!(
        tree_of(&c.dir),
        [
            "tierstore/".to_owned(),
            format!("tierstore/{TS_V}/"),
            format!("tierstore/{TS_V}/other")
        ]
    );
}

#[test]
fn an_invalid_index_already_in_the_channel_is_not_taken_as_a_staged_version() {
    // Catches: a channel index that is the release's bytes but not an index the daemon reads
    // reported as already in the channel.
    let c = channel("sync-invalid-staged");
    let source = FakeSource::default();
    let mut assets = release_of("tierstore", TS_V, "a");
    let invalid = format!(r#"{{"service":"tierstore","version":"{TS_V}"}}"#).into_bytes();
    assets.insert("index.json".to_owned(), invalid.clone());
    resum(&mut assets);
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    let dir = c.dir.join("tierstore").join(TS_V);
    std::fs::create_dir_all(&dir).expect("make the version");
    std::fs::write(dir.join("index.json"), &invalid).expect("write");
    let before = snapshot(&c.dir);

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    ));

    assert_eq!(snapshot(&c.dir), before);
    assert!(
        lines[0].starts_with(&format!("refused {TIERSTORE} v0.3.53: tierstore {TS_V}: ")),
        "{lines:?}"
    );
    assert!(lines[0].contains("index_invalid"), "{lines:?}");
}

#[test]
fn a_repository_whose_releases_cannot_be_read_is_reported_and_the_others_synced() {
    // Catches: one unreachable repository stopping the sync of the others, or passing as done.
    let c = channel("sync-unreadable");
    let source = FakeSource::default();
    let assets = release_of("tierstore", TS_V, "a");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[OTHER, TIERSTORE]),
    ));

    assert_eq!(
        lines[0],
        format!(
            "refused {OTHER}: its releases cannot be read: list the releases of {OTHER}: HTTP 404"
        )
    );
    assert_eq!(
        lines[1],
        format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}")
    );
    assert_eq!(
        snapshot(&c.dir),
        staged("tierstore", TS_V, &assets, "index.json")
    );
}

#[test]
fn an_attestation_answer_for_another_commit_is_refused() {
    // Catches: a package whose attestation names another source commit than the release's.
    let verifier = FakeVerifier::with(Box::new(|file, _| {
        Ok(vec![Attestation {
            signer_workflow: TIERSTORE_SIGNER.to_owned(),
            source_ref: "refs/heads/main".to_owned(),
            source_commit: nth_commit(9).to_string(),
            subjects: vec![file_sha256(file).expect("hash")],
        }])
    }));
    let lines = refused_release("sync-other-commit", |_| {}, &verifier);
    assert!(
        lines[0].contains(&format!("not the pin {}", nth_commit(1))),
        "{lines:?}"
    );
}

#[test]
fn a_release_naming_a_service_its_repository_does_not_ship_is_refused() {
    // Catches: one repository's release claiming another service's version in the channel
    // first, which the channel would then never let the real release replace.
    let c = channel("sync-foreign-service");
    let source = FakeSource::default();
    let mut assets = release_of("hostkeeper", "9.9.9", "foreign");
    let own = release_of("tierstore", TS_V, "a");
    assets.extend(
        own.iter()
            .map(|(n, b)| (n.replace("index.json", "tierstore-index.json"), b.clone())),
    );
    resum(&mut assets);
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    ));

    assert_eq!(
        lines[..2],
        [
            format!(
                "refused {TIERSTORE} v0.3.53: hostkeeper 9.9.9: index.json is the index of a \
                 service a release of {TIERSTORE} does not ship"
            ),
            format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}"),
        ]
    );
    assert!(!c.dir.join("hostkeeper").exists());
}

#[test]
fn a_repository_ships_the_service_it_is_named_for_and_hostkeeper_also_hello() {
    // Catches: hello refused in hostkeeper's release, or allowed in another repository's.
    let service = |s: &str| ServiceName::new(s).expect("a service");
    assert!(ships(&repo(TIERSTORE), &service("tierstore")));
    assert!(!ships(&repo(TIERSTORE), &service("hello")));
    assert!(ships(&repo(REPO), &service("hostkeeper")));
    assert!(ships(&repo(REPO), &service("hello")));
    assert!(!ships(&repo(REPO), &service("tierstore")));
}
