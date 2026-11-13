//! The spent ids of CI's tokens across a restart (design section 8.8, last sentence), in the
//! file beside the state file, and the lock that keeps a second coordinator off the state
//! files (design section 8.1). The wall clock is the testkit's settable one: an id's expiry is
//! the one reading of it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use hostkeeper_oidc::testkit::{
    NOW, STATIC_ISSUER, StaticKeys, clock, config_text, good_claims, jwks_of, keys, mint,
};
use hostkeeper_oidc::{
    Clock, LEEWAY_SECS, MAX_REMEMBERED_JTIS, OidcConfig, Refusal, SpentJtis, Verifier,
};
use serde_json::json;

use crate::jti_file::{COMPACT_SLACK, JtiError, JtiFile, jti_path};
use crate::lock::{FileLock, LockError, StateLock, lock_path};
use crate::ops_tests::temp_beside;
use crate::service::{Coordinator, SetError};
use crate::testutil::{LEGACY, OPERATOR, clock_of, kbf_prefixes, kbf_services, scratch};

/// A verifier for the static issuer whose spent ids are kept in `file`.
fn verifier(file: JtiFile, clock: Clock) -> Verifier {
    Verifier::new(
        OidcConfig::parse(&config_text(STATIC_ISSUER)).expect("a config"),
        Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
        Duration::from_secs(30),
        clock,
        Arc::new(file),
    )
}

/// A token of the static issuer whose `jti` is `jti`, valid at [`NOW`] for 300 seconds.
fn token(jti: &str) -> String {
    let mut claims = good_claims(STATIC_ISSUER);
    claims["jti"] = json!(jti);
    mint(&keys()[0], &claims)
}

/// The second until which a token of [`token`] is refused.
const UNTIL: u64 = NOW + 300 + LEEWAY_SECS;

/// The file of spent ids of a state file in a directory of the test's own.
fn jti_file(name: &str) -> (hostkeeper_testkit::Scratch, PathBuf) {
    let dir = scratch(name);
    let path = jti_path(&dir.path().join("desired.json"));
    (dir, path)
}

fn open(path: &Path, clock: &Clock) -> JtiFile {
    JtiFile::open(path, Arc::clone(clock)).expect("the file of spent ids")
}

/// Catches: an id spent in memory only, so a restart opens a window to replay CI's token: the
/// id is in the file once the token is accepted, and a verifier made over the file again (a
/// restart) refuses the token.
#[tokio::test]
async fn a_spent_jti_stays_spent_across_a_restart() {
    let (_dir, path) = jti_file("jti-restart");
    let (clock, _) = clock();
    let before = verifier(open(&path, &clock), Arc::clone(&clock));
    before.verify(&token("spent")).await.expect("the first use");
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        format!("spent {UNTIL}\n")
    );
    let after = verifier(open(&path, &clock), clock);
    assert_eq!(after.verify(&token("spent")).await, Err(Refusal::Replayed));
}

/// Catches: expired ids kept at a start (the file would only grow) or a live one dropped with
/// them: the start drops the first and keeps the second, in the file too; a missing file starts
/// empty.
#[test]
fn an_expired_jti_is_dropped_at_start() {
    let (_dir, path) = jti_file("jti-expired");
    let (clock, _) = clock();
    assert!(open(&path, &clock).load().is_empty());
    assert_eq!(fs::read_to_string(&path).expect("made"), "");
    fs::write(&path, format!("old {NOW}\nlive {}\n", NOW + 1)).expect("written");
    let file = open(&path, &clock);
    assert_eq!(file.load(), [("live".to_owned(), NOW + 1)]);
    assert_eq!(
        fs::read_to_string(&path).expect("rewritten"),
        format!("live {}\n", NOW + 1)
    );
}

/// Catches: a full cache that makes room by forgetting a live id, or that writes the id it
/// refuses: a start over a file of live ids at the cap refuses a new token, writes nothing,
/// and still refuses a held one.
#[tokio::test]
async fn a_full_jti_cache_refuses_a_new_token_and_forgets_none() {
    let (_dir, path) = jti_file("jti-full");
    let text: String = (0..MAX_REMEMBERED_JTIS)
        .map(|n| format!("held-{n} {UNTIL}\n"))
        .collect();
    fs::write(&path, &text).expect("written");
    let (clock, _) = clock();
    let full = verifier(open(&path, &clock), clock);
    assert_eq!(
        full.verify(&token("new")).await,
        Err(Refusal::ReplayCacheFull)
    );
    assert_eq!(full.verify(&token("held-0")).await, Err(Refusal::Replayed));
    assert_eq!(fs::read_to_string(&path).expect("the file"), text);
}

