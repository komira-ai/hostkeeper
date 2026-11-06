//! A new release staged, a release already in the channel, and one there in part.

use super::*;

#[test]
fn a_new_release_with_good_sums_and_attestations_is_staged_with_its_exact_bytes() {
    // Catches: assets staged with other bytes than the release's, a version staged before its
    // check, a work directory left in the channel, or another repository's release checked
    // against hostkeeper's signer.
    let c = channel("sync-new");
    let source = FakeSource::default();
    let assets = release_of("tierstore", TS_V, "a");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    let verifier = FakeVerifier::attests_as_asked();

    let lines = sync(&source, &verifier, &request(&c, &[TIERSTORE])).expect("synced");

    assert_eq!(
        snapshot(&c.dir),
        staged("tierstore", TS_V, &assets, "index.json")
    );
    assert_eq!(
        lines,
        vec![
            format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}"),
            "channel-sync: 1 versions staged, 0 already in the channel, 0 refused".to_owned(),
        ]
    );
    // Online and from its bundle, each of the three packages and the tarball.
    assert_eq!(verifier.calls.borrow().len(), 8);
    assert!(
        verifier
            .signers
            .borrow()
            .iter()
            .all(|s| s == TIERSTORE_SIGNER)
    );
}

#[test]
fn a_hostkeeper_release_is_checked_by_release_check_and_each_of_its_services_staged() {
    // Catches: a hostkeeper release staged without `release check`, or only one of the three
    // service versions its indexes name staged.
    let c = channel("sync-hostkeeper");
    let version: Version = "0.1.36".parse().expect("a version");
    let assets = hostkeeper_release(&version);
    let source = FakeSource::default();
    publish(&source, REPO, "v0.1.36", 4, &assets);
    let verifier = FakeVerifier::attests_as_asked();

    let lines = sync(&source, &verifier, &request(&c, &[REPO])).expect("synced");

    let mut want = staged("hostkeeper", "0.1.36", &assets, "index.json");
    want.extend(staged("hello", "0.1.36", &assets, "hello-index.json"));
    want.extend(staged(
        "hello",
        "0.1.36-fail-ready",
        &assets,
        "hello-fail-ready-index.json",
    ));
    assert_eq!(snapshot(&c.dir), want);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(
        verifier
            .signers
            .borrow()
            .iter()
            .all(|s| s == SIGNER_WORKFLOW)
    );
    // Every attested asset of the release, as `release check` verifies them.
    let attested = crate::release::contract::attested_names(&version).len();
    assert_eq!(verifier.calls.borrow().len(), 2 * attested);
}

#[test]
fn a_hostkeeper_release_that_fails_release_check_is_not_staged() {
    // Catches: release check's refusal ignored for hostkeeper's own releases.
    let c = channel("sync-hostkeeper-bad");
    let version: Version = "0.1.36".parse().expect("a version");
    let mut assets = hostkeeper_release(&version);
    assets.remove("hostkeeper-0.1.36-linux-x86_64.cdx.json");
    let source = FakeSource::default();
    publish(&source, REPO, "v0.1.36", 4, &assets);

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[REPO]),
    ));

    assert!(snapshot(&c.dir).is_empty());
    assert!(
        lines[0].starts_with(&format!("refused {REPO} v0.1.36: ")),
        "{lines:?}"
    );
    assert!(lines[0].contains("missing hostkeeper-0.1.36-linux-x86_64.cdx.json"));
}

#[test]
fn syncing_twice_leaves_the_channel_byte_identical_and_the_second_run_stages_nothing() {
    // Catches: a second run that rewrites, adds or removes a file, or downloads a release that
    // is already in the channel beyond its index.
    let c = channel("sync-twice");
    let source = FakeSource::default();
    publish(
        &source,
        TIERSTORE,
        "v0.3.53",
        1,
        &release_of("tierstore", TS_V, "a"),
    );
    let verifier = FakeVerifier::attests_as_asked();
    let req = request(&c, &[TIERSTORE]);
    sync(&source, &verifier, &req).expect("first run");
    let before = snapshot(&c.dir);
    source.downloads.borrow_mut().clear();

    let lines = sync(&source, &verifier, &req).expect("second run");

    assert_eq!(snapshot(&c.dir), before);
    assert_eq!(
        lines,
        vec![
            format!("unchanged {TIERSTORE} v0.3.53: tierstore {TS_V}"),
            "channel-sync: 0 versions staged, 1 already in the channel, 0 refused".to_owned(),
        ]
    );
    assert_eq!(
        *source.downloads.borrow(),
        vec![format!("{TIERSTORE} v0.3.53 index.json")]
    );
}

