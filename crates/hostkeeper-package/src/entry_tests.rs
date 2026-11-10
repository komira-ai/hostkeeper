//! Tests of [`fetch_entry`] that need the staging files written through a failing writer. The
//! rows of the design's table are in `tests/fetch.rs`.

use std::cell::Cell;
use std::fs::File;
use std::io;
use std::io::Write;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_proto::hostkeeper::release::v1::Blob;

use super::*;
use crate::testing::{
    BundleSpec, CHANNEL, Failing, FakeClock, FakeFetcher, FakeVerifier, ReadOnlyDir, bundle_json,
    config, full, listing, scratch, serve, service_dir, sha256_hex, signers,
};

const ARCHIVE: &[u8] = b"the bytes of svc 1.0.0";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn bundle() -> String {
    bundle_json(&BundleSpec {
        organisation: "example-org",
        repository: "svc",
        workflow: ".github/workflows/artifacts.yml",
        source_ref: "refs/heads/main",
        certificate_commit: COMMIT,
        provenance_commit: COMMIT,
        runner: "github-hosted",
        subject_name: "svc-1.0.0.tar.zst",
        subject_sha256: &sha256_hex(ARCHIVE),
    })
}

/// An entry whose blobs are each at two locations of the `channel` store, which is the only
/// store the tests' host has.
fn entry() -> Target {
    let at = |locator: &str| {
        vec![
            Location {
                store: "channel".to_owned(),
                locator: format!("one/{locator}"),
            },
            Location {
                store: "channel".to_owned(),
                locator: format!("two/{locator}"),
            },
        ]
    };
    let text = bundle();
    Target {
        installer: "hostkeeper-archive".to_owned(),
        locations: at("svc.tar.zst"),
        digest: sha256_hex(ARCHIVE),
        bytes: ARCHIVE.len() as u64,
        attestation: Some(Blob {
            digest: sha256_hex(text.as_bytes()),
            bytes: text.len() as u64,
            locations: at("svc.sigstore.json"),
        }),
        ..Target::default()
    }
}

fn network() -> FakeFetcher {
    let text = bundle();
    let mut fetcher = FakeFetcher::new();
    for place in ["one", "two"] {
        fetcher = fetcher
            .route(&format!("{CHANNEL}/{place}/svc.tar.zst"), serve(ARCHIVE))
            .route(
                &format!("{CHANNEL}/{place}/svc.sigstore.json"),
                serve(text.as_bytes()),
            );
    }
    fetcher
}

thread_local! {
    /// How many staging files this test's thread has opened a writer for.
    static OPENED: Cell<u32> = const { Cell::new(0) };
}

fn read_only(_: &File) -> Box<dyn Write + '_> {
    Box::new(Failing(io::ErrorKind::ReadOnlyFilesystem))
}

/// The plain writer for the first staging file (the artifact) and `then` for every later one.
fn first_then(file: &File, then: OpenWriter) -> Box<dyn Write + '_> {
    let n = OPENED.get();
    OPENED.set(n + 1);
    if n == 0 {
        stage::plain(file)
    } else {
        then(file)
    }
}

fn bundle_full(file: &File) -> Box<dyn Write + '_> {
    first_then(file, full)
}

fn bundle_read_only(file: &File) -> Box<dyn Write + '_> {
    first_then(file, read_only)
}

fn run(root: &Path, fetcher: &FakeFetcher, writer: OpenWriter) -> Result<Proven, PackageFailure> {
    run_with(root, fetcher, &entry(), writer)
}

fn run_entry(
    root: &Path,
    fetcher: &FakeFetcher,
    target: &Target,
) -> Result<Proven, PackageFailure> {
    run_with(root, fetcher, target, stage::plain)
}

fn run_with(
    root: &Path,
    fetcher: &FakeFetcher,
    target: &Target,
    writer: OpenWriter,
) -> Result<Proven, PackageFailure> {
    let config: DaemonConfig = config(CHANNEL, &"0".repeat(64));
    let stores = Stores::channel(&config.release_channel);
    let verifier = FakeVerifier::answering(Ok(()));
    let bounds = Bounds::default();
    let signers = signers(
        "example-org",
        ".github/workflows/artifacts.yml",
        &["refs/heads/main"],
    );
    let env = EntryEnv {
        fetcher,
        verifier: &verifier,
        clock: &FakeClock::at(1),
        bounds: &bounds,
        stores: &stores,
        packages_root: root,
        signers: &signers,
        platform: Platform::LinuxX86_64,
    };
    fetch_entry_with(
        &env,
        &ServiceName::new("svc").expect("a service"),
        target,
        writer,
    )
}