/// Catches: a token accepted when its id could not be written (a restart would accept it
/// again): the file replaced by a directory refuses CI's set as unavailable, with nothing set;
/// and an id that would not read back as one word is refused the same way.
#[tokio::test]
async fn a_jti_that_cannot_be_written_refuses_the_token() {
    let (dir, path) = jti_file("jti-unwritable");
    let (oidc_clock, _) = clock();
    let file = open(&path, &oidc_clock);
    assert!(
        file.record("a b", UNTIL).is_err(),
        "an id with a space would be two words"
    );
    let store = crate::desired::DesiredStore::open(&dir.path().join("desired.json"), Some(LEGACY))
        .expect("a store");
    let coordinator = Coordinator::new(
        store,
        Arc::new(verifier(file, oidc_clock)),
        OPERATOR.to_owned(),
        clock_of(&Arc::new(std::sync::atomic::AtomicU64::new(1_000))),
    )
    .with_allow_list(
        &crate::testutil::allow_list(&config_text(STATIC_ISSUER)),
        Some(LEGACY),
    )
    .with_services(kbf_services())
    .with_legacy_service(Some(LEGACY.to_owned()))
    .with_prefixes(kbf_prefixes());
    fs::remove_file(&path).expect("removed");
    fs::create_dir(&path).expect("a directory in its place");
    let body = br#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#;
    let set = coordinator
        .set_desired(Some(&token("unwritten")), body)
        .await;
    assert!(matches!(set, Err(SetError::Unavailable)), "{set:?}");
    assert!(
        coordinator
            .status(Some(OPERATOR))
            .expect("status")
            .services
            .is_empty()
    );
    let odd = coordinator.set_desired(Some(&token("a\tb")), body).await;
    assert!(matches!(odd, Err(SetError::Unavailable)), "{odd:?}");
}

/// Catches: a file of spent ids read leniently, so a damaged one starts with ids forgotten:
/// each malformed line stops the open, naming the file and the line; so does a file that cannot
/// be read.
#[test]
fn a_malformed_jti_file_stops_start() {
    let (_dir, path) = jti_file("jti-malformed");
    let (clock, _) = clock();
    for (text, line) in [
        (format!("a {UNTIL}\nb\n"), 2),
        (format!("a {UNTIL}\nb {UNTIL}"), 2),
        (format!("a {UNTIL}\n b\n"), 2),
        ("a -1\n".to_owned(), 1),
        ("a 1 2\n".to_owned(), 1),
        ("a +1\n".to_owned(), 1),
        ("a \n".to_owned(), 1),
        (" 1\n".to_owned(), 1),
        ("a 99999999999999999999999\n".to_owned(), 1),
    ] {
        fs::write(&path, &text).expect("written");
        match JtiFile::open(&path, Arc::clone(&clock)) {
            Err(JtiError::Malformed { path: p, line: n }) => {
                assert_eq!((p.as_path(), n), (path.as_path(), line), "{text:?}");
            }
            other => panic!("{text:?}: {other:?}"),
        }
    }
    fs::write(&path, [0xff, b' ', b'1', b'\n']).expect("written");
    let why = JtiFile::open(&path, Arc::clone(&clock)).expect_err("not UTF-8");
    assert!(why.to_string().contains("line 1"), "{why}");
    assert!(why.to_string().contains("desired.json.jti"), "{why}");
    fs::remove_file(&path).expect("removed");
    fs::create_dir(&path).expect("a directory in its place");
    assert!(matches!(
        JtiFile::open(&path, clock),
        Err(JtiError::Io { .. })
    ));
}

/// Catches: a file that only grows while the coordinator runs, or a rewrite that loses a live
/// id or fails the spend: once the lines outnumber the live ids by the slack, the file is
/// rewritten with the live ids alone; a rewrite that fails leaves the longer file and the spend
/// stands.
#[test]
fn the_file_is_rewritten_shorter_while_it_runs() {
    let (_dir, path) = jti_file("jti-compact");
    let (clock, now) = clock();
    let file = open(&path, &clock);
    for n in 0..=COMPACT_SLACK {
        file.record(&format!("old-{n}"), NOW + 10)
            .expect("recorded");
    }
    now.store(NOW + 20, Ordering::SeqCst);
    let blocker = temp_beside(&path);
    fs::create_dir(&blocker).expect("a directory where the rewrite goes");
    file.record("kept", NOW + 100).expect("the spend stands");
    let lines = fs::read_to_string(&path).expect("the file").lines().count();
    assert_eq!(lines, COMPACT_SLACK + 2, "the longer file is left");
    fs::remove_dir(&blocker).expect("unblocked");
    file.record("last", NOW + 100).expect("recorded");
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        format!("kept {}\nlast {}\n", NOW + 100, NOW + 100)
    );
    assert_eq!(file.load().len(), 2);
    assert!(format!("{file:?}").contains("desired.json.jti"));
}

/// Catches: a lock that a second holder in the same process gets too, or one kept after its
/// guard is gone: the second take of the same file is `Held` until the first guard is dropped;
/// a lock file that cannot be made is an error of its own.
#[test]
fn the_file_lock_is_held_by_one_holder_at_a_time() {
    let dir = scratch("lock-file");
    let path = lock_path(&dir.path().join("desired.json"));
    assert!(path.ends_with("desired.json.lock"));
    let first = FileLock.acquire(&path).expect("the first holder");
    assert!(matches!(FileLock.acquire(&path), Err(LockError::Held)));
    assert!(format!("{first:?}").contains("LockGuard"));
    drop(first);
    let _again = FileLock.acquire(&path).expect("free again");
    let missing = dir.path().join("missing").join("desired.json.lock");
    let why = FileLock.acquire(&missing).expect_err("no directory");
    assert!(matches!(why, LockError::Io(_)), "{why}");
    assert_eq!(LockError::Held.to_string(), "held by another process");
}

/// Catches: a lock that cannot be taken for a reason other than another holder read as held,
/// or as taken.
#[test]
fn a_lock_that_fails_otherwise_is_an_error_of_its_own() {
    let dir = scratch("lock-error");
    let file = fs::File::create(dir.path().join("x.lock")).expect("a file");
    let why = crate::lock::locked(
        Err(std::fs::TryLockError::Error(std::io::Error::other(
            "no locks here",
        ))),
        file,
    )
    .expect_err("not taken");
    assert!(matches!(why, LockError::Io(_)), "{why}");
}
