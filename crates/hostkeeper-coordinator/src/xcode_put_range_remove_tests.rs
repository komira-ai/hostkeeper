//! What the coordinator does with a partial upload besides taking its parts: `Xcode.List` says it,
//! `Xcode.Remove` removes it (`xcode_partial_remove`), and a day without a write lapses it
//! (docs/design/xcode-one-action.md, section 3). The parts come from `xcode_put_range_tests.rs`.

use std::fs;
use std::sync::atomic::Ordering;
use std::time::{Duration, UNIX_EPOCH};

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::XcodeRefusal as Refusal;
use http_body_util::Full;
use hyper::StatusCode;

use super::range::{SIZE, lock_name, part, partial_names, ranged, second_store};
use super::support::{ARCHIVE, Farm, OTHER, ROOMY, farm, farm_with, lines, refusing, uri};
use crate::audit_actions::{XCODE_PARTIAL_REMOVE, XCODE_REMOVE};
use crate::marks_support::operator;
use crate::ops::AuditEntry;
use crate::testutil::ops_of;
use crate::xcode_store::LOCK_STALE;

/// The operator's reason in the removals of these tests.
const WHY: &str = "the download was of the wrong file";

/// A day, the time a partial upload may go without a write.
const DAY: u64 = 24 * 60 * 60;

/// The SHA-256 of [`ARCHIVE`].
fn sha() -> String {
    super::support::digest_of(ARCHIVE)
}

/// The request of the operator to remove the upload `sha256`.
fn remove_of(sha256: &str) -> proto::RemoveXcodeRequest {
    proto::RemoveXcodeRequest {
        sha256: sha256.to_owned(),
        reason: WHY.to_owned(),
    }
}

/// The entries of the audit of `action`, oldest first.
fn entries(farm: &Farm, action: &str) -> Vec<AuditEntry> {
    farm.coordinator
        .ops
        .audit_after(0)
        .expect("the audit")
        .into_iter()
        .filter(|entry| entry.action == action)
        .collect()
}

/// Sets the time of the last write of the file `name` of the store's directory to `unix`.
fn written_at(farm: &Farm, name: &str, unix: u64) {
    fs::OpenOptions::new()
        .write(true)
        .open(farm.dir.join(name))
        .expect("the file")
        .set_modified(UNIX_EPOCH + Duration::from_secs(unix))
        .expect("the time is set");
}

/// A partial upload of the 29 byte `bytes` as Xcode `version` of `build`, of which the first 10
/// bytes were sent.
async fn started(farm: &Farm, bytes: &'static [u8], version: &str, build: &str) {
    let sha256 = super::support::digest_of(bytes);
    let first = ranged(
        &uri(&sha256, version, build),
        0,
        10,
        bytes.len(),
        Full::new(Bytes::from_static(&bytes[..10])),
    );
    assert_eq!(farm.send(first).await.0, StatusCode::ACCEPTED);
}

/// Proves: `Remove` of a SHA-256 the store holds only as a partial upload removes its file and
/// its record, appends one `xcode_partial_remove` entry by the operator (the operator's reason,
/// the release and the SHA-256 as the subject) and no `xcode_remove`, answers that entry, and logs
/// the removal; the query says `none` after it.
///
/// Catches: a partial upload that only a lapse can remove (a wrong file that is resumed again
/// and again), a removal audited as the removal of a stored archive, or one that is not audited.
#[tokio::test]
async fn remove_of_a_partial_upload_removes_it_and_audits_it() {
    let farm = farm("xcode-range-remove");
    farm.send(part(0, 10)).await;
    let sha = sha();

    let answer = farm
        .coordinator
        .xcode_remove(&operator(), remove_of(&sha))
        .expect("removed");

    assert_eq!(listing_of(&farm), Vec::<String>::new());
    let removals = entries(&farm, XCODE_PARTIAL_REMOVE);
    assert_eq!(removals.len(), 1, "{removals:?}");
    let seq = removals[0].seq;
    assert_eq!(
        removals,
        vec![AuditEntry {
            seq,
            by: "operator".to_owned(),
            action: XCODE_PARTIAL_REMOVE.to_owned(),
            service: None,
            node_id: None,
            host_group: None,
            reason: Some(WHY.to_owned()),
            subject: Some(format!("Xcode 27.2 (27B52) {sha}")),
            source: None,
            api_token: None,
        }]
    );
    assert_eq!(entries(&farm, XCODE_REMOVE), Vec::new());
    assert_eq!(
        answer,
        proto::RemoveXcodeResponse {
            entry: Some(proto::AuditEntry {
                seq,
                by: proto::Actor::Operator.into(),
                action: XCODE_PARTIAL_REMOVE.to_owned(),
                reason: Some(WHY.to_owned()),
                by_name: Some("operator".to_owned()),
                ..proto::AuditEntry::default()
            }),
        }
    );
    assert!(
        lines(&farm.log)
            .iter()
            .any(|line| line.contains("a partial Xcode upload was removed")
                && line.contains(&sha)
                && line.contains("received=10")),
        "{:?}",
        lines(&farm.log)
    );
    assert_eq!(
        farm.send(super::range::query(&sha)).await.1,
        serde_json::json!({"state": "none", "received": 0, "size": 0})
    );
}

