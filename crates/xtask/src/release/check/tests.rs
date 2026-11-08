use super::*;
use crate::release::fakes::{
    FakeHistory, FakeVerifier, Fakes, good_attestation, name_of, nth_commit, scratch, write_assets,
};
use crate::testdir::TestDir;

const LOCAL_TAR: &str = "hostkeeper-0.1.5-linux-x86_64.tar.gz";
const CDX: &str = "hostkeeper-coordinator-0.1.5-linux-arm64.cdx.json";
const PACKAGE: &str = "hostkeeper-0.1.5-linux-x86_64.tar.zst";
const HELLO_PACKAGE: &str = "hello-0.1.5-linux-x86_64.tar.zst";
const HELLO_INDEX: &str = "hello-index.json";
const FAIL_READY_PACKAGE: &str = "hello-0.1.5-fail-ready-linux-x86_64.tar.zst";
const FAIL_READY_INDEX: &str = "hello-fail-ready-index.json";

fn version() -> Version {
    "0.1.5".parse().expect("a version")
}

/// A directory holding every asset of the release, and the commit it is pinned to.
fn assets(name: &str) -> (TestDir, Commit) {
    let dir = scratch(name);
    write_assets(&dir, &version(), "x");
    (dir, nth_commit(7))
}

fn run(verifier: &FakeVerifier, dir: &Path, commit: &Commit) -> Result<Vec<String>, Error> {
    check(verifier, dir, &version(), commit)
}

/// A verifier that attests every file well but answers `change` for the file named `target`.
fn changing(
    target: &str,
    commit: &Commit,
    change: fn(Attestation) -> Vec<Attestation>,
) -> FakeVerifier {
    let target = target.to_owned();
    let commit = commit.to_string();
    FakeVerifier::with(Box::new(move |file, _| {
        let good = good_attestation(file, &commit);
        Ok(if name_of(file) == target {
            change(good)
        } else {
            vec![good]
        })
    }))
}

fn refused(result: Result<Vec<String>, Error>) -> Vec<String> {
    match result {
        Err(Error::Refused(lines)) => lines,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_good_directory_passes_with_two_verifier_calls_per_attested_asset() {
    // Catches: a check that skips the online or the offline form of an asset, or counts wrongly.
    let (dir, commit) = assets("check-good");
    let verifier = FakeVerifier::good(&commit);
    assert_eq!(
        run(&verifier, &dir, &commit),
        Ok(vec![format!(
            "release check: the 78 assets of v0.1.5 match SHA256SUMS, and 56 attestations of \
             commit {commit} verified online and from their bundles"
        )])
    );
    let calls = verifier.calls.borrow();
    assert_eq!(calls.len(), 56);
    for name in attested_names(&version()) {
        assert!(calls.contains(&format!("online {name}")), "{name}");
        assert!(
            calls.contains(&format!("offline {name} {name}.sigstore.json")),
            "{name}"
        );
    }
}

#[test]
fn a_missing_asset_and_an_extra_file_are_listed() {
    // Catches: a check that ignores a missing asset or a stray file in the directory.
    let (dir, commit) = assets("check-names");
    std::fs::remove_file(dir.join(CDX)).expect("remove");
    std::fs::write(dir.join("notes.txt"), "x").expect("write");
    std::fs::create_dir(dir.join("sub")).expect("mkdir");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![
            format!("missing {CDX}"),
            "notes.txt is not an asset of v0.1.5".to_owned()
        ]
    );
}

#[test]
fn later_steps_skip_a_file_that_is_missing() {
    // Catches: a check that fails with a read error, or reports a second problem, for a file it
    // already reported missing.
    let (dir, commit) = assets("check-skip");
    std::fs::remove_file(dir.join(PACKAGE)).expect("remove");
    std::fs::remove_file(dir.join(format!("{PACKAGE}.sha256"))).expect("remove");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![
            format!("missing {PACKAGE}"),
            format!("missing {PACKAGE}.sha256")
        ]
    );
}