/// A storage fault stops the fetch at once: a full disk is `no_space`, another write error is
/// `fetch_failed` for the artifact and `attestation_unavailable` for the bundle, only the first
/// source is asked in each case (the fault is the host's, not the source's), and nothing is left
/// staged.
///
/// Catches a disk fault blamed on a source (and so retried at the next), a full disk reported as
/// a network failure, and a partial file left behind.
#[test]
fn a_storage_fault_stops_at_once_with_the_hosts_reason() {
    let scratch = scratch("entry-storage");
    let cases: [(OpenWriter, PackageFailure, usize); 4] = [
        (full, PackageFailure::deferred(NO_SPACE), 1),
        (read_only, PackageFailure::deferred(FETCH_FAILED), 1),
        (bundle_full, PackageFailure::deferred(NO_SPACE), 2),
        (
            bundle_read_only,
            PackageFailure::deferred(ATTESTATION_UNAVAILABLE),
            2,
        ),
    ];
    for (writer, expected, asked) in cases {
        OPENED.set(0);
        let fetcher = network();
        let refusal = run(scratch.path(), &fetcher, writer).expect_err("a disk that fails");
        assert_eq!(refusal, expected);
        assert_eq!(fetcher.calls().len(), asked);
        assert!(listing(&scratch.path().join("svc")).is_empty());
    }
}

/// A service directory that is not a directory is `fetch_failed` before any request.
///
/// Catches a staging file made through a path that is not the service's directory.
#[test]
fn a_service_path_that_is_not_a_directory_defers_with_fetch_failed() {
    let scratch = scratch("entry-not-dir");
    std::fs::write(scratch.path().join("svc"), "a file").expect("a file");
    let fetcher = network();
    let refusal = run(scratch.path(), &fetcher, stage::plain).expect_err("refused");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));
    assert!(fetcher.calls().is_empty());
}

/// With the service's directory in place the whole fetch succeeds through the plain writer, the
/// proof being the verifier's: the archive, once proven, is the entry's.
///
/// Catches a unit-test harness that cannot tell a good run from a refused one.
#[test]
fn the_unit_harness_proves_a_good_entry() {
    let scratch = scratch("entry-good");
    service_dir(&scratch.path().join("svc"));
    let proven = run(scratch.path(), &network(), stage::plain).expect("proven");
    assert_eq!(proven.archive.sha256().as_str(), sha256_hex(ARCHIVE));
    assert!(proven.other_bytes.is_empty());
}

/// An orphan staging file that cannot be removed (a read-only service directory) defers the
/// fetch with `fetch_failed` before any request; once the directory is writable again the next
/// fetch sweeps the orphan and succeeds.
///
/// Catches an orphan sweep that is skipped or whose failure is ignored.
#[test]
fn an_orphan_that_cannot_be_removed_defers_before_any_request() {
    let scratch = scratch("entry-orphan-stuck");
    let dir = scratch.path().join("svc");
    service_dir(&dir);
    std::fs::write(dir.join(".fetch-1-0"), b"partial").expect("an orphan");
    let guard = ReadOnlyDir::new(&dir);
    let fetcher = network();
    let refusal = run(scratch.path(), &fetcher, stage::plain).expect_err("the orphan stays");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));
    assert!(fetcher.calls().is_empty());
    assert_eq!(listing(&dir), [".fetch-1-0"]);
    drop(guard);
    run(scratch.path(), &fetcher, stage::plain).expect("proven once the orphan can go");
}

/// A store that gave other bytes at its first two locations is reported once, and its third
/// location, which has the right bytes, is used; a host that defines none of the entry's stores
/// has no store to ask.
///
/// Catches a store reported once per bad source, a wrong location that ends the walk, and a
/// missing store mistaken for a source.
#[test]
fn other_bytes_are_reported_once_per_store_and_a_missing_store_has_no_source() {
    let scratch = scratch("entry-reported");
    service_dir(&scratch.path().join("svc"));
    let text = bundle();
    let locate = |blob: &mut Vec<Location>| {
        blob.push(Location {
            store: "channel".to_owned(),
            locator: "three/blob".to_owned(),
        });
    };
    let mut target = entry();
    locate(&mut target.locations);
    let fetcher = FakeFetcher::new()
        .route(&format!("{CHANNEL}/one/svc.tar.zst"), serve(b"wrong"))
        .route(&format!("{CHANNEL}/two/svc.tar.zst"), serve(b"wrong"))
        .route(&format!("{CHANNEL}/three/blob"), serve(ARCHIVE))
        .route(
            &format!("{CHANNEL}/one/svc.sigstore.json"),
            serve(text.as_bytes()),
        );
    let proven = run_entry(scratch.path(), &fetcher, &target);
    let proven = proven.expect("proven");
    assert_eq!(proven.other_bytes, ["channel"]);

    let mut nowhere = entry();
    nowhere.locations[0].store = "nowhere".to_owned();
    nowhere.locations.truncate(1);
    let fetcher = network();
    let refusal = run_entry(scratch.path(), &fetcher, &nowhere).expect_err("no store");
    assert_eq!(refusal, PackageFailure::environment(NO_STORE));
    assert!(fetcher.calls().is_empty());
}

