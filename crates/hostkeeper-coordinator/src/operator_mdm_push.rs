//! `Mdm.CreatePushRequest`, `Mdm.PutPushReply` and `Mdm.PutPushCertificate` of
//! `hostkeeper.operator.v1`: the push certificate's lifecycle
//! (`docs/design/coordinator-mdm.md`, sections 4.4 and 4.9), over the sealed records of
//! [`crate::mdm_push`] and the setup's audit ([`crate::operator_mdm_setup`]).
//!
//! All three are in [`crate::grpc::SESSION_ONLY`]. Each is audited by the signed-in user's name at
//! entry: a refusal after the caller is known is one entry of the call's action whose reason is
//! the refusal's token, and a call that goes on is one entry before anything is stored or
//! answered, so that nothing is kept or returned without its entry. An entry names the CSR's or
//! the upload's SHA-256, the topic and the expiry, never a key, a CSR's email or a certificate.
//!
//! The reply to the request arrives by email ([`hostkeeper_mdm_push::mdmcert`]): the coordinator
//! posts the request and then the operator uploads the attachment.

use hostkeeper_mdm_push::identity::{self, IdentityError};
use hostkeeper_mdm_push::mdmcert::{self, SignedRequestError};
use hostkeeper_mdm_push::p7m::{self, ReplyError};
use hostkeeper_mdm_push::request;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;
use sha2::{Digest as _, Sha256};

use crate::audit_actions;
use crate::mdm_push::{pending_bundle, split_pending};
use crate::mdm_secrets::{Secret, SecretKind};
use crate::mdm_setup::SetupFailure;
use crate::service::Coordinator;

/// The most bytes of a certificate or of a key in an upload.
const MAX_PEM_BYTES: usize = 16 * 1024;

/// The most bytes of the emailed reply in an upload (hex text of a CMS message of a few KiB).
const MAX_REPLY_BYTES: usize = 32 * 1024;

impl Coordinator {
    /// `Mdm.CreatePushRequest`: makes the request, keeps its keys sealed, and posts it to
    /// mdmcert.download when asked.
    ///
    /// Both RSA keys are made on a blocking thread, not on a worker of the runtime.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the email is not one address),
    /// `secrets_unavailable`, `audit_unavailable`, `store_failed`, `mdmcert_unavailable`.
    pub async fn mdm_create_push_request(
        &self,
        caller: &crate::operator::Caller,
        request: proto::CreatePushRequestRequest,
    ) -> Result<proto::CreatePushRequestResponse, MdmRefusal> {
        let by = caller.operator()?;
        let action = audit_actions::MDM_PUSH_CSR;
        if !request::email_is_valid(&request.email) {
            return self.refused_push(by, action, None, MdmRefusal::InvalidRequest);
        }
        if !self.mdm_setup.has_key() {
            return self.refused_push(by, action, None, MdmRefusal::SecretsUnavailable);
        }
        let now = now_of(self);
        // Two RSA-2048 keys: seconds of CPU, on a thread that is not a worker's.
        let email = request.email.clone();
        let made = tokio::task::spawn_blocking(move || request::new_request(&email, now)).await;
        let Ok(Ok(made)) = made else {
            tracing::error!("the keys of a push certificate request could not be made");
            return self.refused_push(by, action, None, MdmRefusal::StoreFailed);
        };
        let csr_sha = hex::encode(Sha256::digest(made.csr_pem.as_bytes()));
        self.audit_setup(
            by,
            action,
            Some(SecretKind::PushPendingKey.as_str()),
            format!("csr {csr_sha}, submitted {}", request.submit),
        )?;
        let kept = self
            .mdm_setup
            .put_secret(
                SecretKind::PushEncryptionKey,
                &Secret::new(made.encryption_key_pem.expose().to_vec()),
                None,
                now,
            )
            .and_then(|_| {
                self.mdm_setup.put_secret(
                    SecretKind::PushPendingKey,
                    &Secret::new(pending_bundle(made.key_pem.expose(), &made.csr_pem)),
                    None,
                    now,
                )
            });
        if let Err(why) = kept {
            tracing::error!(%why, "a push certificate request could not be stored");
            return self.refused_push(
                by,
                action,
                Some(SecretKind::PushPendingKey),
                MdmRefusal::StoreFailed,
            );
        }
        if request.submit {
            let timeout = self
                .mdm_setup
                .push_settings()
                .map_or(std::time::Duration::from_secs(30), |(_, timeout)| timeout);
            if let Err(why) = mdmcert::submit(
                &*self.mdmcert,
                &request.email,
                &made.csr_pem,
                &made.encryption_cert_pem,
                timeout,
            )
            .await
            {
                tracing::error!(%why, "the push certificate request was not taken by mdmcert.download");
                return self.refused_push(
                    by,
                    action,
                    Some(SecretKind::PushPendingKey),
                    MdmRefusal::MdmcertUnavailable,
                );
            }
        }
        Ok(proto::CreatePushRequestResponse {
            csr_pem: made.csr_pem,
            encryption_certificate_pem: made.encryption_cert_pem,
            submitted: request.submit,
        })
    }

