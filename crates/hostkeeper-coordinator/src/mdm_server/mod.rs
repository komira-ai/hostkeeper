//! The MDM server inside the coordinator (`docs/design/coordinator-mdm.md`, section 4.1 and
//! task CM11a): the routes `/checkin` and `/mdm` of the Macs' listener, the in-process queue that
//! replaced NanoHUB's, and the TLS of the listener.
//!
//! - [`checkin`]: `Authenticate`, `TokenUpdate` and `CheckOut`. `Authenticate` holds the rule of
//!   owner decision B (4.2): a new ADE enrollment of a serial that is in our ABM table replaces the
//!   live enrollment of the serial, with no operator; one that is not is refused and audited,
//!   unless an operator confirmed it. The check is made here, at `Authenticate`, and not only when
//!   `/enroll` issued the challenge: the model check of enrollment found that a serial removed from
//!   ABM between the two replaced a live enrollment when the check was made only at `/enroll`.
//! - [`connect`]: `/mdm`, where the Mac says it is idle or answers a command and is handed the next
//!   one ([`queue`]).
//! - [`auth`] and [`signature`]: who is calling, by TLS client certificate or `Mdm-Signature`.
//! - [`tls`]: the certificate of the MDM name from the store, hot swapped, with an optional client
//!   certificate and the root hints.
//! - [`pushing`]: the pusher of the push certificate in use.
//!
//! **The route families.** The listener serves two families of routes ([`Family`]): the MDM ones
//! (`/enroll`, `/scep`, `/checkin`, `/mdm`), which need the MDM prerequisites, and the package
//! proxy's (`/apt/...`, `/pkg/...`), which need only the name and its certificate
//! (`docs/design/package-proxy.md` section 3). A family whose prerequisites are not met answers
//! `503 unavailable` for each of its routes while the other family is served, decided for each
//! request ([`MdmServer::serves`]), so a Linux-only farm that never uploads an Apple credential
//! serves its packages. In this task the MDM family's prerequisite is a push identity; the CA of
//! the Macs' identities (CM12c) and the ABM token (CM13) add theirs there. `/enroll` and `/scep`
//! are answered here by the enrollment and SCEP handlers (CM12c); the proxy's routes land with
//! PP2, and a request for one of them answers `404` once its family is served. Every other path
//! answers `404`, whoever asks.
//!
//! **What an unknown enrollment is answered** (section 6, lesson 3; section 12 item 12, not
//! verified): `401` with `{"error":"unauthorized"}`, for a request without a certificate or a
//! valid signature, for a certificate the coordinator did not issue, for an identity a
//! replacement or a check-out retired, and for the certificate of an enrollment that is no longer
//! the live one. `401` is what an HTTP client reads as "authenticate again", and it is not
//! `410 Gone`, which a client may read as "this server no longer manages me" and answer by
//! removing the profile; a test keeps the fake Mac's profile through it. Whether the real client
//! re-runs `Authenticate` after a `401` is not established (the farm Mac settles it).

pub(crate) mod auth;
pub(crate) mod checkin;
pub(crate) mod connect;
pub(crate) mod pushing;
pub(crate) mod queue;
pub(crate) mod signature;
pub(crate) mod tls;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_gate::Gate;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode};

use crate::enrollment::Random;
use crate::http::{BoxError, Peer, error, read_body};
use crate::service::Coordinator;
use crate::store::mdm::{MdmError, MdmStore};
use auth::Refused;
use queue::{MdmQueue, Pushers};
use tls::MdmTls;

/// The path of the enrollment URL the ADE profile sends a Mac to.
pub const ENROLL_PATH: &str = "/enroll";

/// The path of the SCEP URL the enrollment profile names.
pub const SCEP_PATH: &str = "/scep";

/// The path of the check-in URL.
pub const CHECKIN_PATH: &str = "/checkin";

/// The path of the server URL.
pub const MDM_PATH: &str = "/mdm";

/// The most bytes of a check-in or a result read: a result carries the Mac's answer to a command
/// (a profile list, the device information), and 1 MiB is far above any of the pinned commands'.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

type Answer = Response<Full<Bytes>>;

/// A group of routes that is served when its own prerequisites are set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    /// `/enroll`, `/scep`, `/checkin` and `/mdm`.
    Mdm,
    /// `/apt/...` and `/pkg/...`, which need only the name and its certificate.
    Proxy,
}

/// The family a path belongs to; none for a path the listener does not serve.
pub(crate) fn family_of(path: &str) -> Option<Family> {
    match path {
        ENROLL_PATH | SCEP_PATH | CHECKIN_PATH | MDM_PATH => Some(Family::Mdm),
        _ if path.starts_with("/apt/") || path.starts_with("/pkg/") => Some(Family::Proxy),
        _ => None,
    }
}

