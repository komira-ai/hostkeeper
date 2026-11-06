//! The enroller: one step of enrollment or renewal at a time ([`Enroller::run_once`]), and the
//! loop that paces the steps ([`Enroller::run_loop`]).
//!
//! What a step does depends on the pki directory and on the wall clock:
//!
//! | `current`                         | step                                                    |
//! |-----------------------------------|---------------------------------------------------------|
//! | none                              | enroll with the pending key (made first when missing)   |
//! | valid, before the middle of it    | nothing ([`Outcome::Idle`])                             |
//! | valid, from the middle of it on   | `Renew`, signed by the key the host holds               |
//! | expired (past `not_after`)        | enroll again with the same key, `renewal_of` the host   |
//!
//! The order of the checks before anything is sent, for every step that talks to the
//! coordinator: first the coordinator's CA, against the pinned `ca_sha256` (a mismatch is
//! [`Error::UntrustedCoordinator`]: nothing is sent, and no key is made); then the key, which for a
//! host not yet enrolled is read from the pending key or made and written durably, and only after
//! that write returned is the request built and posted. A failed write posts nothing, and neither
//! do claims the host cannot give ([`Error::Claims`]).
//!
//! A posted request is polled: an unsigned poll gets a nonce, a poll signed by the request key
//! gets the answer. An issued certificate is checked (`verify.rs`) and stored as a new
//! generation; one that fails a check leaves the pki directory as it was.

use std::path::PathBuf;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_steps::Clock;
use x509_cert::Certificate;
use x509_cert::der::DecodePem;

use crate::claims::{ClaimsError, ClaimsSource};
use crate::key::{Entropy, HostKey, KeyError};
use crate::store::{KeyFrom, Store, StoreError, StoreLock, Stored};
use crate::validity::{self, WallClock};
use crate::verify::{self, Refusal};
use crate::wire::{
    CoordinatorEnroll, EnrollRequest, EnrollState, HostId, Issued, NONCE_MAX, NONCE_MIN, Poll,
    PollAnswer, Reason, RenewAnswer, RenewRequest, RequestId, TransportError, poll_message,
};

/// The seconds between two steps of [`Enroller::run_loop`], whatever the step did.
pub const RETRY_SECS: u64 = 5;

/// The line [`Enroller::run_loop`] logs, once, while the host waits for approval.
pub const NOT_ENROLLED: &str = "not enrolled; waiting for approval";

/// What the enroller is configured with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The SHA-256 of the CA certificate, from the daemon config.
    pub ca_sha256: Sha256Hex,
    /// The boot image or installer the daemon came from, sent with the request.
    pub image: String,
}

/// Everything the enroller reaches outside itself.
pub struct Ports {
    pub coordinator: Box<dyn CoordinatorEnroll>,
    pub store: Box<dyn Store>,
    pub claims: Box<dyn ClaimsSource>,
    pub entropy: Box<dyn Entropy>,
    /// Read only for certificate validity.
    pub wall: Box<dyn WallClock>,
    /// Paces the retries.
    pub clock: Box<dyn Clock>,
}

/// The host's certificate generation, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub host_id: HostId,
    pub generation: u64,
    pub not_before: u64,
    pub not_after: u64,
    pub cert: PathBuf,
    pub ca: PathBuf,
    pub key: PathBuf,
}

/// What one step did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The certificate is valid and renewal is not due.
    Idle,
    /// The request is posted and not yet approved.
    Pending,
    /// A request was approved and its certificate stored, first or through `renewal_of`.
    Enrolled(Identity),
    /// The certificate was renewed and the new one stored.
    Renewed(Identity),
}

/// Where the host is in its enrollment, as the pki directory says: what the daemon's admin
/// `Status` reports ([`Enroller::enrollment`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Enrollment {
    /// No certificate generation and no pending key: no request was made yet.
    Unenrolled,
    /// No certificate generation, and a pending key, whose request id is the `key_sha256` the
    /// coordinator lists for the request.
    Pending { key_sha256: RequestId },
    /// `current` is a certificate of this host: its host id and the last second it is valid
    /// (earlier than now for an expired one, which the next step enrolls again `renewal_of` the
    /// host).
    Enrolled { host_id: HostId, not_after: u64 },
    /// The pki directory cannot be read, or its `current` generation or pending key is not a
    /// certificate and key of this host: why, as a step would say it.
    Unreadable(String),
}

