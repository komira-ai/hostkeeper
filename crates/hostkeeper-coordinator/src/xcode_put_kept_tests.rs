//! `PUT /v1/xcode/<sha256>` of a release the store would remove at once: older than the releases
//! it keeps. The coordinator, the requests and the bodies are in `xcode_put_support_tests.rs`.

use bytes::Bytes;
use http_body_util::Full;
use hyper::StatusCode;
use serde_json::json;
use std::sync::atomic::Ordering;

use super::lands_meanwhile;
use super::support::{
    ARCHIVE, OTHER, bearer, data, digest_of, farm, files_of, listing, request, uri,
};
use crate::testutil::OPERATOR;

/// Proves: with the store holding 27.2 and no group keeping Xcode (the store keeps one release),
/// a push of 27.1, which the list holds as a release, is a 409 `not_kept` whose body is never
/// read; the store is as it was (27.2's two files alone) and no `xcode_put` entry is audited.
///
/// Catches: an upload of a release that the prune removes in the same request, after the body was
/// received, stored and audited.
#[tokio::test]
async fn an_upload_older_than_the_kept_releases_is_refused_and_stores_nothing() {
    let farm = farm("xcode-put-not-kept");
    lands_meanwhile(&farm.store, OTHER, "27.2", "27B52")();
    let body = data(&[ARCHIVE]);
    let polled = body.polled.clone();
    let asked = request(
        "PUT",
        &uri(&digest_of(ARCHIVE), "27.1", "27B40"),
        bearer(OPERATOR),
        body,
    );

    let answered = farm.send(asked).await;

    assert_eq!(
        answered,
        (StatusCode::CONFLICT, json!({"error": "not_kept"}))
    );
    assert!(!polled.load(Ordering::SeqCst), "the body was read");
    assert_eq!(listing(&farm.dir), files_of(&digest_of(OTHER)));
    assert_eq!(farm.puts(), Vec::new());
}

/// Proves: a newer release committed while the body of an older one streamed makes the older one
/// a 409 `not_kept` at the commit: it is neither stored nor audited and its partial is removed;
/// the newer archive stands.
///
/// Catches: a commit that trusts the check made before the body was read, and so stores a release
/// the prune removes at once.
#[tokio::test]
async fn a_newer_release_landed_while_the_body_streamed_makes_the_upload_not_kept() {
    let farm = farm("xcode-put-not-kept-race");
    let mut body = data(&[ARCHIVE]);
    body.at_end = Some(lands_meanwhile(&farm.store, OTHER, "27.2", "27B52"));
    let asked = request(
        "PUT",
        &uri(&digest_of(ARCHIVE), "27.1", "27B40"),
        bearer(OPERATOR),
        body,
    );

    let answered = farm.send(asked).await;

    assert_eq!(
        answered,
        (StatusCode::CONFLICT, json!({"error": "not_kept"}))
    );
    assert_eq!(listing(&farm.dir), files_of(&digest_of(OTHER)));
    assert_eq!(farm.puts(), Vec::new());
}

/// Proves: the same push of 27.1 is stored when the store holds nothing newer.
///
/// Catches: a refusal that applies to every older release rather than to those the store would
/// remove.
#[tokio::test]
async fn an_older_release_is_stored_when_nothing_newer_is_held() {
    let farm = farm("xcode-put-kept-alone");
    let asked = request(
        "PUT",
        &uri(&digest_of(ARCHIVE), "27.1", "27B40"),
        bearer(OPERATOR),
        Full::new(Bytes::from_static(ARCHIVE)),
    );

    assert_eq!(farm.send(asked).await.0, StatusCode::CREATED);
    assert_eq!(listing(&farm.dir), files_of(&digest_of(ARCHIVE)));
}

/// Proves: with the store holding 27.2 and no group keeping Xcode, a push of 27.1 is stored
/// when a Mac's last report names 27.1 (27B40) installed: the answer is 201, the store holds the
/// pushed bytes under their SHA-256, both releases stay (27.2's files and 27.1's), and one
/// `xcode_put` is audited.
///
/// Catches: a `not_kept` check that does not read the Xcodes the Macs report, which refuses a
/// release a Mac runs once a newer one is held.
#[tokio::test]
async fn an_older_release_a_mac_reports_installed_is_stored() {
    let farm = farm("xcode-put-installed");
    lands_meanwhile(&farm.store, OTHER, "27.2", "27B52")();
    let mut heartbeat = crate::decide_maintenance::tests::beat(&[], None);
    heartbeat.platform = hostkeeper_wire::Platform::DarwinArm64;
    heartbeat.maintenance = Some(crate::maintenance::text_of(
        &hostkeeper_proto::hostkeeper::agent::v1::MaintenanceReport {
            xcodes: vec![hostkeeper_proto::hostkeeper::agent::v1::InstalledXcode {
                version: "27.1".to_owned(),
                build: "27B40".to_owned(),
                path: "/Applications/Xcode-27.1.app".to_owned(),
                selected: true,
                foreign: false,
            }],
            ..Default::default()
        },
    ));
    farm.coordinator
        .heartbeat_with_orders(
            crate::marks_support::who("m1", "storage"),
            heartbeat,
            crate::trusted_root::TrustedRootSaid::default(),
        )
        .expect("answered");
    let asked = request(
        "PUT",
        &uri(&digest_of(ARCHIVE), "27.1", "27B40"),
        bearer(OPERATOR),
        Full::new(Bytes::from_static(ARCHIVE)),
    );

    assert_eq!(farm.send(asked).await.0, StatusCode::CREATED);
    let mut both = files_of(&digest_of(ARCHIVE));
    both.extend(files_of(&digest_of(OTHER)));
    both.sort();
    assert_eq!(listing(&farm.dir), both);
    assert_eq!(
        std::fs::read(farm.store.archive_path(&digest_of(ARCHIVE))).expect("the archive"),
        ARCHIVE
    );
    assert_eq!(farm.puts().len(), 1);
}
