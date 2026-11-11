//! The four sign-in routes of the private listener (`docs/design/operator-login.md`, sections
//! 5 and 6.2): `POST /v1/login`, `POST /v1/logout`, `GET /v1/session` and `POST /v1/password`.
//! They are served only when the coordinator has a [`Login`] and only on the private surface;
//! the heartbeat surface and the LAN listener answer them as they answer any other path.
//!
//! Every answer carries `Cache-Control: no-store`. An error is `{"error":"<code>"}` from a closed
//! set: `bad_request` (400), `invalid_credentials` (401, one text for an unknown name, a wrong
//! password, a disabled user and an initial password past its hour), `sign_in_required` (401),
//! `initial_password_expired` (401), `csrf` (403), `try_later` (429), `store_unreachable` (503),
//! `internal` (500), and `password_policy` (400) whose `reason` is the text of the rule broken.
//! A name or a password that is not in the answer of any error.
//!
//! A session answer, from login, `GET /v1/session` and a password change, is
//! `{"username", "csrf", "must_change"}` and, for a session that must change its password,
//! `must_change_until`, the second after which the change is refused.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Body;
use hyper::header::{CACHE_CONTROL, HeaderValue, SET_COOKIE};
use hyper::http::request::Parts;
use hyper::{Method, Request, Response, StatusCode};
use serde::Deserialize;

use crate::accounts::User;
use crate::audit_actions::{LOGIN, LOGIN_FAILED, LOGIN_LOCKED, LOGOUT, PASSWORD_CHANGED, about};
use crate::http::{BoxError, PeerAddr, error, json, read_body};
use crate::login::{
    COOKIE_NAME, Change, Held, Login, MAX_LOGIN_BYTES, authority, held, same_origin, says_json,
    session_request_ok,
};
use crate::operate::label;
use crate::ops::{AuditEntry, OpsError, Write};
use crate::password::{MAX_PASSWORD_BYTES, PolicyError, Verdict};
use crate::reconcile::COORDINATOR;
use crate::service::Coordinator;
use crate::session::{Failure, SessionView};

/// The path a person posts the name and password to.
pub const LOGIN_PATH: &str = "/v1/login";
/// The path that ends the session of the cookie.
pub const LOGOUT_PATH: &str = "/v1/logout";
/// The path a page reads its session from.
pub const SESSION_PATH: &str = "/v1/session";
/// The path a person changes their own password at.
pub const PASSWORD_PATH: &str = "/v1/password";

type Answer = Response<Full<Bytes>>;

