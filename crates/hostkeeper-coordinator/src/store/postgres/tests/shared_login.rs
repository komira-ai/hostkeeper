//! Two coordinators over one database, signing browsers in through the routes (`login_rows` as
//! the login routes use them): a cookie one coordinator made is answered by the other, an end of
//! it by either is an end for both, and the failed logins of one name counted by both together
//! lock it for both.
//!
//! The clock is a number the rig sets; nothing sleeps.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hyper::StatusCode;

use super::{TestDatabase, off_runtime};
use crate::enrollment::Random;
use crate::login::Login;
use crate::login_routes::{LOGIN_PATH, LOGOUT_PATH, SESSION_PATH};
use crate::login_support::{PASSWORD, Seconds, World, page_post, settings};
use crate::store::postgres::Postgres;
use crate::store::postgres::login_rows::PgLoginRows;

const LIST: &str = "/hostkeeper.operator.v1.Hosts/List";

/// Random bytes that never repeat, from `first` on: two coordinators draw from their own ranges,
/// as two coordinators' operating systems do not repeat each other.
fn counting_from(first: u64) -> Random {
    let next = AtomicU64::new(first);
    Box::new(move |buffer| {
        let n = next.fetch_add(1, Ordering::SeqCst).to_le_bytes();
        for (i, byte) in buffer.iter_mut().enumerate() {
            *byte = n[i % 8].wrapping_add(u8::try_from(i).unwrap_or(0));
        }
        true
    })
}

/// Two stores on one database, as two coordinators of a start-first update hold it.
fn stores() -> (TestDatabase, Postgres, Postgres) {
    let db = TestDatabase::new();
    let (first, second) = off_runtime({
        let (a, b) = (db.dsn(), db.dsn());
        move || {
            let (first, _) = TestDatabase::open_dsn(&a, "first").expect("the first opens");
            let (second, _) = TestDatabase::open_dsn(&b, "second").expect("the second opens");
            (first, second)
        }
    });
    (db, first, second)
}

/// A coordinator whose sign-in keeps its rows in `store`, its random bytes from `first`.
fn coordinator(name: &str, store: &Postgres, first: u64) -> World {
    let store = store.clone();
    World::build(
        name,
        settings(),
        counting_from(first),
        move |coordinator, rig| {
            let rows = PgLoginRows::new(store, settings().sessions, settings().limiter);
            let login = Login::new(
                settings(),
                Arc::new(Seconds(rig.clock.clone())),
                counting_from(first),
            )
            .expect("a sign-in")
            .with_rows(rows);
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
    let (_db, first, second) = stores();
    let a = coordinator("shared-login-a", &first, 1);
    let b = coordinator("shared-login-b", &second, 1_000_000);
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
}

/// Proves: the failed logins of one name are counted by both coordinators together: with the
/// limit of 5, three wrong passwords on one and two on the other lock the name, so the sixth
/// attempt, with the right password, on either is told to try later; another name is not locked;
/// the locked name's right password is refused on both.
///
/// Catches: a limiter held in each coordinator's memory, which lets a guesser make as many
/// attempts as there are coordinators times the limit.
#[tokio::test]
async fn failed_logins_are_counted_by_both_coordinators_together() {
    let (_db, first, second) = stores();
    let a = coordinator("shared-limit-a", &first, 1);
    let b = coordinator("shared-limit-b", &second, 1_000_000);
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
}

/// Proves: the table of sessions over the database's rows does what the in-memory table does for
/// the calls the routes make beside a login: a session that must change its password has it
/// cleared by either coordinator (and the deadline with it), ending every session of a user but
/// the one named ends the others on both coordinators, and the count held is the rows'.
///
/// Catches: a change of password on one coordinator that leaves the browser of the other asking
/// for it again, and sessions of a disabled user that live on in the other coordinator.
#[test]
fn a_session_changed_or_ended_by_one_coordinator_is_so_for_the_other() {
    use crate::session::{SessionSettings, Sessions};

    let (db, first, second) = stores();
    let clock = Arc::new(Seconds(Arc::new(AtomicU64::new(1_000))));
    let over = |store: &Postgres, from| {
        Sessions::new(
            SessionSettings::default(),
            clock.clone(),
            counting_from(from),
        )
        .over(PgLoginRows::new(
            store.clone(),
            SessionSettings::default(),
            crate::session::LimiterSettings::default(),
        ))
    };
    let (a, b) = (over(&first, 1), over(&second, 1_000_000));
    let (kept, view) = a.create("carol", true, Some(5_000)).expect("a session");
    assert_eq!(view.must_change_until, Some(5_000));
    let (other, _) = b.create("carol", false, None).expect("a session");
    let (dave, _) = b.create("dave", false, None).expect("a session");
    assert_eq!(a.len(), 3);

    let seen = b.lookup(kept.as_str()).expect("a's session");
    assert!(seen.must_change && seen.must_change_until == Some(5_000));
    let cleared = b.clear_must_change(kept.as_str()).expect("a's session");
    assert!(!cleared.must_change && cleared.must_change_until.is_none());
    let seen = a.lookup(kept.as_str()).expect("still there");
    assert!(!seen.must_change && seen.must_change_until.is_none());

    assert_eq!(a.end_user("carol", Some(kept.as_str())), 1);
    assert!(b.lookup(other.as_str()).is_none(), "ended for both");
    assert!(b.lookup(kept.as_str()).is_some(), "the one named stays");
    assert_eq!(b.end_user("carol", None), 1);
    assert!(a.lookup(kept.as_str()).is_none());
    assert!(a.end(dave.as_str()));
    assert!(!b.end(dave.as_str()), "there was none to end");
    assert!(a.is_empty() && b.is_empty());
    drop(db);
}
