//! The CA certificate as the hosts see it.
//!
//! A daemon config pins the CA by the SHA-256 of its certificate's DER (`ca_sha256`, design
//! 4.1 and 5.7): [`CaBundle::sha256_hex`] is that value. A poll that finds the certificate made
//! returns it with the host id and the CA certificate ([`PollBundle`]).

use crate::names::HostId;

/// The CA certificate and its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaBundle {
    certificate_der: Vec<u8>,
    sha256: [u8; 32],
}

impl CaBundle {
    /// The bundle of the CA certificate `certificate_der`.
    pub fn of(certificate_der: Vec<u8>) -> CaBundle {
        let digest = ring::digest::digest(&ring::digest::SHA256, &certificate_der);
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(digest.as_ref());
        CaBundle {
            certificate_der,
            sha256,
        }
    }

    /// The CA certificate, DER.
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    /// The SHA-256 of the CA certificate's DER.
    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// The SHA-256 as 64 lower-case hex digits: the `ca_sha256` of a daemon config.
    pub fn sha256_hex(&self) -> String {
        hex::encode(self.sha256)
    }
}

/// What a poll returns once the host's certificate is made: the certificate, the host id it
/// names, and the CA certificate to check the coordinator's with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollBundle {
    pub certificate_der: Vec<u8>,
    pub host_id: HostId,
    pub ca: CaBundle,
}
