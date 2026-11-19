//! Coordinators over one database sharing the login: a cookie one made is answered by the other,
//! the failed logins of a name are counted by all together, and sessions and failures made at
//! once on several coordinators pass neither the cap nor the count.
//!
//! The rows judge by the database's own clock here. Races are made with barriers, in several
//! rounds; nothing sleeps.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Barrier};

use hyper::StatusCode;
use sha2::{Digest as _, Sha256};

use super::login_rows::{counting_from, limits, sessions_of};
use super::{TestDatabase, off_runtime};
use crate::login::Login;
use crate::login_routes::{LOGIN_PATH, LOGOUT_PATH, SESSION_PATH};
use crate::login_support::{PASSWORD, Seconds, World, page_post, settings};
use crate::password::HashSlots;
use crate::session::{Failure, LimiterSettings, LoginLimiter, Refusal, SessionSettings, Sessions};
use crate::store::postgres::Postgres;
use crate::store::postgres::login_rows::PgLoginRows;

const LIST: &str = "/hostkeeper.operator.v1.Hosts/List";

/// `n` stores on one database, as `n` coordinators of a start-first update hold it.
fn stores(n: usize) -> (TestDatabase, Vec<Postgres>) {
    let db = TestDatabase::new();
    let dsns: Vec<_> = (0..n).map(|_| db.dsn()).collect();
    let stores = off_runtime(move || {
        dsns.iter()
            .enumerate()
            .map(|(i, dsn)| {
                TestDatabase::open_dsn(dsn, &format!("coordinator {i}"))
                    .expect("a store opens")
                    .0
            })
            .collect()
    });
    (db, stores)
}

fn stop(stores: Vec<Postgres>) {
    stores.iter().for_each(Postgres::stop);
}

/// The rows of `store` on the database's own clock.
fn rows_of(
    store: &Postgres,
    sessions: SessionSettings,
    limiter: LimiterSettings,
) -> Arc<PgLoginRows> {
    Arc::new(PgLoginRows::new(store.clone(), sessions, limiter, 0))
}

fn table(store: &Postgres, settings: SessionSettings, first: u64) -> Sessions {
    let clock = Arc::new(Seconds(Arc::new(AtomicU64::new(1_000))));
    Sessions::new(settings, clock, counting_from(first)).over(rows_of(
        store,
        settings,
        LimiterSettings::default(),
    ))
}

fn counter(store: &Postgres, settings: LimiterSettings) -> LoginLimiter {
    let clock = Arc::new(Seconds(Arc::new(AtomicU64::new(1_000))));
    let slots = HashSlots::new(std::num::NonZeroUsize::new(2).expect("two"));
    LoginLimiter::new(settings, clock, slots).over(rows_of(
        store,
        SessionSettings::default(),
        settings,
    ))
}

/// A coordinator whose sign-in keeps its rows in `store`, its random bytes from `first`.
fn coordinator(name: &str, store: &Postgres, first: u64) -> World {
    let store = store.clone();
    World::build(
        name,
        settings(),
        counting_from(first),
        move |coordinator, rig| {
            let login = Login::new(
                settings(),
                Arc::new(Seconds(rig.clock.clone())),
                counting_from(first),
            )
            .expect("a sign-in")
            .with_rows(rows_of(&store, settings().sessions, settings().limiter));
            coordinator.with_login(login)
        },
    )
}

/// Proves: a user signed in on one coordinator is signed in on the other: the cookie and the CSRF
/// token the first made are accepted by the second for a call and for the session's own answer;
/// the second's logout ends the session for the first; and a session the first made after that is
/// not confused with it.
///
/// Catches: sessions held in each coordinator's memory, so that a start-first update signs every
/// browser out and a second coordinator answers every other request "signed out".
#[tokio::test]
async fn a_cookie_made_by_one_coordinator_is_answered_by_the_other() {
    let (_db, stores) = stores(2);
    let a = coordinator("shared-login-a", &stores[0], 1);
    let b = coordinator("shared-login-b", &stores[1], 1_000_000);
    let alice = a.sign_in("alice", PASSWORD).await;

    let on_b = b.send(alice.get(SESSION_PATH)).await;
    assert_eq!(on_b.status, StatusCode::OK, "{on_b:?}");
    assert_eq!(on_b.json()["csrf"], alice.csrf);
    assert_eq!(
        b.send(alice.post(LIST, "{}")).await.status,
        StatusCode::OK,
        "a call with a cookie of the other coordinator"
    );

    let out = b.send(page_post(LOGOUT_PATH, Some(&alice), "{}")).await;
    assert!(out.status.is_success(), "{out:?}");
    for world in [&a, &b] {
        assert_eq!(
            world.send(alice.get(SESSION_PATH)).await.status,
            StatusCode::UNAUTHORIZED,
            "the logout of one is the end of the session for both"
        );
    }
    let bob = b.sign_in("bob", PASSWORD).await;
    assert_eq!(a.send(bob.get(SESSION_PATH)).await.status, StatusCode::OK);
    assert_eq!(
        a.login().sessions.len(),
        1,
        "the rows are one table, whichever coordinator is asked"
    );
    stop(stores);
}

