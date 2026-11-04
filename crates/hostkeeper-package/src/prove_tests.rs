use std::sync::Arc;

use hostkeeper_config::daemon::Bounds;
use hostkeeper_hook::{Class, Outcome};

use super::*;
use crate::fetch::{FetchEnv, PackageOrder, fetch_package};
use crate::ports::{Limit, NetError, Sink};
use crate::testing::{
    BundleSpec, FakeClock, FakeFetcher, FakeVerifier, Seen, Serve, bundle_json, config, deliver,
    full, listing, order, scratch, serve, sha256_hex, signers,
};

const ARCHIVE: &[u8] = b"the bytes of tierstore 1.5.0";
const ARCHIVE_URL: &str = "https://releases.example/tierstore/tierstore-1.5.0.tar.zst";
const BUNDLE_URL: &str = "https://releases.example/tierstore/tierstore-1.5.0.tar.zst.sigstore.json";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const REF: &str = "refs/heads/main";

/// A bundle that names `COMMIT` and `REF` and lists `sha256`.
fn bundle_for(sha256: &str, commit: &str) -> String {
    bundle_json(&BundleSpec {
        organisation: "example-org",
        repository: "tierstore",
        workflow: ".github/workflows/artifacts.yml",
        source_ref: REF,
        certificate_commit: commit,
        provenance_commit: commit,
        runner: "github-hosted",
        subject_name: "tierstore-1.5.0.tar.zst",
        subject_sha256: sha256,
    })
}

fn good_bundle() -> Vec<u8> {
    bundle_for(&sha256_hex(ARCHIVE), COMMIT).into_bytes()
}

fn bounds() -> Bounds {
    Bounds {
        attestation_timeout_secs: 60,
        ..Bounds::default()
    }
}

/// A fetcher that serves the archive and `bundle` as the bundle.
fn channel(bundle: Serve) -> FakeFetcher {
    FakeFetcher::new()
        .route(ARCHIVE_URL, serve(ARCHIVE))
        .route(BUNDLE_URL, bundle)
}

/// Fetches the archive of `order` into `root` with `fetcher`.
fn staged(
    fetcher: &FakeFetcher,
    clock: &FakeClock,
    root: &Path,
    order: &PackageOrder,
) -> StagedArchive {
    let channel = config("https://releases.example", &"0".repeat(64)).release_channel;
    let env = FetchEnv {
        fetcher,
        clock,
        bounds: &bounds(),
        release_channel: &channel,
        packages_root: root,
    };
    fetch_package(&env, order).expect("fetched")
}

/// Fetches and proves tierstore 1.5.0 with `fetcher` and `verifier`, writing the bundle through
/// `writer`.
fn prove_in(
    root: &Path,
    fetcher: &FakeFetcher,
    verifier: &FakeVerifier,
    clock: &FakeClock,
    writer: OpenWriter,
) -> Result<VerifiedArchive, PackageFailure> {
    let order = order("tierstore", "1.5.0", ARCHIVE);
    let staged = staged(fetcher, clock, root, &order);
    let signers = signers(
        "example-org",
        ".github/workflows/artifacts.yml",
        &["refs/heads/main"],
    );
    let env = ProveEnv {
        fetcher,
        verifier,
        clock,
        bounds: &bounds(),
        signers: &signers,
    };
    prove_with(&env, staged, writer)
}

fn deferred(reason: &'static str) -> PackageFailure {
    PackageFailure {
        outcome: Outcome::Deferred,
        class: Class::Environment,
        reason,
    }
}

fn failed(reason: &'static str) -> PackageFailure {
    PackageFailure {
        outcome: Outcome::Failure,
        class: Class::Release,
        reason,
    }
}

/// The names in the service's directory.
fn left(root: &Path) -> Vec<String> {
    listing(&root.join("tierstore"))
}

/// Catches a proof that returns the wrong file, commit or ref, asks the verifier about other
/// files or another signer, or keeps the bundle: the verifier is asked once, about the staged
/// archive and the downloaded bundle (both on disk at that moment), the archive's digest, the
/// bundle's commit and ref, and the config's signer; the result carries them; the bundle is gone
/// and the archive goes when the result is dropped.
#[test]
fn a_verified_package_returns_the_archive_commit_and_ref() {
    let scratch = scratch("prove-good");
    let fetcher = channel(serve(&good_bundle()));
    let verifier = FakeVerifier::answering(Ok(()));
    let verified = prove_in(
        scratch.path(),
        &fetcher,
        &verifier,
        &FakeClock::at(1),
        stage::plain,
    )
    .expect("proven");
    assert_eq!(
        std::fs::read(verified.path()).expect("the archive"),
        ARCHIVE
    );
    assert_eq!(verified.sha256().as_str(), sha256_hex(ARCHIVE));
    assert_eq!(verified.size(), 28);
    assert_eq!(verified.commit().as_str(), COMMIT);
    assert_eq!(verified.source_ref().as_str(), REF);

    let seen = verifier.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].archive, verified.path());
    assert_eq!(seen[0].bundle.parent(), verified.path().parent());
    assert_ne!(seen[0].bundle, verified.path());
    assert_eq!(seen[0].sha256, sha256_hex(ARCHIVE));
    assert_eq!(
        (seen[0].commit.as_str(), seen[0].source_ref.as_str()),
        (COMMIT, REF)
    );
    assert_eq!(
        seen[0].signers,
        signers(
            "example-org",
            ".github/workflows/artifacts.yml",
            &["refs/heads/main"]
        )
    );
    assert!(seen[0].files_existed);
    let name = verified
        .path()
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned();
    assert_eq!(left(scratch.path()), [name]);
    let urls: Vec<String> = fetcher.calls().into_iter().map(|call| call.url).collect();
    assert_eq!(urls, [ARCHIVE_URL, BUNDLE_URL]);
    drop(verified);
    assert!(left(scratch.path()).is_empty());
}

