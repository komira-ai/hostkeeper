//! The CA of the Macs' identities, made when it is first needed.
//!
//! Its key and certificate are one sealed secret of the setup, `mdm_ca_key`: a four-byte big-endian
//! length of the certificate, the certificate (DER), and the key (PKCS#8, DER). The coordinator
//! makes it; nothing uploads it (a `PutSecret` of the kind would be read the same way). The CA is
//! an RSA 2048 key with a self-signed certificate of `ca_days` days, apart from the host CA
//! (`docs/design/coordinator-mdm.md`, 4.2).

use std::time::{Duration, SystemTime};

use hostkeeper_mdm_scep::{Ca, Policy};

use super::Tunables;
use crate::audit_actions;
use crate::mdm_secrets::{Secret, SecretKind};
use crate::mdm_server::MdmServer;
use crate::mdm_setup::SetupFailure;
use crate::service::Coordinator;

/// The key size of the CA.
const KEY_BITS: usize = 2048;

/// Why there is no CA to answer with.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CaError {
    /// The coordinator holds no master key, so it can neither keep a CA nor open one.
    #[error("the coordinator holds no master key")]
    NoKey,
    /// The setup could not be read or written, or holds a CA that does not open.
    #[error("the MDM setup: {0}")]
    Setup(String),
    /// The CA could not be made or the one held is not a CA.
    #[error("the identity CA: {0}")]
    Build(String),
    /// The audit row of making it could not be written, so it was not made.
    #[error("the audit row of the new CA could not be written")]
    Audit,
}

impl From<SetupFailure> for CaError {
    fn from(why: SetupFailure) -> Self {
        match why {
            SetupFailure::NoKey => Self::NoKey,
            other => Self::Setup(other.to_string()),
        }
    }
}

/// A CA that could not be made or read.
fn build(why: impl std::fmt::Display) -> CaError {
    CaError::Build(why.to_string())
}

/// The secret for `cert` and `key`.
pub(crate) fn pack(cert: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(cert.len())
        .unwrap_or(u32::MAX)
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(cert);
    out.extend_from_slice(key);
    out
}

/// The certificate and the key of a secret [`pack`] made.
pub(crate) fn unpack(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (length, rest) = bytes.split_first_chunk::<4>()?;
    let length = usize::try_from(u32::from_be_bytes(*length)).ok()?;
    (length <= rest.len()).then(|| rest.split_at(length))
}

fn policy(tune: &Tunables) -> Policy {
    Policy {
        org: tune.org.clone(),
        identity_days: tune.identity_days,
        ..Policy::default()
    }
}

/// The CA held, if there is one.
fn held(coordinator: &Coordinator, tune: &Tunables) -> Result<Option<Ca>, CaError> {
    let Some(secret) = coordinator.mdm_setup.open(SecretKind::MdmCaKey)? else {
        return Ok(None);
    };
    let (cert, key) = unpack(secret.expose())
        .ok_or("the sealed CA is not a certificate and a key")
        .map_err(build)?;
    Ca::from_der(cert, key, policy(tune))
        .map(Some)
        .map_err(build)
}

/// The CA of the Macs' identities with the policy of `tune`: the one held, or a new one made under
/// the lock of `server` and audited before it is stored.
///
/// # Errors
/// [`CaError`].
pub(crate) async fn identity_ca(
    coordinator: &Coordinator,
    server: &MdmServer,
    tune: &Tunables,
) -> Result<Ca, CaError> {
    if let Some(ca) = held(coordinator, tune)? {
        return Ok(ca);
    }
    let _one = server.enroll.creating.lock().await;
    if let Some(ca) = held(coordinator, tune)? {
        return Ok(ca);
    }
    let now = server.now();
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(now).unwrap_or(0));
    let (days, plan) = (tune.ca_days, policy(tune));
    let ca = tokio::task::spawn_blocking(move || {
        Ca::generate(
            &mut rsa::rand_core::UnwrapErr(getrandom::SysRng),
            KEY_BITS,
            "CN=hostkeeper MDM CA,OU=hostkeeper-mdm",
            at,
            days,
            plan,
        )
    })
    .await
    .map_err(build)?
    .map_err(build)?;
    let key = ca.key_pkcs8_der().map_err(build)?;
    let not_after = now.saturating_add(i64::from(tune.ca_days) * 86_400);
    coordinator
        .audit_setup(
            "coordinator",
            audit_actions::MDM_CA_CREATED,
            None,
            format!("a new identity CA, valid {} days", tune.ca_days),
        )
        .map_err(|_| CaError::Audit)?;
    coordinator.mdm_setup.put_secret(
        SecretKind::MdmCaKey,
        &Secret::new(pack(ca.cert_der(), &key)),
        Some(not_after),
        now,
    )?;
    Ok(ca)
}

/// The certificate of the CA held, DER, without making one: none before the first use.
pub(crate) fn held_certificate(setup: &crate::mdm_setup::MdmSetup) -> Option<Vec<u8>> {
    let secret = setup.open(SecretKind::MdmCaKey).ok()??;
    unpack(secret.expose()).map(|(cert, _)| cert.to_vec())
}
