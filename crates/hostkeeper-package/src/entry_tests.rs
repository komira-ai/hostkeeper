//! Tests of [`fetch_pinned`] that need the staging file written through a failing writer, and the
//! unit-level rows of the module's table. The rows with several sources are in `tests/fetch.rs`.

use std::fs::File;
use std::io;
use std::io::Write;

use super::*;
use crate::ports::NetError;
use crate::stores;
use crate::testing::{
    CHANNEL, Failing, FakeClock, FakeFetcher, ReadOnlyDir, full, listing, scratch, serve,
    service_dir, sha256_hex,
};

const ARCHIVE: &[u8] = b"the bytes of svc 1.0.0";

/// An entry whose artifact is at two locations of the `channel` store, which is the only store
/// the tests' host has.
fn entry() -> Target {
    Target {
        installer: "macos-pkg".to_owned(),
        locations: ["one", "two"]
            .iter()
            .map(|place| Location {
                store: "channel".to_owned(),
                locator: format!("{place}/svc.pkg"),
            })
            .collect(),
        digest: sha256_hex(ARCHIVE),
        bytes: ARCHIVE.len() as u64,
        ..Target::default()
    }
}

fn network() -> FakeFetcher {
    FakeFetcher::new()
        .route(&format!("{CHANNEL}/one/svc.pkg"), serve(ARCHIVE))
        .route(&format!("{CHANNEL}/two/svc.pkg"), serve(ARCHIVE))
}

fn read_only(_: &File) -> Box<dyn Write + '_> {
    Box::new(Failing(io::ErrorKind::ReadOnlyFilesystem))
}

/// The stores of a host with the `channel` store alone.
fn channel_only() -> Stores {
    let text = format!("[[store]]\nname = \"channel\"\nkind = \"https\"\nbase = \"{CHANNEL}/\"\n");
    stores::parse(text.as_bytes(), 0).expect("the one store")
}

/// The stores of a host with the `channel` store and an `oci` mirror named `lan`.
fn stores_with_mirror() -> Stores {
    let text = format!(
        "[[store]]\nname = \"channel\"\nkind = \"https\"\nbase = \"{CHANNEL}/\"\n\
         [[store]]\nname = \"lan\"\nkind = \"oci\"\nbase = \"https://lan.example/\"\n\
         mirror = true\nrepository = \"releases\"\n"
    );
    stores::parse(text.as_bytes(), 0).expect("stores")
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
    writer: OpenWriter,
) -> Result<Pinned, PackageFailure> {
    let bounds = Bounds::default();
    let env = PinnedEnv {
        fetcher,
        coordinator: None,
        clock: &FakeClock::at(1),
        bounds: &bounds,
        stores,
        packages_root: root,
    };
    fetch_pinned_with(
        &env,
        &ServiceName::new("svc").expect("a service"),
        target,
        writer,
    )
}

fn pin(root: &Path, fetcher: &FakeFetcher, target: &Target) -> Result<Pinned, PackageFailure> {
    pin_with(root, fetcher, &channel_only(), target, stage::plain)
}

/// A storage fault stops the fetch at once: a full disk is `no_space`, another write error is
/// `fetch_failed`, only the first source is asked in each case (the fault is the host's, not the
/// source's), and nothing is left staged.
///
/// Catches a disk fault blamed on a source (and so retried at the next), a full disk reported as
/// a network failure, and a partial file left behind.
#[test]
fn a_storage_fault_stops_at_once_with_the_hosts_reason() {
    let scratch = scratch("pinned-storage");
    let cases: [(OpenWriter, PackageFailure); 2] = [
        (full, PackageFailure::deferred(NO_SPACE)),
        (read_only, PackageFailure::deferred(FETCH_FAILED)),
    ];
    for (writer, expected) in cases {
        let fetcher = network();
        let refusal = pin_with(scratch.path(), &fetcher, &channel_only(), &entry(), writer)
            .expect_err("a disk that fails");
        assert_eq!(refusal, expected);
        assert_eq!(fetcher.calls().len(), 1);
        assert!(listing(&scratch.path().join("svc")).is_empty());
    }
}

/// An orphan staging file that cannot be removed (a read-only service directory) defers the
/// fetch with `fetch_failed` before any request; once the directory is writable again the next
/// fetch sweeps the orphan and succeeds.
///
/// Catches an orphan sweep that is skipped or whose failure is ignored.
#[test]
fn an_orphan_that_cannot_be_removed_defers_before_any_request() {
    let scratch = scratch("pinned-orphan-stuck");
    let dir = scratch.path().join("svc");
    service_dir(&dir);
    std::fs::write(dir.join(".fetch-1-0"), b"partial").expect("an orphan");
    let guard = ReadOnlyDir::new(&dir);
    let fetcher = network();
    let refusal = pin(scratch.path(), &fetcher, &entry()).expect_err("the orphan stays");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));
    assert!(fetcher.calls().is_empty());
    assert_eq!(listing(&dir), [".fetch-1-0"]);
    drop(guard);
    pin(scratch.path(), &fetcher, &entry()).expect("pinned once the orphan can go");
}