/// The names in the store's directory of `farm`.
fn listing_of(farm: &Farm) -> Vec<String> {
    super::support::listing(&farm.dir)
}

/// Proves: `Remove` of a SHA-256 the store holds neither as an archive nor as a partial upload is
/// still `unknown_archive`, with another partial upload left alone.
///
/// Catches: a removal of a partial upload that answers success for any SHA-256, or that removes
/// the one it was not asked for.
#[tokio::test]
async fn remove_of_a_sha_with_no_upload_is_unknown_and_touches_no_other_upload() {
    let farm = farm("xcode-range-remove-unknown");
    farm.send(part(0, 10)).await;

    assert_eq!(
        farm.coordinator
            .xcode_remove(&operator(), remove_of(&super::support::digest_of(OTHER))),
        Err(Refusal::UnknownArchive)
    );

    assert_eq!(listing_of(&farm), partial_names());
    assert_eq!(entries(&farm, XCODE_PARTIAL_REMOVE), Vec::new());
}

/// Proves: `Remove` of a partial upload whose lock another writer holds is `partial_locked` and
/// removes nothing and audits nothing, up to the second before the lock is stale; at
/// [`LOCK_STALE`] seconds without a refresh it is made.
///
/// Catches: a removal under a request that is writing the file, and a lock that keeps an
/// abandoned upload from ever being removed.
#[tokio::test]
async fn remove_of_a_locked_partial_upload_is_refused_until_the_lock_is_stale() {
    let farm = farm("xcode-range-remove-locked");
    farm.send(part(0, 10)).await;
    let other = second_store(&farm);
    let _writer = other.lock_upload(&sha()).expect("a writer's lock");
    let made = farm.clock.load(Ordering::SeqCst);

    farm.clock.store(made + LOCK_STALE - 1, Ordering::SeqCst);
    assert_eq!(
        farm.coordinator
            .xcode_remove(&operator(), remove_of(&sha())),
        Err(Refusal::PartialLocked)
    );
    let mut held = partial_names();
    held.push(lock_name());
    held.sort();
    assert_eq!(listing_of(&farm), held);
    assert_eq!(entries(&farm, XCODE_PARTIAL_REMOVE), Vec::new());

    farm.clock.store(made + LOCK_STALE, Ordering::SeqCst);
    farm.coordinator
        .xcode_remove(&operator(), remove_of(&sha()))
        .expect("removed");
    assert_eq!(listing_of(&farm), Vec::<String>::new());
    assert_eq!(entries(&farm, XCODE_PARTIAL_REMOVE).len(), 1);
}

/// Proves: a removal of a partial upload whose audit line cannot be appended is
/// `audit_unavailable` and removes nothing, and the lock it took is released.
///
/// Catches: files removed that the audit does not say, and a lock left by a refused removal that
/// blocks the next part.
#[tokio::test]
async fn a_partial_removal_that_cannot_be_audited_removes_nothing() {
    let farm = farm_with("xcode-range-remove-audit", ROOMY, |coordinator, dir| {
        coordinator.with_ops(ops_of(dir).with_append(refusing()))
    });
    farm.send(part(0, 10)).await;

    assert_eq!(
        farm.coordinator
            .xcode_remove(&operator(), remove_of(&sha())),
        Err(Refusal::AuditUnavailable)
    );

    assert_eq!(listing_of(&farm), partial_names());
    assert_eq!(farm.send(part(10, 20)).await.0, StatusCode::ACCEPTED);
}