/// Runs a proof that must be refused with `expected`: the archive and the bundle are both gone
/// afterwards and nothing is returned. Returns what the verifier saw.
fn refused(
    name: &str,
    bundle: Serve,
    answer: Result<(), VerifyError>,
    expected: PackageFailure,
) -> Vec<Seen> {
    let scratch = scratch(name);
    let fetcher = channel(bundle);
    let verifier = FakeVerifier::answering(answer);
    let refusal = prove_in(
        scratch.path(),
        &fetcher,
        &verifier,
        &FakeClock::at(1),
        stage::plain,
    )
    .expect_err("refused");
    assert_eq!(refusal, expected, "{name}");
    assert!(left(scratch.path()).is_empty(), "{name}");
    verifier.seen()
}

/// Catches an unreachable verifier treated as a rejection, which would fail every package (and
/// halt a farm) whenever the attestation service is down: it defers with
/// `attestation_unavailable`, and both files are removed.
#[test]
fn an_unreachable_verifier_defers_with_attestation_unavailable() {
    let seen = refused(
        "prove-unavailable",
        serve(&good_bundle()),
        Err(VerifyError::Unavailable),
        deferred(ATTESTATION_UNAVAILABLE),
    );
    assert_eq!(seen.len(), 1);
}

/// Catches a verifier that ran out of time treated as a rejection: it defers with
/// `attestation_unavailable`, and both files are removed.
#[test]
fn a_timed_out_verifier_defers_with_attestation_unavailable() {
    let seen = refused(
        "prove-timed-out",
        serve(&good_bundle()),
        Err(VerifyError::TimedOut),
        deferred(ATTESTATION_UNAVAILABLE),
    );
    assert_eq!(seen.len(), 1);
}

/// Catches a bundle that could not be downloaded failing the release: a 404, no connection, a
/// body cut short, a stall past the attestation deadline and an adapter that stops for no reason
/// all defer with `attestation_unavailable`, before the verifier is asked, and both files go.
#[test]
fn a_bundle_that_cannot_be_downloaded_defers_with_attestation_unavailable() {
    for error in [
        NetError::Status(404),
        NetError::Connect,
        NetError::Truncated,
    ] {
        let seen = refused(
            "prove-no-bundle",
            Box::new(move |_: &mut dyn Sink, _: &Limit| Err(error)),
            Ok(()),
            deferred(ATTESTATION_UNAVAILABLE),
        );
        assert!(seen.is_empty(), "{error:?}");
    }
    let scratch = scratch("prove-stalled-bundle");
    let clock = Arc::new(FakeClock::at(1));
    let stalls = Arc::clone(&clock);
    let fetcher = channel(Box::new(move |sink: &mut dyn Sink, _: &Limit| {
        stalls.advance(61);
        Ok(deliver(sink, b"{}", true, 1).0)
    }));
    let verifier = FakeVerifier::answering(Ok(()));
    let refusal =
        prove_in(scratch.path(), &fetcher, &verifier, &clock, stage::plain).expect_err("stalled");
    assert_eq!(refusal, deferred(ATTESTATION_UNAVAILABLE));
    assert!(verifier.seen().is_empty());
    assert!(left(scratch.path()).is_empty());
}

/// Catches a rejection that is ever deferred, which would have the same bytes fetched and
/// rejected for ever: it fails the release with `not_attested`, and the archive is removed.
#[test]
fn a_rejected_attestation_fails_the_release() {
    let seen = refused(
        "prove-rejected",
        serve(&good_bundle()),
        Err(VerifyError::Rejected("no matching attestation".to_owned())),
        failed(NOT_ATTESTED),
    );
    assert_eq!(seen.len(), 1);
}

