//! The change of a password at `POST /v1/password`: the current password it needs, the policy, what
//! a change ends and keeps, the hour of an initial password, a store or an audit that cannot
//! write, a change that races another, the checks of a session's `POST`, and the initial-admin
//! file a change deletes. Time is the number the test sets (`World::at`); no test sleeps.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use hyper::StatusCode;
use serde_json::json;

use crate::initial_admin::InitialAdmin;
use crate::login_support::{
    HOUR, NEW_PASSWORD, PASSWORD, START, Signed, World, cheap, counting, page_post, settings,
};
use crate::password::{Passwords, Verdict};
use crate::store::{audit_failure, store_failure};

fn invalid() -> serde_json::Value {
    json!({ "error": "invalid_credentials" })
}

fn said(
    by: &str,
    action: &str,
    subject: Option<&str>,
) -> (String, String, Option<String>, Option<String>) {
    (
        by.to_owned(),
        action.to_owned(),
        subject.map(str::to_owned),
        None,
    )
}

async fn login(world: &World, name: &str, password: &str) -> crate::login_support::Sent {
    let body = json!({ "username": name, "password": password }).to_string();
    world.send(page_post("/v1/login", None, &body)).await
}

fn verifies(world: &World) -> u64 {
    world.login().passwords.verifies()
}

/// The body of a password change.
fn change(current: &str, new: &str) -> String {
    json!({ "current": current, "new": new }).to_string()
}