/// Proves: `Xcode.List` answers each partial upload, the one written longest ago first, with its
/// SHA-256, version, build, the bytes held, the size, the operator who started it and the time of
/// its last write; an archive stored is no upload; a coordinator without a store answers none.
///
/// Catches: uploads in progress that the console and `hk xcode list` cannot show, an order that
/// changes between calls, and a stored archive listed twice.
#[tokio::test]
async fn list_answers_the_partial_uploads_oldest_first() {
    let farm = farm("xcode-range-list");
    farm.coordinator
        .xcode
        .known
        .keep(&super::support::feed())
        .expect("the list is kept");
    assert_eq!(list(&farm).uploads, Vec::new());

    started(&farm, ARCHIVE, "27.2", "27B52").await;
    started(&farm, OTHER, "27.1", "27B40").await;
    let newer = format!(".partial-{}", sha());
    let older = format!(".partial-{}", super::support::digest_of(OTHER));
    written_at(&farm, &newer, 1_800_000_500);
    written_at(&farm, &older, 1_800_000_100);

    let listed = list(&farm);

    assert_eq!(
        listed.uploads,
        vec![
            proto::XcodeUpload {
                sha256: super::support::digest_of(OTHER),
                version: "27.1".to_owned(),
                build: "27B40".to_owned(),
                received_bytes: 10,
                size_bytes: u64::try_from(OTHER.len()).expect("a size"),
                started_by: "operator".to_owned(),
                last_write_unix: 1_800_000_100,
            },
            proto::XcodeUpload {
                sha256: sha(),
                version: "27.2".to_owned(),
                build: "27B52".to_owned(),
                received_bytes: 10,
                size_bytes: u64::try_from(SIZE).expect("a size"),
                started_by: "operator".to_owned(),
                last_write_unix: 1_800_000_500,
            },
        ]
    );
    assert_eq!(listed.archives, Vec::new());

    // The archive that is stored is no upload; the other upload stays.
    assert_eq!(farm.send(part(10, SIZE)).await.0, StatusCode::CREATED);
    let listed = list(&farm);
    assert_eq!(listed.archives.len(), 1);
    assert_eq!(
        listed
            .uploads
            .iter()
            .map(|upload| upload.sha256.as_str())
            .collect::<Vec<_>>(),
        vec![super::support::digest_of(OTHER).as_str()]
    );
}

/// `Xcode.List` of the coordinator of `farm`.
fn list(farm: &Farm) -> proto::ListXcodeResponse {
    farm.coordinator
        .xcode_list(&operator(), &proto::ListXcodeRequest {})
        .expect("the store is listed")
}

/// Proves: the daily job removes a partial upload not written for more than a day, with its
/// record and its lock, and logs each removal by name, and keeps one written within the day; no
/// audit entry is written for a lapse.
///
/// Catches: partial uploads that stay for ever on a store nobody restarts, a lapse of a partial
/// upload that is still being written, a record or lock left behind, and an audit entry for
/// bytes that were never stored.
#[tokio::test]
async fn the_daily_job_removes_the_partial_uploads_a_day_old() {
    let farm = farm("xcode-range-lapse");
    started(&farm, ARCHIVE, "27.2", "27B52").await;
    started(&farm, OTHER, "27.1", "27B40").await;
    let now = farm.coordinator.ops.unix_now();
    let lapsing = partial_names();
    let kept = format!(".partial-{}", super::support::digest_of(OTHER));
    // A stopped writer's lock, which the lapse removes with the upload it belongs to.
    let other = second_store(&farm);
    let dead = other.lock_upload(&sha()).expect("a writer's lock");
    written_at(&farm, &lapsing[0], now - DAY - 1);
    written_at(&farm, &kept, now - DAY + 60);

    farm.coordinator.xcode_feed_job().await;

    assert_eq!(
        listing_of(&farm),
        vec![kept.clone(), format!("{kept}.json")]
    );
    let said = lines(&farm.log);
    for name in [
        lapsing[0].clone(),
        lapsing[1].clone(),
        format!(".partial-{}.lock", sha()),
    ] {
        assert!(
            said.iter().any(
                |line| line.contains("a lapsed file of the Xcode store was removed")
                    && line.contains(&name)
            ),
            "{name}: {said:?}"
        );
    }
    assert_eq!(entries(&farm, XCODE_REMOVE), Vec::new());
    assert_eq!(entries(&farm, XCODE_PARTIAL_REMOVE), Vec::new());
    drop(dead);
}

