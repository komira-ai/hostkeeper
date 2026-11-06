use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_config::daemon::Bounds;
use hostkeeper_hook::{Class, Outcome};
use hostkeeper_steps::{BootId, BootIdError};

use super::*;
use crate::fetch::{FetchEnv, PackageOrder, fetch_package};
use crate::ports::{Attested, Limit, NetError, Sink};
use crate::testing::{
    BundleSpec, CHANNEL, FakeClock, FakeFetcher, FakeVerifier, HOST, Seen, Serve, bundle_json,
    config, deliver, full, index_at, listing, order, package_at, published, scratch, serve,
    sha256_hex, signers,
};

const ARCHIVE: &[u8] = b"the bytes of tierstore 1.5.0";
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

/// The URL of the bundle of the package of `service` at `version`.
fn bundle_at(service: &str, version: &str) -> String {
    format!("{}.sigstore.json", package_at(service, version))
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

/// A fetcher that publishes tierstore 1.5.0 (its index and the archive) and serves `bundle` as
/// the bundle.
fn channel(bundle: Serve) -> FakeFetcher {
    published("tierstore", "1.5.0", ARCHIVE).route(&bundle_at("tierstore", "1.5.0"), bundle)
}

/// Fetches the archive of `order` into `root` with `fetcher`.
fn staged(
    fetcher: &FakeFetcher,
    clock: &dyn Clock,
    root: &Path,
    order: &PackageOrder,
) -> StagedArchive {
    let channel = config(CHANNEL, &"0".repeat(64)).release_channel;
    let env = FetchEnv {
        fetcher,
        clock,
        bounds: &bounds(),
        release_channel: &channel,
        packages_root: root,
        platform: HOST,
    };
    fetch_package(&env, order).expect("fetched")
}

/// Fetches and proves tierstore 1.5.0 with `fetcher` and `verifier`, writing the bundle through
/// `writer`.
fn prove_in(
    root: &Path,
    fetcher: &FakeFetcher,
    verifier: &dyn Verifier,
    clock: &dyn Clock,
    writer: OpenWriter,
) -> Result<VerifiedArchive, PackageFailure> {
    let order = order("tierstore", "1.5.0");
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
/// files or another signer, hands `gh` a bundle without the `.json` extension it dispatches on,
/// or keeps the bundle: the verifier is asked once, about the staged
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
    assert_eq!(verified.platform(), HOST);
    assert_eq!(verified.commit().as_str(), COMMIT);
    assert_eq!(verified.source_ref().as_str(), REF);

    let seen = verifier.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].archive, verified.path());
    assert_eq!(seen[0].bundle.parent(), verified.path().parent());
    assert_ne!(seen[0].bundle, verified.path());
    let bundle_name = seen[0]
        .bundle
        .file_name()
        .expect("a name")
        .to_string_lossy();
    assert!(
        bundle_name.starts_with(".fetch-") && bundle_name.ends_with(".json"),
        "{bundle_name}"
    );
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
    assert_eq!(
        urls,
        [
            index_at("tierstore", "1.5.0"),
            package_at("tierstore", "1.5.0"),
            bundle_at("tierstore", "1.5.0"),
        ]
    );
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
        prove_in(scratch.path(), &fetcher, &verifier, &*clock, stage::plain).expect_err("stalled");
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
/// not JSON, names no commit, names a commit that is not one, or does not list the archive's
/// digest fails the release with `not_attested`, and the verifier is never
/// run.
#[test]
fn a_bundle_with_no_commit_or_not_listing_the_digest_is_rejected_without_running_the_verifier() {
    let other_digest = bundle_for(&sha256_hex(b"other bytes"), COMMIT);
    let no_commit = bundle_for(&sha256_hex(ARCHIVE), "not-a-commit");
    for (name, bundle) in [
        ("prove-not-json", serve(b"<html>not found</html>")),
        ("prove-no-commit", serve(no_commit.as_bytes())),
        ("prove-other-digest", serve(other_digest.as_bytes())),
    ] {
        let seen = refused(name, bundle, Ok(()), failed(NOT_ATTESTED));
        assert!(seen.is_empty(), "{name}");
    }
}