/// Catches a verifier asked about a bundle that cannot be about this archive: a bundle that is
/// not JSON, names no commit, names a commit that is not one, does not list the archive's
/// digest, or is over 1 MiB fails the release with `not_attested`, and the verifier is never
/// run.
#[test]
fn a_bundle_with_no_commit_or_not_listing_the_digest_is_rejected_without_running_the_verifier() {
    let other_digest = bundle_for(&sha256_hex(b"other bytes"), COMMIT);
    let no_commit = bundle_for(&sha256_hex(ARCHIVE), "not-a-commit");
    let oversized = vec![b' '; 1 << 20];
    for (name, bundle) in [
        ("prove-not-json", serve(b"<html>not found</html>")),
        ("prove-no-commit", serve(no_commit.as_bytes())),
        ("prove-other-digest", serve(other_digest.as_bytes())),
        ("prove-at-the-cap-not-a-bundle", serve(&oversized)),
    ] {
        let seen = refused(name, bundle, Ok(()), failed(NOT_ATTESTED));
        assert!(seen.is_empty(), "{name}");
    }
    let mut over = oversized;
    over.push(b' ');
    let seen = refused(
        "prove-over-the-cap",
        serve(&over),
        Ok(()),
        failed(NOT_ATTESTED),
    );
    assert!(seen.is_empty());
}

/// Catches a proof deadline that is not `attestation_timeout_secs` from the start of the proof
/// on the injected clock, or a bundle download bounded by another deadline.
#[test]
fn the_attestation_deadline_is_attestation_timeout_secs_on_the_injected_clock() {
    let scratch = scratch("prove-deadline");
    let fetcher = channel(serve(&good_bundle()));
    let verifier = FakeVerifier::answering(Ok(()));
    let clock = FakeClock::at(1000);
    let order = order("tierstore", "1.5.0", ARCHIVE);
    let staged = staged(&fetcher, &clock, scratch.path(), &order);
    clock.advance(5);
    let signers = signers(
        "example-org",
        ".github/workflows/artifacts.yml",
        &["refs/heads/main"],
    );
    let env = ProveEnv {
        fetcher: &fetcher,
        verifier: &verifier,
        clock: &clock,
        bounds: &bounds(),
        signers: &signers,
    };
    prove(&env, staged).expect("proven");
    assert_eq!(verifier.seen()[0].deadline, clock.stamp(1065));
    assert_eq!(fetcher.calls()[1].limit.deadline, clock.stamp(1065));
}

/// Catches a full disk while the bundle is written reported as anything but `no_space`, or
/// leaving a file: both the archive and the bundle are removed.
#[test]
fn enospc_while_writing_the_bundle_defers_with_no_space() {
    let scratch = scratch("prove-enospc");
    let fetcher = channel(serve(&good_bundle()));
    let verifier = FakeVerifier::answering(Ok(()));
    let refusal = prove_in(scratch.path(), &fetcher, &verifier, &FakeClock::at(1), full)
        .expect_err("a full disk");
    assert_eq!(refusal, deferred(NO_SPACE));
    assert!(left(scratch.path()).is_empty());
    assert!(verifier.seen().is_empty());
}

/// Catches a floor on versions added to the fetch or the proof (design Q13, option (a)): with
/// `9.9.9` installed and `current`, the older `0.0.1` is fetched and proven like any other.
#[test]
fn an_older_version_than_the_running_one_is_not_refused_here() {
    let scratch = scratch("prove-older");
    let dir = scratch.path().join("tierstore");
    std::fs::create_dir_all(dir.join("9.9.9")).expect("a newer version");
    std::os::unix::fs::symlink("9.9.9", dir.join("current")).expect("current");
    let old = b"the bytes of tierstore 0.0.1";
    let fetcher = FakeFetcher::new()
        .route(
            "https://releases.example/tierstore/tierstore-0.0.1.tar.zst",
            serve(old),
        )
        .route(
            "https://releases.example/tierstore/tierstore-0.0.1.tar.zst.sigstore.json",
            serve(bundle_for(&sha256_hex(old), COMMIT).as_bytes()),
        );
    let verifier = FakeVerifier::answering(Ok(()));
    let clock = FakeClock::at(1);
    let staged = staged(
        &fetcher,
        &clock,
        scratch.path(),
        &order("tierstore", "0.0.1", old),
    );
    let signers = signers(
        "example-org",
        ".github/workflows/artifacts.yml",
        &["refs/heads/main"],
    );
    let env = ProveEnv {
        fetcher: &fetcher,
        verifier: &verifier,
        clock: &clock,
        bounds: &bounds(),
        signers: &signers,
    };
    let verified = prove(&env, staged).expect("an older version is proven");
    assert_eq!(std::fs::read(verified.path()).expect("the archive"), old);
}

/// Catches a verifier's answer mapped the wrong way round: each answer and its refusal.
#[test]
fn each_verifier_answer_has_its_refusal() {
    assert_eq!(
        verify_failure(VerifyError::Unavailable),
        deferred(ATTESTATION_UNAVAILABLE)
    );
    assert_eq!(
        verify_failure(VerifyError::TimedOut),
        deferred(ATTESTATION_UNAVAILABLE)
    );
    assert_eq!(
        verify_failure(VerifyError::Rejected(String::new())),
        failed(NOT_ATTESTED)
    );
    assert_eq!(
        bundle_failure(Fault::Unexplained),
        deferred(ATTESTATION_UNAVAILABLE)
    );
    assert_eq!(
        bundle_failure(Fault::Deadline),
        deferred(ATTESTATION_UNAVAILABLE)
    );
    assert_eq!(
        bundle_failure(Fault::Storage(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        ))),
        deferred(ATTESTATION_UNAVAILABLE)
    );
}