#[test]
fn a_directory_that_cannot_be_read_is_an_io_error() {
    // Catches: a check that treats an unreadable directory as an empty one.
    let (dir, commit) = assets("check-unreadable");
    let missing = dir.join("absent");
    let why = std::fs::read_dir(&missing)
        .map(|_| ())
        .expect_err("absent")
        .to_string();
    assert_eq!(
        run(&FakeVerifier::good(&commit), &missing, &commit),
        Err(Error::Io {
            what: format!("read {}", missing.display()),
            why
        })
    );
}

#[test]
fn malformed_sums_lines_are_listed_with_their_numbers() {
    // Catches: a parser that accepts a line without two spaces, an upper-case digest or an empty
    // name.
    let (dir, commit) = assets("check-sums-malformed");
    let sums = dir.join(SUMS);
    let mut text = std::fs::read_to_string(&sums).expect("read");
    let first = text.lines().count() + 1;
    text.push_str("garbage\n");
    text.push_str(&format!("{}  {CDX}\n", "A".repeat(64)));
    text.push_str(&format!("{}  \n", "a".repeat(64)));
    std::fs::write(&sums, text).expect("write");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        (first..first + 3)
            .map(|n| format!("SHA256SUMS line {n} is not '<sha256>  <name>'"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn sums_that_miss_or_add_a_name_are_listed() {
    // Catches: a check that accepts a SHA256SUMS missing an asset or listing a foreign file.
    let (dir, commit) = assets("check-sums-names");
    let sums = dir.join(SUMS);
    let text = std::fs::read_to_string(&sums).expect("read");
    let mut kept: String = text
        .lines()
        .filter(|l| !l.ends_with(CDX))
        .map(|l| format!("{l}\n"))
        .collect();
    kept.push_str(&format!("{}  stray.txt\n", "b".repeat(64)));
    std::fs::write(&sums, kept).expect("write");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![
            format!("SHA256SUMS does not list {CDX}"),
            "SHA256SUMS lists stray.txt, which is not a summed asset of v0.1.5".to_owned()
        ]
    );
}

#[test]
fn an_asset_changed_after_the_sums_were_written_is_listed_with_both_digests() {
    // Catches: a check that trusts SHA256SUMS without hashing the files.
    let (dir, commit) = assets("check-sums-digest");
    let listed = file_sha256(&dir.join(CDX)).expect("hash");
    std::fs::write(dir.join(CDX), "changed").expect("write");
    let actual = file_sha256(&dir.join(CDX)).expect("hash");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![format!(
            "{CDX}: its SHA-256 is {actual}, SHA256SUMS says {listed}"
        )]
    );
}

#[test]
fn a_package_digest_file_that_is_not_exact_is_listed() {
    // Catches: a check that does not compare the .sha256 file with the package's digest.
    let (dir, commit) = assets("check-digest-file");
    let name = format!("{PACKAGE}.sha256");
    let listed = file_sha256(&dir.join(&name)).expect("hash");
    std::fs::write(dir.join(&name), "deadbeef\n").expect("write");
    let actual = file_sha256(&dir.join(&name)).expect("hash");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![
            format!("{name}: its SHA-256 is {actual}, SHA256SUMS says {listed}"),
            format!("{name} is not '<sha256 of {PACKAGE}>  {PACKAGE}' and one line break")
        ]
    );
}

#[test]
fn verify_fails_on_a_wrong_checksum_and_on_a_wrong_signer_workflow() {
    // Catches: a check that stops at the first problem, or accepts an attestation of another
    // signer workflow.
    let (dir, commit) = assets("check-checksum-and-signer");
    let listed = file_sha256(&dir.join(CDX)).expect("hash");
    std::fs::write(dir.join(CDX), "changed").expect("write");
    let actual = file_sha256(&dir.join(CDX)).expect("hash");
    let target = LOCAL_TAR.to_owned();
    let wanted = commit.to_string();
    let verifier = FakeVerifier::with(Box::new(move |file, bundle| {
        let mut a = good_attestation(file, &wanted);
        if bundle.is_some() & (name_of(file) == target) {
            a.signer_workflow = "other-owner/other-repo/.github/workflows/other.yml".to_owned();
        }
        Ok(vec![a])
    }));
    assert_eq!(
        run(&verifier, &dir, &commit),
        Err(Error::Refused(vec![
            format!("{CDX}: its SHA-256 is {actual}, SHA256SUMS says {listed}"),
            format!(
                "{LOCAL_TAR} (offline): signed by other-owner/other-repo/.github/workflows/other.yml, \
                 not komira-ai/hostkeeper/.github/workflows/artifacts.yml"
            ),
        ]))
    );
}

