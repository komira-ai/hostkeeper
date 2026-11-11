//! `/enroll` and `/scep` of the Macs' listener (`docs/design/coordinator-mdm.md`, sections 4.2 and
//! 5.1, task CM12c): how a Mac that has no identity yet gets one.
//!
//! - [`enroll`]: Setup Assistant posts the device blob. A blob that verifies against Apple's roots
//!   (`hostkeeper-mdm-ade`) for a serial in the ABM table is answered with the enrollment profile,
//!   which carries a one-time challenge bound to the serial and the blob's UDID. **Fail closed**: a
//!   blob that cannot be verified gets no challenge on its own. It is held as a request an
//!   operator can confirm ([`pending`]); the operator's confirmation reads the ABM table again,
//!   and the next post of the same serial and UDID gets the challenge, marked confirmed. A serial
//!   that is not in the ABM table is declined `serial_not_in_abm` whether the blob verifies or not
//!   (a manual enrollment, a GET, is declined the same way).
//! - [`scep`]: `GetCACaps`, `GetCACert` and `PKIOperation` over the identity CA ([`ca`]), with the
//!   challenges in the MDM tables ([`scep::StoreChallenges`]). The CA is made when it is first
//!   needed and its key is sealed in the setup.
//!
//! **What this does not do.** It enters no enrollment: `Authenticate` does (`mdm_server::checkin`),
//! under owner decision B, and re-reads the ABM table there. A replayed blob can hold a serial's
//! place in the list of requests and gets a challenge only if it verifies (and a captured blob
//! does: the blob carries no freshness, `docs/design/coordinator-mdm.md` 9.2); it approves
//! nothing, since approval is the gate's, from the nonce.
//!
//! **Limits.** `/enroll` and `/scep` do an RSA operation or a signature check on a stranger's
//! bytes, so each peer address is limited to `enroll_rate_per_min` requests a minute
//! ([`pending::RateLimit`]) before either runs.

pub(crate) mod ca;
pub(crate) mod enroll;
pub(crate) mod pending;
pub(crate) mod scep;

use hyper::StatusCode;

use crate::http::error;
use crate::mdm_settings;
use crate::mdm_setup::MdmSetup;

type Answer = hyper::Response<http_body_util::Full<bytes::Bytes>>;

/// What the enrollment routes keep between requests.
pub(crate) struct Enroller {
    /// The requests that wait for an operator, the serials declined, and the confirmations.
    pub(crate) pending: pending::Pending,
    /// The requests each peer address made this minute.
    pub(crate) limiter: pending::RateLimit,
    /// One CA is made at a time.
    pub(crate) creating: tokio::sync::Mutex<()>,
    /// The DER of the roots a device blob must chain to.
    pub(crate) roots: Vec<Vec<u8>>,
}

impl Enroller {
    /// An enroller whose blobs must chain to `roots`.
    pub(crate) fn new(roots: Vec<Vec<u8>>) -> Self {
        Self {
            pending: pending::Pending::default(),
            limiter: pending::RateLimit::default(),
            creating: tokio::sync::Mutex::new(()),
            roots,
        }
    }
}

/// Why a request to `/enroll` or `/scep` is not served; each is one answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decline {
    /// `429`: this peer made too many requests this minute.
    RateLimited,
    /// `403 serial_not_in_abm`: the serial is not in the ABM table.
    NotInAbm,
    /// `503 waiting_for_operator`: the blob could not be verified; the request waits.
    Held,
    /// `400`: the body is not what the route reads.
    BadRequest,
    /// `405`.
    Method,
    /// `503`: the coordinator cannot serve it now and changed nothing.
    Unavailable,
    /// `500`.
    Internal,
}

impl Decline {
    pub(crate) fn answer(self) -> Answer {
        match self {
            Self::RateLimited => error(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            Self::NotInAbm => error(StatusCode::FORBIDDEN, "serial_not_in_abm"),
            Self::Held => error(StatusCode::SERVICE_UNAVAILABLE, "waiting_for_operator"),
            Self::BadRequest => error(StatusCode::BAD_REQUEST, "bad_request"),
            Self::Method => error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
            Self::Unavailable => error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::Internal => error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

/// The numbers and the text the enrollment reads of the setup, at the moment of a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tunables {
    pub(crate) identity_days: u32,
    pub(crate) ca_days: u32,
    pub(crate) challenge_ttl_s: u32,
    pub(crate) rate_per_min: u32,
    pub(crate) pending_max: u32,
    pub(crate) org: String,
    pub(crate) profile_name: String,
    /// The name the Macs reach the server by (`mdm_name`), when it is set and is a host name.
    pub(crate) dns_name: Option<String>,
}

impl Tunables {
    /// The tunables of `setup`; none when its settings cannot be read.
    pub(crate) fn read(setup: &MdmSetup) -> Option<Self> {
        let set = setup
            .settings()
            .inspect_err(|why| tracing::error!(%why, "the MDM settings could not be read"))
            .ok()?;
        let number = |name: &str| mdm_settings::number_of(&set, name, 0);
        let text = |name: &str| {
            mdm_settings::text_of(&set, name)
                .unwrap_or_default()
                .to_owned()
        };
        Some(Self {
            identity_days: number("identity_days"),
            ca_days: number("ca_days"),
            challenge_ttl_s: number("challenge_ttl_s"),
            rate_per_min: number("enroll_rate_per_min"),
            pending_max: number("enroll_pending_max"),
            org: text("identity_org"),
            profile_name: text("ade_profile_name"),
            dns_name: set
                .get("mdm_name")
                .filter(|name| crate::abm::profile::is_host_name(name))
                .cloned(),
        })
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