/// Proves: the failed logins of one name are counted by both coordinators together: with the
/// limit of 5, three wrong passwords on one and two on the other lock the name, so an attempt with
/// the right password on either is told to try later; another name is not locked.
///
/// Catches: a limiter held in each coordinator's memory, which lets a guesser make as many
/// attempts as there are coordinators times the limit.
#[tokio::test]
async fn failed_logins_are_counted_by_both_coordinators_together() {
    let (_db, stores) = stores(2);
    let a = coordinator("shared-limit-a", &stores[0], 1);
    let b = coordinator("shared-limit-b", &stores[1], 1_000_000);
    let wrong = |name: &str| {
        let body = serde_json::json!({ "username": name, "password": "not the password" });
        page_post(LOGIN_PATH, None, &body.to_string())
    };
    for (world, count) in [(&a, 3), (&b, 2)] {
        for _ in 0..count {
            let sent = world.send(wrong("alice")).await;
            assert_eq!(sent.status, StatusCode::UNAUTHORIZED, "{sent:?}");
        }
    }
    let right = |name: &str| {
        let body = serde_json::json!({ "username": name, "password": PASSWORD });
        page_post(LOGIN_PATH, None, &body.to_string())
    };
    for world in [&a, &b] {
        let sent = world.send(right("alice")).await;
        assert_eq!(sent.status, StatusCode::TOO_MANY_REQUESTS, "{sent:?}");
    }
    assert_eq!(b.send(right("bob")).await.status, StatusCode::OK);
    assert_eq!(a.login().limiter.tracked(), 1);
    stop(stores);
}

/// Proves: a session made on one coordinator is valid on the other: the other finds it, changes
/// it (the change of password clears `must_change` and its deadline for both), uses it by its
/// handle and ends it, ending it again finds none, and the end of a user's sessions on one ends
/// them on the other, sparing the one named; the table holds the SHA-256 of the cookie's id and
/// not the id, and the second of the database's clock, not the caller's.
///
/// Catches: a change or end that one coordinator does not see in the other, a cookie value in the
/// table (a copy of the database would then hold working cookies), and rows judged by a
/// coordinator's own clock.
#[test]
fn session_is_valid_on_the_other_task() {
    let (db, stores) = stores(2);
    let settings = SessionSettings::default();
    let (a, b) = (
        table(&stores[0], settings, 1),
        table(&stores[1], settings, 1_000_000),
    );
    let (kept, made) = a.create("carol", true, Some(5_000)).expect("made on a");
    let (other, _) = b.create("carol", false, None).expect("made on b");
    let (dave, _) = b.create("dave", false, None).expect("made on b");
    assert_eq!((a.len(), b.len()), (3, 3));

    let seen = b.lookup(kept.as_str()).expect("a's session on b");
    assert_eq!(seen, made);
    assert!(seen.must_change);
    assert_eq!(seen.must_change_until, Some(5_000));
    let cleared = b
        .clear_must_change(kept.as_str())
        .expect("a's session on b");
    assert!(!cleared.must_change && cleared.must_change_until.is_none());
    assert_eq!(a.lookup(kept.as_str()), Some(cleared));
    assert!(b.touch(&Sessions::key_of(kept.as_str())));
    assert!(a.is_live(&Sessions::key_of(other.as_str())));

    let hash = Sha256::digest(kept.as_str().as_bytes());
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        db.rows(
            "SELECT encode(id_sha256, 'hex') FROM sessions WHERE username = 'carol' ORDER BY serial"
        )[0],
        [Some(hex)]
    );
    for id in [kept.as_str(), other.as_str(), dave.as_str()] {
        let held = db.rows("SELECT id_sha256::text || username || csrf FROM sessions");
        assert!(
            held.iter()
                .flatten()
                .flatten()
                .all(|text| !text.contains(id)),
            "no row holds the cookie's id"
        );
    }
    let created = db.rows("SELECT created_unix FROM sessions WHERE username = 'dave'");
    let created: u64 = created[0][0]
        .as_deref()
        .expect("a time")
        .parse()
        .expect("a number");
    assert!(
        created > 1_700_000_000,
        "the database's second, not the test clock's 1000"
    );

    assert_eq!(a.end_user("carol", Some(kept.as_str())), Ok(1));
    assert_eq!(b.lookup(other.as_str()), None, "ended for both");
    assert!(b.lookup(kept.as_str()).is_some(), "the one named stays");
    assert_eq!(b.end_user("carol", None), Ok(1));
    assert_eq!(a.lookup(kept.as_str()), None);
    assert_eq!(a.end(dave.as_str()), Ok(true));
    assert_eq!(b.end(dave.as_str()), Ok(false), "there was none to end");
    assert!(a.is_empty() && b.is_empty());
    stop(stores);
}