/// Catches a proof deadline that is not `attestation_timeout_secs` from the start of the proof
/// on the injected clock, or a bundle download bounded by another deadline.
#[test]
fn the_attestation_deadline_is_attestation_timeout_secs_on_the_injected_clock() {
    let scratch = scratch("prove-deadline");
    let fetcher = channel(serve(&good_bundle()));
    let verifier = FakeVerifier::answering(Ok(()));
    let clock = FakeClock::at(1000);
    let order = order("tierstore", "1.5.0");
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
    assert_eq!(fetcher.calls()[2].limit.deadline, clock.stamp(1065));
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
    let fetcher = published("tierstore", "0.0.1", old).route(
        &bundle_at("tierstore", "0.0.1"),
        serve(bundle_for(&sha256_hex(old), COMMIT).as_bytes()),
    );
    let verifier = FakeVerifier::answering(Ok(()));
    let clock = FakeClock::at(1);
    let staged = staged(
        &fetcher,
        &clock,
        scratch.path(),
        &order("tierstore", "0.0.1"),
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

/// Padded with trailing spaces to exactly `len` bytes.
fn padded(mut bytes: Vec<u8>, len: u64) -> Vec<u8> {
    bytes.resize(usize::try_from(len).expect("a length"), b' ');
    bytes
}

/// Serves `body`, announced or not, in chunks of 4 KiB.
fn serve_in_blocks(body: Vec<u8>, announce: bool) -> Serve {
    Box::new(move |sink: &mut dyn Sink, _: &Limit| Ok(deliver(sink, &body, announce, 4096).0))
}

/// Catches an off-by-one at the bundle cap that a non-JSON body would hide: a valid bundle
/// padded with trailing spaces to exactly 1 MiB, announced or not, is read and verified; one byte
/// more fails the release with `not_attested` (by the header, or at the first byte past the cap)
/// and the verifier is not asked.
#[test]
fn a_bundle_of_exactly_1_mib_is_proven_and_one_byte_more_is_not_attested() {
    for announce in [true, false] {
        let at_cap = scratch("prove-bundle-cap");
        let fetcher = channel(serve_in_blocks(
            padded(good_bundle(), MAX_BUNDLE_BYTES),
            announce,
        ));
        let verifier = FakeVerifier::answering(Ok(()));
        let verified = prove_in(
            at_cap.path(),
            &fetcher,
            &verifier,
            &FakeClock::at(1),
            stage::plain,
        )
        .expect("a bundle at the cap");
        assert_eq!(verified.commit().as_str(), COMMIT, "{announce}");
        assert_eq!(verifier.seen().len(), 1, "{announce}");

        let scratch = scratch("prove-bundle-over");
        let fetcher = channel(serve_in_blocks(
            padded(good_bundle(), MAX_BUNDLE_BYTES + 1),
            announce,
        ));
        let verifier = FakeVerifier::answering(Ok(()));
        let refusal = prove_in(
            scratch.path(),
            &fetcher,
            &verifier,
            &FakeClock::at(1),
            stage::plain,
        )
        .expect_err("a bundle over the cap");
        assert_eq!(refusal, failed(NOT_ATTESTED), "{announce}");
        assert!(verifier.seen().is_empty(), "{announce}");
        assert!(left(scratch.path()).is_empty(), "{announce}");
    }
}

/// A verifier that moves the clock before it answers.
struct Slow<'a> {
    inner: FakeVerifier,
    clock: &'a FakeClock,
    secs: u64,
}

impl Verifier for Slow<'_> {
    fn verify(&self, request: &Attest<'_>) -> Result<Attested, VerifyError> {
        self.clock.advance(self.secs);
        self.inner.verify(request)
    }
}

/// Catches a verdict taken after the proof's time ran out: a verifier that answers `Ok` after
/// the clock passed the 60 s deadline (at exactly 60 s) is deferred with
/// `attestation_unavailable` and both files are removed; one that answers at 59 s is accepted.
#[test]
fn an_ok_that_arrives_at_the_deadline_is_not_taken() {
    let late = scratch("prove-late-ok");
    let clock = FakeClock::at(100);
    let fetcher = channel(serve(&good_bundle()));
    let slow = Slow {
        inner: FakeVerifier::answering(Ok(())),
        clock: &clock,
        secs: 60,
    };
    let refusal = prove_in(late.path(), &fetcher, &slow, &clock, stage::plain)
        .expect_err("an answer at the deadline");
    assert_eq!(refusal, deferred(ATTESTATION_UNAVAILABLE));
    let seen = slow.inner.seen();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].files_existed);
    assert!(left(late.path()).is_empty());

    let scratch = scratch("prove-ok-in-time");
    let clock = FakeClock::at(100);
    let slow = Slow {
        inner: FakeVerifier::answering(Ok(())),
        clock: &clock,
        secs: 59,
    };
    prove_in(scratch.path(), &fetcher, &slow, &clock, stage::plain)
        .expect("an answer one second before the deadline");
}

/// A clock that, once told to, reads `jump` seconds late: the first read after `arm` is on time,
/// and every later one is `jump` seconds late. It moves with the number of reads, never with real time.
struct Tripwire {
    now: u64,
    jump: u64,
    reads: AtomicU64,
    late_from: AtomicU64,
}

impl Tripwire {
    fn new(now: u64, jump: u64) -> Tripwire {
        Tripwire {
            now,
            jump,
            reads: AtomicU64::new(0),
            late_from: AtomicU64::new(u64::MAX),
        }
    }

    /// Lets the next read be on time and every one after it late.
    fn arm(&self) {
        self.late_from
            .store(self.reads.load(Ordering::SeqCst) + 1, Ordering::SeqCst);
    }
}

impl Clock for Tripwire {
    fn mono_secs(&self) -> u64 {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        let late = u64::from(read >= self.late_from.load(Ordering::SeqCst));
        self.now + self.jump * late
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the tripwire", crate::testing::BOOT)
    }

    fn sleep_secs(&self, _: u64) {}
}

