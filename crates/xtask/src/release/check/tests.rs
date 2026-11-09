use super::*;
use crate::release::fakes::{
    FakeHistory, FakeVerifier, Fakes, good_attestation, name_of, nth_commit, scratch, write_assets,
};
use crate::testdir::TestDir;

const LOCAL_TAR: &str = "hostkeeper-0.1.5-linux-x86_64.tar.gz";
const CDX: &str = "hostkeeper-coordinator-0.1.5-linux-arm64.cdx.json";
const DEB: &str = "hello_0.1.5_amd64.deb";

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
            "release check: the 58 assets of v0.1.5 match SHA256SUMS, and 50 attestations of \
             commit {commit} verified online and from their bundles"
        )])
    );
    let calls = verifier.calls.borrow();
    assert_eq!(calls.len(), 50);
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
    std::fs::remove_file(dir.join(DEB)).expect("remove");
    std::fs::remove_file(dir.join(format!("{DEB}.sigstore.json"))).expect("remove");
    assert_eq!(
        refused(run(&FakeVerifier::good(&commit), &dir, &commit)),
        vec![
            format!("missing {DEB}"),
            format!("missing {DEB}.sigstore.json")
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
    assert_eq!(everything.len(), 50);
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
    assert_eq!(lines.len(), 50);
    assert_eq!(
        lines[0],
        format!("{first} (online): verify: no attestation")
    );
    assert_eq!(
        lines[1],
        format!("{first} (offline): verify: no attestation")
    );
}

#[test]
fn a_release_without_its_macos_assets_is_refused_naming_each() {
    // Catches: a release that ships the Linux platforms only, the macOS binary, package, SBOM,
    // installer, hello's .pkg and their bundles missing, passing the check as a complete release.
    let (dir, commit) = assets("check-no-macos");
    let mac: Vec<String> = asset_names(&version(), &[Platform::MacosArm64])
        .into_iter()
        .filter(|n| n.contains("macos-arm64"))
        .collect();
    assert_eq!(mac.len(), 7);
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

#[test]
fn an_image_index_that_is_not_the_one_of_the_two_image_tarballs_is_listed() {
    // Catches: a release whose registry digest (the image index's SHA-256) names manifests other
    // than its two image tarballs', or a check that skips the image index.
    let (dir, commit) = assets("check-image-index");
    let name = "hostkeeper-coordinator-0.1.5.image.json";
    let good = std::fs::read(dir.join(name)).expect("read");
    std::fs::write(dir.join(name), b"{}").expect("write");
    let lines = refused(run(&FakeVerifier::good(&commit), &dir, &commit));
    let made = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&good))
    );
    let have = format!(
        "sha256:{}",
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"{}"))
    );
    assert!(
        lines.contains(&format!(
            "{name} is not the image index of the two image tarballs: it is {have}, they make {made}"
        )),
        "{lines:?}"
    );

    // Without its tarballs the index is not judged: their absence is listed instead.
    let (dir, commit) = assets("check-image-index-absent");
    std::fs::remove_file(dir.join("hostkeeper-coordinator-0.1.5-linux-arm64.image.tar"))
        .expect("remove");
    let lines = refused(run(&FakeVerifier::good(&commit), &dir, &commit));
    assert!(
        lines.contains(&"missing hostkeeper-coordinator-0.1.5-linux-arm64.image.tar".to_owned()),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("is not the image index")),
        "{lines:?}"
    );
}
