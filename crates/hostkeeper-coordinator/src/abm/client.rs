//! The ABM client (`docs/design/coordinator-mdm.md`, 4.5): the OAuth session, and the calls the
//! sync and the token upload make, over the transport port.
//!
//! **The session.** `GET /session` with the OAuth header ([`super::oauth`]) answers the
//! `auth_session_token` that every other call carries as `X-ADM-Auth-Session`; the client keeps
//! it between calls, for the token it was made for. Apple answers a session that ran out with
//! `403` (not `401`), or with `200` and a body that is not JSON; either refreshes the session
//! once and repeats the call, and a second one is [`AbmError::SessionRefused`]. On `/session` a
//! `403` whose body says `signature_invalid` is a fault of the credential
//! ([`AbmError::CredentialRefused`]) and one that says `timestamp_refused` a fault of this
//! machine's clock, which Apple judges to within an hour ([`AbmError::ClockRefused`]).
//!
//! **Pacing.** A `429` that names `Retry-After` is repeated after that many seconds, at most
//! [`Pacing::retries`] times and never waiting longer than [`Pacing::max_wait`]; one that names
//! none, or still comes after the last retry, is [`AbmError::RateLimited`], and the next pass of
//! the sync tries again. Waiting goes through the injected sleeper, so no test waits.
//!
//! **Never shown.** No error, log line or `Debug` of this module holds a token part, a signature,
//! the session or a body: an error names the step and the status.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use hyper::Method;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::oauth;
use super::transport::{AbmRequest, AbmResponse, AbmTransport, TransportError};
use super::wire::{Account, AssignAnswer, DevicePage, ProfileAnswer, SessionAnswer};
use crate::enrollment::Random;
use crate::mdm_secrets::AbmToken;

/// The most devices one page asks for: Apple's maximum for `/server/devices` and `/devices/sync`.
pub(crate) const PAGE_LIMIT: u32 = 1000;

/// The protocol version the calls name; Apple does not require it and accepts 3.
const PROTOCOL_VERSION: &str = "3";

/// Why a call to ABM did not give its answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AbmError {
    /// No answer came.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The system gave no random bytes for a nonce.
    #[error("the system gave no random bytes for an OAuth nonce")]
    Random,
    /// ABM refused the signature: the token's parts do not belong together, or it was revoked.
    #[error("ABM refused the token's signature (signature_invalid): the token is not good")]
    CredentialRefused,
    /// ABM refused the timestamp, which it takes within an hour of its own time.
    #[error(
        "ABM refused the timestamp (timestamp_refused): this machine's clock is off by an hour or more"
    )]
    ClockRefused,
    /// ABM refused the session twice running.
    #[error("ABM refused the session twice")]
    SessionRefused,
    /// The cursor is not one ABM takes (too old or unknown): a full fetch gives a new one.
    #[error("ABM refused the cursor (a CURSOR error)")]
    InvalidCursor,
    /// ABM asked for fewer requests (429).
    #[error("ABM asked for fewer requests (429), retry after {retry_after:?} seconds")]
    RateLimited {
        /// What `Retry-After` named.
        retry_after: Option<u64>,
    },
    /// Any other status.
    #[error("ABM answered status {0}")]
    Status(u16),
    /// The answer is not what the call expects.
    #[error("ABM's answer to {0} is not what it should be")]
    BadAnswer(&'static str),
}

/// How a call is paced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pacing {
    /// How long one request may take.
    pub(crate) timeout: Duration,
    /// How many times a `429` that names `Retry-After` is repeated.
    pub(crate) retries: u32,
    /// The longest wait taken for one `Retry-After`: the sync interval, since a later retry is the
    /// next pass.
    pub(crate) max_wait: Duration,
}

/// Waits: production sleeps; a test records.
pub(crate) type Sleep = Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;

/// The production sleeper.
pub(crate) fn tokio_sleep() -> Sleep {
    Arc::new(|wait| Box::pin(tokio::time::sleep(wait)))
}

/// The client: the transport, the clock and the sources the signature needs, and the session.
pub(crate) struct AbmClient {
    transport: Arc<dyn AbmTransport>,
    clock: hostkeeper_oidc::Clock,
    random: Random,
    sleep: Sleep,
    session: Mutex<Option<Held>>,
}

