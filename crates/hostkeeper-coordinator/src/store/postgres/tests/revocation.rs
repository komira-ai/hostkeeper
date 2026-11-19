//! Sessions that cannot be ended over a database's rows: a logout, a password change, a disabled
//! or deleted user and a reset password answer that the store failed, and the sessions they could
//! not end stay, until the call is made again and ends them.
//!
//! The delete of the `sessions` table is refused by a trigger made after the sessions are; reads
//! and the other writes work, so the failure is that of one write and no session is lost. Nothing
//! sleeps.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hyper::StatusCode;
use serde_json::json;

use super::login_rows::counting_from;
use super::{TestDatabase, off_runtime};
use crate::login::Login;
use crate::login_routes::{LOGOUT_PATH, PASSWORD_PATH, SESSION_PATH};
use crate::login_support::{NEW_PASSWORD, PASSWORD, Seconds, Signed, World, page_post, settings};
use crate::operator::Caller;
use crate::store::postgres::Postgres;
use crate::store::postgres::login_rows::PgLoginRows;

const THIRD: &str = "yet another long password";

/// A coordinator whose sign-in keeps its rows in `store`.
fn coordinator(name: &str, store: &Postgres) -> World {
    let store = store.clone();
    World::build(
        name,
        settings(),
        counting_from(1),
        move |coordinator, rig| {
            let rows = Arc::new(PgLoginRows::new(
                store,
                settings().sessions,
                settings().limiter,
                0,
            ));
            let login = Login::new(
                settings(),
                Arc::new(Seconds(rig.clock.clone())),
                counting_from(1),
            )
            .expect("a sign-in")
            .with_rows(rows);
            coordinator.with_login(login)
        },
    )
}

/// A database, a store on it and a coordinator over the store.
fn start(name: &str) -> (TestDatabase, Postgres, World) {
    let db = TestDatabase::new();
    let dsn = db.dsn();
    let (store, _) =
        off_runtime(move || TestDatabase::open_dsn(&dsn, "revocation").expect("the store opens"));
    let world = coordinator(name, &store);
    (db, store, world)
}

/// Makes the delete of a session fail, as the write of one table that fails with the rest of the
/// database well.
fn refuse_deletes(db: &TestDatabase) {
    db.run(
        "CREATE FUNCTION hostkeeper.refuse() RETURNS trigger LANGUAGE plpgsql AS \
         $$BEGIN RAISE EXCEPTION 'refused by the test'; END$$; \
         CREATE TRIGGER refuse BEFORE DELETE ON hostkeeper.sessions \
         FOR EACH ROW EXECUTE FUNCTION hostkeeper.refuse()",
    )
    .expect("the deletes are refused");
}

/// Lets the deletes work again.
fn allow_deletes(db: &TestDatabase) {
    db.run("DROP TRIGGER refuse ON hostkeeper.sessions")
        .expect("the deletes are allowed");
}

/// How many sessions of `name` the table holds.
fn held(db: &TestDatabase, name: &str) -> String {
    let rows = db.rows(&format!(
        "SELECT count(*) FROM sessions WHERE username = '{name}'"
    ));
    rows[0][0].clone().expect("a count")
}

async fn status(world: &World, session: &Signed) -> StatusCode {
    world.send(session.get(SESSION_PATH)).await.status
}

fn change(current: &str, new: &str) -> String {
    json!({ "current": current, "new": new }).to_string()
}

fn operator(name: &str) -> Caller {
    Caller::Operator(name.parse().expect("a name"))
}

/// Proves: a logout whose session cannot be deleted answers 503 `store_unreachable` with no
/// cookie cleared and no logout audited, the session still answers, and the same logout once the
/// deletes work ends it and is audited.
///
/// Catches: a logout that answers success and clears the cookie while the row, and so the session
/// for every coordinator, lives.
#[tokio::test]
async fn a_logout_that_cannot_end_its_session_answers_unreachable_and_keeps_it() {
    let (db, store, world) = start("revocation-logout");
    let alice = world.sign_in("alice", PASSWORD).await;
    refuse_deletes(&db);

    let sent = world.send(page_post(LOGOUT_PATH, Some(&alice), "{}")).await;
    assert_eq!(sent.status, StatusCode::SERVICE_UNAVAILABLE, "{sent:?}");
    assert_eq!(sent.json(), json!({ "error": "store_unreachable" }));
    assert_eq!(sent.header("set-cookie"), None, "the cookie is not cleared");
    assert_eq!(status(&world, &alice).await, StatusCode::OK);
    assert_eq!(held(&db, "alice"), "1");
    let actions =
        |world: &World| -> Vec<String> { world.said().into_iter().map(|said| said.1).collect() };
    assert_eq!(actions(&world), ["login"], "no logout is audited");

    allow_deletes(&db);
    let sent = world.send(page_post(LOGOUT_PATH, Some(&alice), "{}")).await;
    assert!(sent.status.is_success(), "{sent:?}");
    assert_eq!(status(&world, &alice).await, StatusCode::UNAUTHORIZED);
    assert_eq!(held(&db, "alice"), "0");
    assert_eq!(actions(&world), ["login", "logout"]);
    store.stop();
}