/// The stores of a host with the `channel` store and an `oci` mirror named `lan`.
fn stores_with_mirror() -> Stores {
    let text = format!(
        "[[store]]\nname = \"channel\"\nkind = \"https\"\nbase = \"{CHANNEL}/\"\n\
         [[store]]\nname = \"lan\"\nkind = \"oci\"\nbase = \"https://lan.example/\"\n\
         mirror = true\nrepository = \"releases\"\n"
    );
    crate::stores::parse(text.as_bytes(), 0).expect("stores")
}

/// The URL `lan` is asked for the artifact.
fn lan_url() -> String {
    format!(
        "https://lan.example/v2/releases/blobs/sha256:{}",
        sha256_hex(ARCHIVE)
    )
}

fn pin_with(
    root: &Path,
    fetcher: &FakeFetcher,
    stores: &Stores,
    target: &Target,
) -> Result<Pinned, PackageFailure> {
    let bounds = Bounds::default();
    let verifier = FakeVerifier::answering(Ok(()));
    let signers = signers(
        "example-org",
        ".github/workflows/artifacts.yml",
        &["refs/heads/main"],
    );
    let env = EntryEnv {
        fetcher,
        verifier: &verifier,
        clock: &FakeClock::at(1),
        bounds: &bounds,
        stores,
        packages_root: root,
        signers: &signers,
        platform: Platform::MacosArm64,
    };
    fetch_pinned(&env, &ServiceName::new("svc").expect("a service"), target)
}

fn pin(root: &Path, fetcher: &FakeFetcher, target: &Target) -> Result<Pinned, PackageFailure> {
    let config: DaemonConfig = config(CHANNEL, &"0".repeat(64));
    let stores = Stores::channel(&config.release_channel);
    pin_with(root, fetcher, &stores, target)
}

/// An entry with no attestation, whose artifact is at the `one` location of `channel`.
fn unattested() -> Target {
    let mut target = entry();
    target.attestation = None;
    target.locations.truncate(1);
    target
}

/// The right bytes from a location give a [`Pinned`] whose file holds exactly those bytes and
/// whose digest and size are the entry's; the file is in the service's staging directory while
/// the `Pinned` lives and gone once it is dropped.
///
/// Catches a pinned fetch that hands back a file other than the one it checked, a digest or size
/// that is not the entry's, and a staging file left behind after use.
#[test]
fn a_pinned_fetch_gives_the_entrys_bytes_and_removes_the_file_on_drop() {
    let scratch = scratch("pinned-good");
    let fetcher = network();
    let target = entry();
    let pinned = pin(scratch.path(), &fetcher, &target).expect("pinned");
    assert_eq!(std::fs::read(pinned.path()).expect("the file"), ARCHIVE);
    assert_eq!(pinned.sha256().as_str(), sha256_hex(ARCHIVE));
    assert_eq!(pinned.size(), ARCHIVE.len() as u64);
    assert!(pinned.other_bytes.is_empty());
    assert_eq!(
        pinned.path().parent(),
        Some(scratch.path().join("svc").as_path())
    );
    assert_eq!(listing(&scratch.path().join("svc")).len(), 1);
    let path = pinned.path().to_path_buf();
    drop(pinned);
    assert!(!path.exists());
    assert!(listing(&scratch.path().join("svc")).is_empty());
}

/// An entry with no attestation is fetched by its pin, where `fetch_entry` refuses it as
/// `not_attested`; an attestation field that could not be fetched (a size of 0, a digest that is
/// no digest) is not read, and only the artifact's URL is ever requested.
///
/// Catches a pinned fetch that demands an attestation, that reads one it is given, or that asks
/// for the bundle.
#[test]
fn a_pinned_fetch_needs_no_attestation_and_never_asks_for_the_bundle() {
    let scratch = scratch("pinned-no-bundle");
    let target = unattested();
    let fetcher = network();
    let refusal = run_entry(scratch.path(), &fetcher, &target).expect_err("not attested");
    assert_eq!(refusal, PackageFailure::release(NOT_ATTESTED));
    assert!(fetcher.calls().is_empty());

    let fetcher = network();
    pin(scratch.path(), &fetcher, &target).expect("pinned");
    let asked: Vec<String> = fetcher.calls().into_iter().map(|call| call.url).collect();
    assert_eq!(asked, [format!("{CHANNEL}/one/svc.tar.zst")]);

    let mut odd = entry();
    odd.attestation = Some(Blob {
        digest: "not a digest".to_owned(),
        bytes: 0,
        locations: Vec::new(),
    });
    let fetcher = network();
    pin(scratch.path(), &fetcher, &odd).expect("pinned");
    assert!(
        fetcher
            .calls()
            .iter()
            .all(|call| call.url.ends_with("svc.tar.zst"))
    );
}

