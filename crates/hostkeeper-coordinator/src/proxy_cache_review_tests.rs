//! The package proxy cache's races and slow readers: a key committed while its asker is on the
//! way, a stale hit that finds its file gone, a replacing commit that fails, and an asker that
//! does not read.

use std::fs;

use super::tests::{Fake, Plan, digest, get, key, limits, open, origin, read, scratch};
use super::{Claim, FetchError, Flight, Served};

/// Proves: an ask for a key that is already committed is never made the leader of a second fill:
/// the claim says it is cached, and no flight is left in the books.
///
/// Catches: an asker that found no entry, lost the race to a commit, and fetched the object again.
#[tokio::test]
async fn a_key_committed_before_the_claim_is_not_led_again() {
    let dir = scratch("pp1-claim");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::whole(b"abc"));
    read(get(&cache, "c", &origin).await.expect("an answer"))
        .await
        .expect("the body");
    let claim = cache.claim(&digest("c"));
    assert!(matches!(claim, Claim::Cached));
    assert!(cache.shared.state().flights.is_empty());
    let hit = get(&cache, "c", &origin).await.expect("an answer");
    assert_eq!(hit.served, Served::Hit);
    assert_eq!(fake.calls(), 1);
}

/// Proves: a hit that read an object and then found its file gone forgets only that object: when
/// the key has since been committed again, the new object, its files and its bytes are kept.
///
/// Catches: a late forget that deletes a freshly committed object.
#[tokio::test]
async fn a_stale_forget_leaves_a_newer_object_alone() {
    let dir = scratch("pp1-stale");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::whole(b"0123456789"));
    read(get(&cache, "s", &origin).await.expect("an answer"))
        .await
        .expect("the body");
    let name = digest("s");
    let stale = cache.shared.state().entries[&name].born;
    fs::remove_file(dir.path().join("objects").join(&name)).expect("wiped");
    let refilled = get(&cache, "s", &origin).await.expect("an answer");
    assert_eq!(refilled.served, Served::Filled);
    read(refilled).await.expect("the body");

    cache.shared.forget(&name, stale);

    assert_eq!(cache.bytes(), 10);
    assert!(dir.path().join("objects").join(&name).exists());
    let hit = get(&cache, "s", &origin).await.expect("an answer");
    assert_eq!(hit.served, Served::Hit);
    assert_eq!(read(hit).await.expect("the body"), b"0123456789");
    assert_eq!(fake.calls(), 2);
}

/// Proves: a commit that replaces a committed object and fails at its first step leaves the books
/// and the disk agreeing: the old object is not counted and its files are deleted.
///
/// Catches: old files that stay on the disk, counted nowhere, past the cache's size.
#[tokio::test]
async fn a_replacing_commit_that_fails_leaves_no_untracked_files() {
    let dir = scratch("pp1-replace");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::whole(b"abcdef"));
    read(get(&cache, "r", &origin).await.expect("an answer"))
        .await
        .expect("the body");
    let name = digest("r");
    assert_eq!(cache.bytes(), 6);

    // The second fill's sidecar cannot be written: its place is a directory.
    let tmp_body = cache.shared.layout.tmp(&name, 1);
    fs::create_dir(format!("{}.meta", tmp_body.display())).expect("a blocker");
    let flight = Flight {
        shared: cache.shared.clone(),
        digest: name.clone(),
        finished: false,
    };
    let answer = cache
        .lead(&key("r"), flight, &origin, &limits())
        .await
        .expect("an answer");
    assert_eq!(read(answer).await.expect("the body"), b"abcdef");

    assert_eq!(cache.bytes(), 0);
    assert!(cache.shared.state().entries.is_empty());
    assert!(!dir.path().join("objects").join(&name).exists());
    assert_eq!(cache.stats().store_failures, 1);
}

/// Proves: an asker that does not read does not stall a fill that is being stored: the fill
/// commits, a waiter gets the file as a hit and not a pass-through, and the asker, when it reads,
/// gets the frames that were ahead and then an error that says it fell behind.
///
/// Catches: one stuck client holding the flight and a place of `proxy_fetches` open for the rest.
#[tokio::test(start_paused = true)]
async fn an_asker_that_does_not_read_does_not_stall_the_fill() {
    let dir = scratch("pp1-behind");
    let cache = open(dir.path());
    let fake = Fake::new();
    let frames: Vec<&[u8]> = vec![b"0123456789"; 20];
    let origin = origin(&fake, move |_| Plan::frames(&frames));
    let leader = get(&cache, "b", &origin).await.expect("an answer");
    assert_eq!(leader.served, Served::Filled);

    let waiter = get(&cache, "b", &origin).await.expect("an answer");
    assert_eq!(waiter.served, Served::Hit);
    assert_eq!(read(waiter).await.expect("the body").len(), 200);
    assert_eq!(fake.calls(), 1);
    assert_eq!(cache.stats().store_failures, 0);
    assert_eq!(cache.bytes(), 200);

    assert_eq!(read(leader).await, Err(FetchError::Behind));
}

/// Proves: `open_hit` opens a committed object's file and reads its bytes with no read of the
/// origin, counts a hit, and is none for a key that is not committed; an object whose file was
/// wiped is forgotten (no entry, no counted bytes) and the next ask fills it again.
///
/// Catches: a pkg served from a file the books no longer hold, and a wiped cache that is never
/// refilled.
#[tokio::test]
async fn an_open_hit_is_the_committed_file_and_a_wiped_one_is_forgotten() {
    use std::io::Read as _;

    let dir = scratch("pp3-open-hit");
    let cache = open(dir.path());
    let fake = Fake::new();
    let origin = origin(&fake, |_| Plan::whole(b"0123456789"));
    assert!(cache.open_hit(&key("o")).is_none());
    read(get(&cache, "o", &origin).await.expect("an answer"))
        .await
        .expect("the body");
    let mut text = String::new();
    cache
        .open_hit(&key("o"))
        .expect("a committed object")
        .read_to_string(&mut text)
        .expect("its bytes");
    assert_eq!(text, "0123456789");
    assert_eq!((fake.calls(), cache.stats().hits), (1, 1));
    let files = dir.path().join("objects");
    for entry in fs::read_dir(&files).expect("the objects") {
        fs::remove_file(entry.expect("an entry").path()).expect("wiped");
    }
    assert!(cache.open_hit(&key("o")).is_none());
    assert_eq!(cache.bytes(), 0);
    let again = get(&cache, "o", &origin).await.expect("an answer");
    assert_eq!(again.served, Served::Filled);
    read(again).await.expect("the body");
    assert!(cache.open_hit(&key("o")).is_some());
}
