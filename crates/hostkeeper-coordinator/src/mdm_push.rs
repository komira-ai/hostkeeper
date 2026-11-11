//! The push certificate in the MDM setup (`docs/design/coordinator-mdm.md`, sections 4.4 and 4.9):
//! how its records are kept, what is read of them, and the transport a coordinator has when none
//! is configured.
//!
//! **Records.** `push_pending_key` holds the key of the pending request and its CSR, two PEM
//! blocks ([`pending_bundle`], [`split_pending`]); `push_encryption_key` the key mdmcert.download
//! encrypts its reply to; `push_key` and `push_cert` the pair in use, which
//! [`MdmSetup::push_identity`] reads together. The newest version of a kind is the one in use.
//! Uploading a certificate stores the key, then the certificate: a failure between the two leaves
//! the new key beside the old certificate, and [`MdmSetup::push_identity`] then pairs the
//! certificate with the key it replaced (kept for `secret_grace_days`), so the old identity keeps
//! working until the upload is made again.

use std::sync::Arc;
use std::time::Duration;

use hostkeeper_mdm_push::BoxFuture;
use hostkeeper_mdm_push::identity::{IdentityError, PushIdentity};
use hostkeeper_mdm_push::mdmcert::{HttpReply, MdmcertTransport, TransportError};
use hostkeeper_mdm_push::pusher::PushSettings;

use crate::mdm_secrets::SecretKind;
use crate::mdm_settings;
use crate::mdm_setup::{MdmSetup, SetupFailure};

/// The first line of a CSR in PEM, where the pending key ends and its CSR begins.
const CSR_BEGIN: &str = "-----BEGIN CERTIFICATE REQUEST-----";

/// Why the push identity could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PushIdentityError {
    /// The setup could not be read, or a record did not open.
    #[error(transparent)]
    Setup(#[from] SetupFailure),
    /// The certificate and key held are not a push identity.
    #[error(transparent)]
    Identity(#[from] IdentityError),
}

/// The bytes of the pending-request record: the key's PEM, then the CSR's.
#[must_use]
pub fn pending_bundle(key_pem: &[u8], csr_pem: &str) -> Vec<u8> {
    let mut bundle = key_pem.to_vec();
    if !bundle.ends_with(b"\n") {
        bundle.push(b'\n');
    }
    bundle.extend_from_slice(csr_pem.as_bytes());
    bundle
}

/// The key (PEM) and the CSR (PEM) of a pending-request record; none when it holds no CSR.
#[must_use]
pub fn split_pending(bundle: &[u8]) -> Option<(Vec<u8>, String)> {
    let text = std::str::from_utf8(bundle).ok()?;
    let at = text.find(CSR_BEGIN)?;
    Some((text.as_bytes()[..at].to_vec(), text[at..].to_owned()))
}

impl MdmSetup {
    /// The push identity in use: the newest `push_cert` with the newest `push_key` that matches it;
    /// none when either is not held.
    ///
    /// # Errors
    /// [`PushIdentityError::Setup`]: no master key, a record that does not open, or the store;
    /// `Identity`: the two do not make a push identity (the key is not the certificate's).
    pub fn push_identity(&self) -> Result<Option<PushIdentity>, PushIdentityError> {
        let (Some(cert), keys) = (
            self.open(SecretKind::PushCert)?,
            self.open_held(SecretKind::PushKey)?,
        ) else {
            return Ok(None);
        };
        // The newest key that is the certificate's: an upload that stored its key and then failed
        // to store its certificate leaves the new key beside the old certificate, and the key it
        // replaced, still held, is the one that matches.
        let mut first_error = None;
        for key in &keys {
            match PushIdentity::parse(cert.expose(), key.expose()) {
                Ok(identity) => return Ok(Some(identity)),
                Err(why) => {
                    first_error.get_or_insert(why);
                }
            }
        }
        first_error.map_or(Ok(None), |why| Err(why.into()))
    }

    /// The push settings: `push_retry_s`, `push_expiry_s` and `push_priority`, as set or by their
    /// defaults, and `push_timeout_s`.
    ///
    /// # Errors
    /// [`SetupFailure::Store`]: the settings cannot be read.
    pub fn push_settings(&self) -> Result<(PushSettings, Duration), SetupFailure> {
        let set = self.settings()?;
        let settings = PushSettings {
            retry_s: u64::from(mdm_settings::number_of(&set, "push_retry_s", 3600)),
            expiry_s: u64::from(mdm_settings::number_of(&set, "push_expiry_s", 86_400)),
            priority: u8::try_from(mdm_settings::number_of(&set, "push_priority", 10))
                .unwrap_or(10),
        };
        let timeout = u64::from(mdm_settings::number_of(&set, "push_timeout_s", 30));
        Ok((settings, Duration::from_secs(timeout)))
    }
}

/// The transport of a coordinator built with none: every request fails, so that a test or a
/// coordinator that was not given one never reaches the network.
#[derive(Debug)]
pub struct NoMdmcert;

impl MdmcertTransport for NoMdmcert {
    fn post_sign_request(
        &self,
        _body: Vec<u8>,
        _timeout: Duration,
    ) -> BoxFuture<'_, Result<HttpReply, TransportError>> {
        Box::pin(async {
            Err(TransportError(
                "no transport to mdmcert.download".to_owned(),
            ))
        })
    }
}

impl crate::service::Coordinator {
    /// The same coordinator posting the push certificate request through `transport`.
    #[must_use]
    pub fn with_mdmcert(mut self, transport: Arc<dyn MdmcertTransport>) -> Self {
        self.mdmcert = transport;
        self
    }
}

#[cfg(test)]
#[path = "mdm_push_tests.rs"]
mod tests;