/// Proves: a password change needs the current password: a wrong one is 401 `invalid_credentials`,
/// changes nothing and counts as a failed login (the fifth locks the name, and a locked name is
/// 429 `try_later`); a body that is not a change is a 400.
#[tokio::test]
async fn a_password_change_needs_the_current_password_and_counts_a_wrong_one() {
    let world = World::new("password-wrong");
    let alice = world.sign_in("alice", PASSWORD).await;
    let before = world.coordinator.accounts().snapshot();
    for body in [
        "not json",
        r#"{"current":"x"}"#,
        r#"{"current":"x","new":"y","z":1}"#,
    ] {
        let sent = world.send(alice.post("/v1/password", body)).await;
        assert_eq!(sent.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(sent.json(), json!({ "error": "bad_request" }));
    }
    for _ in 0..5 {
        let sent = world
            .send(alice.post("/v1/password", &change("not the password", NEW_PASSWORD)))
            .await;
        assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
        assert_eq!(sent.json(), invalid());
    }
    let locked = world
        .send(alice.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(locked.json(), json!({ "error": "try_later" }));
    assert!(world.coordinator.accounts().snapshot() == before);
    assert_eq!(
        world.said(),
        [
            said("alice", "login", None),
            said("coordinator", "login_failed", Some("alice")),
            said("coordinator", "login_locked", Some("alice")),
        ]
    );
}

/// Proves: a new password that breaks the policy is 400 `password_policy` with the text of the rule
/// and never the password: too short, the user's name, the current password, over 256 bytes.
#[tokio::test]
async fn a_new_password_that_breaks_the_policy_is_refused_with_the_rule() {
    let world = World::new("password-policy");
    let alice = world.sign_in("alice", PASSWORD).await;
    let long = "p".repeat(257);
    let cases = [
        ("short", "the password must be at least 12 characters"),
        (
            PASSWORD,
            "the new password must differ from the current one",
        ),
        (&long, "the password must be at most 256 bytes"),
    ];
    for (new, reason) in cases {
        let sent = world
            .send(alice.post("/v1/password", &change(PASSWORD, new)))
            .await;
        assert_eq!(sent.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            sent.json(),
            json!({ "error": "password_policy", "reason": reason })
        );
    }
    let named = World::new("password-policy-name");
    let mut hash = crate::login_support::user("a-long-user-name");
    hash.must_change = false;
    named
        .coordinator
        .accounts()
        .change(|held| held.with_user(hash))
        .expect("a user");
    let signed = named.sign_in("a-long-user-name", PASSWORD).await;
    let sent = named
        .send(signed.post("/v1/password", &change(PASSWORD, "a-long-user-name")))
        .await;
    assert_eq!(
        sent.json(),
        json!({ "error": "password_policy", "reason": "the password must not be the user name" })
    );
    let held = world.coordinator.accounts().snapshot();
    let alice_hash = held.user("alice").expect("alice").hash.clone();
    assert_eq!(
        Passwords::new(cheap())
            .expect("passwords")
            .verify(PASSWORD, &alice_hash.into()),
        Verdict::Right { rehash: None }
    );
}

/// Proves: a change stores the new hash, clears `must_change` and the deadline, keeps the session
/// that asked (now free of `must_change`), ends every other session of the user and no one
/// else's, audits `password_changed` by the user, and the old password no longer logs in where the
/// new one does.
#[tokio::test]
async fn a_password_change_ends_other_sessions_and_keeps_this_one() {
    let world = World::new("password-change");
    let first = world.sign_in("newbie", PASSWORD).await;
    let second = world.sign_in("newbie", PASSWORD).await;
    let other = world.sign_in("alice", PASSWORD).await;
    let sent = world
        .send(first.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::OK);
    assert_eq!(
        sent.json(),
        json!({ "username": "newbie", "csrf": first.csrf, "must_change": false })
    );
    assert_eq!(sent.header("cache-control"), Some("no-store"));
    let held = world.coordinator.accounts().snapshot();
    let newbie = held.user("newbie").expect("newbie");
    assert!(!newbie.must_change);
    assert_eq!(newbie.initial_expires_at, None);
    assert_eq!(
        world.send(first.get("/v1/session")).await.json()["must_change"],
        json!(false)
    );
    assert_eq!(
        world.send(second.get("/v1/session")).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        world.send(other.get("/v1/session")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        login(&world, "newbie", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&world, "newbie", NEW_PASSWORD).await.status,
        StatusCode::OK
    );
    let said = world.said();
    assert_eq!(
        said[3],
        (
            "newbie".to_owned(),
            "password_changed".to_owned(),
            None,
            None
        )
    );
}

/// Uses the session at three times, the last `until`, so it is never idle for its 30 minutes: what
/// the page does while a person types a new password.
async fn keep_alive(world: &World, session: &Signed, until: u64) {
    for at in [START + 1700, START + 3400, until] {
        world.at(at);
        assert_eq!(
            world.send(session.get("/v1/session")).await.status,
            StatusCode::OK
        );
    }
}

/// Proves: an initial password is changed up to its deadline and not after: at the last second it
/// is, at the deadline it is 401 `initial_password_expired` and nothing changes, and so is a
/// voided one; a session that outlived the deadline is still a session.
#[tokio::test]
async fn an_initial_password_is_changed_before_its_hour_is_out_and_not_after() {
    let world = World::new("password-deadline");
    let session = world.sign_in("newbie", PASSWORD).await;
    keep_alive(&world, &session, START + HOUR - 1).await;
    let sent = world
        .send(session.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::OK, "the last second of the hour");

    let world = World::new("password-deadline-late");
    let session = world.sign_in("newbie", PASSWORD).await;
    keep_alive(&world, &session, START + HOUR).await;
    let sent = world
        .send(session.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), json!({ "error": "initial_password_expired" }));
    assert_eq!(
        world.send(session.get("/v1/session")).await.status,
        StatusCode::OK,
        "the session outlives the hour"
    );

    let world = World::new("password-voided");
    let session = world.sign_in("newbie", PASSWORD).await;
    world
        .coordinator
        .accounts()
        .change(|held| held.with_user_edited("newbie", |user| user.initial_void = true))
        .expect("voided");
    let sent = world
        .send(session.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.json(), json!({ "error": "initial_password_expired" }));
}

/// Proves: a session whose user was disabled or deleted cannot change a password: 401
/// `sign_in_required`.
#[tokio::test]
async fn a_user_who_is_gone_cannot_change_a_password() {
    let world = World::new("password-gone");
    let alice = world.sign_in("alice", PASSWORD).await;
    world
        .coordinator
        .accounts()
        .change(|held| held.with_enabled("alice", false))
        .expect("disabled");
    let sent = world
        .send(alice.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), json!({ "error": "sign_in_required" }));
}

/// Proves: a store that cannot write refuses the change 503 `store_unreachable` and the old
/// password still works; an audit that cannot be written refuses it 500 `internal`, changes
/// nothing and leaves the other sessions.
#[tokio::test]
async fn a_change_that_cannot_be_stored_or_audited_changes_nothing() {
    let world = World::new("password-down");
    let alice = world.sign_in("alice", PASSWORD).await;
    let second = world.sign_in("alice", PASSWORD).await;
    world
        .fake
        .refuse(|commit| commit.accounts.is_some().then(store_failure));
    let sent = world
        .send(alice.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(sent.json(), json!({ "error": "store_unreachable" }));
    world
        .fake
        .refuse(|commit| commit.accounts.is_none().then(audit_failure));
    let sent = world
        .send(alice.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(sent.json(), json!({ "error": "internal" }));
    world.fake.heal();
    assert_eq!(
        world.send(second.get("/v1/session")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        login(&world, "alice", PASSWORD).await.status,
        StatusCode::OK
    );
}

/// Proves: a change made while another one was being hashed is refused (the password it checked
/// is no longer the user's), and a session ended while the change ran is told to sign in.
#[tokio::test]
async fn a_change_that_raced_another_is_refused() {
    let world = World::new("password-raced");
    let alice = world.sign_in("alice", PASSWORD).await;
    let weak = Arc::downgrade(&world.coordinator);
    world.login().when_hashed(move || {
        let coordinator = weak.upgrade().expect("the coordinator");
        let other = Passwords::new(cheap())
            .expect("passwords")
            .hash("the other change won")
            .expect("a hash");
        coordinator
            .accounts()
            .change(|held| {
                held.with_user_edited("alice", |user| user.hash = other.as_str().to_owned())
            })
            .expect("changed");
    });
    let sent = world
        .send(alice.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), invalid());

    let world = World::new("password-signed-out");
    let bob = world.sign_in("bob", PASSWORD).await;
    let weak = Arc::downgrade(&world.coordinator);
    let id = bob.id.clone();
    world.login().when_hashed(move || {
        let coordinator = weak.upgrade().expect("the coordinator");
        coordinator.login().expect("a sign-in").sessions.end(&id);
    });
    let sent = world
        .send(bob.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), json!({ "error": "sign_in_required" }));
}

/// Proves: a change refuses a session's call that fails the checks of a session's `POST`: no CSRF
/// header, a wrong one, another origin, a cross-site request, a body not said to be JSON; each is
/// 403 `csrf` and changes nothing. No session at all is 401 `sign_in_required`.
#[tokio::test]
async fn a_password_change_is_a_session_post_and_nothing_else() {
    let world = World::new("password-csrf");
    let alice = world.sign_in("alice", PASSWORD).await;
    let body = change(PASSWORD, NEW_PASSWORD);
    let mutations: [(&str, Option<&str>); 6] = [
        ("x-hk-csrf", None),
        ("x-hk-csrf", Some("wrong")),
        ("origin", Some("https://evil.test")),
        ("sec-fetch-site", Some("same-site")),
        ("content-type", Some("text/plain")),
        ("content-type", None),
    ];
    for (name, value) in mutations {
        let mut request = alice.post("/v1/password", &body);
        request.headers_mut().remove(name);
        if let Some(value) = value {
            request
                .headers_mut()
                .insert(name, value.parse().expect("a value"));
        }
        let sent = world.send(request).await;
        assert_eq!(sent.status, StatusCode::FORBIDDEN, "{name}: {value:?}");
        assert_eq!(sent.json(), json!({ "error": "csrf" }));
    }
    assert_eq!(verifies(&world), 1, "only the login hashed");
    let sent = world.send(page_post("/v1/password", None, &body)).await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), json!({ "error": "sign_in_required" }));
}