/// A store that gave other bytes at its first two locations is reported once, and its third
/// location, which has the right bytes, is used.
///
/// Catches a store reported once per bad source, and a wrong location that ends the walk.
#[test]
fn other_bytes_are_reported_once_per_store() {
    let scratch = scratch("pinned-reported");
    service_dir(&scratch.path().join("svc"));
    let mut target = entry();
    target.locations.push(Location {
        store: "channel".to_owned(),
        locator: "three/svc.pkg".to_owned(),
    });
    let fetcher = FakeFetcher::new()
        .route(&format!("{CHANNEL}/one/svc.pkg"), serve(b"wrong"))
        .route(&format!("{CHANNEL}/two/svc.pkg"), serve(b"wrong"))
        .route(&format!("{CHANNEL}/three/svc.pkg"), serve(ARCHIVE));
    let pinned = pin(scratch.path(), &fetcher, &target).expect("pinned");
    assert_eq!(pinned.other_bytes, ["channel"]);
}

/// The right bytes from a location give a [`Pinned`] whose file holds exactly those bytes and
/// whose digest and size are the entry's; the file is in the service's staging directory while
/// the `Pinned` lives and gone once it is dropped; an attestation field of the entry is not read
/// and only the artifact's URL is ever requested.
///
/// Catches a pinned fetch that hands back a file other than the one it checked, a digest or size
/// that is not the entry's, a staging file left behind after use, and a bundle asked for.
#[test]
fn a_pinned_fetch_gives_the_entrys_bytes_and_removes_the_file_on_drop() {
    let scratch = scratch("pinned-good");
    let fetcher = network();
    let mut target = entry();
    target.locations.truncate(1);
    target.attestation = Some(hostkeeper_proto::hostkeeper::release::v1::Blob {
        digest: "not a digest".to_owned(),
        bytes: 0,
        locations: Vec::new(),
    });
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
    let asked: Vec<String> = fetcher.calls().into_iter().map(|call| call.url).collect();
    assert_eq!(asked, [format!("{CHANNEL}/one/svc.pkg")]);
    let path = pinned.path().to_path_buf();
    drop(pinned);
    assert!(!path.exists());
    assert!(listing(&scratch.path().join("svc")).is_empty());
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
    let mut target = entry();
    target.locations.truncate(1);

    let wrong = FakeFetcher::new().route(&format!("{CHANNEL}/one/svc.pkg"), serve(b"wrong"));
    let refusal = pin(scratch.path(), &wrong, &target).expect_err("other bytes");
    assert_eq!(refusal, PackageFailure::release(PACKAGE_DIGEST_MISMATCH));
    assert!(listing(&svc).is_empty());

    let silent = FakeFetcher::new();
    let refusal = pin(scratch.path(), &silent, &target).expect_err("nothing answers");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));
    assert_eq!(silent.calls().len(), 1);
    assert!(listing(&svc).is_empty());

    let mut nowhere = target;
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
        .route(&format!("{CHANNEL}/one/svc.pkg"), serve(ARCHIVE));
    let pinned = pin_with(
        scratch.path(),
        &fetcher,
        &stores_with_mirror(),
        &entry(),
        stage::plain,
    )
    .expect("pinned");
    assert_eq!(pinned.other_bytes, ["lan"]);
    assert_eq!(std::fs::read(pinned.path()).expect("the file"), ARCHIVE);
    let asked: Vec<String> = fetcher.calls().into_iter().map(|call| call.url).collect();
    assert_eq!(asked, [lan_url(), format!("{CHANNEL}/one/svc.pkg")]);
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
        let mut target = entry();
        target.bytes = bytes;
        target.digest = digest.to_owned();
        let fetcher = network();
        let refusal = pin(scratch.path(), &fetcher, &target).expect_err("refused");
        assert_eq!(refusal, PackageFailure::release(PACKAGE_DIGEST_MISMATCH));
        assert!(fetcher.calls().is_empty());
        assert!(!scratch.path().join("svc").exists());
    }
}

/// A service path that is not a directory is `fetch_failed` before any request.
///
/// Catches a pinned fetch that stages its file through a path that is not the service's
/// directory.
#[test]
fn a_pinned_fetch_with_a_service_path_that_is_not_a_directory_defers() {
    let scratch = scratch("pinned-not-dir");
    std::fs::write(scratch.path().join("svc"), "a file").expect("a file");
    let fetcher = network();
    let refusal = pin(scratch.path(), &fetcher, &entry()).expect_err("refused");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));
    assert!(fetcher.calls().is_empty());
}

