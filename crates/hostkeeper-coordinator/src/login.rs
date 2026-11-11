//! The operator's sign-in: the state it keeps, and the rules by which a request is a session's
//! (`docs/design/operator-login.md`, sections 5.1 to 5.3).
//!
//! [`Login`] is the session table ([`Sessions`]), the failure counts and the hash slots
//! ([`LoginLimiter`]) and the password hashing ([`Passwords`]) of one coordinator, over one
//! injected clock. The four routes that use it are in [`crate::login_routes`]; the JSON gateway
//! uses [`held`] and [`session_request_ok`] to tell a call made with the cookie of a session.
//!
//! A cookie is ambient authority: a browser attaches it to a request that a hostile page
//! caused. So a request with an `Authorization` header is a bearer's and its cookie is never
//! read, and a request with a cookie and no `Authorization` header is a session's, accepted
//! only when the header `X-HK-CSRF` is the session's token, the `Origin` header (when there is
//! one) is the listener's own, `Sec-Fetch-Site` (when there is one) is `same-origin`, and the
//! body says it is JSON. The gRPC path reads the `Authorization` header alone.

use std::num::NonZeroUsize;
use std::sync::Arc;

use hostkeeper_enrollment::validity::UnixClock;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, HOST, ORIGIN};
use hyper::{HeaderMap, Uri};

use crate::enrollment::Random;
use crate::password::{
    HashSlots, PasswordError, PasswordSettings, Passwords, PolicyError, StoredHash, Verdict,
};
use crate::session::{LimiterSettings, LoginLimiter, SessionSettings, SessionView, Sessions};

/// The name of the session cookie. The `__Host-` prefix makes a browser refuse the cookie unless
/// it came over a secure connection with `Path=/` and no `Domain`.
pub const COOKIE_NAME: &str = "__Host-hk_session";

/// The header that carries the session's CSRF token.
pub const CSRF_HEADER: &str = "x-hk-csrf";

/// The most bytes of a session id a request is read for: a longer value is no session's.
const MAX_ID_BYTES: usize = 128;

/// The most bytes of a login, logout or password change request read.
pub const MAX_LOGIN_BYTES: usize = 4096;

/// The settings of the sign-in: what the flags of the login give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoginSettings {
    /// The session table's deadlines and cap.
    pub sessions: SessionSettings,
    /// The failure counts and the lock.
    pub limiter: LimiterSettings,
    /// The password hashes that may run at once (`--login-max-hashes`); 0 is taken as 1.
    pub max_hashes: usize,
    /// The password hash parameters and the length rule.
    pub passwords: PasswordSettings,
}

impl Default for LoginSettings {
    fn default() -> Self {
        Self {
            sessions: SessionSettings::default(),
            limiter: LimiterSettings::default(),
            max_hashes: 2,
            passwords: PasswordSettings::default(),
        }
    }
}

/// The sessions, the limiter and the passwords of one coordinator.
pub struct Login {
    pub(crate) sessions: Sessions,
    pub(crate) limiter: LoginLimiter,
    pub(crate) passwords: Arc<Passwords>,
    clock: Arc<dyn UnixClock>,
    /// A test's progress event: told after a password was hashed and before what it decides is
    /// acted on, so a test can change the user in that window instead of waiting for a race.
    #[cfg(test)]
    hashed: std::sync::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login").finish_non_exhaustive()
    }
}

impl Login {
    /// An empty sign-in under `settings`, judged by `clock`; session ids and CSRF tokens are
    /// drawn from `random`.
    ///
    /// # Errors
    /// [`PasswordError::Params`]: the password hash parameters are outside what Argon2id takes.
    pub fn new(
        settings: LoginSettings,
        clock: Arc<dyn UnixClock>,
        random: Random,
    ) -> Result<Self, PasswordError> {
        let slots =
            HashSlots::new(NonZeroUsize::new(settings.max_hashes).unwrap_or(NonZeroUsize::MIN));
        Ok(Self {
            sessions: Sessions::new(settings.sessions, Arc::clone(&clock), random),
            limiter: LoginLimiter::new(settings.limiter, Arc::clone(&clock), slots),
            passwords: Arc::new(Passwords::new(settings.passwords)?),
            clock,
            #[cfg(test)]
            hashed: std::sync::Mutex::new(None),
        })
    }

