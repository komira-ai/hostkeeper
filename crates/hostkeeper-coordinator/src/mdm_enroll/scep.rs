//! `/scep`: `GetCACaps`, `GetCACert` and `PKIOperation` over the identity CA, with the
//! challenges in the MDM tables.
//!
//! A Mac's client calls `GetCACert`, `GetCACaps` and then posts the request
//! (`docs/design/coordinator-mdm-cm12a.md`; the spike measured it). A retry after a failure is a
//! new session with a new `transactionID`, and possibly the same key: the replay is keyed on the
//! challenge and the key, so a challenge that was not consumed is redeemed by the retry, one that
//! was is answered with the certificate stored for the same key, and another key is refused.

use std::net::IpAddr;
use std::time::{Duration, SystemTime};

use hostkeeper_mdm_scep::{Challenges, Error, KeyHash, Redeem};
use hyper::Method;
use sha2::{Digest, Sha256};

use super::{Answer, Decline, Tunables, ca, ca_unavailable, reply};
use crate::mdm_server::MdmServer;
use crate::service::Coordinator;
use crate::store::mdm::{Consumed, IdentityState, Issuance, MdmStore};

/// The challenges of the MDM tables, as the SCEP library redeems them.
pub(crate) struct StoreChallenges<'a> {
    /// The tables.
    pub(crate) store: &'a dyn MdmStore,
    /// The time, unix seconds.
    pub(crate) now: i64,
}

/// The hash the tables keep a challenge under.
pub(crate) fn challenge_hash(challenge: &str) -> String {
    hex::encode(Sha256::digest(challenge.as_bytes()))
}

fn failed(why: impl std::fmt::Display) -> Redeem {
    tracing::error!(%why, "the MDM store failed while a challenge was redeemed");
    Redeem::Failed(Error::Build("the challenge store"))
}

/// A SCEP message that names no transaction, so no reply can be formed.
fn no_transaction(why: Error) -> Decline {
    tracing::warn!(%why, "refused a SCEP message that names no transaction");
    Decline::BadRequest
}

/// When the certificate `der` stops being valid, unix seconds; 0 (already expired) for bytes that
/// are no certificate, which the CA does not issue.
fn not_after(der: &[u8]) -> i64 {
    x509_parser::parse_x509_certificate(der)
        .map_or(0, |(_, cert)| cert.validity().not_after.timestamp())
}

impl Challenges for StoreChallenges<'_> {
    fn redeem(
        &mut self,
        challenge: &str,
        key_hash: KeyHash,
        issue: &mut dyn FnMut(&str) -> Result<Vec<u8>, Error>,
    ) -> Redeem {
        let (challenge, key) = (challenge_hash(challenge), hex::encode(key_hash));
        let row = match self.store.challenge(&challenge) {
            Ok(Some(row)) => row,
            Ok(None) => return Redeem::Unknown,
            Err(why) => return failed(why),
        };
        match (row.state, row.issued) {
            (IdentityState::Retired | IdentityState::Dead, _) => return Redeem::Unknown,
            (IdentityState::Issued, Some(stored))
                if row.key_sha256.as_deref() == Some(key.as_str()) =>
            {
                return Redeem::Replayed(stored.cert_der);
            }
            (IdentityState::Issued, _) => return Redeem::Consumed,
            (IdentityState::Challenged, _) => {}
        }
        if row.expires_unix <= self.now {
            return Redeem::Unknown;
        }
        let der = match issue(&row.serial) {
            Ok(der) => der,
            Err(why) => return Redeem::Failed(why),
        };
        let issuance = Issuance {
            cert_sha256: hex::encode(Sha256::digest(&der)),
            cert_der: der.clone(),
            not_after_unix: not_after(&der),
        };
        match self
            .store
            .consume_challenge(&challenge, &key, &issuance, self.now)
        {
            Ok(Consumed::Issued) => Redeem::Issued(der),
            // A request of the same key won the race: its certificate is the answer.
            Ok(Consumed::Replayed(stored)) => Redeem::Replayed(stored.cert_der),
            // Another key won the race (the only refusal left once the row was read open).
            Ok(Consumed::Refused(_)) => Redeem::Consumed,
            Err(why) => failed(why),
        }
    }
}

/// The value of the `operation` parameter of `query`.
fn operation(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("operation="))
}

/// Answers a request to `/scep`.
pub(crate) async fn answer(
    coordinator: &Coordinator,
    server: &MdmServer,
    peer: Option<IpAddr>,
    method: &Method,
    query: Option<&str>,
    body: &[u8],
) -> Answer {
    match run(coordinator, server, peer, method, query, body).await {
        Ok(answer) => answer,
        Err(decline) => decline.answer(),
    }
}

async fn run(
    coordinator: &Coordinator,
    server: &MdmServer,
    peer: Option<IpAddr>,
    method: &Method,
    query: Option<&str>,
    body: &[u8],
) -> Result<Answer, Decline> {
    let tune = Tunables::read(&coordinator.mdm_setup).ok_or(Decline::Unavailable)?;
    let now = server.now();
    if !server.enroll.limiter.allow(peer, now, tune.rate_per_min) {
        return Err(Decline::RateLimited);
    }
    let operation = operation(query).ok_or(Decline::BadRequest)?;
    let wants_post = operation == "PKIOperation";
    if *method
        != if wants_post {
            Method::POST
        } else {
            Method::GET
        }
    {
        return Err(Decline::Method);
    }
    let ca = ca::identity_ca(coordinator, server, &tune)
        .await
        .map_err(ca_unavailable)?;
    match operation {
        "GetCACaps" => Ok(reply("text/plain", ca.ca_caps().as_bytes().to_vec())),
        "GetCACert" => Ok(reply("application/x-x509-ca-cert", ca.ca_cert().to_vec())),
        "PKIOperation" => {
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap_or(0));
            let mut challenges = StoreChallenges {
                store: &*server.store,
                now,
            };
            let done = ca
                .pki_operation(
                    body,
                    &mut challenges,
                    at,
                    &mut rsa::rand_core::UnwrapErr(getrandom::SysRng),
                )
                .map_err(no_transaction)?;
            tracing::info!(verdict = ?done.verdict, "answered a SCEP PKIOperation");
            Ok(reply("application/x-pki-message", done.der))
        }
        _ => Err(Decline::BadRequest),
    }
}
