//! `Mdm.Status` of `hostkeeper.operator.v1` and the *configured* computation
//! (`docs/design/coordinator-mdm.md`, section 4.9), as plain functions over the messages, as the
//! other operator calls are ([`crate::operator`]).
//!
//! *Configured* is computed from the records at each call and stored nowhere, so there is no flag
//! to forget or to leave stale: a certificate that expires turns it off by itself. It holds when
//! the push certificate and key are a pair and the certificate has not expired; the ABM token is
//! held and has not expired; the name is set; and a TLS certificate that names the name, and its
//! key, are a pair and the certificate has not expired. Each condition that is not met is named
//! ([`proto::MdmCondition`]). Expiry is read from the material: the end of validity of a
//! certificate, the `access_secret_expiry` of the ABM token. Nothing of any value is returned: the
//! answer holds the facts read, and what `ListSecrets` shows of each secret.
//!
//! Not yet part of it, since the code that records them is not built: that Apple accepts the ABM
//! token and that a first sync was done (the ABM client's task).

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::{MdmCondition, MdmRefusal};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::mdm_certs::CertFacts;
use crate::mdm_name;
use crate::mdm_secrets::{AbmToken, Secret, SecretKind};
use crate::mdm_setup::SetupFailure;
use crate::operator::Caller;
use crate::operator_mdm_setup::{read_failed, secret_proto};
use crate::service::Coordinator;

/// What the setup holds of one kind of secret.
enum Held {
    /// None.
    Absent,
    /// A version is held and does not open.
    Unreadable,
    /// The version in use, opened.
    Open(Secret),
}

impl Coordinator {
    /// `Mdm.Status`: whether the setup is complete, what is missing, and what is read of the
    /// push certificate, the ABM token and the TLS certificate in use.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `store_failed`.
    pub fn mdm_status(&self, caller: &Caller) -> Result<proto::MdmStatusResponse, MdmRefusal> {
        caller.operator()?;
        self.setup_status()
    }

    /// The status at the coordinator's time.
    ///
    /// # Errors
    /// `store_failed`: the setup cannot be read.
    pub(crate) fn setup_status(&self) -> Result<proto::MdmStatusResponse, MdmRefusal> {
        let now = i64::try_from(self.ops.unix_now()).unwrap_or(i64::MAX);
        let infos = self.mdm_setup.list_secrets().map_err(read_failed)?;
        let name = self
            .mdm_setup
            .settings()
            .map_err(read_failed)?
            .remove(mdm_name::KEY)
            .unwrap_or_default();
        let mut status = proto::MdmStatusResponse {
            sealing: self.mdm_setup.has_key(),
            enrollment_url: if name.is_empty() {
                String::new()
            } else {
                mdm_name::enrollment_url(&name)
            },
            name_locked: !name.is_empty() && self.macs_enrolled(),
            name,
            ..proto::MdmStatusResponse::default()
        };
        let in_use = |kind: SecretKind| {
            infos
                .iter()
                .filter(|info| info.kind == kind)
                .max_by_key(|info| info.version)
                .map(secret_proto)
        };
        status.push_cert = in_use(SecretKind::PushCert);
        status.push_key = in_use(SecretKind::PushKey);
        status.abm_token = in_use(SecretKind::AbmToken);
        status.tls_cert = in_use(SecretKind::TlsCert);
        status.tls_key = in_use(SecretKind::TlsKey);

        let mut missing = Vec::new();
        self.check_push(&mut status, &mut missing, now)?;
        self.check_abm(&mut status, &mut missing, now)?;
        if status.name.is_empty() {
            missing.push(MdmCondition::NameMissing);
        }
        self.check_tls(&mut status, &mut missing, now)?;
        status.configured = missing.is_empty();
        status.missing = missing.into_iter().map(i32::from).collect();
        Ok(status)
    }

    /// The version in use of `kind`, opened.
    fn held(&self, kind: SecretKind) -> Result<Held, MdmRefusal> {
        match self.mdm_setup.open(kind) {
            Ok(Some(secret)) => Ok(Held::Open(secret)),
            Ok(None) | Err(SetupFailure::NoKey) => Ok(Held::Absent),
            Err(SetupFailure::Seal(_)) => Ok(Held::Unreadable),
            Err(other) => Err(read_failed(other)),
        }
    }