    /// `Mdm.PutPushReply`: decrypts the emailed reply and answers the signed request.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (empty, or over 32 KiB), `no_pending_request`,
    /// `reply_undecryptable`, `reply_not_for_request`, `audit_unavailable`, `store_failed`.
    pub fn mdm_put_push_reply(
        &self,
        caller: &crate::operator::Caller,
        request: proto::PutPushReplyRequest,
    ) -> Result<proto::PutPushReplyResponse, MdmRefusal> {
        let by = caller.operator()?;
        let action = audit_actions::MDM_PUSH_REPLY_PUT;
        let kind = Some(SecretKind::PushEncryptionKey);
        if request.reply.is_empty() || request.reply.len() > MAX_REPLY_BYTES {
            return self.refused_push(by, action, kind, MdmRefusal::InvalidRequest);
        }
        let held = |kind: SecretKind| {
            self.mdm_setup.open(kind).map_err(|why| {
                tracing::error!(%why, kind = kind.as_str(), "a push request record could not be opened");
                MdmRefusal::StoreFailed
            })
        };
        let (key, pending) = match (
            held(SecretKind::PushEncryptionKey),
            held(SecretKind::PushPendingKey),
        ) {
            (Ok(Some(key)), Ok(Some(pending))) => (key, pending),
            (Err(refusal), _) | (_, Err(refusal)) => {
                return self.refused_push(by, action, kind, refusal);
            }
            _ => return self.refused_push(by, action, kind, MdmRefusal::NoPendingRequest),
        };
        let Some((_, csr_pem)) = split_pending(pending.expose()) else {
            tracing::error!("the pending push request holds no CSR");
            return self.refused_push(by, action, kind, MdmRefusal::StoreFailed);
        };
        let plain = match p7m::decrypt(&request.reply, key.expose()) {
            Ok(plain) => plain,
            Err(ReplyError::Key(why)) => {
                tracing::error!(%why, "the encryption key of the pending push request is not readable");
                return self.refused_push(by, action, kind, MdmRefusal::StoreFailed);
            }
            Err(_) => return self.refused_push(by, action, kind, MdmRefusal::ReplyUndecryptable),
        };
        if let Err(why) = mdmcert::check_signed_request(&plain, &csr_pem) {
            return match why {
                SignedRequestError::NotPlist
                | SignedRequestError::Missing(_)
                | SignedRequestError::CsrMismatch => {
                    self.refused_push(by, action, kind, MdmRefusal::ReplyNotForRequest)
                }
            };
        }
        self.audit_setup(
            by,
            action,
            kind.map(SecretKind::as_str),
            format!("reply {}", hex::encode(Sha256::digest(&request.reply))),
        )?;
        Ok(proto::PutPushReplyResponse {
            signed_request: plain,
        })
    }