#[test]
fn verify_fails_when_the_attested_commit_differs_from_the_pin() {
    // Catches: a check that accepts an attestation of a commit other than the pinned one.
    let (dir, commit) = assets("check-commit");
    let other = nth_commit(8);
    let verifier = changing(LOCAL_TAR, &commit, |mut a| {
        a.source_commit = nth_commit(8).to_string();
        vec![a]
    });
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            format!("{LOCAL_TAR} (online): attests the commit {other}, not the pin {commit}"),
            format!("{LOCAL_TAR} (offline): attests the commit {other}, not the pin {commit}"),
        ]
    );
    let everything = refused(run(&FakeVerifier::good(&other), &dir, &commit));
    assert_eq!(everything.len(), 56);
}

#[test]
fn an_attestation_of_another_ref_is_listed() {
    // Catches: a check that does not compare the ref of the signing run.
    let (dir, commit) = assets("check-ref");
    let verifier = changing(LOCAL_TAR, &commit, |mut a| {
        a.source_ref = "refs/heads/dev".to_owned();
        vec![a]
    });
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            format!("{LOCAL_TAR} (online): attests the ref refs/heads/dev, not refs/heads/main"),
            format!("{LOCAL_TAR} (offline): attests the ref refs/heads/dev, not refs/heads/main"),
        ]
    );
}

#[test]
fn a_subject_that_is_not_the_file_is_listed_and_every_attestation_counts() {
    // Catches: a check that looks only at the first attestation, or not at the subjects.
    let (dir, commit) = assets("check-subject");
    let digest = file_sha256(&dir.join(LOCAL_TAR)).expect("hash");
    let verifier = changing(LOCAL_TAR, &commit, |good| {
        let mut bad = good.clone();
        bad.subjects = vec!["c".repeat(64)];
        vec![good, bad]
    });
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            format!("{LOCAL_TAR} (online): does not name the file's SHA-256 {digest}"),
            format!("{LOCAL_TAR} (offline): does not name the file's SHA-256 {digest}"),
        ]
    );
}

#[test]
fn an_empty_attestation_list_is_listed() {
    // Catches: a check that takes no attestation at all for a pass.
    let (dir, commit) = assets("check-empty");
    let verifier = changing(LOCAL_TAR, &commit, |_| Vec::new());
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            format!("{LOCAL_TAR} (online): no attestation"),
            format!("{LOCAL_TAR} (offline): no attestation"),
        ]
    );
}

#[test]
fn a_verifier_error_is_listed_for_every_file() {
    // Catches: a check that lets a verifier failure pass, or aborts on the first one.
    let (dir, commit) = assets("check-verifier-error");
    let fakes = Fakes::over(FakeHistory::default());
    let lines = refused(run(&fakes.verifier, &dir, &commit));
    let names = attested_names(&version());
    let first = &names[0];
    assert_eq!(lines.len(), 56);
    assert_eq!(
        lines[0],
        format!("{first} (online): verify: no attestation")
    );
    assert_eq!(
        lines[1],
        format!("{first} (offline): verify: no attestation")
    );
}

/// Writes `text` as the release index of `package` in `dir` and rewrites `SHA256SUMS` to match,
/// so only the index check can object.
fn replace_index_of(dir: &Path, package: Package, text: &str) {
    std::fs::write(dir.join(package.index_name()), text).expect("write the index");
    let mut sums = String::new();
    for name in summed_names(&version()) {
        let digest = file_sha256(&dir.join(&name)).expect("hash");
        sums.push_str(&format!("{digest}  {name}\n"));
    }
    std::fs::write(dir.join(SUMS), sums).expect("write the sums");
}