#[test]
fn a_version_in_the_channel_with_another_index_is_refused_left_alone_and_the_others_staged() {
    // Catches: a version in the channel overwritten by a rebuilt release, or one refusal that
    // stops the run before the other new releases are staged.
    let c = channel("sync-other-index");
    let source = FakeSource::default();
    let old = "0.3.52+7e5f9df025cc";
    publish(
        &source,
        TIERSTORE,
        "v0.3.52",
        1,
        &release_of("tierstore", old, "a"),
    );
    let new = release_of("tierstore", TS_V, "b");
    publish(&source, TIERSTORE, "v0.3.53", 2, &new);
    let dir = c.dir.join("tierstore").join(old);
    std::fs::create_dir_all(&dir).expect("make the version");
    std::fs::write(dir.join("index.json"), b"an index of other bytes").expect("write");

    let lines = refused_lines(sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    ));

    assert_eq!(
        std::fs::read(dir.join("index.json")).expect("read"),
        b"an index of other bytes"
    );
    let mut want = staged("tierstore", TS_V, &new, "index.json");
    want.insert(format!("tierstore/{old}/"), Vec::new());
    want.insert(
        format!("tierstore/{old}/index.json"),
        b"an index of other bytes".to_vec(),
    );
    assert_eq!(snapshot(&c.dir), want);
    assert_eq!(
        lines,
        vec![
            format!(
                "refused {TIERSTORE} v0.3.52: tierstore {old}: {} holds another index than the \
                 release's; a release in the channel never changes, so it is left as it is",
                dir.join("index.json").display()
            ),
            format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}"),
            "channel-sync: 1 versions staged, 0 already in the channel, 1 refused".to_owned(),
        ]
    );
}

#[test]
fn a_package_in_the_channel_with_other_bytes_is_refused_and_left_alone() {
    // Catches: a staged package of other bytes (the index the same) replaced by the release's.
    let c = channel("sync-other-package");
    let source = FakeSource::default();
    let assets = release_of("tierstore", TS_V, "a");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    let verifier = FakeVerifier::attests_as_asked();
    let req = request(&c, &[TIERSTORE]);
    sync(&source, &verifier, &req).expect("first run");
    let package = c
        .dir
        .join("tierstore")
        .join(TS_V)
        .join(format!("tierstore-{TS_V}-linux-arm64.tar.zst"));
    std::fs::remove_file(&package).expect("remove the staged package");
    std::fs::write(&package, b"a rebuilt package").expect("write other bytes");
    let before = snapshot(&c.dir);

    let lines = refused_lines(sync(&source, &verifier, &req));

    assert_eq!(snapshot(&c.dir), before);
    assert!(
        lines[0].starts_with(&format!("refused {TIERSTORE} v0.3.53: tierstore {TS_V}: ")),
        "{lines:?}"
    );
    assert!(
        lines[0].contains("already staged with other bytes"),
        "{lines:?}"
    );
}

#[test]
fn a_version_cut_short_in_the_channel_is_finished_in_place() {
    // Catches: a version left half staged (a package without its index) refused or left so.
    let c = channel("sync-unfinished");
    let source = FakeSource::default();
    let assets = release_of("tierstore", TS_V, "a");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    let dir = c.dir.join("tierstore").join(TS_V);
    std::fs::create_dir_all(&dir).expect("make the version");
    let name = format!("tierstore-{TS_V}-macos-arm64.tar.zst");
    std::fs::write(dir.join(&name), &assets[&name]).expect("write a package");

    sync(
        &source,
        &FakeVerifier::attests_as_asked(),
        &request(&c, &[TIERSTORE]),
    )
    .expect("synced");

    assert_eq!(
        snapshot(&c.dir),
        staged("tierstore", TS_V, &assets, "index.json")
    );
}

#[test]
fn a_staged_index_with_a_missing_bundle_is_finished_in_place() {
    // Catches: a version whose index is there taken as whole while a bundle it needs is not.
    let c = channel("sync-missing-bundle");
    let source = FakeSource::default();
    let assets = release_of("tierstore", TS_V, "a");
    publish(&source, TIERSTORE, "v0.3.53", 1, &assets);
    let verifier = FakeVerifier::attests_as_asked();
    let req = request(&c, &[TIERSTORE]);
    sync(&source, &verifier, &req).expect("first run");
    let bundle = format!("tierstore-{TS_V}-linux-x86_64.tar.zst.sigstore.json");
    std::fs::remove_file(c.dir.join("tierstore").join(TS_V).join(&bundle)).expect("remove");

    let lines = sync(&source, &verifier, &req).expect("second run");

    assert_eq!(
        snapshot(&c.dir),
        staged("tierstore", TS_V, &assets, "index.json")
    );
    assert_eq!(
        lines[0],
        format!("staged {TIERSTORE} v0.3.53: tierstore {TS_V}")
    );
}