/// Whether `path` is one of the four routes.
pub(crate) fn is_route(path: &str) -> bool {
    [LOGIN_PATH, LOGOUT_PATH, SESSION_PATH, PASSWORD_PATH].contains(&path)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordBody {
    current: String,
    new: String,
}

/// Why a sign-in call was refused.
#[derive(Debug, PartialEq, Eq)]
enum Refused {
    /// One text for every way a login or a current password can be wrong.
    Invalid,
    /// No session, or one whose user is gone or disabled.
    SignIn,
    /// The name is locked, or no hash slot is free.
    TryLater,
    /// The store did not take the change.
    Unreachable,
    /// The initial password's hour has passed.
    Expired,
    /// The new password breaks the policy.
    Policy(PolicyError),
    /// Something the coordinator could not do: the audit, the random bytes, a hash.
    Internal,
}

impl Refused {
    fn answer(&self) -> Answer {
        match self {
            Self::Invalid => error(StatusCode::UNAUTHORIZED, "invalid_credentials"),
            Self::SignIn => error(StatusCode::UNAUTHORIZED, "sign_in_required"),
            Self::TryLater => error(StatusCode::TOO_MANY_REQUESTS, "try_later"),
            Self::Unreachable => error(StatusCode::SERVICE_UNAVAILABLE, "store_unreachable"),
            Self::Expired => error(StatusCode::UNAUTHORIZED, "initial_password_expired"),
            Self::Policy(why) => json(
                StatusCode::BAD_REQUEST,
                &serde_json::json!({ "error": "password_policy", "reason": why.to_string() }),
            ),
            Self::Internal => error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

/// `answer` with the headers every answer of these routes has.
fn sealed(mut answer: Answer) -> Answer {
    answer
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    answer
}

/// The attributes of the cookie: a browser keeps it only over TLS, a page cannot read it, a
/// request from another site does not carry it, and (with the `__Host-` prefix) it has no
/// `Domain` and the path `/`. It has no `Max-Age`: the server's deadlines are what count.
const ATTRIBUTES: &str = "Secure; HttpOnly; SameSite=Strict; Path=/";

fn with_cookie(mut answer: Answer, cookie: &str) -> Answer {
    // The id is base64url and the rest is fixed text: always a header value.
    answer.headers_mut().extend(
        HeaderValue::from_str(cookie)
            .ok()
            .map(|value| (SET_COOKIE, value)),
    );
    answer
}

/// What a session says, as the JSON of the three answers.
fn session_json(view: &SessionView) -> serde_json::Value {
    let mut body = serde_json::json!({
        "username": view.username,
        "csrf": view.csrf(),
        "must_change": view.must_change,
    });
    if let Some(until) = view.must_change_until.filter(|_| view.must_change) {
        body["must_change_until"] = until.into();
    }
    body
}

/// The peer's address as the audit records it, if the listener knew it.
fn source_of(parts: &Parts) -> Option<String> {
    parts
        .extensions
        .get::<PeerAddr>()
        .map(|peer| peer.0.ip().to_string())
}

/// Answers a request for one of the four routes ([`is_route`]) that came in on the private
/// surface of a coordinator that has `login`. Any other method of the path is a 405.
pub(crate) async fn answer<B>(
    coordinator: &Coordinator,
    login: &Login,
    request: Request<B>,
    patience: Duration,
) -> Answer
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let (parts, body) = request.into_parts();
    let answered = match (&parts.method, parts.uri.path()) {
        (&Method::POST, LOGIN_PATH) => sign_in(coordinator, login, &parts, body, patience).await,
        (&Method::POST, LOGOUT_PATH) => sign_out(coordinator, login, &parts),
        (&Method::GET, SESSION_PATH) => current(login, &parts),
        (&Method::POST, PASSWORD_PATH) => {
            password(coordinator, login, &parts, body, patience).await
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
    };
    sealed(answered)
}

/// The JSON of a request body of at most [`MAX_LOGIN_BYTES`] that arrives in time.
async fn body_of<B, T>(body: B, patience: Duration) -> Option<T>
where
    B: Body,
    B::Error: Into<BoxError>,
    T: serde::de::DeserializeOwned,
{
    let bytes = read_body(body, MAX_LOGIN_BYTES, patience).await?;
    serde_json::from_slice(&bytes).ok()
}

fn csrf() -> Answer {
    error(StatusCode::FORBIDDEN, "csrf")
}

fn bad_request() -> Answer {
    error(StatusCode::BAD_REQUEST, "bad_request")
}

/// `POST /v1/login`.
async fn sign_in<B>(
    coordinator: &Coordinator,
    login: &Login,
    parts: &Parts,
    body: B,
    patience: Duration,
) -> Answer
where
    B: Body,
    B::Error: Into<BoxError>,
{
    if !same_origin(&parts.headers, authority(&parts.headers, &parts.uri))
        || !says_json(&parts.headers)
    {
        return csrf();
    }
    let Some(Credentials { username, password }) = body_of(body, patience).await else {
        return bad_request();
    };
    if !label(&username) || password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
        return bad_request();
    }
    match coordinator
        .sign_in(login, &username, password, source_of(parts))
        .await
    {
        Ok((id, view)) => with_cookie(
            json(StatusCode::OK, &session_json(&view)),
            &format!("{COOKIE_NAME}={id}; {ATTRIBUTES}"),
        ),
        Err(refused) => refused.answer(),
    }
}

/// The session of a request that changes something: the cookie names a live session, and the
/// request passes the checks of a session's `POST`.
fn session_post(login: &Login, parts: &Parts) -> Result<Held, Box<Answer>> {
    let held = held(login, &parts.headers).ok_or_else(|| Box::new(Refused::SignIn.answer()))?;
    if session_request_ok(
        &held.view,
        &parts.headers,
        authority(&parts.headers, &parts.uri),
    ) {
        Ok(held)
    } else {
        Err(Box::new(csrf()))
    }
}

/// `POST /v1/logout`: the session ends, and the browser is told to forget the cookie. A session
/// that must change its password may sign out.
fn sign_out(coordinator: &Coordinator, login: &Login, parts: &Parts) -> Answer {
    let held = match session_post(login, parts) {
        Ok(held) => held,
        Err(refused) => return *refused,
    };
    login.sessions.end(&held.id);
    let mut entry = AuditEntry::new(&held.view.username, LOGOUT);
    entry.source = source_of(parts);
    if let Err(why) = coordinator.audit(entry) {
        tracing::warn!(%why, "a logout could not be audited");
    }
    with_cookie(
        json(StatusCode::OK, &serde_json::json!({})),
        &format!("{COOKIE_NAME}=; Max-Age=0; {ATTRIBUTES}"),
    )
}

/// `GET /v1/session`: the cookie alone, so a page that reloads gets its CSRF token back. Its
/// answer is readable by a page of the listener's own origin only: no `Access-Control-*` header
/// is sent.
fn current(login: &Login, parts: &Parts) -> Answer {
    match held(login, &parts.headers) {
        Some(held) => json(StatusCode::OK, &session_json(&held.view)),
        None => Refused::SignIn.answer(),
    }
}

/// `POST /v1/password`.
async fn password<B>(
    coordinator: &Coordinator,
    login: &Login,
    parts: &Parts,
    body: B,
    patience: Duration,
) -> Answer
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let held = match session_post(login, parts) {
        Ok(held) => held,
        Err(refused) => return *refused,
    };
    let Some(PasswordBody { current, new }) = body_of(body, patience).await else {
        return bad_request();
    };
    match coordinator
        .change_password(login, &held, current, new, source_of(parts))
        .await
    {
        Ok(view) => json(StatusCode::OK, &session_json(&view)),
        Err(refused) => refused.answer(),
    }
}

impl Coordinator {
    /// Writes `entry` to the audit before anything it records is done.
    fn audit(&self, entry: AuditEntry) -> Result<(), OpsError> {
        self.ops
            .change(Write::BeforeMemory, entry, |_| {})
            .map(drop)
    }

    /// Counts a failed login or password check of `name` and audits it: the first failure of a
    /// run and the lock, not every failure. An entry that cannot be written is logged: the
    /// answer is a refusal either way.
    fn note_failure(&self, login: &Login, name: &str, source: Option<String>) {
        let action = match login.limiter.failed(name) {
            Failure::First => LOGIN_FAILED,
            Failure::Locked => LOGIN_LOCKED,
            Failure::Counted => return,
        };
        let mut entry = about(COORDINATOR, action, name);
        entry.source = source;
        if let Err(why) = self.audit(entry) {
            tracing::warn!(%why, "a failed login could not be audited");
        }
    }

    /// The user `name`, if there is one that may sign in: enabled, and with no initial credential
    /// that has been voided or has passed its deadline at `now`.
    fn signing_in(&self, name: &str, now: u64) -> Option<User> {
        self.accounts()
            .snapshot()
            .user(name)
            .filter(|user| {
                user.enabled
                    && !user.initial_void
                    && user.initial_expires_at.is_none_or(|until| now < until)
            })
            .cloned()
    }

    /// A login: `Ok` is the session id for the cookie and what the session says.
    ///
    /// A locked name or a full set of hash slots is refused before any hash. An unknown name, a
    /// disabled user and a credential past its hour spend one verify against a fixed hash, so
    /// the refusal, its text and its time are the same as a wrong password's. A right password
    /// is audited as the session is made (a login that cannot be audited gets none), and the user's last sign-in (and a stronger hash,
    /// when the settings grew) is stored; a store that cannot write refuses the login.
    async fn sign_in(
        &self,
        login: &Login,
        name: &str,
        password: String,
        source: Option<String>,
    ) -> Result<(String, SessionView), Refused> {
        let slot = login.limiter.admit(name).map_err(|_| Refused::TryLater)?;
        let user = self.signing_in(name, login.now());
        let verdict = login
            .verify(password, user.as_ref().map(|user| user.hash.clone()))
            .await;
        drop(slot);
        #[cfg(test)]
        login.hashed();
        let (Some(user), Verdict::Right { rehash }) = (user, verdict) else {
            self.note_failure(login, name, source);
            return Err(Refused::Invalid);
        };
        // The password was right when the hash began: it is not if the user was disabled, or
        // given another password, while it ran.
        if self
            .signing_in(name, login.now())
            .is_none_or(|now| now.hash != user.hash)
        {
            return Err(Refused::Invalid);
        }
        login.limiter.succeeded(name);
        let now = login.now();
        self.accounts()
            .change(|held| {
                held.with_user_edited(name, |user| {
                    user.last_login_at = Some(now);
                    if let Some(rehash) = rehash {
                        user.hash = rehash.as_str().to_owned();
                    }
                })
            })
            .map_err(|why| {
                tracing::error!(%why, "a login could not be stored");
                Refused::Unreachable
            })?;
        let until = user.initial_expires_at.filter(|_| user.must_change);
        let (id, view) = login
            .sessions
            .create(name, user.must_change, until)
            .map_err(|why| {
                tracing::error!(%why, "a session could not be made");
                Refused::Internal
            })?;
        let mut entry = AuditEntry::new(name, LOGIN);
        entry.source = source;
        if let Err(why) = self.audit(entry) {
            // A login that is not on the record gives no session: nobody has the id yet.
            login.sessions.end(id.as_str());
            tracing::error!(%why, "a login could not be audited");
            return Err(Refused::Internal);
        }
        Ok((id.as_str().to_owned(), view))
    }

    /// A change of the signed-in user's own password: `Ok` is what the session says now.
    ///
    /// The current password is verified like a login's, and a wrong one is counted as a failed
    /// login. The new one must pass the policy. A password that was an initial one is changed
    /// only before its hour is out. On success the hash is stored (audited first), `must_change`
    /// and the deadline are cleared, every other session of the user ends, this one stays, and
    /// the initial-admin file that made the password, if there is one, is deleted.
    async fn change_password(
        &self,
        login: &Login,
        held: &Held,
        current: String,
        new: String,
        source: Option<String>,
    ) -> Result<SessionView, Refused> {
        let name = held.view.username.as_str();
        let user = self
            .accounts()
            .snapshot()
            .user(name)
            .filter(|user| user.enabled)
            .cloned()
            .ok_or(Refused::SignIn)?;
        if user.initial_void
            || user
                .initial_expires_at
                .is_some_and(|until| login.now() >= until)
        {
            return Err(Refused::Expired);
        }
        let slot = login.limiter.admit(name).map_err(|_| Refused::TryLater)?;
        let changed = login
            .change(name.to_owned(), current, new, user.hash.clone())
            .await;
        drop(slot);
        #[cfg(test)]
        login.hashed();
        let hash = match changed {
            Change::Hash(hash) => hash,
            Change::Wrong => {
                self.note_failure(login, name, source);
                return Err(Refused::Invalid);
            }
            Change::Policy(why) => return Err(Refused::Policy(why)),
            Change::Failed => return Err(Refused::Internal),
        };
        // Another change, or a disabling, while the hashes ran: this one was of a password that
        // is no longer the user's.
        if self
            .signing_in(name, login.now())
            .is_none_or(|now| now.hash != user.hash)
        {
            return Err(Refused::Invalid);
        }
        let mut entry = AuditEntry::new(name, PASSWORD_CHANGED);
        entry.source = source;
        self.audit(entry).map_err(|why| {
            tracing::error!(%why, "a password change could not be audited");
            Refused::Internal
        })?;
        self.accounts()
            .change(|held| {
                held.with_user_edited(name, |user| {
                    user.hash = hash;
                    user.must_change = false;
                    user.initial_expires_at = None;
                })
            })
            .map_err(|why| {
                tracing::error!(%why, "a password change could not be stored");
                Refused::Unreachable
            })?;
        login.limiter.succeeded(name);
        login.sessions.end_user(name, Some(&held.id));
        if let Some(id) = &user.initial_id
            && let Some(file) = &self.initial_admin
            && let Err(why) = file.delete_if_adopted(name, id)
        {
            tracing::warn!(%why, "the initial-admin file was not deleted");
        }
        login
            .sessions
            .clear_must_change(&held.id)
            .ok_or(Refused::SignIn)
    }
}
