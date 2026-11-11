//! `Mdm.PutAbmToken` and `Mdm.GetAbmPublicKey` of `hostkeeper.operator.v1`
//! (`docs/design/coordinator-mdm.md`, 4.5 and 4.9), over the coordinator's ABM client
//! ([`crate::abm`]) and its MDM setup ([`crate::mdm_setup`]).
//!
//! Who may and what is audited are as [`crate::operator_mdm_setup`] says: both are session-only
//! writes, each is one audit entry by the signed-in user before the store is touched, and a
//! refusal after the caller is known adds an entry of the same action whose reason is the
//! refusal's token. No answer, refusal, log line or entry holds the token, the key or the session.
//!
//! **The token.** It comes as the JSON of its four parts and expiry (the takeover: NanoDEP keeps
//! them decrypted) or as the `.p7m` ABM hands over, opened with the `abm_key` held. Before it is
//! kept it is proved to Apple (a session and `GET /account`), so that a token that is not good
//! stores nothing. Its expiry is the token's own `access_secret_expiry`.
//!
//! **The key pair.** `PutSecret` takes the `abm_key` and `abm_cert` of a takeover, and judges them
//! at upload (an RSA key in PEM, a certificate in PEM, and one made for the other). When
//! NanoDEP's pair cannot be had, [`Coordinator::mdm_get_abm_public_key`] makes a new one and
//! answers its certificate, for the operator to add to the server entry in ABM.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

use crate::abm::client::AbmError;
use crate::abm::{Abm, SyncError, keys, p7m};
use crate::audit_actions;
use crate::mdm_secrets::{AbmToken, MAX_SECRET_BYTES, Secret, SecretKind};
use crate::mdm_setup::SetupFailure;
use crate::operator::Caller;
use crate::operator_mdm_setup::secret_proto;
use crate::service::Coordinator;

impl Coordinator {
    /// The same coordinator running the ABM client `abm`.
    #[must_use]
    pub(crate) fn with_abm(mut self, abm: Abm) -> Self {
        self.abm = Some(std::sync::Arc::new(abm));
        self
    }

    /// Whether this coordinator runs the ABM client: it does when it has an MDM store, which is
    /// the database, or with files the directory beside the state file that
    /// `--mdm-secrets-key-file` makes.
    #[must_use]
    pub fn runs_abm(&self) -> bool {
        self.abm.is_some()
    }

    /// `Mdm.PutAbmToken`: proves the token to ABM and keeps it as the next version of `abm_token`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (both forms or neither, a value over 32 KiB,
    /// a token that is not its five parts or whose expiry is not RFC 3339), `abm_unavailable`,
    /// `secrets_unavailable`, `abm_key_missing`, `abm_token_unreadable`, `abm_credential_refused`,
    /// `abm_clock_refused`, `abm_unreachable`, `audit_unavailable`, `store_failed`.
    pub(crate) async fn mdm_put_abm_token(
        &self,
        caller: &Caller,
        request: proto::PutAbmTokenRequest,
    ) -> Result<proto::PutAbmTokenResponse, MdmRefusal> {
        let by = caller.operator()?;
        let kind = SecretKind::AbmToken;
        let refuse =
            |refusal| self.refused_abm(by, kind, audit_actions::MDM_ABM_TOKEN_PUT, refusal);
        let proto::PutAbmTokenRequest {
            token_json,
            token_p7m,
        } = request;
        let (json, p7m) = (Secret::new(token_json), Secret::new(token_p7m));
        let Some(abm) = self.abm.as_ref() else {
            return refuse(MdmRefusal::AbmUnavailable);
        };
        let one_form = json.is_empty() != p7m.is_empty();
        if !one_form || json.len().max(p7m.len()) > MAX_SECRET_BYTES {
            return refuse(MdmRefusal::InvalidRequest);
        }
        if !self.mdm_setup.has_key() {
            return refuse(MdmRefusal::SecretsUnavailable);
        }
        let material = if p7m.is_empty() {
            json
        } else {
            match self.open_p7m(&p7m) {
                Ok(opened) => opened,
                Err(why) => return refuse(why),
            }
        };
        let Ok(token) = AbmToken::parse(material.expose()) else {
            return refuse(if p7m.is_empty() {
                MdmRefusal::InvalidRequest
            } else {
                MdmRefusal::AbmTokenUnreadable
            });
        };
        let Some(expires) = expiry_unix(&token.access_secret_expiry) else {
            return refuse(MdmRefusal::InvalidRequest);
        };
        let account = match abm.accept_token(&self.mdm_setup, &token).await {
            Ok(account) => account,
            Err(why) => return refuse(abm_refusal(&why)),
        };
        let fingerprint = crate::mdm_setup::fingerprint(&material);
        self.audit_setup(
            by,
            audit_actions::MDM_ABM_TOKEN_PUT,
            Some(kind.as_str()),
            format!("fingerprint {fingerprint}, expires {expires}"),
        )?;
        let now = i64::try_from(self.ops.unix_now()).unwrap_or(i64::MAX);
        match self
            .mdm_setup
            .put_secret(kind, &material, Some(expires), now)
        {
            Ok(info) => Ok(proto::PutAbmTokenResponse {
                secret: Some(secret_proto(&info)),
                server_name: account.server_name,
                org_name: account.org_name,
            }),
            Err(why) => {
                tracing::error!(%why, "the ABM token could not be stored");
                refuse(MdmRefusal::StoreFailed)
            }
        }
    }