/// Proves: a file of the directory that is not a partial upload or an upload file is never
/// removed by a lapse, whatever its age, and the lock of a partial upload with no file is removed
/// by its own age.
///
/// Catches: a sweep that removes an archive or its record because it is old.
#[tokio::test]
async fn a_lapse_removes_only_uploads() {
    let farm = farm("xcode-range-lapse-only");
    assert_eq!(
        farm.send(super::support::push(ARCHIVE)).await.0,
        StatusCode::CREATED
    );
    fs::write(farm.dir.join(lock_name()), "someone 0\n").expect("a lock with no upload");
    let now = farm.coordinator.ops.unix_now();
    let names = listing_of(&farm);
    for name in &names {
        written_at(&farm, name, now - 10 * DAY);
    }

    farm.coordinator.lapse_xcode_uploads();

    assert_eq!(
        listing_of(&farm),
        names
            .into_iter()
            .filter(|name| name != &lock_name())
            .collect::<Vec<_>>()
    );
}

/// Proves: a lapse that cannot be made (the directory is gone) is one error line that says so, and
/// the job goes on.
///
/// Catches: a lapse that fails silently, or one that stops the job's other work.
#[tokio::test]
async fn a_lapse_that_fails_is_logged() {
    let farm = farm("xcode-range-lapse-fails");
    fs::remove_dir_all(&farm.dir).expect("the store's directory goes");

    farm.coordinator.lapse_xcode_uploads();

    assert!(
        lines(&farm.log)
            .iter()
            .any(|line| line.contains("the lapsed uploads of the Xcode store could not be removed")),
        "{:?}",
        lines(&farm.log)
    );
}

/// Proves: a removal of a partial upload is `store_failed` when its record cannot be read, when
/// its lock cannot be examined and when its file cannot be removed; the first two change nothing
/// and write no entry, and the third has written its entry first (the audit comes before the
/// files go) and released its lock.
///
/// Catches: a failure of the disk answered as success or as `unknown_archive`, and a lock left by
/// a failed removal that blocks the next part.
#[tokio::test]
async fn a_partial_removal_that_hits_a_disk_failure_is_store_failed() {
    let farm = farm("xcode-range-remove-disk");
    farm.send(part(0, 10)).await;
    let record = farm.dir.join(&partial_names()[1]);
    let data = farm.dir.join(&partial_names()[0]);
    let lock = farm.dir.join(lock_name());
    let failed = Err(Refusal::StoreFailed);

    let kept = fs::read(&record).expect("the record");
    fs::remove_file(&record).expect("the record goes");
    fs::create_dir(&record).expect("a directory in its place");
    assert_eq!(
        farm.coordinator
            .xcode_remove(&operator(), remove_of(&sha())),
        failed
    );
    fs::remove_dir(&record).expect("the directory goes");
    fs::write(&record, kept).expect("the record is back");

    fs::create_dir(&lock).expect("a directory in the lock's place");
    assert_eq!(
        farm.coordinator
            .xcode_remove(&operator(), remove_of(&sha())),
        failed
    );
    fs::remove_dir(&lock).expect("the directory goes");
    assert_eq!(entries(&farm, XCODE_PARTIAL_REMOVE), Vec::new());

    fs::remove_file(&data).expect("the file goes");
    fs::create_dir(&data).expect("a directory in its place");
    fs::write(data.join("inside"), b"x").expect("a file inside");
    assert_eq!(
        farm.coordinator
            .xcode_remove(&operator(), remove_of(&sha())),
        failed
    );
    assert_eq!(entries(&farm, XCODE_PARTIAL_REMOVE).len(), 1);
    assert!(!lock.exists(), "the lock is released");
}