/// Proves: a password change whose other sessions cannot be ended answers 503
/// `store_unreachable`, yet the new password is stored (it signs in, the old one does not) and the
/// other session still answers; the change made again once the deletes work ends the other
/// sessions and keeps the one that made it.
///
/// Catches: a change that answers success with the other sessions alive (a stolen cookie that a
/// password change was to end), and a change that is lost because its sessions could not be ended.
#[tokio::test]
async fn a_password_change_that_cannot_end_other_sessions_says_so_and_ends_them_when_repeated() {
    let (db, store, world) = start("revocation-password");
    let held_by_change = world.sign_in("alice", PASSWORD).await;
    let other = world.sign_in("alice", PASSWORD).await;
    refuse_deletes(&db);

    let sent = world
        .send(held_by_change.post(PASSWORD_PATH, &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::SERVICE_UNAVAILABLE, "{sent:?}");
    assert_eq!(sent.json(), json!({ "error": "store_unreachable" }));
    assert_eq!(status(&world, &other).await, StatusCode::OK);
    assert_eq!(held(&db, "alice"), "2");
    let body = |password: &str| json!({ "username": "alice", "password": password }).to_string();
    let old = world
        .send(page_post("/v1/login", None, &body(PASSWORD)))
        .await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED, "{old:?}");
    let new = world
        .send(page_post("/v1/login", None, &body(NEW_PASSWORD)))
        .await;
    assert_eq!(new.status, StatusCode::OK, "the new password is stored");

    allow_deletes(&db);
    let sent = world
        .send(held_by_change.post(PASSWORD_PATH, &change(NEW_PASSWORD, THIRD)))
        .await;
    assert!(sent.status.is_success(), "{sent:?}");
    assert_eq!(status(&world, &other).await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(&world, &held_by_change).await, StatusCode::OK);
    assert_eq!(held(&db, "alice"), "1", "the one that changed it stays");
    store.stop();
}

/// Proves: disabling a user, deleting a user and resetting a password, when the user's sessions
/// cannot be ended, each answer `store_failed` and leave the session in the table; made again
/// once the deletes work, the reset ends it.
///
/// Catches: an account change that reports success while the user's sessions, which it was to
/// end, still work.
#[tokio::test]
async fn disabling_deleting_and_resetting_a_user_refuse_when_the_sessions_cannot_be_ended() {
    let (db, store, world) = start("revocation-accounts");
    world.sign_in("alice", PASSWORD).await;
    world.sign_in("bob", PASSWORD).await;
    refuse_deletes(&db);
    let c = Arc::clone(&world.coordinator);
    let (disabled, reset, deleted) = off_runtime(move || {
        let by = operator("dora");
        let alice = "alice".to_owned();
        (
            c.users_disable(
                &by,
                &proto::DisableUserRequest {
                    name: alice.clone(),
                },
            ),
            c.users_reset_password(
                &by,
                &proto::ResetPasswordRequest {
                    name: "bob".to_owned(),
                },
            ),
            c.users_delete(&by, &proto::DeleteUserRequest { name: alice }),
        )
    });
    for (call, answer) in [
        ("disable", disabled.map(drop)),
        ("reset", reset.map(drop)),
        ("delete", deleted.map(drop)),
    ] {
        assert_eq!(answer, Err(proto::UsersRefusal::StoreFailed), "{call}");
    }
    assert_eq!(
        (held(&db, "alice").as_str(), held(&db, "bob").as_str()),
        ("1", "1")
    );

    allow_deletes(&db);
    let c = Arc::clone(&world.coordinator);
    off_runtime(move || {
        c.users_reset_password(
            &operator("dora"),
            &proto::ResetPasswordRequest {
                name: "bob".to_owned(),
            },
        )
        .map(drop)
    })
    .expect("a reset that ends the sessions");
    assert_eq!(held(&db, "bob"), "0");
    store.stop();
}
