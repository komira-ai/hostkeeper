//! What the tests of the sign-in share: a coordinator with a sign-in over cheap password hashes,
//! a store and an audit that a test can make refuse, a few users, a clock the test sets, and the
//! requests a browser sends.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use hostkeeper_enrollment::validity::UnixClock;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request, StatusCode};
use serde_json::Value;

use crate::accounts::{AccountStore, Accounts, User};
use crate::enrollment::Random;
use crate::grpc::route;
use crate::http::Surface;
use crate::login::{Login, LoginSettings};
use crate::ops::{AuditEntry, OpsState, OpsStore};
use crate::password::{PasswordSettings, Passwords};
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::Rig;

/// The password of every user the world holds.
pub(crate) const PASSWORD: &str = "correct horse battery";
/// A password that passes the policy and is not [`PASSWORD`].
pub(crate) const NEW_PASSWORD: &str = "another long password";
/// The authority every request is made to, and the origin of a page it serves.
pub(crate) const HOST: &str = "hk.test";
/// The second the rig's clock starts at.
pub(crate) const START: u64 = 1_000;
/// How long an initial password the world holds is good.
pub(crate) const HOUR: u64 = 3600;
/// The name of the cookie.
pub(crate) const COOKIE: &str = "__Host-hk_session";

/// The least Argon2id takes, so a hash takes microseconds.
pub(crate) fn cheap() -> PasswordSettings {
    PasswordSettings {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
        min_length: 12,
    }
}

/// The settings of a sign-in over [`cheap`] hashes.
pub(crate) fn settings() -> LoginSettings {
    LoginSettings {
        passwords: cheap(),
        ..LoginSettings::default()
    }
}

/// A clock that reads the number the test sets.
pub(crate) struct Seconds(pub Arc<AtomicU64>);

impl UnixClock for Seconds {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Random bytes that differ at each call and never repeat.
pub(crate) fn counting() -> Random {
    let next = AtomicU64::new(1);
    Box::new(move |buffer| {
        let n = next.fetch_add(1, Ordering::SeqCst).to_le_bytes();
        for (i, byte) in buffer.iter_mut().enumerate() {
            *byte = n[i % 8].wrapping_add(u8::try_from(i).unwrap_or(0));
        }
        true
    })
}

/// A user whose password is [`PASSWORD`] under [`cheap`] hashes, enabled and with nothing to
/// change.
pub(crate) fn user(name: &str) -> User {
    let hash = Passwords::new(cheap())
        .expect("passwords")
        .hash(PASSWORD)
        .expect("a hash");
    User {
        name: name.to_owned(),
        hash: hash.as_str().to_owned(),
        enabled: true,
        created_by: "coordinator".to_owned(),
        created_at: 1,
        ..User::default()
    }
}

/// `alice` and `bob` (ordinary users), `dora` (disabled), `newbie` (must change an initial
/// password good for an hour from [`START`]) and `late` (an initial password whose hour has
/// passed).
pub(crate) fn users() -> Accounts {
    let mut accounts = Accounts::default();
    let mut dora = user("dora");
    dora.enabled = false;
    let mut newbie = user("newbie");
    newbie.must_change = true;
    newbie.initial_expires_at = Some(START + HOUR);
    newbie.initial_id = Some("file-1".to_owned());
    let mut late = user("late");
    late.must_change = true;
    late.initial_expires_at = Some(START - 1);
    for user in [user("alice"), user("bob"), dora, newbie, late] {
        accounts = accounts.with_user(user).expect("a user");
    }
    accounts
}

/// A coordinator with a sign-in, over a fake store that holds the users and the audit.
pub(crate) struct World {
    /// The rig that keeps the directory, the clock and the log sink alive.
    pub rig: Rig,
    /// The coordinator, shared as the listeners share it.
    pub coordinator: Arc<Coordinator>,
    /// The store under the accounts and the audit: a test makes it refuse commits.
    pub fake: Fake,
}

impl World {
    pub(crate) fn new(name: &str) -> Self {
        Self::with(name, settings(), counting())
    }

    pub(crate) fn with(name: &str, settings: LoginSettings, random: Random) -> Self {
        Self::build(name, settings, random, |coordinator, _| coordinator)
    }