/// Writes `text` as `index.json` (see [`replace_index_of`]).
fn replace_index(dir: &Path, text: &str) {
    replace_index_of(dir, Package::Hostkeeper, text);
}

/// The index of `package` `write_assets` wrote.
fn good_index_of(dir: &Path, package: Package) -> ReleaseIndex {
    ReleaseIndex::of_dir(
        dir,
        package.service().name(),
        &package.version_text(&version()),
        package.platforms(),
    )
    .expect("index")
}

/// The hostkeeper index `write_assets` wrote.
fn good_index(dir: &Path) -> ReleaseIndex {
    good_index_of(dir, Package::Hostkeeper)
}

#[test]
fn an_index_that_is_not_the_release_index_is_listed_with_why() {
    // Catches: an index of another version, one that is not JSON, or one with a field the
    // daemon's reader refuses, passing the check.
    let (dir, commit) = assets("check-index-invalid");
    let verifier = FakeVerifier::good(&commit);
    let other = good_index(&dir)
        .to_json()
        .replace("\"version\":\"0.1.5\"", "\"version\":\"0.1.4\"");
    replace_index(&dir, &other);
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            "index.json: index_invalid: the index is of hostkeeper 0.1.4, not hostkeeper 0.1.5"
                .to_owned()
        ]
    );
}

#[test]
fn the_mdm_index_is_held_to_its_one_package_and_to_its_service() {
    // Catches: an mdm index that names a linux-arm64 package (which no release builds), or one
    // of another service, passing the check because only hostkeeper's and hello's indexes are
    // read.
    let (dir, commit) = assets("check-index-mdm");
    let verifier = FakeVerifier::good(&commit);
    let good = good_index_of(&dir, Package::Mdm);
    assert_eq!(good.packages.len(), 1);
    let mut wider = good.clone();
    let mut arm = wider.packages[0].clone();
    arm.platform = Platform::LinuxArm64;
    arm.file = "mdm-0.1.5-linux-arm64.tar.zst".to_owned();
    wider.packages.insert(0, arm);
    replace_index_of(&dir, Package::Mdm, &wider.to_json());
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            "mdm-index.json does not list exactly the packages of the directory with their sizes \
             and SHA-256 digests"
                .to_owned()
        ]
    );
    let hello = good
        .to_json()
        .replace("\"service\":\"mdm\"", "\"service\":\"hello\"");
    replace_index_of(&dir, Package::Mdm, &hello);
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            "mdm-index.json: index_invalid: the index is of hello 0.1.5, not mdm 0.1.5".to_owned()
        ]
    );
}

#[test]
fn an_index_with_another_size_or_digest_is_listed() {
    // Catches: an index whose entry is not the package beside it (another size or SHA-256), which
    // would make every host refuse the package with package_digest_mismatch.
    let (dir, commit) = assets("check-index-other-digest");
    let verifier = FakeVerifier::good(&commit);
    let mut index = good_index(&dir);
    index.packages[0].bytes += 1;
    replace_index(&dir, &index.to_json());
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            "index.json does not list exactly the packages of the directory with their sizes and \
             SHA-256 digests"
                .to_owned()
        ]
    );
}

#[test]
fn an_index_that_is_not_written_canonically_is_listed() {
    // Catches: an index whose bytes differ from the canonical form, so two builds of one release
    // would publish different index files.
    let (dir, commit) = assets("check-index-spaced");
    let verifier = FakeVerifier::good(&commit);
    let spaced = good_index(&dir)
        .to_json()
        .replace(",\"schema\"", ", \"schema\"");
    replace_index(&dir, &spaced);
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec!["index.json is not written canonically".to_owned()]
    );
}

#[test]
fn a_missing_index_is_listed_once_and_not_read() {
    // Catches: a check that reads an index that is not there, or that does not report it.
    let (dir, commit) = assets("check-index-missing");
    let verifier = FakeVerifier::good(&commit);
    std::fs::remove_file(dir.join("index.json")).expect("remove");
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec!["missing index.json".to_owned()]
    );
}

