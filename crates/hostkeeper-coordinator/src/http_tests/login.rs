//! The sign-in routes of the private listener, driven through the router with the requests a
//! browser sends: a login and its cookie, the same refusal for every wrong login, the lock, the
//! hash cap, the session answer and logout. The change of a password is in `password.rs`. Time is
//! the number the test sets (`World::at`); no test sleeps.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use serde_json::{Value, json};

use crate::http::{PeerAddr, Surface};
use crate::login::LoginSettings;
use crate::login_support::{
    COOKIE, HOST, HOUR, NEW_PASSWORD, PASSWORD, START, Sent, Signed, World, cheap, counting,
    page_post, settings,
};
use crate::password::{PasswordSettings, Passwords, Verdict};
use crate::store::{audit_failure, store_failure};

fn credentials(name: &str, password: &str) -> String {
    json!({ "username": name, "password": password }).to_string()
}

async fn login(world: &World, name: &str, password: &str) -> Sent {
    world
        .send(page_post("/v1/login", None, &credentials(name, password)))
        .await
}

fn verifies(world: &World) -> u64 {
    world.login().passwords.verifies()
}

fn invalid() -> Value {
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

/// Proves: a login sets the cookie with exactly the attributes of the design and no other, answers
/// the user's name, the session's CSRF token and `must_change`, is not cached, and is on the
/// audit as the user's `login` with the peer's address when the listener knew it.
#[tokio::test]
async fn a_login_sets_the_cookie_of_the_design_and_answers_the_csrf_token() {
    let world = World::new("login-cookie");
    let sent = login(&world, "alice", PASSWORD).await;
    assert_eq!(sent.status, StatusCode::OK);
    let set = sent.header("set-cookie").expect("a cookie");
    let (pair, attributes) = set.split_once("; ").expect("attributes");
    assert_eq!(attributes, "Secure; HttpOnly; SameSite=Strict; Path=/");
    let id = pair
        .strip_prefix(&format!("{COOKIE}="))
        .expect("the cookie's name");
    assert_eq!(id.len(), 43, "32 bytes, base64url");
    assert_eq!(sent.headers.get_all("set-cookie").iter().count(), 1);
    assert_eq!(sent.header("cache-control"), Some("no-store"));
    assert_eq!(sent.header("content-type"), Some("application/json"));
    let csrf = sent.json()["csrf"].as_str().expect("a token").to_owned();
    assert_eq!(csrf.len(), 43);
    assert_ne!(csrf, id);
    assert_eq!(
        sent.json(),
        json!({ "username": "alice", "csrf": csrf, "must_change": false })
    );
    assert_eq!(world.said(), [said("alice", "login", None)]);

    let mut from_peer = page_post("/v1/login", None, &credentials("bob", PASSWORD));
    from_peer
        .extensions_mut()
        .insert(PeerAddr("192.0.2.7:51000".parse().expect("an address")));
    assert_eq!(world.send(from_peer).await.status, StatusCode::OK);
    assert_eq!(
        world.audit()[1].source.as_deref(),
        Some("192.0.2.7"),
        "the address without the port"
    );
}

/// Proves: an unknown name, a wrong password, a disabled user and an initial password past its
/// hour are answered with the same status, headers and bytes, each spent exactly one password
/// verify (the count, not the time), and none made a session. The first failure of each name is
/// on the audit as `login_failed` by `coordinator` about the name.
#[tokio::test]
async fn every_wrong_login_is_the_same_answer_and_spends_one_verify() {
    let world = World::new("login-same");
    let cases = [
        ("nobody", PASSWORD),
        ("alice", "not the password"),
        ("dora", PASSWORD),
        ("late", PASSWORD),
    ];
    let mut answers = Vec::new();
    for (name, password) in cases {
        let before = verifies(&world);
        let sent = login(&world, name, password).await;
        assert_eq!(verifies(&world) - before, 1, "{name}");
        answers.push(sent);
    }
    for sent in &answers {
        assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
        assert_eq!(sent.json(), invalid());
        assert_eq!(sent.headers, answers[0].headers);
        assert_eq!(sent.body, answers[0].body);
        assert!(sent.header("set-cookie").is_none());
    }
    assert!(world.login().sessions.is_empty());
    assert_eq!(
        world.said(),
        [
            said("coordinator", "login_failed", Some("nobody")),
            said("coordinator", "login_failed", Some("alice")),
            said("coordinator", "login_failed", Some("dora")),
            said("coordinator", "login_failed", Some("late")),
        ]
    );
}

/// Proves: the fifth failure of a name locks it: the audit holds the first failure and the lock
/// and not the failures between; a login of the locked name, with the right password, is 429
/// `try_later` and starts no hash; the lock ends by the clock, at its last second and not before.
#[tokio::test]
async fn the_fifth_failure_locks_the_name_and_the_lock_ends_by_the_clock() {
    let world = World::new("login-lock");
    for _ in 0..5 {
        let sent = login(&world, "alice", "not the password").await;
        assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    }
    let before = verifies(&world);
    let locked = login(&world, "alice", PASSWORD).await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(locked.json(), json!({ "error": "try_later" }));
    assert_eq!(verifies(&world), before, "no hash for a locked name");
    world.at(START + 899);
    assert_eq!(
        login(&world, "alice", PASSWORD).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    world.at(START + 900);
    assert_eq!(
        login(&world, "alice", PASSWORD).await.status,
        StatusCode::OK
    );
    assert_eq!(
        world.said(),
        [
            said("coordinator", "login_failed", Some("alice")),
            said("coordinator", "login_locked", Some("alice")),
            said("alice", "login", None),
        ]
    );
    // A success cleared the count: four more failures do not lock.
    for _ in 0..4 {
        login(&world, "alice", "not the password").await;
    }
    assert_eq!(
        login(&world, "alice", PASSWORD).await.status,
        StatusCode::OK
    );
}

/// Proves: a login that finds every hash slot taken is 429 `try_later`, starts no hash and counts
/// no failure; with the slot given back it is answered.
#[tokio::test]
async fn a_login_with_no_free_hash_slot_is_told_to_try_later() {
    let world = World::with(
        "login-slots",
        LoginSettings {
            max_hashes: 1,
            ..settings()
        },
        counting(),
    );
    let slot = world.login().limiter.admit("someone").expect("a slot");
    let before = verifies(&world);
    let sent = login(&world, "alice", PASSWORD).await;
    assert_eq!(sent.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(sent.json(), json!({ "error": "try_later" }));
    assert_eq!(verifies(&world), before);
    assert_eq!(world.login().limiter.tracked(), 0);
    drop(slot);
    assert_eq!(
        login(&world, "alice", PASSWORD).await.status,
        StatusCode::OK
    );
}

/// Proves: a body that is not a login is a 400 `bad_request` that starts no hash, counts nothing
/// and makes no session: not JSON, a missing key, an extra key, a name that breaks the grammar
/// (capitals, empty, too long), no password, a password over 256 bytes, and a body over 4 KiB.
#[tokio::test]
async fn a_body_that_is_not_a_login_is_a_bad_request() {
    let world = World::new("login-bad-body");
    let long_name = "a".repeat(33);
    let long_password = "p".repeat(257);
    let huge = format!(
        r#"{{"username":"alice","password":"{}"}}"#,
        "p".repeat(4096)
    );
    let bodies = [
        "not json".to_owned(),
        r#"{"username":"alice"}"#.to_owned(),
        r#"{"username":"alice","password":"x","more":1}"#.to_owned(),
        credentials("Alice", PASSWORD),
        credentials("", PASSWORD),
        credentials(&long_name, PASSWORD),
        credentials("alice", ""),
        credentials("alice", &long_password),
        huge,
    ];
    for body in bodies {
        let sent = world.send(page_post("/v1/login", None, &body)).await;
        assert_eq!(sent.status, StatusCode::BAD_REQUEST, "{body:.60}");
        assert_eq!(sent.json(), json!({ "error": "bad_request" }));
    }
    assert_eq!(verifies(&world), 0);
    assert_eq!(world.login().limiter.tracked(), 0);
    assert!(world.login().sessions.is_empty());
    assert!(world.audit().is_empty());
}

/// Proves: a login from another site is refused 403 `csrf` before the body is read: another
/// `Origin`, the origin `null`, an `Origin` with no authority asked, `Sec-Fetch-Site:
/// cross-site`, and a body that does not say it is JSON; none starts a hash or makes a session.
#[tokio::test]
async fn a_login_from_another_site_is_refused_as_csrf() {
    let world = World::new("login-csrf");
    let body = credentials("alice", PASSWORD);
    let mutations: [(&str, Option<&str>); 5] = [
        ("origin", Some("https://evil.test")),
        ("origin", Some("null")),
        ("sec-fetch-site", Some("cross-site")),
        ("content-type", Some("text/plain")),
        ("content-type", None),
    ];
    for (name, value) in mutations {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/login")
            .header("host", HOST)
            .header("origin", format!("https://{HOST}"))
            .header("sec-fetch-site", "same-origin")
            .header("content-type", "application/json");
        builder = builder.header(name, "placeholder");
        let mut request = builder
            .body(Full::new(Bytes::from(body.clone())))
            .expect("a request");
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
    // An origin when the request names no authority at all.
    let no_host = Request::builder()
        .method("POST")
        .uri("/v1/login")
        .header("origin", format!("https://{HOST}"))
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.clone())))
        .expect("a request");
    assert_eq!(world.send(no_host).await.status, StatusCode::FORBIDDEN);
    assert_eq!(verifies(&world), 0);
    assert!(world.login().sessions.is_empty());
    // The same body with the page's headers, and with the JSON type in capitals and a charset,
    // is a login.
    let mut request = page_post("/v1/login", None, &body);
    request.headers_mut().insert(
        "content-type",
        "Application/JSON; charset=utf-8".parse().expect("a value"),
    );
    assert_eq!(world.send(request).await.status, StatusCode::OK);
    // A client with no browser headers at all (`hkctl`, a script) is a login too.
    let plain = Request::builder()
        .method("POST")
        .uri("/v1/login")
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("a request");
    assert_eq!(world.send(plain).await.status, StatusCode::OK);
}

/// Proves: a login of a user whose stored hash is weaker than the settings stores a new hash of
/// the password under the settings, and the user's last sign-in time; the password still works.
#[tokio::test]
async fn a_login_stores_the_last_sign_in_and_a_stronger_hash() {
    let stronger = PasswordSettings {
        memory_kib: 16,
        ..cheap()
    };
    let world = World::with(
        "login-rehash",
        LoginSettings {
            passwords: stronger,
            ..settings()
        },
        counting(),
    );
    world.at(START + 5);
    assert_eq!(
        login(&world, "alice", PASSWORD).await.status,
        StatusCode::OK
    );
    let held = world.coordinator.accounts().snapshot();
    let alice = held.user("alice").expect("alice");
    assert_eq!(alice.last_login_at, Some(START + 5));
    assert!(
        alice.hash.starts_with("$argon2id$v=19$m=16,t=1,p=1$"),
        "{}",
        alice.hash
    );
    assert_eq!(
        Passwords::new(stronger)
            .expect("passwords")
            .verify(PASSWORD, &alice.hash.clone().into()),
        Verdict::Right { rehash: None }
    );
}

/// Proves: a failed login whose audit entry cannot be written is still refused as any wrong login
/// is, and still counted: the audit is not what the refusal waits for.
#[tokio::test]
async fn a_failure_the_audit_cannot_record_is_still_refused_and_counted() {
    let world = World::new("login-failure-unaudited");
    world.fake.fail();
    let sent = login(&world, "alice", "not the password").await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), invalid());
    assert_eq!(world.login().limiter.tracked(), 1);
    world.fake.heal();
    assert!(world.audit().is_empty());
}

