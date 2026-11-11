//! The renewal of the certificate for the MDM name (`docs/design/coordinator-mdm.md`, section
//! 4.10): when it is due, one attempt at a time, the wait after a failure, and the timer.
//!
//! **When.** The renewer is configured once the setup holds the MDM name (`mdm_name`), the zone
//! (`dns_zone`) and a DNS credential; before that it does nothing and reports nothing, because an
//! unconfigured setup is not a failing one. A certificate is due when none is held, or the one in
//! use expires within `tls_renew_days` (default 30). The stored certificate is read, not the
//! expiry an upload was labelled with, so an upload with no label is renewed too.
//!
//! **After a failure** the next attempt waits `tls_retry_s` (default 3600) and the failure is
//! kept in the [`TlsStatus`] the alerts read: `mdm_tls_renewal_failed` stands until an attempt
//! ends in a certificate, and `mdm_dns_credential_rejected` stands while the provider says 401 or
//! 403.
//!
//! **One at a time.** An attempt holds a lock for its whole run, and a tick that finds it held
//! does nothing. This is the single flight the design gives the store's lease: v1 has one
//! coordinator on a store (the state lock refuses a second), so a lock in the process is the
//! lease.
//!
//! **A pair that does not match** is due: the key is stored before the chain, and a renewal that
//! stops between the two (a store error, an exit) leaves a new key with the old chain, which the
//! listener cannot serve. The next tick finds the key is not the leaf's and orders again.
//!
//! **A new certificate** is stored as the next version of `tls_key` and then `tls_cert`; the
//! listener's resolver ([`crate::mdm_server::tls::MdmTls`]) reads the newest versions of both for
//! the next handshake, and a connection already open keeps what it was accepted under.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use super::Clock;
use super::dns::{CredentialError, DnsCredential, DnsProviders};
use super::https::Https;
use super::order::{self, OrderError, Plan};
use super::status::TlsStatus;
use crate::mdm_secrets::{Secret, SecretKind};
use crate::mdm_settings;
use crate::mdm_setup::{MdmSetup, SetupFailure};

/// The settings key the MDM name is kept under (written by the name rpc).
const NAME_KEY: &str = "mdm_name";

/// What a tick did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// The setup lacks the name, the zone or the credential, or cannot be read.
    NotConfigured,
    /// The certificate in use is not due.
    NotDue,
    /// An attempt failed lately and the wait after it is not over.
    Waiting,
    /// An attempt is running.
    Busy,
    /// A certificate was issued and stored.
    Renewed,
    /// The attempt failed.
    Failed,
}

/// What a renewer is built from.
pub struct Parts {
    /// The setup: secrets and settings.
    pub setup: Arc<MdmSetup>,
    /// The HTTPS port of the ACME exchange.
    pub https: Arc<dyn Https>,
    /// The DNS providers.
    pub providers: Arc<dyn DnsProviders>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
    /// The status the alerts read.
    pub status: Arc<TlsStatus>,
}

/// Issues and renews the certificate.
pub struct TlsRenewer {
    parts: Parts,
    attempt: tokio::sync::Mutex<()>,
}

/// The settings an attempt reads.
struct Config {
    plan: Plan,
    renew_days: u32,
    retry_s: u32,
}

impl Config {
    /// The configuration, none while the name or the zone is not set.
    fn read(setup: &MdmSetup) -> Result<Option<Self>, SetupFailure> {
        let set = setup.settings()?;
        let name = set.get(NAME_KEY).cloned().unwrap_or_default();
        let zone = mdm_settings::text_of(&set, "dns_zone");
        if name.is_empty() || zone.is_empty() {
            return Ok(None);
        }
        let number = |key: &str| mdm_settings::days_of(&set, key, 0);
        let contact = mdm_settings::text_of(&set, "acme_contact");
        Ok(Some(Self {
            plan: Plan {
                name,
                zone,
                directory: mdm_settings::text_of(&set, "acme_directory"),
                contact: (!contact.is_empty()).then_some(contact),
                ttl_s: number("dns_ttl_s"),
                poll_s: number("dns_poll_s"),
                dns_timeout_s: number("dns_timeout_s"),
                settle_s: number("dns_settle_s"),
                acme_timeout_s: number("acme_timeout_s"),
            },
            renew_days: number("tls_renew_days"),
            retry_s: number("tls_retry_s"),
        }))
    }
}