#[test]
fn a_missing_hello_package_digest_file_or_bundle_is_listed_and_later_steps_skip_it() {
    // Catches: a check that does not hold the hello packages to the release's asset list, or
    // reads a hello package it already reported missing.
    let (dir, commit) = assets("check-hello-missing");
    std::fs::remove_file(dir.join(HELLO_PACKAGE)).expect("remove");
    std::fs::remove_file(dir.join(format!("{HELLO_PACKAGE}.sha256"))).expect("remove");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![
            format!("missing {HELLO_PACKAGE}"),
            format!("missing {HELLO_PACKAGE}.sha256")
        ]
    );
    let (dir, commit) = assets("check-hello-no-bundle");
    let bundle = bundle_of("hello-0.1.5-linux-arm64.tar.zst");
    std::fs::remove_file(dir.join(&bundle)).expect("remove");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![format!("missing {bundle}")]
    );
}

#[test]
fn the_hello_packages_are_verified_online_and_from_their_bundles() {
    // Catches: a check that leaves the hello packages out of the attestations it verifies, or
    // accepts a hello package attested for another commit.
    let (dir, commit) = assets("check-hello-attested");
    let verifier = FakeVerifier::good(&commit);
    run(&verifier, &dir, &commit).expect("a good directory");
    let calls = verifier.calls.borrow();
    for platform in ["linux-x86_64", "linux-arm64", "macos-arm64"] {
        let name = format!("hello-0.1.5-{platform}.tar.zst");
        assert!(calls.contains(&format!("online {name}")), "{name}");
        assert!(
            calls.contains(&format!("offline {name} {name}.sigstore.json")),
            "{name}"
        );
    }
    let other = nth_commit(8);
    let verifier = changing(HELLO_PACKAGE, &commit, |mut a| {
        a.source_commit = nth_commit(8).to_string();
        vec![a]
    });
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            format!("{HELLO_PACKAGE} (online): attests the commit {other}, not the pin {commit}"),
            format!("{HELLO_PACKAGE} (offline): attests the commit {other}, not the pin {commit}"),
        ]
    );
}

#[test]
fn the_hello_index_is_checked_as_the_index_of_hello_and_named_in_its_problems() {
    // Catches: a hello index that is the hostkeeper one under another name, one that is of
    // another version, lists a package with another digest, or is not written canonically.
    let hello = |dir: &Path| good_index_of(dir, Package::Hello);
    let (dir, commit) = assets("check-hello-index-other-service");
    let verifier = FakeVerifier::good(&commit);
    replace_index_of(&dir, Package::Hello, &good_index(&dir).to_json());
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![format!(
            "{HELLO_INDEX}: index_invalid: the index is of hostkeeper 0.1.5, not hello 0.1.5"
        )]
    );

    let (dir, commit) = assets("check-hello-index-other-digest");
    let verifier = FakeVerifier::good(&commit);
    let mut index = hello(&dir);
    index.packages[1].bytes += 1;
    replace_index_of(&dir, Package::Hello, &index.to_json());
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![format!(
            "{HELLO_INDEX} does not list exactly the packages of the directory with their sizes \
             and SHA-256 digests"
        )]
    );

    let (dir, commit) = assets("check-hello-index-spaced");
    let verifier = FakeVerifier::good(&commit);
    let spaced = hello(&dir).to_json().replace(",\"schema\"", ", \"schema\"");
    replace_index_of(&dir, Package::Hello, &spaced);
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![format!("{HELLO_INDEX} is not written canonically")]
    );

    let (dir, commit) = assets("check-hello-index-missing");
    std::fs::remove_file(dir.join(HELLO_INDEX)).expect("remove");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![format!("missing {HELLO_INDEX}")]
    );
}

