//! `Mdm.PutSecret`, `Mdm.PutSetting`, `Mdm.ListSecrets` and `Mdm.ListSettings` of
//! `hostkeeper.operator.v1`, as plain functions over their messages, as the other operator calls
//! are ([`crate::operator`]), over the coordinator's MDM setup ([`crate::mdm_setup`]).
//!
//! Who may: the two reads take the operator's token or a signed-in user. The two writes are in
//! [`crate::grpc::SESSION_ONLY`], so the transport refuses a bearer before the call is made
//! (`PERMISSION_DENIED`, `session_required`); here a caller is the signed-in user's name or it is
//! refused as every operator call is.
//!
//! What is audited: each write that gets as far as a known caller is one audit entry, by the
//! user, before the store is touched, so that no secret is stored without its entry; a write the
//! store then refuses, or a request refused for what it holds, adds an entry of the same action
//! whose reason is the refusal's token. An entry names the kind, the fingerprint and the expiry
//! (or the setting and its value), never the material. A read is not audited.
//!
//! What is never shown: no answer, refusal, log line or audit entry holds a secret's value or any
//! encoding of it. A request's `Debug` is never formatted here, and the value lives in a
//! [`Secret`] from the moment it is taken out of the request.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

use crate::abm::keys;
use crate::audit_actions;
use crate::mdm_secrets::{AbmToken, MAX_SECRET_BYTES, Secret, SecretKind};
use crate::mdm_settings;
use crate::mdm_setup::{MdmSetup, SecretInfo, SetupFailure};
use crate::operator::Caller;
use crate::ops::{AuditEntry, Write};
use crate::service::Coordinator;

impl Coordinator {
    /// The same coordinator keeping the MDM setup in `setup`.
    #[must_use]
    pub fn with_mdm_setup(self, setup: MdmSetup) -> Self {
        self.with_shared_mdm_setup(Arc::new(setup))
    }

    /// The same coordinator keeping the MDM setup in `setup`, which the MDM server shares.
    #[must_use]
    pub fn with_shared_mdm_setup(mut self, setup: Arc<MdmSetup>) -> Self {
        self.mdm_setup = setup;
        self
    }

    /// The MDM setup: the sealed secrets and the settings.
    #[must_use]
    pub fn mdm_setup(&self) -> &Arc<MdmSetup> {
        &self.mdm_setup
    }

    /// One audit entry, `action` by `by` about `subject`, with `reason`.
    pub(crate) fn audit_setup(
        &self,
        by: &str,
        action: &str,
        subject: Option<&str>,
        reason: String,
    ) -> Result<(), MdmRefusal> {
        let mut entry = AuditEntry::new(by, action);
        entry.subject = subject.map(str::to_owned);
        entry.reason = Some(reason);
        self.ops
            .change(Write::BeforeMemory, entry, |_| {})
            .map(drop)
            .map_err(|why| {
                tracing::error!(%why, "an MDM setup change could not be audited: it is not made");
                MdmRefusal::AuditUnavailable
            })
    }

    /// `Mdm.PutSecret`: seals the material as the next version of its kind.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (an unknown kind, a value that is empty or
    /// over 32 KiB, an ABM token that is not its five parts), `secrets_unavailable` (no master
    /// key), `audit_unavailable`, `store_failed`.
    pub fn mdm_put_secret(
        &self,
        caller: &Caller,
        request: proto::PutSecretRequest,
    ) -> Result<proto::PutSecretResponse, MdmRefusal> {
        let by = caller.operator()?;
        let proto::PutSecretRequest {
            kind,
            material,
            not_after_unix,
        } = request;
        let material = Secret::new(material);
        let Some(kind) = SecretKind::parse(&kind) else {
            return self.refused_put(by, None, MdmRefusal::InvalidRequest);
        };
        let wellformed = !material.is_empty()
            && material.len() <= MAX_SECRET_BYTES
            && self.kind_takes(kind, &material);
        if !wellformed {
            return self.refused_put(by, Some(kind), MdmRefusal::InvalidRequest);
        }
        if !self.mdm_setup.has_key() {
            return self.refused_put(by, Some(kind), MdmRefusal::SecretsUnavailable);
        }
        let now = i64::try_from(self.ops.unix_now()).unwrap_or(i64::MAX);
        let fingerprint = crate::mdm_setup::fingerprint(&material);
        let expiry = not_after_unix.map_or_else(|| "none".to_owned(), |at| at.to_string());
        self.audit_setup(
            by,
            kind.put_action(),
            Some(kind.as_str()),
            format!("fingerprint {fingerprint}, expires {expiry}"),
        )?;
        match self
            .mdm_setup
            .put_secret(kind, &material, not_after_unix, now)
        {
            Ok(info) => Ok(proto::PutSecretResponse {
                secret: Some(secret_proto(&info)),
            }),
            Err(why) => {
                tracing::error!(%why, kind = kind.as_str(), "an MDM secret could not be stored");
                self.refused_put(by, Some(kind), MdmRefusal::StoreFailed)
            }
        }
    }