/// The session held, and which token it was made for.
struct Held {
    token: String,
    session: String,
}

impl AbmClient {
    /// A client over `transport`, reading the time from `clock`.
    pub(crate) fn new(
        transport: Arc<dyn AbmTransport>,
        clock: hostkeeper_oidc::Clock,
        random: Random,
        sleep: Sleep,
    ) -> Self {
        Self {
            transport,
            clock,
            random,
            sleep,
            session: Mutex::new(None),
        }
    }

    /// The calls with `token`, paced as `pacing` says.
    pub(crate) fn api<'a>(&'a self, token: &'a AbmToken, pacing: Pacing) -> Api<'a> {
        Api {
            client: self,
            token,
            pacing,
            id: token_id(token),
        }
    }

    fn held(&self, id: &str) -> Option<String> {
        let held = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        held.as_ref()
            .filter(|held| held.token == id)
            .map(|held| held.session.clone())
    }

    fn keep(&self, id: &str, session: String) {
        *self.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(Held {
            token: id.to_owned(),
            session,
        });
    }

    fn forget(&self) {
        *self.session.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Names a token without showing it: the SHA-256 of its parts, hex.
fn token_id(token: &AbmToken) -> String {
    let mut digest = Sha256::new();
    for part in [
        &token.consumer_key,
        &token.consumer_secret,
        &token.access_token,
        &token.access_secret,
    ] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

/// The calls of the client with one token.
pub(crate) struct Api<'a> {
    client: &'a AbmClient,
    token: &'a AbmToken,
    pacing: Pacing,
    id: String,
}

#[derive(Serialize)]
struct PageAsk<'a> {
    limit: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<&'a str>,
}

#[derive(Serialize)]
struct AssignAsk<'a> {
    profile_uuid: &'a str,
    devices: &'a [String],
}