#[test]
fn the_hostkeeper_index_does_not_stand_for_the_hello_index() {
    // Catches: each index checked against the other service's packages, so that a good release
    // fails or a swapped pair passes.
    let (dir, commit) = assets("check-index-swapped");
    let verifier = FakeVerifier::good(&commit);
    let hello = good_index_of(&dir, Package::Hello).to_json();
    let hostkeeper = good_index(&dir).to_json();
    replace_index_of(&dir, Package::Hello, &hostkeeper);
    replace_index_of(&dir, Package::Hostkeeper, &hello);
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            "index.json: index_invalid: the index is of hello 0.1.5, not hostkeeper 0.1.5"
                .to_owned(),
            format!(
                "{HELLO_INDEX}: index_invalid: the index is of hostkeeper 0.1.5, not hello 0.1.5"
            ),
        ]
    );
}

#[test]
fn a_missing_failing_build_package_digest_file_or_bundle_is_listed_and_later_steps_skip_it() {
    // Catches: a check that does not hold hello's failing build to the release's asset list, or
    // reads a package of it that it already reported missing.
    let (dir, commit) = assets("check-fail-ready-missing");
    std::fs::remove_file(dir.join(FAIL_READY_PACKAGE)).expect("remove");
    std::fs::remove_file(dir.join(format!("{FAIL_READY_PACKAGE}.sha256"))).expect("remove");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![
            format!("missing {FAIL_READY_PACKAGE}"),
            format!("missing {FAIL_READY_PACKAGE}.sha256")
        ]
    );
    let (dir, commit) = assets("check-fail-ready-no-bundle");
    let bundle = bundle_of("hello-0.1.5-fail-ready-linux-arm64.tar.zst");
    std::fs::remove_file(dir.join(&bundle)).expect("remove");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![format!("missing {bundle}")]
    );
}

#[test]
fn the_failing_build_packages_are_verified_online_and_from_their_bundles() {
    // Catches: a check that leaves the failing build out of the attestations it verifies (a host
    // trusts only attested packages, so an unattested one would be refused there), or accepts one
    // attested for another commit.
    let (dir, commit) = assets("check-fail-ready-attested");
    let verifier = FakeVerifier::good(&commit);
    run(&verifier, &dir, &commit).expect("a good directory");
    let calls = verifier.calls.borrow();
    for platform in ["linux-x86_64", "linux-arm64", "macos-arm64"] {
        let name = format!("hello-0.1.5-fail-ready-{platform}.tar.zst");
        assert!(calls.contains(&format!("online {name}")), "{name}");
        assert!(
            calls.contains(&format!("offline {name} {name}.sigstore.json")),
            "{name}"
        );
    }
    let other = nth_commit(8);
    let verifier = changing(FAIL_READY_PACKAGE, &commit, |mut a| {
        a.source_commit = nth_commit(8).to_string();
        vec![a]
    });
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![
            format!(
                "{FAIL_READY_PACKAGE} (online): attests the commit {other}, not the pin {commit}"
            ),
            format!(
                "{FAIL_READY_PACKAGE} (offline): attests the commit {other}, not the pin {commit}"
            ),
        ]
    );
}

#[test]
fn the_failing_build_index_is_the_index_of_hello_at_the_failing_version() {
    // Catches: a failing-build index that is hello's own index under another name (its version
    // and files are the plain package's), one that lists another digest, one not written
    // canonically, or one missing.
    let failing = |dir: &Path| good_index_of(dir, Package::HelloFailReady);
    let (dir, commit) = assets("check-fail-ready-index-plain");
    let verifier = FakeVerifier::good(&commit);
    assert_eq!(failing(&dir).version, "0.1.5-fail-ready");
    assert_eq!(failing(&dir).service, "hello");
    replace_index_of(
        &dir,
        Package::HelloFailReady,
        &good_index_of(&dir, Package::Hello).to_json(),
    );
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![format!(
            "{FAIL_READY_INDEX}: index_invalid: the index is of hello 0.1.5, not hello 0.1.5-fail-ready"
        )]
    );

    let (dir, commit) = assets("check-fail-ready-index-digest");
    let verifier = FakeVerifier::good(&commit);
    let mut index = failing(&dir);
    index.packages[0].bytes += 1;
    replace_index_of(&dir, Package::HelloFailReady, &index.to_json());
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![format!(
            "{FAIL_READY_INDEX} does not list exactly the packages of the directory with their \
             sizes and SHA-256 digests"
        )]
    );

    let (dir, commit) = assets("check-fail-ready-index-spaced");
    let verifier = FakeVerifier::good(&commit);
    let spaced = failing(&dir)
        .to_json()
        .replace(",\"schema\"", ", \"schema\"");
    replace_index_of(&dir, Package::HelloFailReady, &spaced);
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        vec![format!("{FAIL_READY_INDEX} is not written canonically")]
    );

    let (dir, commit) = assets("check-fail-ready-index-missing");
    std::fs::remove_file(dir.join(FAIL_READY_INDEX)).expect("remove");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![format!("missing {FAIL_READY_INDEX}")]
    );
}