    /// From now on, `hook` runs each time a password has been hashed and what it decides is about
    /// to be acted on.
    #[cfg(test)]
    pub(crate) fn when_hashed(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self
            .hashed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(hook));
    }

    /// Tells the test's hook that a password was hashed.
    #[cfg(test)]
    pub(crate) fn hashed(&self) {
        if let Some(hook) = self
            .hashed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            hook();
        }
    }

    /// The time now, in Unix seconds, by the injected clock.
    pub(crate) fn now(&self) -> u64 {
        self.clock.now_unix()
    }

    /// Whether `password` is the one `stored` holds, or, when there is no stored hash (no such
    /// user, a disabled one, a void credential), one verify against the fixed hash so the time
    /// does not say which. Run off the runtime's threads; a task that did not finish says no.
    pub(crate) async fn verify(&self, password: String, stored: Option<String>) -> Verdict {
        let passwords = Arc::clone(&self.passwords);
        tokio::task::spawn_blocking(move || match stored {
            Some(stored) => passwords.verify(&password, &StoredHash::from(stored)),
            None => {
                passwords.verify_absent(&password);
                Verdict::Wrong
            }
        })
        .await
        .unwrap_or(Verdict::Wrong)
    }

    /// The checks of a password change, off the runtime's threads: `current` is the password
    /// `stored` holds, `new` is allowed for `user` by the policy, and then the hash of `new`.
    pub(crate) async fn change(
        &self,
        user: String,
        current: String,
        new: String,
        stored: String,
    ) -> Change {
        let passwords = Arc::clone(&self.passwords);
        tokio::task::spawn_blocking(move || {
            if passwords.verify(&current, &StoredHash::from(stored)) == Verdict::Wrong {
                return Change::Wrong;
            }
            if let Err(why) = passwords.check_policy(&new, &user, Some(&current)) {
                return Change::Policy(why);
            }
            passwords.hash(&new).map_or(Change::Failed, |hash| {
                Change::Hash(hash.as_str().to_owned())
            })
        })
        .await
        .unwrap_or(Change::Failed)
    }
}

/// What the checks of a password change found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// The current password is not the one held.
    Wrong,
    /// The new password breaks the policy.
    Policy(PolicyError),
    /// The new password's hash.
    Hash(String),
    /// The hash could not be made, or the task did not finish.
    Failed,
}

/// A live session a request named.
pub(crate) struct Held {
    /// The cookie's value, which names the session in the table.
    pub id: String,
    /// What the session says about itself.
    pub view: SessionView,
}

/// The value of the session cookie among the `Cookie` headers of `headers`: the first of that
/// name, if it is not longer than a session id can be.
fn cookie_in(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|line| line.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty() && value.len() <= MAX_ID_BYTES)
}

/// The live session the cookie of `headers` names, its idle deadline pushed out. None for a
/// request with an `Authorization` header (a bearer's: the cookie is not read), no cookie, or a
/// cookie that names no live session.
pub(crate) fn held(login: &Login, headers: &HeaderMap) -> Option<Held> {
    if headers.contains_key(AUTHORIZATION) {
        return None;
    }
    let id = cookie_in(headers)?;
    let view = login.sessions.lookup(id)?;
    Some(Held {
        id: id.to_owned(),
        view,
    })
}

/// The authority the request was made to: its `Host` header, or the authority of its target
/// (an HTTP/2 request has no `Host` header).
pub(crate) fn authority<'a>(headers: &'a HeaderMap, uri: &'a Uri) -> Option<&'a str> {
    headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        .or_else(|| uri.authority().map(hyper::http::uri::Authority::as_str))
}

/// Whether the request comes from the listener's own origin, as far as it says: an `Origin`
/// header, when there is one, is `https://` and the authority asked, and `Sec-Fetch-Site`, when
/// there is one, is `same-origin`.
pub(crate) fn same_origin(headers: &HeaderMap, authority: Option<&str>) -> bool {
    let origin_ok = headers.get(ORIGIN).is_none_or(|origin| {
        authority.is_some_and(|authority| {
            origin
                .to_str()
                .is_ok_and(|origin| origin.eq_ignore_ascii_case(&format!("https://{authority}")))
        })
    });
    let fetch_ok = headers
        .get("sec-fetch-site")
        .is_none_or(|site| site.as_bytes() == b"same-origin");
    origin_ok && fetch_ok
}

/// Whether the body of the request says it is JSON: the media type `application/json`, with
/// any parameters.
pub(crate) fn says_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

/// Whether a `POST` of `view`'s session is accepted: its CSRF header, the origin and the content
/// type are as the design says. Each failure is a 403 `csrf` and changes nothing.
pub(crate) fn session_request_ok(
    view: &SessionView,
    headers: &HeaderMap,
    authority: Option<&str>,
) -> bool {
    headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|presented| view.csrf_matches(presented))
        && same_origin(headers, authority)
        && says_json(headers)
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