/// Why an attempt failed, in words that hold no secret.
#[derive(Debug, thiserror::Error)]
enum Failure {
    #[error("the setup could not be read: {0}")]
    Setup(#[from] SetupFailure),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error(transparent)]
    Order(#[from] OrderError),
    #[error("the certificate the CA issued is not usable: {0}")]
    Issued(&'static str),
}

impl TlsRenewer {
    /// A renewer over `parts`.
    #[must_use]
    pub fn new(parts: Parts) -> Self {
        Self {
            parts,
            attempt: tokio::sync::Mutex::new(()),
        }
    }

    /// Looks at whether the certificate is due and, if it is and no wait stands, orders one.
    pub async fn tick(&self) -> Tick {
        let setup = &self.parts.setup;
        let config = match Config::read(setup) {
            Ok(Some(config)) => config,
            Ok(None) => return Tick::NotConfigured,
            Err(why) => {
                tracing::error!(%why, "the certificate for the MDM name is not looked at: the setup could not be read");
                return Tick::NotConfigured;
            }
        };
        // A credential that is held but does not open is a failure to report when a renewal is
        // due, not a reason to alert at every tick: it is carried to the attempt.
        let Some(credential) = setup.open(SecretKind::DnsCredential).transpose() else {
            return Tick::NotConfigured;
        };
        let now = self.parts.clock.now_unix();
        // A failure is cleared when the certificate in use is good again, whatever made it so
        // (an upload of 4.9 included), and a new DNS credential ends the wait the old one earned.
        self.parts
            .status
            .credential_is(self.version_of(SecretKind::DnsCredential));
        if !self.due(&config, now) {
            self.parts.status.succeeded();
            return Tick::NotDue;
        }
        if now < self.parts.status.retry_at_unix() {
            return Tick::Waiting;
        }
        let Ok(_one) = self.attempt.try_lock() else {
            return Tick::Busy;
        };
        match self.run(&config, credential).await {
            Ok(()) => {
                self.parts.status.succeeded();
                tracing::info!(name = %config.plan.name, "the certificate for the MDM name was renewed");
                Tick::Renewed
            }
            Err(why) => self.fail(&config, &why),
        }
    }

    fn fail(&self, config: &Config, why: &Failure) -> Tick {
        let now = self.parts.clock.now_unix();
        let rejected = matches!(why, Failure::Order(order) if order.dns_rejected());
        tracing::error!(%why, name = %config.plan.name, "the certificate for the MDM name could not be renewed");
        self.parts.status.failed(
            now,
            now.saturating_add(u64::from(config.retry_s)),
            &why.to_string(),
            rejected,
            self.version_of(SecretKind::DnsCredential),
        );
        Tick::Failed
    }

    /// The newest version of `kind` held, 0 when none or the list cannot be read.
    fn version_of(&self, kind: SecretKind) -> u32 {
        self.parts
            .setup
            .list_secrets()
            .unwrap_or_default()
            .iter()
            .filter(|info| info.kind == kind)
            .map(|info| info.version)
            .max()
            .unwrap_or(0)
    }

    /// Whether no usable certificate is held: none, the one in use expires within the renew
    /// window, or the key in use is not the key of its leaf (a renewal that stopped between its
    /// two writes leaves that, and the pair that was in use keeps being served until it is repaired).
    fn due(&self, config: &Config, now: u64) -> bool {
        let window = u64::from(config.renew_days).saturating_mul(86_400);
        match self.parts.setup.open(SecretKind::TlsCert) {
            Ok(Some(cert)) => {
                let key = match self.parts.setup.open(SecretKind::TlsKey) {
                    Ok(key) => key,
                    // A record that does not open is not replaced: an operator looks at it.
                    Err(_) => return false,
                };
                key.is_none_or(|key| !key_matches(key.expose(), cert.expose()))
                    || leaf_not_after(cert.expose())
                        .is_none_or(|not_after| not_after < now.saturating_add(window))
            }
            Ok(None) => true,
            // A record that does not open is not replaced: an operator looks at it.
            Err(_) => false,
        }
    }

    async fn run(
        &self,
        config: &Config,
        credential: Result<Secret, SetupFailure>,
    ) -> Result<(), Failure> {
        let setup = &self.parts.setup;
        let credential = DnsCredential::parse(credential?.expose())?;
        let dns = self.parts.providers.open(&credential)?;
        let stored = setup.open(SecretKind::AcmeAccountKey)?;
        let (account, created) = order::open_account(
            &self.parts.https,
            &config.plan,
            stored.as_ref().map(Secret::expose),
        )
        .await?;
        // The account is stored before it is used: an order that fails still leaves the account
        // the next attempt reuses, and the CA's rate limit on new accounts is not spent twice.
        if let Some(created) = created {
            setup.put_secret(
                SecretKind::AcmeAccountKey,
                &Secret::new(created),
                None,
                self.now_i64(),
            )?;
        }
        let issued = order::issue(&account, &config.plan, &*dns, &*self.parts.clock).await?;
        let not_after = leaf_not_after(issued.chain_pem.as_bytes())
            .ok_or(Failure::Issued("its first certificate cannot be read"))?;
        let not_after = i64::try_from(not_after).ok();
        setup.put_secret(
            SecretKind::TlsKey,
            &Secret::new(issued.key_pem.into_bytes()),
            None,
            self.now_i64(),
        )?;
        setup.put_secret(
            SecretKind::TlsCert,
            &Secret::new(issued.chain_pem.into_bytes()),
            not_after,
            self.now_i64(),
        )?;
        Ok(())
    }

    fn now_i64(&self) -> i64 {
        i64::try_from(self.parts.clock.now_unix()).unwrap_or(i64::MAX)
    }
}

/// Whether the private key PEM `key` is the key of the first certificate of the PEM `chain`.
fn key_matches(key: &[u8], chain: &[u8]) -> bool {
    let Ok(key) = PrivateKeyDer::from_pem_slice(key) else {
        return false;
    };
    let Some(public) = rustls::crypto::ring::sign::any_supported_type(&key)
        .ok()
        .and_then(|key| key.public_key().map(|public| public.into_owned()))
    else {
        return false;
    };
    let Some(Ok(leaf)) = CertificateDer::pem_slice_iter(chain).next() else {
        return false;
    };
    x509_parser::parse_x509_certificate(&leaf)
        .is_ok_and(|(_, parsed)| parsed.public_key().raw == public.as_ref())
}

/// When the first certificate of the PEM `chain` expires, unix seconds.
fn leaf_not_after(chain: &[u8]) -> Option<u64> {
    let leaf = CertificateDer::pem_slice_iter(chain).next()?.ok()?;
    let (_, parsed) = x509_parser::parse_x509_certificate(&leaf).ok()?;
    u64::try_from(parsed.validity().not_after.timestamp()).ok()
}

/// Ticks the renewer every `tls_check_s` seconds (read again each time, default 300), forever.
/// The first tick is at once.
pub async fn renew_every(renewer: Arc<TlsRenewer>) -> Infallible {
    loop {
        renewer.tick().await;
        let seconds = renewer
            .parts
            .setup
            .settings()
            .map_or(300, |set| mdm_settings::days_of(&set, "tls_check_s", 300));
        renewer
            .parts
            .clock
            .sleep(Duration::from_secs(seconds.into()))
            .await;
    }
}

#[cfg(test)]
#[path = "renewal_tests.rs"]
mod tests;
