//! Fetch of an entry's artifact by its pin (docs/design/packages.md, section 7.4), through the
//! public API only: the host's stores are a real `stores.toml` in a scratch directory, and the
//! network is a fake fetcher keyed by URL (or [`NetFetcher`] over an in-memory transport for the
//! rows that are the fetcher's own: redirects, the credential, the token challenge). The clock
//! moves only when a test moves it.

#[path = "fetch/network.rs"]
mod network;
#[path = "fetch/support.rs"]
mod support;

use hostkeeper_hook::{Class, Outcome};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use support::*;

/// A fetcher where every URL in `serving` answers with its bytes.
fn with(host: &Host, serving: &[(String, Serve)]) -> FakeFetcher {
    let mut fetcher = FakeFetcher::new(&host.clock);
    for (url, serve) in serving {
        fetcher = fetcher.route(url, serve.clone());
    }
    fetcher
}

/// The URLs of `urls` that `fetcher` was asked for, in order.
fn asked(fetcher: &FakeFetcher, urls: &[String]) -> Vec<String> {
    fetcher
        .urls()
        .into_iter()
        .filter(|u| urls.contains(u))
        .collect()
}

/// A mirror answers with the right bytes: it is asked first, the location is never asked, and the
/// pinned artifact has the entry's size and SHA-256; the staging file is the only file left, and
/// it is removed with its value.
///
/// Catches mirrors tried after the locations, a location asked although a mirror gave the bytes,
/// and a staging file left behind.
#[test]
fn a_mirror_with_the_right_bytes_wins_and_the_locations_are_not_asked() {
    let host = Host::new("mirror-wins", true);
    let fetcher = with(
        &host,
        &[
            (mirror_url("m1"), Serve::Bytes(ARCHIVE.to_vec())),
            (artifact_url(), Serve::Bytes(wrong())),
        ],
    );
    let pinned = host.run(&fetcher, &entry()).expect("pinned");
    assert!(pinned.other_bytes.is_empty());
    assert_eq!(
        asked(&fetcher, &[mirror_url("m1"), artifact_url()]),
        [mirror_url("m1")]
    );
    assert_eq!(pinned.sha256().as_str(), sha256(ARCHIVE));
    assert_eq!(
        pinned.size(),
        u64::try_from(ARCHIVE.len()).expect("a length")
    );
    assert_eq!(fs::read(pinned.path()).expect("the artifact"), ARCHIVE);
    assert_eq!(host.staged().len(), 1, "{:?}", host.staged());
    let kept = pinned.path().to_owned();
    drop(pinned);
    assert!(!kept.exists());
    assert!(host.staged().is_empty());
}

/// A mirror answers with other bytes and then a location with the right ones: the location wins
/// and the mirror is reported as having given other bytes, once; the second mirror, being down,
/// is asked too.
///
/// Catches a mirror's wrong bytes failing the order when a location has the right ones, and a
/// mismatch that is not reported.
#[test]
fn a_mirror_with_other_bytes_is_reported_and_the_location_is_used() {
    let host = Host::new("mirror-wrong", true);
    let fetcher = with(
        &host,
        &[
            (mirror_url("m1"), Serve::Bytes(wrong())),
            (artifact_url(), Serve::Bytes(ARCHIVE.to_vec())),
        ],
    );
    let pinned = host.run(&fetcher, &entry()).expect("pinned");
    assert_eq!(pinned.other_bytes, ["m1"]);
    assert_eq!(
        asked(
            &fetcher,
            &[mirror_url("m1"), mirror_url("m2"), artifact_url()]
        ),
        [mirror_url("m1"), mirror_url("m2"), artifact_url()]
    );
}

/// Only mirrors answer and every one gives other bytes (the entry's locations name a store the
/// host does not define): `store_digest_mismatch`, deferred. A location that also gave other
/// bytes makes it the release's fault, a mirror down and a location wrong too.
///
/// Catches a mirror's disagreement blamed on the release, and a location's blamed on the host.
#[test]
fn only_mirrors_with_other_bytes_defer_and_a_wrong_location_fails_the_release() {
    let host = Host::new("only-mirrors", true);
    let undefined = entry_of(ARCHIVE, &[("nowhere", ARTIFACT_AT)]);
    let fetcher = with(
        &host,
        &[
            (mirror_url("m1"), Serve::Bytes(wrong())),
            (mirror_url("m2"), Serve::Bytes(wrong())),
        ],
    );
    let refusal = host.run(&fetcher, &undefined).expect_err("refused");
    assert_eq!(refusal, deferred("store_digest_mismatch"));
    assert!(host.staged().is_empty());

    let fetcher = with(
        &host,
        &[
            (mirror_url("m1"), Serve::Bytes(wrong())),
            (mirror_url("m2"), Serve::Status(503)),
            (artifact_url(), Serve::Bytes(wrong())),
        ],
    );
    let refusal = host.run(&fetcher, &entry()).expect_err("refused");
    assert_eq!(refusal, release("package_digest_mismatch"));
    assert!(host.staged().is_empty());
}