/// How the coordinator broke the enrollment protocol.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("it answered for request {answered}, not {sent}")]
    OtherRequest {
        sent: RequestId,
        answered: RequestId,
    },
    #[error("it answered an unsigned poll without a nonce")]
    NoChallenge,
    #[error("its nonce has {0} bytes, not {NONCE_MIN} to {NONCE_MAX}")]
    NonceLength(usize),
    #[error("it answered a signed poll with another nonce")]
    ChallengeAgain,
}

/// Why a step failed. A failed step changes nothing it has not finished, and is retried.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "the coordinator presents a CA whose SHA-256 is {presented}, not the pinned ca_sha256; nothing was sent"
    )]
    UntrustedCoordinator { presented: String },
    #[error("the coordinator could not be reached: {0}")]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the host key: {0}")]
    Key(#[from] KeyError),
    #[error("the host's claims: {0}")]
    Claims(#[from] ClaimsError),
    #[error("the pki directory's generation {0} is not a certificate and key of this host")]
    Corrupt(u64),
    #[error("enrollment denied: {0}")]
    Denied(Reason),
    #[error("renewal refused: {0}")]
    RenewRefused(Reason),
    #[error("the coordinator broke the enrollment protocol: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("the issued certificate was refused: {0}")]
    Refused(#[from] Refusal),
}

/// The current generation, read.
struct Held {
    number: u64,
    host_id: HostId,
    not_before: u64,
    not_after: u64,
    key: HostKey,
}

/// The poll's answer, once it is neither an error nor a denial.
enum Polled {
    Pending,
    Issued(Issued),
}

/// Enrollment and renewal of one host.
pub struct Enroller {
    settings: Settings,
    ports: Ports,
    /// The request already posted by this process, with its `renewal_of`; cleared when a
    /// certificate is installed or an issued one turned out expired, so the next request is
    /// posted again.
    posted: Option<(RequestId, Option<HostId>)>,
    /// The host an issued certificate named when it expired before it could be installed: the
    /// pending key returns as `renewal_of` that host.
    returning: Option<HostId>,
    _lock: StoreLock,
}

impl Enroller {
    /// Takes the store lock, refused when another enroller holds it, and finishes or discards
    /// what a crash left in the pki directory. The lock is held until the enroller is dropped.
    pub fn open(settings: Settings, ports: Ports) -> Result<Enroller, Error> {
        let lock = ports.store.open()?;
        Ok(Enroller {
            settings,
            ports,
            posted: None,
            returning: None,
            _lock: lock,
        })
    }

    /// One step; see the module documentation.
    pub fn run_once(&mut self) -> Result<Outcome, Error> {
        let Some(held) = self.held()? else {
            return self.enroll(None);
        };
        let wall = &*self.ports.wall;
        if validity::expired(wall, held.not_after) {
            self.enroll(Some(held))
        } else if validity::renewal_due(wall, held.not_before, held.not_after) {
            self.renew(held)
        } else {
            Ok(Outcome::Idle)
        }
    }

    /// Runs [`Enroller::run_once`] while `keep_going` says so, sleeping [`RETRY_SECS`] on the
    /// clock after each step. A step's line goes to `log` only when it differs from the line of
    /// the step before: a host waiting for approval logs [`NOT_ENROLLED`] once. After each step,
    /// before the sleep, `seen` is given the host's enrollment ([`Enroller::enrollment`]).
    pub fn run_loop(
        &mut self,
        log: &mut dyn FnMut(&str),
        seen: &mut dyn FnMut(Enrollment),
        keep_going: &mut dyn FnMut() -> bool,
    ) {
        let mut last: Option<String> = None;
        while keep_going() {
            let line = describe(&self.run_once());
            if line != last {
                if let Some(text) = &line {
                    log(text);
                }
                last = line;
            }
            seen(self.enrollment());
            self.ports.clock.sleep_secs(RETRY_SECS);
        }
    }

    /// The host's enrollment, read from the pki directory as a step reads it: the current
    /// generation as [`Enroller::run_once`] parses it, else the pending key. Changes nothing but
    /// what reading the pending key makes durable ([`Store::pending_key`]).
    pub fn enrollment(&self) -> Enrollment {
        self.read_enrollment()
            .unwrap_or_else(|error| Enrollment::Unreadable(error.to_string()))
    }

    fn read_enrollment(&self) -> Result<Enrollment, Error> {
        if let Some(held) = self.held()? {
            return Ok(Enrollment::Enrolled {
                host_id: held.host_id,
                not_after: held.not_after,
            });
        }
        Ok(match self.ports.store.pending_key()? {
            None => Enrollment::Unenrolled,
            Some(der) => Enrollment::Pending {
                key_sha256: HostKey::from_pkcs8(&der)?.request_id(),
            },
        })
    }

    /// Removes the generations older than generation `used`, when `used` is still current: to be
    /// called with the generation a heartbeat or renewal succeeded with, so a generation that
    /// has not worked is never the only one left, and a late confirmation of an older generation
    /// removes nothing.
    pub fn confirm_generation(&mut self, used: u64) -> Result<(), Error> {
        Ok(self.ports.store.prune(used)?)
    }

    /// The current generation, read and parsed.
    fn held(&self) -> Result<Option<Held>, Error> {
        let Some(stored) = self.ports.store.current()? else {
            return Ok(None);
        };
        let number = stored.number;
        read_held(stored, self.settings.ca_sha256.as_str())
            .map(Some)
            .ok_or(Error::Corrupt(number))
    }

    /// Checks the coordinator's CA against the pin; nothing is sent before this passes.
    fn check_trust(&mut self) -> Result<(), Error> {
        let presented = verify::sha256_hex(&self.ports.coordinator.presented_ca_der()?);
        if presented != self.settings.ca_sha256.as_str() {
            return Err(Error::UntrustedCoordinator { presented });
        }
        Ok(())
    }

    /// The pending key; made, and written durably, when there is none.
    fn pending_key(&mut self) -> Result<HostKey, Error> {
        if let Some(der) = self.ports.store.pending_key()? {
            return Ok(HostKey::from_pkcs8(&der)?);
        }
        let key = HostKey::generate(&mut *self.ports.entropy)?;
        self.ports.store.write_pending_key(&key.pkcs8_der()?)?;
        Ok(key)
    }

    /// Enrolls with the pending key, or with the key of `renewal` for a host that expired.
    fn enroll(&mut self, renewal: Option<Held>) -> Result<Outcome, Error> {
        self.check_trust()?;
        let (key, from, renewal_of) = match renewal {
            Some(held) => (
                held.key,
                KeyFrom::Generation(held.number),
                Some(held.host_id),
            ),
            None => (
                self.pending_key()?,
                KeyFrom::Pending,
                self.returning.clone(),
            ),
        };
        let id = key.request_id();
        let wanted = (id.clone(), renewal_of.clone());
        if self.posted.as_ref() != Some(&wanted) {
            let request = EnrollRequest {
                csr_der: key.csr_der()?,
                claims: self.ports.claims.claims()?,
                image: self.settings.image.clone(),
                renewal_of: renewal_of.clone(),
            };
            let answer = self.ports.coordinator.enroll(&request)?;
            if answer.request_id != id {
                return Err(ProtocolError::OtherRequest {
                    sent: id,
                    answered: answer.request_id,
                }
                .into());
            }
            if let EnrollState::Denied(reason) = answer.state {
                return Err(Error::Denied(reason));
            }
            self.posted = Some(wanted);
        }
        match self.poll(&key, &id)? {
            Polled::Pending => Ok(Outcome::Pending),
            Polled::Issued(issued) => match self.store(&issued, &key, from, renewal_of.as_ref()) {
                Ok(identity) => {
                    self.posted = None;
                    self.returning = None;
                    Ok(Outcome::Enrolled(identity))
                }
                Err(error) => {
                    self.note_expired(&error, &issued);
                    Err(error)
                }
            },
        }
    }

    /// After a refusal of an issued certificate as expired (approved, then not installed until
    /// past its end): the next step posts again, as `renewal_of` the host it named, so that one
    /// approval returns the key the host holds.
    fn note_expired(&mut self, error: &Error, issued: &Issued) {
        if let Error::Refused(Refusal::NotValidNow { not_after, .. }) = error
            && validity::expired(&*self.ports.wall, *not_after)
        {
            self.posted = None;
            self.returning = HostId::new(&issued.host_id);
        }
    }

    /// An unsigned poll for the nonce, then the poll signed by `key`.
    fn poll(&mut self, key: &HostKey, id: &RequestId) -> Result<Polled, Error> {
        let unsigned = Poll {
            request_id: id.clone(),
            nonce: Vec::new(),
            signature: Vec::new(),
        };
        let PollAnswer::Challenge(nonce) = self.ports.coordinator.poll(&unsigned)? else {
            return Err(ProtocolError::NoChallenge.into());
        };
        if !(NONCE_MIN..=NONCE_MAX).contains(&nonce.len()) {
            return Err(ProtocolError::NonceLength(nonce.len()).into());
        }
        let signed = Poll {
            request_id: id.clone(),
            signature: key.sign(&poll_message(id, &nonce)),
            nonce,
        };
        match self.ports.coordinator.poll(&signed)? {
            PollAnswer::Pending => Ok(Polled::Pending),
            PollAnswer::Issued(issued) => Ok(Polled::Issued(issued)),
            PollAnswer::Denied(reason) => Err(Error::Denied(reason)),
            PollAnswer::Challenge(_) => Err(ProtocolError::ChallengeAgain.into()),
        }
    }

    /// Renews the certificate of `held`, signed by its key.
    fn renew(&mut self, held: Held) -> Result<Outcome, Error> {
        self.check_trust()?;
        let request = RenewRequest {
            host_id: held.host_id.clone(),
            csr_der: held.key.csr_der()?,
        };
        match self.ports.coordinator.renew(&request)? {
            RenewAnswer::Refused(reason) => Err(Error::RenewRefused(reason)),
            RenewAnswer::Issued(issued) => {
                let from = KeyFrom::Generation(held.number);
                let identity = self.store(&issued, &held.key, from, Some(&held.host_id))?;
                Ok(Outcome::Renewed(identity))
            }
        }
    }

    /// Checks `issued` and stores it as a new generation.
    fn store(
        &mut self,
        issued: &Issued,
        key: &HostKey,
        from: KeyFrom,
        held: Option<&HostId>,
    ) -> Result<Identity, Error> {
        let pin = self.settings.ca_sha256.as_str();
        let verified = verify::verify(issued, key, pin, held, &*self.ports.wall)?;
        let written =
            self.ports
                .store
                .write_generation(&issued.cert_pem, &issued.ca_bundle_pem, from);
        // An error once `current` started to switch may hide a published generation: it is this
        // certificate's when `current` holds it.
        let stored = match written {
            Ok(stored) => stored,
            Err(error) => match self.ports.store.current() {
                Ok(Some(stored)) if stored.cert_pem == issued.cert_pem => stored,
                _ => return Err(error.into()),
            },
        };
        Ok(Identity {
            host_id: verified.host_id,
            generation: stored.number,
            not_before: verified.not_before,
            not_after: verified.not_after,
            cert: stored.cert_path(),
            ca: stored.ca_path(),
            key: stored.key_path(),
        })
    }
}

/// A stored generation as a certificate of this host and its key; `None` when it is not one:
/// the certificate must pass every check of `verify.rs` but the validity window (an expired
/// generation is still the host's, for `renewal_of`) against its own CA bundle and the pin, for
/// the key stored beside it.
fn read_held(stored: Stored, pin: &str) -> Option<Held> {
    let cert = Certificate::from_pem(stored.cert_pem.as_bytes()).ok()?;
    let host_id = verify::common_name(&cert)?
        .strip_prefix("host:")?
        .to_owned();
    let key = HostKey::from_pkcs8(&stored.key_der).ok()?;
    let issued = Issued {
        host_id,
        cert_pem: stored.cert_pem,
        ca_bundle_pem: stored.ca_pem,
    };
    let verified = verify::check(&issued, &key, pin, None).ok()?;
    Some(Held {
        number: stored.number,
        host_id: verified.host_id,
        not_before: verified.not_before,
        not_after: verified.not_after,
        key,
    })
}

/// The line a step logs; `None` for a quiet step.
fn describe(result: &Result<Outcome, Error>) -> Option<String> {
    match result {
        Ok(Outcome::Idle) => None,
        Ok(Outcome::Pending) => Some(NOT_ENROLLED.to_owned()),
        Ok(Outcome::Enrolled(identity)) => Some(format!(
            "enrolled as {} (generation {})",
            identity.host_id.subject(),
            identity.generation
        )),
        Ok(Outcome::Renewed(identity)) => Some(format!(
            "renewed the certificate of {} (generation {})",
            identity.host_id.subject(),
            identity.generation
        )),
        Err(error) => Some(error.to_string()),
    }
}