/// What a route reads of the client certificate of the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClientCert {
    /// Nothing: the route is answered whether or not a certificate was presented, and a
    /// certificate is not read (`/enroll`, `/scep`, where the Mac has no identity yet).
    Ignored,
    /// A certificate, or an `Mdm-Signature`, that names an issued identity (`/checkin`, `/mdm`).
    Required,
}

/// The rule of a path; none for a path that is not an MDM route.
pub(crate) fn client_cert(path: &str) -> Option<ClientCert> {
    match path {
        ENROLL_PATH | SCEP_PATH => Some(ClientCert::Ignored),
        CHECKIN_PATH | MDM_PATH => Some(ClientCert::Required),
        _ => None,
    }
}

/// How a request that is refused is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `401`: no valid credential, an unknown or ended identity.
    Unauthorized,
    /// `403`: a credential that is valid for something else, or a rule that refuses.
    Forbidden,
    /// `400`: the body is not read.
    BadRequest,
    /// `503`: the coordinator cannot do it now and nothing was changed.
    Unavailable,
    /// `500`: the store failed.
    Internal,
}

impl Refusal {
    /// The answer.
    pub(crate) fn answer(self) -> Answer {
        match self {
            Self::Unauthorized => error(StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden => error(StatusCode::FORBIDDEN, "forbidden"),
            Self::BadRequest => error(StatusCode::BAD_REQUEST, "bad_request"),
            Self::Unavailable => error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::Internal => error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl From<Refused> for Refusal {
    fn from(why: Refused) -> Self {
        match why {
            Refused::Unauthenticated | Refused::UnknownIdentity | Refused::NotLive => {
                Self::Unauthorized
            }
            Refused::OtherDevice => Self::Forbidden,
            Refused::Store(why) => Self::from(why),
        }
    }
}

impl From<MdmError> for Refusal {
    fn from(why: MdmError) -> Self {
        tracing::error!(%why, "the MDM store failed");
        Self::Internal
    }
}

/// An empty `200`: what the Mac is answered when there is nothing to say.
pub(crate) fn ok() -> Answer {
    Response::new(Full::new(Bytes::new()))
}

/// The commands tried on one connection, by enrollment: a claim on the connection skips them.
/// Every request of a connection carries the connection's list.
#[derive(Clone, Debug, Default)]
pub struct ConnectionTried(Arc<Mutex<BTreeMap<String, Vec<String>>>>);

impl ConnectionTried {
    /// The commands tried on this connection for `enrollment_id`.
    pub(crate) fn of(&self, enrollment_id: &str) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(enrollment_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Records that command `id` was delivered or deferred on this connection for `enrollment_id`.
    pub(crate) fn add(&self, enrollment_id: &str, id: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(enrollment_id.to_owned())
            .or_default()
            .push(id.to_owned());
    }
}

/// What an [`MdmServer`] is made of.
pub(crate) struct Parts {
    /// The MDM tables.
    pub(crate) store: Arc<dyn MdmStore>,
    /// The time.
    pub(crate) clock: Arc<dyn UnixClock>,
    /// Random bytes, for the ids of enrollments.
    pub(crate) random: Random,
    /// The gate, when the coordinator runs it: it hears each check-in and result.
    pub(crate) gate: Option<Arc<Gate>>,
    /// The queue of the commands.
    pub(crate) queue: Arc<MdmQueue>,
    /// Who pushes.
    pub(crate) pushers: Pushers,
    /// The TLS of the listener.
    pub(crate) tls: Arc<MdmTls>,
    /// How often the queue looks at what is due ([`sweep_every`]).
    pub(crate) sweep: Duration,
}

/// The MDM server: what the routes of the Macs' listener use.
pub(crate) struct MdmServer {
    pub(crate) store: Arc<dyn MdmStore>,
    clock: Arc<dyn UnixClock>,
    random: Random,
    pub(crate) gate: Option<Arc<Gate>>,
    pub(crate) queue: Arc<MdmQueue>,
    pushers: Pushers,
    tls: Arc<MdmTls>,
    sweep: Duration,
    /// One `Authenticate` at a time: the live enrollment of a serial is read, then replaced.
    pub(crate) authenticating: tokio::sync::Mutex<()>,
    /// What `/enroll` and `/scep` keep between requests.
    pub(crate) enroll: crate::mdm_enroll::Enroller,
}

impl MdmServer {
    pub(crate) fn new(parts: Parts) -> Self {
        let Parts {
            store,
            clock,
            random,
            gate,
            queue,
            pushers,
            tls,
            sweep,
        } = parts;
        Self {
            store,
            clock,
            random,
            gate,
            queue,
            pushers,
            tls,
            sweep,
            authenticating: tokio::sync::Mutex::new(()),
            enroll: crate::mdm_enroll::Enroller::new(hostkeeper_mdm_ade::roots::apple()),
        }
    }

    /// The same server, whose device blobs must chain to `roots` (DER) and not to Apple's: the
    /// tests' fake Apple.
    #[cfg(test)]
    pub(crate) fn with_apple_roots(mut self, roots: Vec<Vec<u8>>) -> Self {
        self.enroll.roots = roots;
        self
    }

    /// The TLS of the listener.
    pub(crate) fn tls(&self) -> &Arc<MdmTls> {
        &self.tls
    }

    /// Whether the ABM table lists `serial` and has not marked it removed: the one reading of
    /// "this Mac is ours" that `/enroll`, the operator's confirmation and `Authenticate` share.
    ///
    /// # Errors
    /// The store failed.
    pub(crate) fn serial_in_abm(&self, serial: &str) -> Result<bool, MdmError> {
        Ok(self
            .store
            .abm_device(serial)?
            .is_some_and(|device| device.removed_unix.is_none()))
    }

    /// The time, in seconds.
    pub(crate) fn now(&self) -> i64 {
        i64::try_from(self.clock.now_unix()).unwrap_or(i64::MAX)
    }

    /// Fills `buffer` with random bytes; `false` when the system gave none.
    pub(crate) fn random(&self, buffer: &mut [u8]) -> bool {
        (self.random)(buffer)
    }

    /// Whether the routes of `family` are served now. The proxy's need the name's certificate. The
    /// MDM ones need that and a push identity (the prerequisites of the identity CA and of ABM are
    /// added here by CM12c and CM13).
    pub(crate) fn serves(&self, family: Family) -> bool {
        match family {
            Family::Proxy => self.tls.config().is_some(),
            Family::Mdm => self.tls.config().is_some() && (self.pushers)().is_some(),
        }
    }
}

/// Has the queue look at what is due, every `--mdm-queue-sweep-s` seconds, forever: the commands
/// whose timeout passed are queued again and pushed, and a Mac whose push floor elapsed with a
/// command still queued is pushed again.
pub(crate) async fn sweep_every(server: Arc<MdmServer>) -> std::convert::Infallible {
    let mut ticks = tokio::time::interval(server.sweep);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticks.tick().await;
    loop {
        ticks.tick().await;
        if let Err(why) = server.queue.sweep().await {
            tracing::error!(%why, "the MDM queue could not be looked at");
        }
    }
}

/// Answers the request, whose path belongs to `family` ([`family_of`]), from `peer`.
pub(crate) async fn answer<B>(
    coordinator: &Coordinator,
    server: &MdmServer,
    family: Family,
    peer: &Peer,
    request: Request<B>,
    patience: Duration,
) -> Answer
where
    B: hyper::body::Body,
    B::Error: Into<BoxError>,
{
    let path = request.uri().path().to_owned();
    if !server.serves(family) {
        return Refusal::Unavailable.answer();
    }
    let Some(rule) = client_cert(&path) else {
        // The proxy's routes (PP2) have not landed.
        return error(StatusCode::NOT_FOUND, "not_found");
    };
    if rule == ClientCert::Required && request.method() != Method::PUT {
        return error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed");
    }
    let (parts, body) = request.into_parts();
    let Some(body) = read_body(body, MAX_BODY_BYTES, patience).await else {
        return Refusal::BadRequest.answer();
    };
    if rule == ClientCert::Ignored {
        let from = parts
            .extensions
            .get::<crate::http::PeerAddr>()
            .map(|addr| addr.0.ip());
        return if path == ENROLL_PATH {
            crate::mdm_enroll::enroll::answer(coordinator, server, from, &parts.method, &body).await
        } else {
            crate::mdm_enroll::scep::answer(
                coordinator,
                server,
                from,
                &parts.method,
                parts.uri.query(),
                &body,
            )
            .await
        };
    }
    let tried = parts
        .extensions
        .get::<ConnectionTried>()
        .cloned()
        .unwrap_or_default();
    let answered = if path == CHECKIN_PATH {
        checkin::answer(coordinator, server, peer, &parts.headers, &body).await
    } else {
        connect::answer(server, peer, &parts.headers, &body, &tried).await
    };
    answered.unwrap_or_else(Refusal::answer)
}

#[cfg(test)]
mod lease_tests;

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
mod flaky;

#[cfg(test)]
pub(crate) mod support;

/// A queued `DeviceInformation` number `n` for the enrollment `enrollment_id`, as the store takes it.
#[cfg(test)]
fn queue_new(n: u8, enrollment_id: &str) -> crate::store::mdm::NewCommand {
    crate::store::mdm::NewCommand {
        id: support::Rig::command_id(n),
        enrollment_id: enrollment_id.to_owned(),
        command_type: "DeviceInformation".to_owned(),
        payload: b"<plist/>".to_vec(),
        created_unix: i64::try_from(support::NOW).unwrap_or(0),
    }
}
