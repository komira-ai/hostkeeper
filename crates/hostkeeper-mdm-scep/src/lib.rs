//! The coordinator's SCEP server as a library (docs/design/coordinator-mdm.md 4.2, task CM12b):
//! RFC 8894 as a Mac's SCEP client speaks it, over the MDM identity CA of this crate.
//!
//! - [`Ca::ca_caps`] answers `GetCACaps`, [`Ca::ca_cert`] answers `GetCACert`.
//! - [`Ca::pki_operation`] answers `PKIOperation`: it checks the signed request, opens the CMS
//!   envelope, checks the certificate request, redeems the one-time challenge through the
//!   [`Challenges`] port and answers with a signed `CertRep`, enveloped to the sender when it carries
//!   the certificate.
//!
//! The crate is pure: bytes in, bytes out, the clock and the random source passed in, no I/O. Rules:
//!
//! - **A refused request still gets a reply.** A request that parses as far as its `transactionID`
//!   and refuses after that answers with a signed FAILURE `CertRep` (the caller sends it with HTTP
//!   200, which is what a Mac's client reads); only a message with no `transactionID` is an
//!   [`Error`], for which the caller answers with an HTTP error.
//! - **A refusal consumes nothing.** The store records a challenge only together with the certificate
//!   issued for it.
//! - **The replay is keyed on the challenge and the hash of the certificate request's public key**,
//!   not on the `transactionID`: a Mac that retries opens a new session with a new `transactionID`
//!   (measured, `coordinator-mdm-cm12a.md`), and the stored certificate is the answer to that retry
//!   too. A different key presenting a consumed challenge is refused.
//! - **The subject is the serial the challenge was issued for**, `CN=<serial>, O=<org>,
//!   OU=hostkeeper-mdm`; the subject of the request is ignored.
//! - **The signature is checked over the bytes the sender signed.** The signed attributes are
//!   hashed as received, with the tag changed to `SET`, not re-encoded, and a sender's BER set order
//!   or encoding does not break a valid message.
//!
//! `cms` supplies the builder of the signed and enveloped reply and has no verify or decrypt side,
//! which this crate writes. Its builder leaves out `contentType` and `messageDigest` of the signed
//! attributes when there is no content, which a failure reply has and RFC 5652 section 5.3 forbids;
//! the reply passes the digest of the empty string to avoid that (a test checks a failure reply
//! with a verifier of its own).

mod asn1;
mod ca;
mod envelope;
mod error;
mod oids;
mod reply;
mod request;

use std::time::SystemTime;

pub use ca::{Ca, Policy};
pub use error::Error;
pub use rsa::rand_core::CryptoRng;

/// What `GetCACaps` answers: the capabilities this server implements. `DES3` and `SHA-1` are
/// accepted from a client and not advertised; there is no `Renewal` (section 4.2 re-issues the
/// identity with an `InstallProfile`).
pub const CA_CAPS: &str = "POSTPKIOperation\nSHA-256\nAES\nSCEPStandard\n";

/// The hash of a certificate request's public key (SHA-256 of its `SubjectPublicKeyInfo`).
pub type KeyHash = [u8; 32];

/// The store of one-time challenges, which the coordinator implements on its own store (CM12c) and
/// the tests on a map.
pub trait Challenges {
    /// Redeem `challenge` for the holder of `key_hash`, atomically: in one transaction find the
    /// challenge, check it has not expired, and then
    ///
    /// - not consumed: call `issue` with the serial the challenge was issued for, record the
    ///   certificate it returns together with `key_hash` (consuming the challenge), and return
    ///   [`Redeem::Issued`]; when `issue` fails record nothing and return [`Redeem::Failed`];
    /// - consumed by the same `key_hash`: return [`Redeem::Replayed`] with the stored certificate,
    ///   without calling `issue`;
    /// - consumed by another key: [`Redeem::Consumed`];
    /// - not found or expired: [`Redeem::Unknown`].
    fn redeem(
        &mut self,
        challenge: &str,
        key_hash: KeyHash,
        issue: &mut dyn FnMut(&str) -> Result<Vec<u8>, Error>,
    ) -> Redeem;
}