/// The path of the pkg of the coordinator tests.
const PKG_PATH: &str = "/repositories/acme/pool/group/config.pkg";

/// A `macos-pkg` entry whose only location is `PKG_PATH` in the `coordinator` store.
fn in_coordinator() -> Target {
    let mut target = entry();
    target.locations = vec![Location {
        store: "coordinator".to_owned(),
        locator: PKG_PATH[1..].to_owned(),
    }];
    target
}

/// [`pin_with`] for `target` with `coordinator` as the fetcher of the coordinator store.
fn pin_via(
    root: &Path,
    fetcher: &FakeFetcher,
    coordinator: Option<&FakeFetcher>,
    target: &Target,
) -> Result<Pinned, PackageFailure> {
    let stores = channel_only();
    let bounds = Bounds::default();
    let env = PinnedEnv {
        fetcher,
        coordinator: coordinator.map(|fetcher| fetcher as &dyn Fetcher),
        clock: &FakeClock::at(1),
        bounds: &bounds,
        stores: &stores,
        packages_root: root,
    };
    fetch_pinned(&env, &ServiceName::new("svc").expect("a service"), target)
}

/// A location in the `coordinator` store is asked of the coordinator fetcher alone, at `/` and
/// its locator, and the bytes it gives are accepted by the entry's size and SHA-256.
///
/// Catches a coordinator location asked of the web fetcher, a path other than `/` and the
/// locator, and bytes taken without the digest check.
#[test]
fn a_coordinator_location_is_fetched_through_the_coordinator_fetcher_by_its_path() {
    let scratch = scratch("pinned-coordinator");
    let web = FakeFetcher::new();
    let coordinator = FakeFetcher::new().route(PKG_PATH, serve(ARCHIVE));
    let pinned = pin_via(scratch.path(), &web, Some(&coordinator), &in_coordinator())
        .expect("pinned from the coordinator");
    assert_eq!(std::fs::read(pinned.path()).expect("the file"), ARCHIVE);
    assert_eq!(pinned.sha256().as_str(), sha256_hex(ARCHIVE));
    assert_eq!(pinned.size(), ARCHIVE.len() as u64);
    assert!(web.calls().is_empty());
    let calls = coordinator.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].url, PKG_PATH);
}

/// Other bytes from the coordinator are `package_digest_mismatch` (the location is named by the
/// entry), a 404 and a refused connection defer `fetch_failed`.
///
/// Catches coordinator bytes taken without the digest, and a coordinator failure that is not a
/// deferral.
#[test]
fn the_coordinators_wrong_bytes_are_refused_and_its_failures_defer() {
    let scratch = scratch("pinned-coordinator-bad");
    let web = FakeFetcher::new();
    let wrong = FakeFetcher::new().route(PKG_PATH, serve(b"other bytes"));
    let refusal =
        pin_via(scratch.path(), &web, Some(&wrong), &in_coordinator()).expect_err("wrong bytes");
    assert_eq!(refusal, PackageFailure::release(PACKAGE_DIGEST_MISMATCH));

    let missing = FakeFetcher::new();
    let refusal =
        pin_via(scratch.path(), &web, Some(&missing), &in_coordinator()).expect_err("a 404");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));

    let refused = FakeFetcher::new().route(PKG_PATH, Box::new(|_, _| Err(NetError::Connect)));
    let refusal =
        pin_via(scratch.path(), &web, Some(&refused), &in_coordinator()).expect_err("refused");
    assert_eq!(refusal, PackageFailure::deferred(FETCH_FAILED));
    assert!(web.calls().is_empty());
}

/// Without a coordinator fetcher a coordinator location is no source (`no_store`), nothing is
/// asked, and a channel location of the same entry is still used.
///
/// Catches a coordinator location sent to the web fetcher, and one that hides the entry's other
/// locations.
#[test]
fn without_a_coordinator_fetcher_a_coordinator_location_is_no_store() {
    let scratch = scratch("pinned-no-coordinator");
    let web = network();
    let refusal =
        pin_via(scratch.path(), &web, None, &in_coordinator()).expect_err("no coordinator");
    assert_eq!(refusal, PackageFailure::environment(NO_STORE));
    assert!(web.calls().is_empty());

    let mut both = in_coordinator();
    both.locations.extend(entry().locations);
    let pinned = pin_via(scratch.path(), &web, None, &both).expect("the channel location serves");
    assert_eq!(pinned.sha256().as_str(), sha256_hex(ARCHIVE));
    assert_eq!(web.calls().len(), 1);
}