    /// The certificate of `cert` and the key of `key`, read: none when either is missing (its
    /// condition, `missing_cert` or `missing_key`, is added), else what the certificate says and
    /// whether the key is its. An unreadable pair adds `invalid`.
    fn read_pair(
        &self,
        (cert, key): (SecretKind, SecretKind),
        (missing_cert, missing_key, invalid): (MdmCondition, MdmCondition, MdmCondition),
        missing: &mut Vec<MdmCondition>,
    ) -> Result<Option<CertFacts>, MdmRefusal> {
        let (cert, key) = (self.held(cert)?, self.held(key)?);
        let absent = |held: &Held| matches!(held, Held::Absent);
        let (no_cert, no_key) = (absent(&cert), absent(&key));
        if no_cert {
            missing.push(missing_cert);
        }
        if no_key {
            missing.push(missing_key);
        }
        if no_cert || no_key {
            return Ok(None);
        }
        let facts = match &cert {
            Held::Open(pem) => CertFacts::read(pem.expose()),
            _ => None,
        };
        let pair = match (&facts, &key) {
            (Some(facts), Held::Open(pem)) => facts.is_key(pem.expose()),
            _ => false,
        };
        if !pair {
            missing.push(invalid);
        }
        Ok(facts)
    }

    /// The push certificate and key: held, a pair, not expired.
    fn check_push(
        &self,
        status: &mut proto::MdmStatusResponse,
        missing: &mut Vec<MdmCondition>,
        now: i64,
    ) -> Result<(), MdmRefusal> {
        let conditions = (
            MdmCondition::PushCertMissing,
            MdmCondition::PushKeyMissing,
            MdmCondition::PushPairInvalid,
        );
        let kinds = (SecretKind::PushCert, SecretKind::PushKey);
        if let Some(facts) = self.read_pair(kinds, conditions, missing)? {
            status.push_topic = facts.topic.unwrap_or_default();
            status.push_not_after_unix = Some(facts.not_after_unix);
            if facts.not_after_unix <= now {
                missing.push(MdmCondition::PushCertExpired);
            }
            if facts.not_before_unix > now {
                missing.push(MdmCondition::PushCertNotYetValid);
            }
        }
        Ok(())
    }

    /// The ABM token: held, readable, not expired.
    fn check_abm(
        &self,
        status: &mut proto::MdmStatusResponse,
        missing: &mut Vec<MdmCondition>,
        now: i64,
    ) -> Result<(), MdmRefusal> {
        let expiry = match self.held(SecretKind::AbmToken)? {
            Held::Absent => {
                missing.push(MdmCondition::AbmTokenMissing);
                return Ok(());
            }
            Held::Unreadable => None,
            Held::Open(json) => AbmToken::parse(json.expose())
                .ok()
                .and_then(|token| OffsetDateTime::parse(&token.access_secret_expiry, &Rfc3339).ok())
                .map(OffsetDateTime::unix_timestamp),
        };
        let Some(expiry) = expiry else {
            missing.push(MdmCondition::AbmTokenUnreadable);
            return Ok(());
        };
        status.abm_not_after_unix = Some(expiry);
        if expiry <= now {
            missing.push(MdmCondition::AbmTokenExpired);
        }
        Ok(())
    }

    /// The TLS certificate and key: held, a pair, naming the name, not expired.
    fn check_tls(
        &self,
        status: &mut proto::MdmStatusResponse,
        missing: &mut Vec<MdmCondition>,
        now: i64,
    ) -> Result<(), MdmRefusal> {
        let conditions = (
            MdmCondition::TlsCertMissing,
            MdmCondition::TlsKeyMissing,
            MdmCondition::TlsPairInvalid,
        );
        let kinds = (SecretKind::TlsCert, SecretKind::TlsKey);
        if let Some(facts) = self.read_pair(kinds, conditions, missing)? {
            if !status.name.is_empty() && !facts.names_host(&status.name) {
                missing.push(MdmCondition::TlsNameMismatch);
            }
            status.tls_not_after_unix = Some(facts.not_after_unix);
            if facts.not_after_unix <= now {
                missing.push(MdmCondition::TlsCertExpired);
            }
            if facts.not_before_unix > now {
                missing.push(MdmCondition::TlsCertNotYetValid);
            }
            status.tls_names = facts.names;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "mdm_status_tests.rs"]
mod tests;