#[test]
fn a_release_without_its_macos_assets_is_refused_naming_each() {
    // Catches: a release that ships the Linux platforms only, the macOS binary, package, SBOM,
    // installer and their bundles missing, passing the check as a complete release.
    let (dir, commit) = assets("check-no-macos");
    let mac: Vec<String> = asset_names(&version(), &[Platform::MacosArm64])
        .into_iter()
        .filter(|n| n.contains("macos-arm64"))
        .collect();
    assert_eq!(mac.len(), 14);
    for name in &mac {
        std::fs::remove_file(dir.join(name)).expect("remove a macOS asset");
    }
    let mut sums = String::new();
    for name in summed_names(&version()).iter().filter(|n| !mac.contains(n)) {
        let digest = file_sha256(&dir.join(name)).expect("hash");
        sums.push_str(&format!("{digest}  {name}\n"));
    }
    std::fs::write(dir.join(SUMS), sums).expect("write the sums");
    let mut want: Vec<String> = mac.iter().map(|n| format!("missing {n}")).collect();
    want.extend(
        mac.iter()
            .filter(|n| summed_names(&version()).contains(n))
            .map(|n| format!("{SUMS} does not list {n}")),
    );
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        want
    );
}

#[test]
fn an_index_that_leaves_out_the_macos_package_is_listed() {
    // Catches: an index of the Linux packages only beside a macOS package, so that a Mac finds
    // no package in the release index.
    let (dir, commit) = assets("check-index-no-macos");
    let mut index = good_index(&dir);
    assert_eq!(
        index
            .packages
            .iter()
            .map(|e| e.platform)
            .collect::<Vec<_>>(),
        Platform::ALL
    );
    index
        .packages
        .retain(|e| e.platform != Platform::MacosArm64);
    replace_index(&dir, &index.to_json());
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![
            "index.json does not list exactly the packages of the directory with their sizes and \
             SHA-256 digests"
                .to_owned()
        ]
    );
}

#[test]
fn an_hkctl_asset_that_is_not_the_releases_script_is_listed() {
    // Catches: `release check` passing a command line of another release or another commit, or a
    // file that is not the script at all, as `hkctl-<version>`.
    let (dir, commit) = assets("check-hkctl");
    let verifier = FakeVerifier::good(&commit);
    let name = crate::release::contract::hkctl_name(&version());
    let wrong = format!(
        "{name} does not hold exactly one version line `_hk_version='hkctl 0.1.5+{}'`",
        commit.short()
    );
    // Another release's, and this release's at another commit.
    for line in ["hkctl 0.1.4+000000000000", "hkctl 0.1.5+0123456789ab"] {
        std::fs::write(
            dir.join(&name),
            format!("#!/usr/bin/env bash\n_hk_version='{line}'\n"),
        )
        .expect("write");
        let lines = refused(run(&verifier, &dir, &commit));
        assert!(lines.contains(&wrong), "{lines:?}");
    }
    std::fs::write(dir.join(&name), "not a script\n").expect("write");
    let lines = refused(run(&verifier, &dir, &commit));
    assert!(
        lines.contains(&format!("{name} does not start with `#!/usr/bin/env bash`")),
        "{lines:?}"
    );
    // Absent, it is listed once, as missing, and no later step reads it.
    std::fs::remove_file(dir.join(&name)).expect("remove");
    assert_eq!(
        refused(run(&verifier, &dir, &commit)),
        [format!("missing {name}")]
    );
}
