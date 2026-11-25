//! The end of an upload that resumes ([`crate::xcode_put_range`]): the lock kept while the whole
//! file is hashed, a partial upload that holds every byte, and what a refused commit leaves. The
//! parts and the rig are those of `xcode_put_range_tests.rs`.

use std::fs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use hyper::StatusCode;
use serde_json::json;

use super::range::{SIZE, lock_name, part, partial_names, ranged, second_store};
use super::support::{
    ARCHIVE, Chunks, Piece, ROOMY, digest_of, farm, farm_with, good_uri, listing, refusing,
};
use crate::testutil::ops_of;
use crate::xcode_store::{LOCK_STALE, UploadLock};

/// Proves: the lock is refreshed while the whole file is hashed: a writer whose lock another
/// coordinator took as stale (the clock passed [`LOCK_STALE`] seconds since its last refresh,
/// which is what a hash that takes long amounts to) is 409 `partial_locked` and commits nothing,
/// where a writer that holds its lock commits; the bytes stay, and once the other writer lets go a
/// request commits them.
///
/// Catches: a hash that runs under a lock that has gone stale, whose writer then stores the
/// archive and removes the partial upload under another coordinator's request.
#[tokio::test]
async fn a_lock_taken_while_the_file_is_hashed_stops_the_commit() {
    let farm = farm("xcode-range-hash-lock");
    let sha = digest_of(ARCHIVE);
    assert_eq!(farm.send(part(0, 10)).await.0, StatusCode::ACCEPTED);
    let other: &'static _ = Box::leak(Box::new(second_store(&farm)));
    let taken: Arc<Mutex<Option<UploadLock<'static>>>> = Arc::default();
    let at_end = {
        let (clock, taken, sha) = (Arc::clone(&farm.clock), Arc::clone(&taken), sha.clone());
        move || {
            // The body has given its last byte: the hash is next, and the time passes.
            clock.fetch_add(LOCK_STALE, Ordering::SeqCst);
            *taken.lock().expect("the guard") =
                Some(other.lock_upload(&sha).expect("a stale lock is taken over"));
        }
    };
    let last = Chunks {
        pieces: vec![Piece::Data(ARCHIVE[10..].to_vec())].into(),
        at_end: Some(Box::new(at_end)),
        ..Chunks::default()
    };

    let answered = farm
        .send(ranged(&good_uri(ARCHIVE), 10, SIZE, SIZE, last))
        .await;

    assert_eq!(
        answered,
        (StatusCode::CONFLICT, json!({"error": "partial_locked"}))
    );
    assert_eq!(farm.puts(), Vec::new());
    assert_eq!(farm.store.archives().expect("the archives"), Vec::new());
    let mut kept = partial_names();
    kept.push(lock_name());
    kept.sort();
    assert_eq!(listing(&farm.dir), kept);
    assert_eq!(
        fs::read(farm.dir.join(&partial_names()[0])).expect("the bytes"),
        ARCHIVE
    );

    taken.lock().expect("the guard").take();
    assert_eq!(farm.send(part(SIZE - 1, SIZE)).await.0, StatusCode::CREATED);
    assert_eq!(
        fs::read(farm.store.archive_path(&sha)).expect("the archive"),
        ARCHIVE
    );
}

/// Proves: a partial upload that holds every byte (the request that wrote the last of them was cut
/// off before the answer) is committed by a part that names any of the archive: the last byte
/// again, or the first part, whose body is not read; the archive is stored, audited once, and the
/// partial upload is gone.
///
/// Catches: a full partial upload that every request refuses as `offset_mismatch` with `received`
/// the whole size, which only a removal clears.
#[tokio::test]
async fn a_partial_upload_that_holds_every_byte_is_committed_by_any_part() {
    for (n, (first, end)) in [(SIZE - 1, SIZE), (0, 10)].into_iter().enumerate() {
        let farm = farm(&format!("xcode-range-full-{n}"));
        let sha = digest_of(ARCHIVE);
        let cut = Chunks {
            pieces: vec![Piece::Data(ARCHIVE.to_vec()), Piece::Fail].into(),
            ..Chunks::default()
        };
        let answered = farm
            .send(ranged(&good_uri(ARCHIVE), 0, SIZE, SIZE, cut))
            .await;
        assert_eq!(answered.0, StatusCode::BAD_REQUEST, "{n}");
        assert_eq!(listing(&farm.dir), partial_names(), "{n}");
        assert_eq!(
            farm.send(super::range::query(&sha)).await.1,
            json!({"state": "partial", "received": SIZE, "size": SIZE}),
            "{n}"
        );

        let answered = farm.send(part(first, end)).await;

        assert_eq!(answered.0, StatusCode::CREATED, "{n}: {answered:?}");
        assert_eq!(
            fs::read(farm.store.archive_path(&sha)).expect("the archive"),
            ARCHIVE,
            "{n}"
        );
        assert_eq!(farm.puts().len(), 1, "{n}");
        for name in partial_names() {
            assert!(!farm.dir.join(&name).exists(), "{n}: {name} is gone");
        }
    }
}

/// Proves: a commit that fails (here the audit line cannot be appended) leaves the partial upload
/// with all its bytes, the answer is the commit's (503 `audit_unavailable`), nothing is stored,
/// and the query still says `partial` with the whole size, so that the bytes are not sent again.
///
/// Catches: a momentary failure of the audit or the store that discards a complete upload and
/// forces the whole archive to be sent again.
#[tokio::test]
async fn a_failed_commit_of_the_last_part_keeps_the_bytes() {
    let farm = farm_with("xcode-range-audit", ROOMY, |coordinator, dir| {
        coordinator.with_ops(ops_of(dir).with_append(refusing()))
    });
    assert_eq!(farm.send(part(0, 10)).await.0, StatusCode::ACCEPTED);

    let answered = farm.send(part(10, SIZE)).await;

    assert_eq!(
        answered,
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "audit_unavailable"})
        )
    );
    assert_eq!(listing(&farm.dir), partial_names());
    assert_eq!(farm.store.archives().expect("the archives"), Vec::new());
    assert_eq!(
        farm.send(super::range::query(&digest_of(ARCHIVE))).await.1,
        json!({"state": "partial", "received": SIZE, "size": SIZE})
    );
}