impl Api<'_> {
    /// `GET /account`: the token's proof.
    ///
    /// # Errors
    /// [`AbmError`].
    pub(crate) async fn account(&self) -> Result<Account, AbmError> {
        let body = self.call(Method::GET, "/account", None).await?;
        serde_json::from_slice(&body).map_err(|_| AbmError::BadAnswer("GET /account"))
    }

    /// A page of `POST /server/devices`: the first when `cursor` is none, else the one after it.
    ///
    /// # Errors
    /// [`AbmError`].
    pub(crate) async fn fetch(&self, cursor: Option<&str>) -> Result<DevicePage, AbmError> {
        self.page("/server/devices", cursor).await
    }

    /// A page of `POST /devices/sync` after `cursor`.
    ///
    /// # Errors
    /// [`AbmError::InvalidCursor`] when ABM no longer takes the cursor, and the rest of
    /// [`AbmError`].
    pub(crate) async fn changes(&self, cursor: &str) -> Result<DevicePage, AbmError> {
        self.page("/devices/sync", Some(cursor)).await
    }

    async fn page(&self, path: &str, cursor: Option<&str>) -> Result<DevicePage, AbmError> {
        let ask = PageAsk {
            limit: PAGE_LIMIT,
            cursor,
        };
        let body = self.call(Method::POST, path, Some(json(&ask))).await?;
        serde_json::from_slice(&body).map_err(|_| AbmError::BadAnswer("a device page"))
    }

    /// `POST /profile`: defines `profile` and answers the uuid ABM gave it.
    ///
    /// # Errors
    /// [`AbmError`].
    pub(crate) async fn define_profile(
        &self,
        profile: &serde_json::Value,
    ) -> Result<String, AbmError> {
        let body = self
            .call(Method::POST, "/profile", Some(json(profile)))
            .await?;
        let answer: ProfileAnswer =
            serde_json::from_slice(&body).map_err(|_| AbmError::BadAnswer("POST /profile"))?;
        Ok(answer.profile_uuid)
    }

    /// Assigns `profile_uuid` to `serials`: `POST /profile/devices`, and `PUT` when ABM answers
    /// the `POST` with a `4xx` that is not about the session or the rate (Apple's text has
    /// `POST`; NanoDEP uses `PUT`, for the simulator). The outcome of each serial, by serial.
    ///
    /// # Errors
    /// [`AbmError`].
    pub(crate) async fn assign(
        &self,
        profile_uuid: &str,
        serials: &[String],
    ) -> Result<AssignAnswer, AbmError> {
        let ask = json(&AssignAsk {
            profile_uuid,
            devices: serials,
        });
        let body = match self
            .call(Method::POST, "/profile/devices", Some(ask.clone()))
            .await
        {
            Err(AbmError::Status(400..=499)) => {
                self.call(Method::PUT, "/profile/devices", Some(ask))
                    .await?
            }
            other => other?,
        };
        serde_json::from_slice(&body).map_err(|_| AbmError::BadAnswer("an assignment"))
    }

    /// One call with the session: made when there is none, and made again once when ABM refuses it.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, AbmError> {
        let mut refreshed = false;
        loop {
            let session = match self.client.held(&self.id) {
                Some(session) => session,
                None => self.open_session().await?,
            };
            let request = AbmRequest {
                method: method.clone(),
                path: path.to_owned(),
                headers: vec![
                    ("X-ADM-Auth-Session", session),
                    ("X-Server-Protocol-Version", PROTOCOL_VERSION.to_owned()),
                ],
                body: body.clone(),
                timeout: self.pacing.timeout,
            };
            let answer = self.send(request).await?;
            match answer.status {
                200..=299 if is_json(&answer.body) => return Ok(answer.body),
                200..=299 | 403 => {
                    if refreshed {
                        return Err(AbmError::SessionRefused);
                    }
                    refreshed = true;
                    self.client.forget();
                }
                400 if contains(&answer.body, "CURSOR") => {
                    return Err(AbmError::InvalidCursor);
                }
                status => return Err(AbmError::Status(status)),
            }
        }
    }

    /// `GET /session`, signed; the session is kept.
    async fn open_session(&self) -> Result<String, AbmError> {
        let mut nonce = [0_u8; 16];
        if !(self.client.random)(&mut nonce) {
            return Err(AbmError::Random);
        }
        let url = format!("{}/session", super::transport::ABM_BASE);
        let authorization =
            oauth::authorization(self.token, &url, (self.client.clock)(), &hex::encode(nonce));
        let request = AbmRequest {
            method: Method::GET,
            path: "/session".to_owned(),
            headers: vec![
                ("Authorization", authorization),
                ("X-Server-Protocol-Version", PROTOCOL_VERSION.to_owned()),
            ],
            body: None,
            timeout: self.pacing.timeout,
        };
        let answer = self.send(request).await?;
        match answer.status {
            200..=299 => {
                let session: SessionAnswer = serde_json::from_slice(&answer.body)
                    .map_err(|_| AbmError::BadAnswer("GET /session"))?;
                self.client
                    .keep(&self.id, session.auth_session_token.clone());
                Ok(session.auth_session_token)
            }
            403 if contains(&answer.body, "timestamp_refused") => Err(AbmError::ClockRefused),
            401 | 403 if contains(&answer.body, "signature_invalid") => {
                Err(AbmError::CredentialRefused)
            }
            403 => Err(AbmError::SessionRefused),
            status => Err(AbmError::Status(status)),
        }
    }

    /// Sends `request`, repeating it after the `Retry-After` of a `429`.
    async fn send(&self, request: AbmRequest) -> Result<AbmResponse, AbmError> {
        let mut retries = 0;
        loop {
            let answer = self.client.transport.send(request.clone()).await?;
            if answer.status != 429 {
                return Ok(answer);
            }
            match answer.retry_after {
                Some(seconds) if retries < self.pacing.retries => {
                    retries += 1;
                    let wait = Duration::from_secs(seconds).min(self.pacing.max_wait);
                    (self.client.sleep)(wait).await;
                }
                retry_after => return Err(AbmError::RateLimited { retry_after }),
            }
        }
    }
}

/// `value` as the JSON bytes of a request (it is serialisable, so this always writes).
fn json(value: &impl Serialize) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

fn is_json(body: &[u8]) -> bool {
    serde_json::from_slice::<serde::de::IgnoredAny>(body).is_ok()
}

fn contains(body: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(body).contains(needle)
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
