//! `Mdm.PutSecret`, `Mdm.PutSetting`, `Mdm.PutName`, `Mdm.ListSecrets` and `Mdm.ListSettings` of
//! `hostkeeper.operator.v1`, as plain functions over their messages, as the other operator calls
//! are ([`crate::operator`]), over the coordinator's MDM setup ([`crate::mdm_setup`]).
//!
//! Who may: the reads take the operator's token or a signed-in user. The three writes are in
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
use crate::mdm_name;
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
            with_certificate,
        } = request;
        let material = Secret::new(material);
        let with_certificate = Secret::new(with_certificate);
        let Some(kind) = SecretKind::parse(&kind) else {
            return self.refused_put(by, None, MdmRefusal::InvalidRequest);
        };
        let wellformed = !material.is_empty()
            && material.len() <= MAX_SECRET_BYTES
            && self.kind_takes(kind, &material, &with_certificate);
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
            Ok(info) => {
                if !with_certificate.is_empty() {
                    self.put_certificate_of(by, &with_certificate, now)?;
                }
                Ok(proto::PutSecretResponse {
                    secret: Some(secret_proto(&info)),
                })
            }
            Err(why) => {
                tracing::error!(%why, kind = kind.as_str(), "an MDM secret could not be stored");
                self.refused_put(by, Some(kind), MdmRefusal::StoreFailed)
            }
        }
    }

    /// Seals `certificate`, already checked against the `abm_key` just stored, as the next
    /// `abm_cert`, audited by `by`.
    fn put_certificate_of(
        &self,
        by: &str,
        certificate: &Secret,
        now: i64,
    ) -> Result<(), MdmRefusal> {
        let kind = SecretKind::AbmCert;
        self.audit_setup(
            by,
            kind.put_action(),
            Some(kind.as_str()),
            format!(
                "fingerprint {}, expires none",
                crate::mdm_setup::fingerprint(certificate)
            ),
        )?;
        self.mdm_setup
            .put_secret(kind, certificate, None, now)
            .map(drop)
            .or_else(|why| {
                tracing::error!(%why, "the ABM certificate could not be stored");
                self.refused_put(by, Some(kind), MdmRefusal::StoreFailed)
                    .map(drop)
            })
    }

    /// Whether `material` is what `kind` holds, as far as it can be judged at upload: the ABM
    /// token is its five parts, the ABM key an RSA private key in PEM, and the ABM certificate a
    /// certificate in PEM that is the one of the ABM key held, when one is. An ABM key is also
    /// refused when it is not the one of the `abm_cert` held, unless `with_certificate` is the
    /// certificate it goes with (which is then stored after it): a key that breaks the pair would
    /// lose the key every `.p7m` for that certificate is encrypted to. `with_certificate` is
    /// taken by no other kind. The other kinds are judged by the task that uses them.
    fn kind_takes(&self, kind: SecretKind, material: &Secret, with_certificate: &Secret) -> bool {
        if kind != SecretKind::AbmKey && !with_certificate.is_empty() {
            return false;
        }
        match kind {
            SecretKind::AbmToken => AbmToken::parse(material.expose()).is_ok(),
            SecretKind::AbmKey => self.key_pairs(material, with_certificate),
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

    /// Whether the ABM key `material` is an RSA private key that goes with `with_certificate`,
    /// or, when none is sent, with the `abm_cert` held, if there is one.
    fn key_pairs(&self, material: &Secret, with_certificate: &Secret) -> bool {
        let Ok(key) = keys::parse_key(material.expose()) else {
            return false;
        };
        if !with_certificate.is_empty() {
            return keys::check_certificate(with_certificate.expose(), Some(&key)).is_ok();
        }
        match self.mdm_setup.open(SecretKind::AbmCert) {
            Ok(Some(held)) => keys::check_certificate(held.expose(), Some(&key)).is_ok(),
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

    /// `Mdm.PutName`: sets the MDM name.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (not a DNS name of the kind
    /// [`mdm_name::normalize`] takes), `name_locked` (a Mac has enrolled and the name set differs from
    /// this one), `audit_unavailable`, `store_failed`.
    pub fn mdm_put_name(
        &self,
        caller: &Caller,
        request: &proto::PutNameRequest,
    ) -> Result<proto::PutNameResponse, MdmRefusal> {
        let by = caller.operator()?;
        let Some(name) = mdm_name::normalize(&request.name) else {
            return self.refused_name(by, None, MdmRefusal::InvalidRequest);
        };
        let held = self.mdm_setup.settings().map_err(read_failed)?;
        // A first name is always allowed, since Macs that enrolled before this setup hold the
        // name they enrolled at and the operator sets that one; a change is not, once a Mac has.
        let changes = held.get(mdm_name::KEY).is_some_and(|set| *set != name);
        if changes && self.macs_enrolled() {
            return self.refused_name(by, Some(&name), MdmRefusal::NameLocked);
        }
        self.audit_setup(
            by,
            audit_actions::MDM_NAME_SET,
            None,
            format!("set to {name}"),
        )?;
        if let Err(why) = self.mdm_setup.set_setting(mdm_name::KEY, &name) {
            tracing::error!(%why, "the MDM name could not be stored");
            return self.refused_name(by, Some(&name), MdmRefusal::StoreFailed);
        }
        Ok(proto::PutNameResponse {
            enrollment_url: mdm_name::enrollment_url(&name),
            name,
        })
    }

    /// The refusal `refusal` of a `PutName`, audited by `by` with its token as the reason, and the
    /// name when it was one.
    fn refused_name(
        &self,
        by: &str,
        name: Option<&str>,
        refusal: MdmRefusal,
    ) -> Result<proto::PutNameResponse, MdmRefusal> {
        let reason = match name {
            Some(name) => format!("refused: {} for {name}", refusal.as_str_name()),
            None => format!("refused: {}", refusal.as_str_name()),
        };
        self.audit_setup(by, audit_actions::MDM_NAME_SET, None, reason)?;
        Err(refusal)
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
pub(crate) fn read_failed(why: SetupFailure) -> MdmRefusal {
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