/// The answer of [`Challenges::redeem`].
#[derive(Debug, PartialEq, Eq)]
pub enum Redeem {
    /// The challenge was open; the certificate (DER) was issued and recorded.
    Issued(Vec<u8>),
    /// The challenge was consumed by this key; the stored certificate (DER).
    Replayed(Vec<u8>),
    /// The challenge was consumed by another key.
    Consumed,
    /// No such challenge, or it expired.
    Unknown,
    /// The challenge was open and the certificate could not be issued; nothing was recorded.
    Failed(Error),
}

/// What became of a `PKIOperation`, for the coordinator's audit; the reply bytes are in [`Reply`].
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Verdict {
    /// A certificate was issued and the challenge consumed.
    Issued,
    /// A retry with the same key got the stored certificate. This proves knowledge of the
    /// challenge and of the key's public half, which is public, and not possession of the private
    /// key: the reply is enveloped to the signer certificate of the retry, which is not tied to
    /// the key hash. The caller must not treat it as proof of key possession (for example by
    /// marking the device enrolled).
    Replayed,
    /// A FAILURE reply: the request was refused.
    Refused(Refusal),
}

/// Why a request was refused; each maps to the `failInfo` of the reply.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Refusal {
    /// The signature, the digest or the certificate request's self-signature did not verify, or the
    /// envelope could not be opened (`badMessageCheck`).
    BadMessage(&'static str),
    /// An algorithm this server does not implement (`badAlg`).
    BadAlg(&'static str),
    /// A well-formed request this server does not answer: another message type, a key that is not
    /// RSA or is too short, no challenge (`badRequest`).
    BadRequest(&'static str),
    /// The challenge is unknown or expired (`badRequest`).
    ChallengeUnknown,
    /// The challenge was consumed by another key (`badRequest`).
    ChallengeConsumed,
    /// The certificate could not be issued (`badRequest`).
    IssueFailed,
}

/// The answer to a `PKIOperation`.
#[derive(Debug)]
pub struct Reply {
    /// The DER of the `CertRep`, content type `application/x-pki-message`.
    pub der: Vec<u8>,
    /// What happened.
    pub verdict: Verdict,
}

impl Ca {
    /// `GetCACaps`: the body, `text/plain`.
    pub fn ca_caps(&self) -> &'static str {
        CA_CAPS
    }

    /// `GetCACert`: the DER of the CA certificate, `application/x-x509-ca-cert`.
    pub fn ca_cert(&self) -> &[u8] {
        self.cert_der()
    }

    /// `PKIOperation`: `body` is the DER of the PKCS#7 message (the POST body, or the decoded
    /// `message` of a GET). `now` and `rng` are the clock and the random source.
    ///
    /// The caller's contract: this opens an envelope with the CA's RSA private key before the
    /// challenge can be looked up (the challenge is inside the encrypted request), and the signer
    /// of a request is unauthenticated, so the caller rate-limits per peer before calling it (CM12c
    /// tests that limit). The reason in [`Verdict::Refused`] is for the audit log; it is never sent
    /// to the peer, which sees only the failure reply, because the reasons differ in what they
    /// reveal about the envelope. The input is DER; BER encodings (indefinite lengths, segmented
    /// octet strings) are answered with [`Error::Malformed`], and no capture of a real Mac's
    /// request has confirmed they are not needed.
    pub fn pki_operation(
        &self,
        body: &[u8],
        store: &mut dyn Challenges,
        now: SystemTime,
        rng: &mut impl CryptoRng,
    ) -> Result<Reply, Error> {
        let message = request::Message::parse(body)?;
        let outcome = request::process(self, &message, store, now, rng);
        reply::build_reply(self, &message, outcome, rng)
    }
}