/// A world whose coordinator looks at an initial-admin file in its directory.
fn with_file(name: &str) -> World {
    World::build(name, settings(), counting(), |coordinator, rig| {
        let clock = Arc::clone(&rig.clock);
        let admin = InitialAdmin::new(
            rig.dir.path().join("initial-admin.toml"),
            HOUR,
            cheap(),
            Arc::new(move || clock.load(std::sync::atomic::Ordering::SeqCst)),
        )
        .expect("an initial admin");
        coordinator.with_initial_admin(admin)
    })
}

fn put_file(world: &World, id: &str, user: &str, valid_until: u64, mode: u32) {
    let path = world.rig.dir.path().join("initial-admin.toml");
    let _ = fs::remove_file(&path);
    fs::write(
        &path,
        format!(
            "id = \"{id}\"\nuser = \"{user}\"\npassword = \"{PASSWORD}\"\nvalid_until = {valid_until}\n"
        ),
    )
    .expect("written");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
}

fn file_exists(world: &World) -> bool {
    world.rig.dir.path().join("initial-admin.toml").exists()
}

/// Proves: the whole first-admin path: a file is adopted into the user `admin` with the file's
/// password, who signs in with it and is told to change it, changes it, and the file that held the
/// old password is deleted by that change; the new password logs in and the file's does not.
#[tokio::test]
async fn the_first_change_deletes_the_file_that_made_the_password() {
    let world = with_file("password-file");
    put_file(&world, "first", "admin", START + HOUR, 0o400);
    world.coordinator.look_at_initial_admin().expect("a look");
    let session = world.sign_in("admin", PASSWORD).await;
    assert_eq!(
        world.send(session.get("/v1/session")).await.json()["must_change"],
        json!(true)
    );
    assert!(file_exists(&world));
    let sent = world
        .send(session.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::OK);
    assert!(!file_exists(&world), "the file is deleted by the change");
    assert_eq!(
        login(&world, "admin", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&world, "admin", NEW_PASSWORD).await.status,
        StatusCode::OK
    );
}

/// Proves: a file of another id (a later one, for a lost password) is not the one the change
/// answers for and stays; and a file that does not read is left for its expiry, the change is made
/// all the same.
#[tokio::test]
async fn a_change_leaves_a_file_it_did_not_come_from_and_one_that_does_not_read() {
    let world = with_file("password-other-file");
    put_file(&world, "first", "admin", START + HOUR, 0o400);
    world.coordinator.look_at_initial_admin().expect("a look");
    let session = world.sign_in("admin", PASSWORD).await;
    put_file(&world, "second", "admin", START + HOUR, 0o400);
    let sent = world
        .send(session.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::OK);
    assert!(file_exists(&world), "another id's file stays");

    let world = with_file("password-bad-file");
    put_file(&world, "first", "admin", START + HOUR, 0o400);
    world.coordinator.look_at_initial_admin().expect("a look");
    let session = world.sign_in("admin", PASSWORD).await;
    put_file(&world, "first", "admin", START + HOUR, 0o644);
    let sent = world
        .send(session.post("/v1/password", &change(PASSWORD, NEW_PASSWORD)))
        .await;
    assert_eq!(sent.status, StatusCode::OK);
    assert!(file_exists(&world), "a file that does not read stays");
}