    /// The token JSON inside `p7m`, opened with the `abm_key` held.
    fn open_p7m(&self, p7m: &Secret) -> Result<Secret, MdmRefusal> {
        let held = self
            .mdm_setup
            .open(SecretKind::AbmKey)
            .map_err(|why| store_failed(&why))?;
        let Some(pem) = held else {
            return Err(MdmRefusal::AbmKeyMissing);
        };
        let key = keys::parse_key(pem.expose()).map_err(unkept)?;
        p7m::token_json(p7m.expose(), &key)
            .map(Secret::new)
            .map_err(|_| MdmRefusal::AbmTokenUnreadable)
    }

    /// `Mdm.GetAbmPublicKey`: the certificate of the ABM server entry's key, the pair made first
    /// when none is held.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `abm_unavailable`, `secrets_unavailable`, `audit_unavailable`,
    /// `store_failed`.
    pub(crate) async fn mdm_get_abm_public_key(
        &self,
        caller: &Caller,
        _request: &proto::GetAbmPublicKeyRequest,
    ) -> Result<proto::GetAbmPublicKeyResponse, MdmRefusal> {
        let by = caller.operator()?;
        let action = audit_actions::MDM_ABM_CERT_GET;
        let refuse = |refusal| self.refused_abm(by, SecretKind::AbmCert, action, refusal);
        if self.abm.is_none() {
            return refuse(MdmRefusal::AbmUnavailable);
        }
        if !self.mdm_setup.has_key() {
            return refuse(MdmRefusal::SecretsUnavailable);
        }
        self.audit_setup(
            by,
            action,
            Some(SecretKind::AbmCert.as_str()),
            "requested".to_owned(),
        )?;
        let now = self.ops.unix_now();
        match self.pair(now).await {
            Ok((cert, created)) => Ok(proto::GetAbmPublicKeyResponse {
                fingerprint_sha256: crate::mdm_setup::fingerprint(&cert),
                certificate_pem: String::from_utf8_lossy(cert.expose()).into_owned(),
                created,
            }),
            Err(why) => refuse(why),
        }
    }