    /// `Mdm.PutPushCertificate`: stores a push certificate with its key.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (empty, or over 16 KiB), `secrets_unavailable`,
    /// `certificate_invalid`, `key_invalid`, `key_mismatch`, `certificate_expired`, `topic_changed`,
    /// `no_pending_request`, `audit_unavailable`, `store_failed`.
    pub fn mdm_put_push_certificate(
        &self,
        caller: &crate::operator::Caller,
        request: proto::PutPushCertificateRequest,
    ) -> Result<proto::PutPushCertificateResponse, MdmRefusal> {
        let by = caller.operator()?;
        let action = audit_actions::MDM_PUSH_CERT_PUT;
        let kind = Some(SecretKind::PushCert);
        let proto::PutPushCertificateRequest {
            certificate_pem,
            key_pem,
        } = request;
        if certificate_pem.is_empty()
            || certificate_pem.len() > MAX_PEM_BYTES
            || key_pem.len() > MAX_PEM_BYTES
        {
            return self.refused_push(by, action, kind, MdmRefusal::InvalidRequest);
        }
        if !self.mdm_setup.has_key() {
            return self.refused_push(by, action, kind, MdmRefusal::SecretsUnavailable);
        }
        let Ok(info) = identity::read_certificate(&certificate_pem) else {
            return self.refused_push(by, action, kind, MdmRefusal::CertificateInvalid);
        };
        let store_failed = |why: SetupFailure| {
            tracing::error!(%why, "the push certificate records could not be read");
            MdmRefusal::StoreFailed
        };
        // The key: the one uploaded with the certificate, or the pending request's.
        let key_pem = if key_pem.is_empty() {
            let pending = self
                .mdm_setup
                .open(SecretKind::PushPendingKey)
                .map_err(store_failed);
            match pending {
                Ok(Some(pending)) => split_pending(pending.expose()).map(|(key, _)| key),
                Ok(None) => None,
                Err(refusal) => return self.refused_push(by, action, kind, refusal),
            }
        } else {
            Some(key_pem)
        };
        let Some(key_pem) = key_pem else {
            return self.refused_push(by, action, kind, MdmRefusal::NoPendingRequest);
        };
        let Ok(key) = identity::read_key(&key_pem) else {
            return self.refused_push(by, action, kind, MdmRefusal::KeyInvalid);
        };
        match info.matches(&key) {
            Ok(true) => {}
            Ok(false) => return self.refused_push(by, action, kind, MdmRefusal::KeyMismatch),
            Err(_) => return self.refused_push(by, action, kind, MdmRefusal::KeyInvalid),
        }
        let current = match self
            .mdm_setup
            .open(SecretKind::PushCert)
            .map_err(store_failed)
        {
            Ok(current) => current,
            Err(refusal) => return self.refused_push(by, action, kind, refusal),
        };
        let current_topic = current
            .as_ref()
            .and_then(|held| identity::read_certificate(held.expose()).ok())
            .map(|held| held.topic);
        if let Err(why) = info.judge(now_of(self), current_topic.as_deref()) {
            let refusal = match why {
                IdentityError::Expired { .. } => MdmRefusal::CertificateExpired,
                IdentityError::TopicChanged => MdmRefusal::TopicChanged,
                _ => MdmRefusal::CertificateInvalid,
            };
            return self.refused_push(by, action, kind, refusal);
        }
        let fingerprint = hex::encode(Sha256::digest(&certificate_pem));
        self.audit_setup(
            by,
            action,
            kind.map(SecretKind::as_str),
            format!(
                "topic {}, fingerprint {fingerprint}, expires {}",
                info.topic, info.not_after_unix
            ),
        )?;
        let now = now_of(self);
        let stored = self
            .mdm_setup
            .put_secret(SecretKind::PushKey, &Secret::new(key_pem), None, now)
            .and_then(|_| {
                self.mdm_setup.put_secret(
                    SecretKind::PushCert,
                    &Secret::new(certificate_pem),
                    Some(info.not_after_unix),
                    now,
                )
            });
        match stored {
            Ok(secret) => Ok(proto::PutPushCertificateResponse {
                topic: info.topic,
                not_before_unix: info.not_before_unix,
                not_after_unix: info.not_after_unix,
                secret: Some(crate::operator_mdm_setup::secret_proto(&secret)),
                renewal: current.is_some(),
            }),
            Err(why) => {
                tracing::error!(%why, "a push certificate could not be stored");
                self.refused_push(by, action, kind, MdmRefusal::StoreFailed)
            }
        }
    }

    /// The refusal `refusal` of a push certificate call, audited by `by` as `action` with its
    /// token as the reason.
    fn refused_push<T>(
        &self,
        by: &str,
        action: &str,
        kind: Option<SecretKind>,
        refusal: MdmRefusal,
    ) -> Result<T, MdmRefusal> {
        self.audit_setup(
            by,
            action,
            kind.map(SecretKind::as_str),
            format!("refused: {}", refusal.as_str_name()),
        )?;
        Err(refusal)
    }
}

/// The coordinator's clock in unix seconds, as the signed seconds the records keep.
fn now_of(coordinator: &Coordinator) -> i64 {
    i64::try_from(coordinator.ops.unix_now()).unwrap_or(i64::MAX)
}