/// Proves: a store that cannot write refuses the login 503 `store_unreachable`: no cookie, no
/// session, nothing on the audit; once it can, the login is made.
#[tokio::test]
async fn a_store_that_cannot_write_refuses_the_login() {
    let world = World::new("login-store-down");
    world
        .fake
        .refuse(|commit| commit.accounts.is_some().then(store_failure));
    let sent = login(&world, "alice", PASSWORD).await;
    assert_eq!(sent.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(sent.json(), json!({ "error": "store_unreachable" }));
    assert!(sent.header("set-cookie").is_none());
    assert!(world.login().sessions.is_empty());
    assert!(world.audit().is_empty());
    world.fake.heal();
    assert_eq!(
        login(&world, "alice", PASSWORD).await.status,
        StatusCode::OK
    );
}

/// Proves: a login whose audit entry cannot be written is 500 `internal` and leaves no session
/// behind; a login whose session cannot get random bytes is 500 `internal` and audits nothing.
#[tokio::test]
async fn a_login_that_cannot_be_audited_or_given_a_session_is_refused() {
    let world = World::new("login-audit-down");
    world
        .fake
        .refuse(|commit| commit.accounts.is_none().then(audit_failure));
    let sent = login(&world, "alice", PASSWORD).await;
    assert_eq!(sent.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(sent.json(), json!({ "error": "internal" }));
    assert!(sent.header("set-cookie").is_none());
    assert!(world.login().sessions.is_empty(), "the session was ended");

    let blind = World::with("login-no-random", settings(), Box::new(|_| false));
    let sent = login(&blind, "alice", PASSWORD).await;
    assert_eq!(sent.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(blind.audit().is_empty());
}

/// Proves: a user disabled, or given another password, while the password was being hashed gets
/// no session and is not counted as a failure: the password was right when the hash began.
#[tokio::test]
async fn a_user_changed_while_the_password_is_hashed_gets_no_session() {
    let world = World::new("login-raced");
    let weak = Arc::downgrade(&world.coordinator);
    world.login().when_hashed(move || {
        let coordinator = weak.upgrade().expect("the coordinator");
        coordinator
            .accounts()
            .change(|held| held.with_enabled("alice", false))
            .expect("disabled");
    });
    let sent = login(&world, "alice", PASSWORD).await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), invalid());
    assert!(world.login().sessions.is_empty());
    assert_eq!(world.login().limiter.tracked(), 0);

    let weak = Arc::downgrade(&world.coordinator);
    world.login().when_hashed(move || {
        let coordinator = weak.upgrade().expect("the coordinator");
        let other = Passwords::new(cheap())
            .expect("passwords")
            .hash(NEW_PASSWORD)
            .expect("a hash");
        coordinator
            .accounts()
            .change(|held| {
                held.with_user_edited("bob", |user| user.hash = other.as_str().to_owned())
            })
            .expect("changed");
    });
    assert_eq!(
        login(&world, "bob", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert!(world.login().sessions.is_empty());
}

/// Proves: a password stored by another request after the login's checks passed and before its
/// write is not overwritten by the login's rehash or its last sign-in time: the compare is made
/// in the store's own step, the login is refused, and no session is made.
#[tokio::test]
async fn a_password_stored_after_the_checks_is_not_undone_by_a_rehash() {
    let stronger = PasswordSettings {
        memory_kib: 16,
        ..cheap()
    };
    let world = World::with(
        "login-rehash-raced",
        LoginSettings {
            passwords: stronger,
            ..settings()
        },
        counting(),
    );
    let other = Passwords::new(cheap())
        .expect("passwords")
        .hash(NEW_PASSWORD)
        .expect("a hash");
    let stored = other.as_str().to_owned();
    let weak = Arc::downgrade(&world.coordinator);
    world.login().when_checked(move || {
        let coordinator = weak.upgrade().expect("the coordinator");
        let other = stored.clone();
        coordinator
            .accounts()
            .change(|held| held.with_user_edited("alice", |user| user.hash = other))
            .expect("changed");
    });
    let sent = login(&world, "alice", PASSWORD).await;
    assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
    assert_eq!(sent.json(), invalid());
    assert!(world.login().sessions.is_empty());
    let held = world.coordinator.accounts().snapshot();
    let alice = held.user("alice").expect("alice");
    assert_eq!(alice.hash, other.as_str());
    assert_eq!(alice.last_login_at, None);
}

/// Proves: `GET /v1/session` answers a live session's user, CSRF token and `must_change` with the
/// cookie alone (no CSRF header), the deadline only for a session that must change its password,
/// and 401 `sign_in_required` for no cookie, an unknown one, an `Authorization` header (the
/// cookie is not read beside a bearer), and a session past its idle or absolute deadline; each use
/// pushes the idle deadline out.
#[tokio::test]
async fn the_session_route_answers_a_live_session_and_no_other() {
    let world = World::new("session-route");
    let alice = world.sign_in("alice", PASSWORD).await;
    let sent = world.send(alice.get("/v1/session")).await;
    assert_eq!(sent.status, StatusCode::OK);
    assert_eq!(
        sent.json(),
        json!({ "username": "alice", "csrf": alice.csrf, "must_change": false })
    );
    assert_eq!(sent.header("cache-control"), Some("no-store"));
    assert!(sent.header("set-cookie").is_none());

    let newbie = world.sign_in("newbie", PASSWORD).await;
    assert_eq!(
        world.send(newbie.get("/v1/session")).await.json(),
        json!({
            "username": "newbie",
            "csrf": newbie.csrf,
            "must_change": true,
            "must_change_until": START + HOUR
        })
    );

    let signed_out = json!({ "error": "sign_in_required" });
    let anonymous = |request: Request<Full<Bytes>>| world.send(request);
    let no_cookie = Request::builder()
        .method("GET")
        .uri("/v1/session")
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let unknown = Signed {
        id: "x".repeat(43),
        csrf: String::new(),
    };
    let long = Signed {
        id: "x".repeat(129),
        csrf: String::new(),
    };
    for request in [
        no_cookie,
        unknown.get("/v1/session"),
        long.get("/v1/session"),
    ] {
        let sent = anonymous(request).await;
        assert_eq!(sent.status, StatusCode::UNAUTHORIZED);
        assert_eq!(sent.json(), signed_out);
    }
    let mut beside_bearer = alice.get("/v1/session");
    beside_bearer
        .headers_mut()
        .insert("authorization", "Bearer anything".parse().expect("a value"));
    assert_eq!(world.send(beside_bearer).await.json(), signed_out);

    // Idle: each use pushes the deadline 30 minutes out; the absolute deadline is 12 hours.
    world.at(START + 1799);
    assert_eq!(
        world.send(alice.get("/v1/session")).await.status,
        StatusCode::OK
    );
    world.at(START + 1799 + 1799);
    assert_eq!(
        world.send(alice.get("/v1/session")).await.status,
        StatusCode::OK
    );
    world.at(START + 1799 + 1799 + 1800);
    assert_eq!(
        world.send(alice.get("/v1/session")).await.json(),
        signed_out
    );
    let start = START + 1799 + 1799 + 1800;
    let bob = world.sign_in("bob", PASSWORD).await;
    let mut now = start;
    while now + 1500 < start + 43199 {
        now += 1500;
        world.at(now);
        assert_eq!(
            world.send(bob.get("/v1/session")).await.status,
            StatusCode::OK
        );
    }
    world.at(start + 43199);
    assert_eq!(
        world.send(bob.get("/v1/session")).await.status,
        StatusCode::OK
    );
    world.at(start + 43200);
    assert_eq!(world.send(bob.get("/v1/session")).await.json(), signed_out);
}

/// Proves: logout ends the session and tells the browser to drop the cookie; it needs the CSRF
/// header like any `POST`, is audited as the user's `logout`, is allowed to a session that must
/// still change its password, and a second logout of the same cookie is 401 `sign_in_required`.
#[tokio::test]
async fn logout_ends_the_session_and_clears_the_cookie() {
    let world = World::new("logout");
    let alice = world.sign_in("alice", PASSWORD).await;
    let mut forged = alice.post("/v1/logout", "{}");
    forged
        .headers_mut()
        .insert("x-hk-csrf", "wrong".parse().expect("a value"));
    let refused = world.send(forged).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(refused.json(), json!({ "error": "csrf" }));
    assert_eq!(
        world.send(alice.get("/v1/session")).await.status,
        StatusCode::OK
    );

    let sent = world.send(alice.post("/v1/logout", "{}")).await;
    assert_eq!(sent.status, StatusCode::OK);
    assert_eq!(
        sent.header("set-cookie"),
        Some("__Host-hk_session=; Max-Age=0; Secure; HttpOnly; SameSite=Strict; Path=/")
    );
    assert_eq!(sent.header("cache-control"), Some("no-store"));
    assert_eq!(sent.json(), json!({}));
    let again = world.send(alice.post("/v1/logout", "{}")).await;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED);
    assert_eq!(again.json(), json!({ "error": "sign_in_required" }));
    assert_eq!(
        world.send(alice.get("/v1/session")).await.status,
        StatusCode::UNAUTHORIZED
    );

    let newbie = world.sign_in("newbie", PASSWORD).await;
    assert_eq!(
        world.send(newbie.post("/v1/logout", "{}")).await.status,
        StatusCode::OK
    );
    assert_eq!(
        world.said(),
        [
            said("alice", "login", None),
            said("alice", "logout", None),
            said("newbie", "login", None),
            said("newbie", "logout", None),
        ]
    );
}

/// Proves: a logout the audit cannot record still ends the session.
#[tokio::test]
async fn a_logout_that_cannot_be_audited_still_ends_the_session() {
    let world = World::new("logout-audit-down");
    let alice = world.sign_in("alice", PASSWORD).await;
    world.fake.fail();
    let sent = world.send(alice.post("/v1/logout", "{}")).await;
    assert_eq!(sent.status, StatusCode::OK);
    assert!(world.login().sessions.is_empty());
}

/// Proves: the sign-in routes take their own method only (any other is 405 `method_not_allowed`
/// with no cache), and are the private surface's alone: the heartbeat surface answers all four
/// as it answers any path it does not serve; and a coordinator with no sign-in answers them as
/// any other path, not found.
#[tokio::test]
async fn the_routes_take_their_own_method_and_the_private_surface_only() {
    let world = World::new("login-methods");
    for (method, path) in [
        ("GET", "/v1/login"),
        ("GET", "/v1/logout"),
        ("POST", "/v1/session"),
        ("GET", "/v1/password"),
        ("PUT", "/v1/login"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Full::new(Bytes::new()))
            .expect("a request");
        let sent = world.send(request).await;
        assert_eq!(
            sent.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}"
        );
        assert_eq!(sent.json(), json!({ "error": "method_not_allowed" }));
        assert_eq!(sent.header("cache-control"), Some("no-store"));
    }
    let alice = world.sign_in("alice", PASSWORD).await;
    for (method, path) in [
        ("POST", "/v1/login"),
        ("POST", "/v1/logout"),
        ("GET", "/v1/session"),
        ("POST", "/v1/password"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("cookie", format!("{COOKIE}={}", alice.id))
            .body(Full::new(Bytes::new()))
            .expect("a request");
        let sent = world.send_on(Surface::Heartbeat, request).await;
        assert_eq!(sent.status, StatusCode::NOT_FOUND, "{method} {path}");
    }
    let bare = World::build("login-none", settings(), counting(), |_, rig| {
        crate::testutil::Rig::start(&rig.dir, &rig.clock)
    });
    assert!(bare.coordinator.login().is_none());
    let sent = bare
        .send(page_post(
            "/v1/login",
            None,
            &credentials("alice", PASSWORD),
        ))
        .await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);
}