    /// The certificate of the pair held and whether it was made now: the key and the certificate
    /// made when neither is held, the certificate made when only the key is held; a certificate
    /// held without its key is `abm_key_missing`, never replaced.
    async fn pair(&self, now: u64) -> Result<(Secret, bool), MdmRefusal> {
        let _pairing = self.abm_pairing.lock().await;
        let setup = &self.mdm_setup;
        let stored = i64::try_from(now).unwrap_or(i64::MAX);
        let held = |kind| setup.open(kind).map_err(|why| store_failed(&why));
        let cert = held(SecretKind::AbmCert)?;
        let key = held(SecretKind::AbmKey)?;
        if let (Some(cert), Some(_)) = (&cert, &key) {
            return Ok((cert.clone(), false));
        }
        if cert.is_some() {
            // Only the key is missing here: the held certificate may be the one given to ABM, so never replace it.
            return Err(MdmRefusal::AbmKeyMissing);
        }
        let (key, key_made) = match key {
            Some(pem) => (keys::parse_key(pem.expose()).map_err(unkept)?, false),
            None => {
                let bits = keys::KEY_BITS;
                let made = tokio::task::spawn_blocking(move || keys::generate(bits))
                    .await
                    .map_err(unkept)?
                    .map_err(unkept)?;
                (made, true)
            }
        };
        let cert = keys::self_signed(&key, now).map_err(unkept)?;
        let cert = Secret::new(cert.into_bytes());
        let mut parts = vec![(SecretKind::AbmCert, cert.clone())];
        if key_made {
            let pem = keys::pkcs1_pem(&key).map_err(unkept)?;
            parts.insert(0, (SecretKind::AbmKey, Secret::new(pem.into_bytes())));
        }
        for (kind, value) in &parts {
            setup
                .put_secret(*kind, value, None, stored)
                .map_err(|why| store_failed(&why))?;
        }
        Ok((cert, true))
    }

    /// The refusal `refusal` of a call about `kind`, audited under `action` with its token.
    fn refused_abm<T>(
        &self,
        by: &str,
        kind: SecretKind,
        action: &str,
        refusal: MdmRefusal,
    ) -> Result<T, MdmRefusal> {
        self.audit_setup(
            by,
            action,
            Some(kind.as_str()),
            format!("refused: {}", refusal.as_str_name()),
        )?;
        Err(refusal)
    }

    /// Whether `serial` is in our ABM now: in the table `abm_devices` of the sync and not marked
    /// removed (`docs/design/coordinator-mdm.md`, 4.6). The approval of a Mac and its enrollment
    /// ask it, and decline `serial_not_in_abm` when it is not. A coordinator with no ABM client
    /// has no table: no serial is.
    ///
    /// # Errors
    /// The store failed: the caller declines nothing on a store it could not read.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "asked by the approval and the enrollment, tasks CM9 and CM12c"
        )
    )]
    pub(crate) fn serial_in_abm(&self, serial: &str) -> Result<bool, crate::store::mdm::MdmError> {
        self.abm
            .as_ref()
            .map_or(Ok(false), |abm| abm.serial_in_abm(serial))
    }
}

/// The refusal of a failed proof of a token.
fn abm_refusal(why: &SyncError) -> MdmRefusal {
    match why {
        SyncError::Abm(AbmError::CredentialRefused) => MdmRefusal::AbmCredentialRefused,
        SyncError::Abm(AbmError::ClockRefused) => MdmRefusal::AbmClockRefused,
        SyncError::Abm(_) => MdmRefusal::AbmUnreachable,
        SyncError::Setup(_) | SyncError::Store(_) | SyncError::Token => {
            tracing::error!(%why, "the ABM token could not be proved");
            MdmRefusal::StoreFailed
        }
    }
}

/// The refusal of a step that could not make or keep a value, whatever its own error.
fn unkept<E>(_: E) -> MdmRefusal {
    MdmRefusal::StoreFailed
}

/// The refusal of a read the store failed.
fn store_failed(why: &SetupFailure) -> MdmRefusal {
    tracing::error!(%why, "the ABM key pair could not be read");
    MdmRefusal::StoreFailed
}

/// `text`, an RFC 3339 time, as unix seconds.
fn expiry_unix(text: &str) -> Option<i64> {
    let at =
        time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()?;
    Some(at.unix_timestamp())
}

/// Runs the ABM sync of `coordinator` ([`crate::abm::sync_every`]) for as long as it is polled;
/// a coordinator with no ABM client never finishes and does nothing.
pub async fn abm_sync_every(coordinator: std::sync::Arc<Coordinator>) -> std::convert::Infallible {
    match coordinator.abm.clone() {
        Some(abm) => {
            let setup = std::sync::Arc::clone(&coordinator.mdm_setup);
            crate::abm::sync_every(abm, setup, crate::abm::client::tokio_sleep()).await
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
#[path = "operator_abm_tests.rs"]
mod tests;