/// A mirror that hangs past the deadline is given up on and a location, with its own deadline,
/// gives the bytes; the hung mirror is not "other bytes".
///
/// Catches one deadline shared by all sources (the location would have none left) and a hang
/// counted as a mismatch.
#[test]
fn a_hung_mirror_does_not_use_up_the_next_sources_deadline() {
    let host = Host::new("mirror-hangs", true);
    let fetcher = with(
        &host,
        &[
            (mirror_url("m1"), Serve::Hang),
            (artifact_url(), Serve::Bytes(ARCHIVE.to_vec())),
        ],
    );
    let pinned = host.run(&fetcher, &entry()).expect("pinned");
    assert!(pinned.other_bytes.is_empty());
}

/// Every source is down (a status, nothing there, a hang): `fetch_failed`, deferred, and nothing
/// is left staged.
///
/// Catches a down source treated as bad bytes.
#[test]
fn every_source_down_defers_with_fetch_failed() {
    let host = Host::new("all-down", true);
    let fetcher = with(
        &host,
        &[
            (mirror_url("m1"), Serve::Status(500)),
            (mirror_url("m2"), Serve::Hang),
            (artifact_url(), Serve::Status(404)),
        ],
    );
    let refusal = host.run(&fetcher, &entry()).expect_err("refused");
    assert_eq!(refusal, deferred("fetch_failed"));
    assert!(host.staged().is_empty());
}

/// With no mirror, every location of the entry giving other bytes is the release's fault.
///
/// Catches wrong bytes at a location treated as a fault of the host.
#[test]
fn every_location_with_other_bytes_fails_the_release() {
    let host = Host::new("all-wrong", false);
    let both = entry_of(ARCHIVE, &[("a", ARTIFACT_AT), ("b", ARTIFACT_AT)]);
    let at_b = format!("https://b.example/{ARTIFACT_AT}");
    let fetcher = with(
        &host,
        &[
            (artifact_url(), Serve::Bytes(wrong())),
            (at_b.clone(), Serve::Bytes(wrong())),
        ],
    );
    let refusal = host.run(&fetcher, &both).expect_err("refused");
    assert_eq!(refusal, release("package_digest_mismatch"));
    assert!(fetcher.urls().ends_with(&[artifact_url(), at_b]));
    assert!(host.staged().is_empty());
}

/// A host that defines no mirror and none of the entry's stores cannot ask anyone: `no_store`, a
/// failure of the environment, and not one request is made. A location of an undefined store is
/// skipped and the next is used; so is a location whose locator breaks the grammar.
///
/// Catches a missing store treated as a down source, and a skipped location that stops the walk.
#[test]
fn no_store_fails_the_environment_and_an_undefined_location_is_skipped() {
    let host = Host::new("no-store", false);
    let no_store = failure(Outcome::Failure, Class::Environment, "no_store");

    let nobody = entry_of(ARCHIVE, &[("nowhere", ARTIFACT_AT)]);
    let fetcher = published(&host);
    let refusal = host.run(&fetcher, &nobody).expect_err("refused");
    assert_eq!(refusal, no_store);
    assert!(fetcher.urls().is_empty());

    let bad_locator = entry_of(ARCHIVE, &[("a", "../etc/passwd")]);
    let refusal = host
        .run(&published(&host), &bad_locator)
        .expect_err("refused");
    assert_eq!(refusal, no_store);

    let skipping = entry_of(ARCHIVE, &[("nowhere", ARTIFACT_AT), ("a", ARTIFACT_AT)]);
    let fetcher = published(&host);
    host.run(&fetcher, &skipping).expect("pinned");
    assert!(!fetcher.urls().iter().any(|u| u.contains("nowhere")));
}