/// Catches a verifier started when the proof's time is already gone after the download: the
/// clock is on time through the end of the bundle's download and 60 s late at the next read,
/// before the verifier would be asked; the order is deferred with `attestation_unavailable`, the
/// verifier is never run, and both files go.
#[test]
fn a_proof_whose_time_ran_out_after_the_download_does_not_start_the_verifier() {
    let scratch = scratch("prove-late-start");
    let clock = Arc::new(Tripwire::new(100, 60));
    let armed = Arc::clone(&clock);
    let body = good_bundle();
    let fetcher = channel(Box::new(move |sink: &mut dyn Sink, _: &Limit| {
        let (got, _) = deliver(sink, &body, true, 64);
        armed.arm();
        Ok(got)
    }));
    let verifier = FakeVerifier::answering(Ok(()));
    let refusal = prove_in(scratch.path(), &fetcher, &verifier, &*clock, stage::plain)
        .expect_err("no time left");
    assert_eq!(refusal, deferred(ATTESTATION_UNAVAILABLE));
    assert!(verifier.seen().is_empty());
    assert!(left(scratch.path()).is_empty());
}

/// Catches a bundle accepted whose download ended as the deadline passed: the clock moves 60 s
/// right after the last byte of the bundle, the order is deferred with
/// `attestation_unavailable`, the verifier is not run, and both files go.
#[test]
fn a_bundle_download_that_ends_as_the_deadline_passes_does_not_start_the_verifier() {
    let scratch = scratch("prove-ends-late");
    let clock = Arc::new(FakeClock::at(100));
    let moves = Arc::clone(&clock);
    let body = good_bundle();
    let fetcher = channel(Box::new(move |sink: &mut dyn Sink, _: &Limit| {
        let (got, _) = deliver(sink, &body, true, 64);
        moves.advance(60);
        Ok(got)
    }));
    let verifier = FakeVerifier::answering(Ok(()));
    let refusal = prove_in(scratch.path(), &fetcher, &verifier, &*clock, stage::plain)
        .expect_err("the deadline passed");
    assert_eq!(refusal, deferred(ATTESTATION_UNAVAILABLE));
    assert!(verifier.seen().is_empty());
    assert!(left(scratch.path()).is_empty());
}

/// Catches a bundle that cannot be read back being treated as empty or as a verdict: the
/// staging files are removed while the bundle is being served, the order is deferred with
/// `attestation_unavailable`, the verifier is not run, and nothing is left.
#[test]
fn a_bundle_that_cannot_be_read_back_defers_with_attestation_unavailable() {
    let scratch = scratch("prove-readback");
    let dir = scratch.path().join("tierstore");
    let body = good_bundle();
    let fetcher = channel(Box::new(move |sink: &mut dyn Sink, _: &Limit| {
        let (got, _) = deliver(sink, &body, true, 64);
        for entry in std::fs::read_dir(&dir).expect("list the directory") {
            std::fs::remove_file(entry.expect("an entry").path()).expect("remove the file");
        }
        Ok(got)
    }));
    let verifier = FakeVerifier::answering(Ok(()));
    let refusal = prove_in(
        scratch.path(),
        &fetcher,
        &verifier,
        &FakeClock::at(1),
        stage::plain,
    )
    .expect_err("a bundle that is gone");
    assert_eq!(refusal, deferred(ATTESTATION_UNAVAILABLE));
    assert!(verifier.seen().is_empty());
    assert!(left(scratch.path()).is_empty());
}

/// Catches a proof that treats an attested build as the one the order meant, which this crate
/// cannot know: the index is unsigned, so a channel (or anyone in front of it) that serves the
/// index entry, the archive and the bundle of another attested build of the same service,
/// version and platform is believed. Two archives A and B are published; the channel serves B's
/// entry, B's archive and B's bundle; the fetch and the proof succeed with B's digest. This is
/// the guarantee, not a defect: the attestation binds a build to the trusted signer, and the
/// manifest step then compares the manifest's service, version and platform.
#[test]
fn coordinated_substitution_of_index_archive_and_bundle_with_another_attested_build_is_accepted() {
    let build_a = b"build A of tierstore 1.5.0";
    let build_b = b"build B of tierstore 1.5.0";
    assert_ne!(sha256_hex(build_a), sha256_hex(build_b));
    for (name, build) in [("a", build_a), ("b", build_b)] {
        let scratch = scratch(&format!("prove-substitution-{name}"));
        let channel = published("tierstore", "1.5.0", build).route(
            &bundle_at("tierstore", "1.5.0"),
            serve(bundle_for(&sha256_hex(build), COMMIT).as_bytes()),
        );
        let verifier = FakeVerifier::answering(Ok(()));
        let verified = prove_in(
            scratch.path(),
            &channel,
            &verifier,
            &FakeClock::at(1),
            stage::plain,
        )
        .expect("an attested build is accepted");
        assert_eq!(verified.sha256().as_str(), sha256_hex(build), "{name}");
        assert_eq!(std::fs::read(verified.path()).expect("the archive"), build);
        assert_eq!(verifier.seen()[0].sha256, sha256_hex(build), "{name}");
    }
}