/// Proves: failed logins of one name split over two coordinators are counted together: with the
/// limit of 5, three on one and two on the other are a first, three counted and the lock, the name
/// is then locked on both (a login is refused as locked on both, one that is not locked is
/// admitted), the lock is of the name alone, and a success on one clears it for the other.
///
/// Catches: counts kept per coordinator, which let a guesser have the limit times the number of
/// coordinators in attempts.
#[test]
fn login_failures_count_across_tasks() {
    let (_db, stores) = stores(2);
    let limiter = limits(5, 900, 100);
    let (a, b) = (counter(&stores[0], limiter), counter(&stores[1], limiter));
    assert_eq!(a.failed("mallory"), Failure::First);
    assert_eq!(b.failed("mallory"), Failure::Counted);
    assert_eq!(a.failed("mallory"), Failure::Counted);
    assert_eq!(b.failed("mallory"), Failure::Counted);
    assert!(!a.is_locked("mallory") && !b.is_locked("mallory"));
    assert_eq!(a.failed("mallory"), Failure::Locked);
    for side in [&a, &b] {
        assert!(side.is_locked("mallory"));
        assert_eq!(side.admit("mallory").map(|_| ()), Err(Refusal::Locked));
        assert!(side.admit("nobody").is_ok());
        assert_eq!(side.tracked(), 1);
    }
    b.succeeded("mallory");
    assert!(!a.is_locked("mallory"));
    assert_eq!(a.tracked(), 0);
    stop(stores);
}

/// Proves: sessions made at once on three coordinators never pass `--session-max-count`: in each
/// of six rounds, three coordinators make three sessions each at the same moment, and exactly 4
/// are held afterwards, whatever the order the makes took.
///
/// Catches: a cap that is counted and then inserted without a lock, which lets simultaneous logins
/// each see room and all insert.
#[test]
fn sessions_made_together_never_pass_the_cap() {
    let (db, stores) = stores(3);
    let settings = sessions_of(1800, 43_200, 4);
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = stores
        .iter()
        .enumerate()
        .map(|(i, store)| {
            let table = table(store, settings, 1 + 1_000_000 * i as u64);
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                for round in 0..6 {
                    barrier.wait();
                    for _ in 0..3 {
                        table.create("u", false, None).expect("made");
                    }
                    barrier.wait();
                    let held = table.len();
                    assert_eq!(held, 4, "round {round}");
                    barrier.wait();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("the coordinator's thread");
    }
    assert_eq!(
        db.rows("SELECT count(*) FROM sessions"),
        [[Some("4".to_owned())]]
    );
    stop(stores);
}

/// Proves: failed logins of one name made at once on three coordinators are all counted and lock
/// the name once: in each of five rounds, on a name of its own, three coordinators count three
/// failures each at the same moment, with the limit of 5 exactly one is the first, exactly one is
/// the lock and seven are counted, and the stored count is nine.
///
/// Catches: two failures that read the same count and write the same count (one lost), and two
/// failures that both see the fifth and both lock (two audit lines for one lock).
#[test]
fn failures_counted_together_are_all_counted_and_lock_once() {
    let (db, stores) = stores(3);
    let limiter = limits(5, 900, 100);
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = stores
        .iter()
        .map(|store| {
            let counts = counter(store, limiter);
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                for round in 0..5 {
                    barrier.wait();
                    for _ in 0..3 {
                        seen.push((round, counts.failed(&format!("name{round}"))));
                    }
                }
                seen
            })
        })
        .collect();
    let seen: Vec<(i32, Failure)> = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("the coordinator's thread"))
        .collect();
    for round in 0..5 {
        let of = |wanted: Failure| {
            seen.iter()
                .filter(|(r, failure)| *r == round && *failure == wanted)
                .count()
        };
        assert_eq!(of(Failure::First), 1, "round {round}");
        assert_eq!(of(Failure::Locked), 1, "round {round}");
        assert_eq!(of(Failure::Counted), 7, "round {round}");
        assert_eq!(
            db.rows(&format!(
                "SELECT failures FROM login_failures WHERE name = 'name{round}'"
            )),
            [[Some("9".to_owned())]]
        );
    }
    stop(stores);
}