/// A source that sends one byte more than the entry's size (announced, or not announced) is
/// stopped and counts as giving other bytes; one that sends one byte fewer does too.
///
/// Catches a size bound that is off by one, and bytes accepted past the size.
#[test]
fn one_byte_over_or_under_is_other_bytes() {
    let mut over = ARCHIVE.to_vec();
    over.push(b'!');
    let mut under = ARCHIVE.to_vec();
    under.pop();
    for (case, serve) in [
        ("announced over", Serve::Bytes(over.clone())),
        ("unannounced over", Serve::Unannounced(over)),
        ("under", Serve::Bytes(under)),
    ] {
        let host = Host::new("off-by-one", false);
        let fetcher = with(&host, &[(artifact_url(), serve)]);
        let refusal = host.run(&fetcher, &entry()).expect_err(case);
        assert_eq!(refusal, release("package_digest_mismatch"), "{case}");
        assert!(host.staged().is_empty(), "{case}");
    }
}

/// An entry that asks for more than the host allows, or that no fetch can be made for, is
/// refused before any request: bytes over `max_package_bytes` or 0, a digest that is not one.
///
/// Catches the bound checked only after the transfer, and a malformed entry that reaches the
/// network.
#[test]
fn an_entry_that_cannot_be_fetched_is_refused_before_any_request() {
    let host = Host::new("refused-first", true);
    let fetcher = published(&host);
    let mut over = entry();
    over.bytes = 65;
    let mut none = entry();
    none.bytes = 0;
    let mut digest = entry();
    digest.digest = "xyz".to_owned();
    for (case, target) in [
        ("bytes over max_package_bytes", over),
        ("no bytes", none),
        ("a digest that is not one", digest),
    ] {
        let refusal = host.run(&fetcher, &target).expect_err(case);
        assert_eq!(refusal, release("package_digest_mismatch"), "{case}");
    }
    assert!(fetcher.urls().is_empty(), "{:?}", fetcher.urls());
    assert!(host.staged().is_empty());
}

/// Orphan staging files of earlier fetches in the service's directory are removed before the
/// fetch, and other names are left.
///
/// Catches a fetch that leaves an earlier fetch's partial file, or removes a file that is not a
/// staging file.
#[test]
fn orphan_staging_files_are_removed_first() {
    let host = Host::new("orphans", false);
    let dir = host.root().join("svc");
    fs::create_dir(&dir).expect("the service directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    fs::write(dir.join(".fetch-1-1"), "partial").expect("an orphan");
    fs::write(dir.join("1.0.0"), "kept").expect("another file");
    let refusal = host
        .run(&FakeFetcher::new(&host.clock), &entry())
        .expect_err("nothing is served");
    assert_eq!(refusal, deferred("fetch_failed"));
    assert_eq!(host.staged(), ["1.0.0"]);
}

/// A service directory that group or others can write is refused as `fetch_failed`, without a
/// request.
///
/// Catches staging files made in a directory another account can write.
#[test]
fn a_service_directory_others_can_write_is_refused() {
    let host = Host::new("open-dir", false);
    let dir = host.root().join("svc");
    fs::create_dir(&dir).expect("the service directory");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).expect("its mode");
    let fetcher = published(&host);
    let refusal = host.run(&fetcher, &entry()).expect_err("refused");
    assert_eq!(refusal, deferred("fetch_failed"));
    assert!(fetcher.urls().is_empty());
}

/// A store that gave other bytes at two sources (two locations in it) is reported once.
///
/// Catches a store listed once per bad source, which would log one store twice.
#[test]
fn a_store_that_gave_other_bytes_twice_is_reported_once() {
    let host = Host::new("reported-once", false);
    let target: Target = entry_of(
        ARCHIVE,
        &[("a", "x/one"), ("a", "x/two"), ("b", ARTIFACT_AT)],
    );
    let bad = Serve::Bytes(wrong());
    let fetcher = with(
        &host,
        &[
            ("https://a.example/x/one".to_owned(), bad.clone()),
            ("https://a.example/x/two".to_owned(), bad),
            (
                format!("https://b.example/{ARTIFACT_AT}"),
                Serve::Bytes(ARCHIVE.to_vec()),
            ),
        ],
    );
    let pinned = host.run(&fetcher, &target).expect("pinned");
    assert_eq!(pinned.other_bytes, ["a"]);
}

/// Only the artifact's URL is ever asked for, whatever else the entry carries.
///
/// Catches a fetch that asks for a second blob of the entry.
#[test]
fn an_entry_with_an_attestation_field_is_fetched_by_its_artifact_alone() {
    let host = Host::new("no-bundle", false);
    let mut target = entry();
    target.attestation = Some(hostkeeper_proto::hostkeeper::release::v1::Blob {
        digest: sha256(b"a bundle"),
        bytes: 8,
        locations: locations(&[("a", "svc/1.0.0/svc.sigstore.json")]),
    });
    let fetcher = published(&host);
    host.run(&fetcher, &target).expect("pinned");
    assert_eq!(fetcher.urls(), [artifact_url()]);
}