    /// Whether `material` is what `kind` holds, as far as it can be judged at upload: the ABM
    /// token is its five parts, the ABM key an RSA private key in PEM, and the ABM certificate a
    /// certificate in PEM that is the one of the ABM key held, when one is. The other kinds are
    /// judged by the task that uses them.
    fn kind_takes(&self, kind: SecretKind, material: &Secret) -> bool {
        match kind {
            SecretKind::AbmToken => AbmToken::parse(material.expose()).is_ok(),
            SecretKind::AbmKey => keys::parse_key(material.expose()).is_ok(),
            SecretKind::AbmCert => {
                let held = self
                    .mdm_setup
                    .open(SecretKind::AbmKey)
                    .ok()
                    .flatten()
                    .and_then(|pem| keys::parse_key(pem.expose()).ok());
                keys::check_certificate(material.expose(), held.as_ref()).is_ok()
            }
            _ => true,
        }
    }

    /// The refusal `refusal` of a `PutSecret`, audited by `by` with its token as the reason.
    fn refused_put(
        &self,
        by: &str,
        kind: Option<SecretKind>,
        refusal: MdmRefusal,
    ) -> Result<proto::PutSecretResponse, MdmRefusal> {
        let action = kind.map_or(audit_actions::MDM_SECRET_PUT, SecretKind::put_action);
        self.audit_setup(
            by,
            action,
            kind.map(SecretKind::as_str),
            format!("refused: {}", refusal.as_str_name()),
        )?;
        Err(refusal)
    }

    /// `Mdm.PutSetting`: sets one setting.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (a name this release does not know, a value
    /// the setting does not take), `audit_unavailable`, `store_failed`.
    pub fn mdm_put_setting(
        &self,
        caller: &Caller,
        request: &proto::PutSettingRequest,
    ) -> Result<proto::PutSettingResponse, MdmRefusal> {
        let by = caller.operator()?;
        let checked = mdm_settings::find(&request.name)
            .and_then(|setting| setting.check(&request.value).map(|value| (setting, value)));
        let Some((setting, value)) = checked else {
            let name = mdm_settings::find(&request.name).map(|setting| setting.name);
            self.audit_setup(
                by,
                audit_actions::MDM_SETTING_SET,
                name,
                format!("refused: {}", MdmRefusal::InvalidRequest.as_str_name()),
            )?;
            return Err(MdmRefusal::InvalidRequest);
        };
        self.audit_setup(
            by,
            audit_actions::MDM_SETTING_SET,
            Some(setting.name),
            format!("set to {value}"),
        )?;
        if let Err(why) = self.mdm_setup.set_setting(setting.name, &value) {
            tracing::error!(%why, setting = setting.name, "an MDM setting could not be stored");
            self.audit_setup(
                by,
                audit_actions::MDM_SETTING_SET,
                Some(setting.name),
                format!("refused: {}", MdmRefusal::StoreFailed.as_str_name()),
            )?;
            return Err(MdmRefusal::StoreFailed);
        }
        Ok(proto::PutSettingResponse {
            name: setting.name.to_owned(),
            value,
        })
    }

    /// `Mdm.ListSecrets`: what is shown of every version of every secret.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `store_failed`.
    pub fn mdm_list_secrets(
        &self,
        caller: &Caller,
    ) -> Result<proto::ListSecretsResponse, MdmRefusal> {
        caller.operator()?;
        let secrets = self.mdm_setup.list_secrets().map_err(read_failed)?;
        Ok(proto::ListSecretsResponse {
            secrets: secrets.iter().map(secret_proto).collect(),
            sealing: self.mdm_setup.has_key(),
        })
    }

    /// `Mdm.ListSettings`: every setting, with its value.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `store_failed`.
    pub fn mdm_list_settings(
        &self,
        caller: &Caller,
    ) -> Result<proto::ListSettingsResponse, MdmRefusal> {
        caller.operator()?;
        let settings = self.mdm_setup.list_settings().map_err(read_failed)?;
        Ok(proto::ListSettingsResponse {
            settings: settings
                .into_iter()
                .map(|(name, value, is_default)| proto::MdmSetting {
                    name: name.to_owned(),
                    value,
                    is_default,
                })
                .collect(),
        })
    }
}

/// The refusal of a read the store failed.
fn read_failed(why: SetupFailure) -> MdmRefusal {
    tracing::error!(%why, "the MDM setup could not be read");
    MdmRefusal::StoreFailed
}

/// The answer's row of `info`.
pub(crate) fn secret_proto(info: &SecretInfo) -> proto::SecretInfo {
    proto::SecretInfo {
        kind: info.kind.as_str().to_owned(),
        version: info.version,
        fingerprint_sha256: info.fingerprint.clone(),
        not_after_unix: info.not_after_unix,
        created_unix: info.created_unix,
        superseded_unix: info.superseded_unix,
    }
}