    /// [`World::with`], the coordinator further made by `build` before it is shared.
    pub(crate) fn build(
        name: &str,
        settings: LoginSettings,
        random: Random,
        build: impl FnOnce(Coordinator, &Rig) -> Coordinator,
    ) -> Self {
        let rig = Rig::new(name);
        let fake = Fake::new();
        let login =
            Login::new(settings, Arc::new(Seconds(rig.clock.clone())), random).expect("a sign-in");
        let coordinator = Rig::start(&rig.dir, &rig.clock)
            .with_ops(OpsStore::over(Arc::new(fake.clone()), OpsState::default()))
            .with_accounts(AccountStore::over(Arc::new(fake.clone()), users()))
            .with_login(login);
        let coordinator = build(coordinator, &rig);
        Self {
            rig,
            coordinator: Arc::new(coordinator),
            fake,
        }
    }

    /// The coordinator's sign-in.
    pub(crate) fn login(&self) -> &Login {
        self.coordinator.login().expect("a sign-in")
    }

    /// Sets the clock to `now`, in seconds.
    pub(crate) fn at(&self, now: u64) {
        self.rig.at(now);
    }

    /// The audit, in order.
    pub(crate) fn audit(&self) -> Vec<AuditEntry> {
        self.coordinator.ops.audit_after(0).expect("the audit")
    }

    /// The audit's `(by, action, subject, source)`, in order.
    pub(crate) fn said(&self) -> Vec<(String, String, Option<String>, Option<String>)> {
        self.audit()
            .into_iter()
            .map(|entry| (entry.by, entry.action, entry.subject, entry.source))
            .collect()
    }

    /// What the private surface answers `request` with.
    pub(crate) async fn send(&self, request: Request<Full<Bytes>>) -> Sent {
        self.send_on(Surface::Control, request).await
    }

    /// What `surface` answers `request` with.
    pub(crate) async fn send_on(&self, surface: Surface, request: Request<Full<Bytes>>) -> Sent {
        let response = route(
            &self.coordinator,
            surface,
            &None,
            request,
            std::time::Duration::from_secs(30),
        )
        .await;
        let (parts, body) = response.into_parts();
        let body = body.collect().await.expect("a body").to_bytes();
        Sent {
            status: parts.status,
            headers: parts.headers,
            body,
        }
    }

    /// Signs `name` in with `password` as a page of the listener's origin does.
    pub(crate) async fn sign_in(&self, name: &str, password: &str) -> Signed {
        let body = serde_json::json!({ "username": name, "password": password }).to_string();
        let sent = self.send(page_post("/v1/login", None, &body)).await;
        assert_eq!(sent.status, StatusCode::OK, "{sent:?}");
        let set = sent.header("set-cookie").expect("a cookie").to_owned();
        let id = set
            .strip_prefix(&format!("{COOKIE}="))
            .and_then(|rest| rest.split(';').next())
            .expect("the cookie's value")
            .to_owned();
        Signed {
            id,
            csrf: sent.json()["csrf"]
                .as_str()
                .expect("a csrf token")
                .to_owned(),
        }
    }
}

/// What a route answered.
#[derive(Debug)]
pub(crate) struct Sent {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Sent {
    pub(crate) fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }

    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// A session a test signed in: the cookie's value and the CSRF token.
pub(crate) struct Signed {
    pub id: String,
    pub csrf: String,
}

impl Signed {
    /// A `POST` of `path` with `body` as the page of the session sends it.
    pub(crate) fn post(&self, path: &str, body: &str) -> Request<Full<Bytes>> {
        page_post(path, Some(self), body)
    }

    /// A `GET` of `path` with the cookie alone.
    pub(crate) fn get(&self, path: &str) -> Request<Full<Bytes>> {
        Request::builder()
            .method("GET")
            .uri(path)
            .header("host", HOST)
            .header("cookie", format!("{COOKIE}={}", self.id))
            .body(Full::new(Bytes::new()))
            .expect("a request")
    }
}

/// A `POST` of `path` with `body` as the page served by the listener sends it: its origin, its
/// content type and, with a session, its cookie and CSRF header.
pub(crate) fn page_post(path: &str, session: Option<&Signed>, body: &str) -> Request<Full<Bytes>> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", HOST)
        .header("origin", format!("https://{HOST}"))
        .header("sec-fetch-site", "same-origin")
        .header("content-type", "application/json");
    if let Some(session) = session {
        builder = builder
            .header("cookie", format!("{COOKIE}={}", session.id))
            .header("x-hk-csrf", &session.csrf);
    }
    builder
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request")
}