/// A location that gives other bytes is `package_digest_mismatch` of the release; every source
/// failing to answer is a deferred `fetch_failed`; a host that defines none of the entry's
/// stores has `no_store` and nothing is asked; nothing is left staged in any case.
///
/// Catches a wrong artifact accepted, a down network blamed on the release, a missing store
/// mistaken for a source, and a partial file left behind.
#[test]
fn a_pinned_fetch_refuses_other_bytes_and_reports_a_silent_network_and_a_missing_store() {
    let scratch = scratch("pinned-refused");
    let svc = scratch.path().join("svc");

    let wrong = FakeFetcher::new().route(&format!("{CHANNEL}/one/svc.tar.zst"), serve(b"wrong"));
    let refusal = pin(scratch.path(), &wrong, &unattested()).expect_err("other bytes");
    assert_eq!(refusal, PackageFailure::release(PACKAGE_DIGEST_MISMATCH));
    assert!(listing(&svc).is_empty());

    let silent = FakeFetcher::new();
    let refusal = pin(scratch.path(), &silent, &unattested()).expect_err("nothing answers");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));
    assert_eq!(silent.calls().len(), 1);
    assert!(listing(&svc).is_empty());

    let mut nowhere = unattested();
    nowhere.locations[0].store = "nowhere".to_owned();
    let fetcher = network();
    let refusal = pin(scratch.path(), &fetcher, &nowhere).expect_err("no store");
    assert_eq!(refusal, PackageFailure::environment(NO_STORE));
    assert!(fetcher.calls().is_empty());
}

/// A mirror that gives other bytes is asked first and recorded in `other_bytes`, and the
/// location after it, with the right bytes, wins.
///
/// Catches a mirror skipped in a pinned fetch, a mirror's wrong bytes ending the walk, and a
/// mirror that is not reported.
#[test]
fn a_pinned_fetch_records_a_mirror_that_gave_other_bytes() {
    let scratch = scratch("pinned-mirror");
    let fetcher = FakeFetcher::new()
        .route(&lan_url(), serve(b"wrong"))
        .route(&format!("{CHANNEL}/one/svc.tar.zst"), serve(ARCHIVE));
    let pinned = pin_with(
        scratch.path(),
        &fetcher,
        &stores_with_mirror(),
        &unattested(),
    )
    .expect("pinned");
    assert_eq!(pinned.other_bytes, ["lan"]);
    assert_eq!(std::fs::read(pinned.path()).expect("the file"), ARCHIVE);
    let asked: Vec<String> = fetcher.calls().into_iter().map(|call| call.url).collect();
    assert_eq!(asked, [lan_url(), format!("{CHANNEL}/one/svc.tar.zst")]);
}

/// An entry whose size is 0 or over `max_package_bytes`, or whose digest is no digest, is
/// `package_digest_mismatch` of the release before any request, and nothing is staged.
///
/// Catches a size or digest that no fetch can ask for being requested anyway.
#[test]
fn a_pinned_fetch_refuses_an_unaskable_pin_before_any_request() {
    let scratch = scratch("pinned-pin");
    let over = Bounds::default().max_package_bytes + 1;
    let cases: [(u64, &str); 3] = [
        (0, &sha256_hex(ARCHIVE)),
        (over, &sha256_hex(ARCHIVE)),
        (ARCHIVE.len() as u64, "not a digest"),
    ];
    for (bytes, digest) in cases {
        let mut target = unattested();
        target.bytes = bytes;
        target.digest = digest.to_owned();
        let fetcher = network();
        let refusal = pin(scratch.path(), &fetcher, &target).expect_err("refused");
        assert_eq!(refusal, PackageFailure::release(PACKAGE_DIGEST_MISMATCH));
        assert!(fetcher.calls().is_empty());
        assert!(!scratch.path().join("svc").exists());
    }
}

/// A service path that is not a directory is `fetch_failed` before any request, as it is for a
/// fetch with a proof.
///
/// Catches a pinned fetch that stages its file through a path that is not the service's
/// directory.
#[test]
fn a_pinned_fetch_with_a_service_path_that_is_not_a_directory_defers() {
    let scratch = scratch("pinned-not-dir");
    std::fs::write(scratch.path().join("svc"), "a file").expect("a file");
    let fetcher = network();
    let refusal = pin(scratch.path(), &fetcher, &unattested()).expect_err("refused");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));
    assert!(fetcher.calls().is_empty());
}
