//! The certificates of the services a host runs (docs/design/service-identity.md, section 4):
//! [`ServiceIdentities`] keeps, for each service it is told wants an identity, a P-256 key and a
//! certificate for it in the host's identity directory.
//!
//! One pass ([`ServiceIdentities::step`]) takes each wanted service in turn:
//!
//! 1. it is skipped while its last try failed less than [`RETRY_AFTER_SECS`] ago on the monotonic
//!    clock;
//! 2. its account must have resolved to a user and group ([`Wanted::account`]); an account the
//!    host does not have is the service's reported error, and nothing is written;
//! 3. the host's own id and trusted CA bundle are read ([`HostTrust`]);
//! 4. the service's directory is made ([`IdentityFiles::prepare`]) and the installed certificate
//!    read: when it is there and short of half its life, nothing more is done;
//! 5. the key is read, or made and written to `key.pem` before the coordinator is asked, so a
//!    restart asks again for the same key;
//! 6. the coordinator is asked ([`ServiceCertIssuer`]) for a certificate for a request signed by
//!    that key;
//! 7. an answer is checked ([`IdentityRefusal`]) and installed (`ca.pem`, then `cert.pem`); a
//!    refusal of the coordinator, an answer that fails a check or a failure of the transport
//!    keeps what is installed and is the service's reported error.
//!
//! A service that is not wanted any more is dropped from the report; its files stay until its
//! uninstall removes them ([`IdentityFiles::remove`]). [`ServiceIdentities::run_loop`] makes a
//! pass every [`CHECK_SECS`] on the monotonic clock. The wall clock is read only in
//! `validity.rs`.
//!
//! [`IdentityState`] holds what each service last reported, for the daemon's heartbeat.

mod files;
mod trust;
mod verify;

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_config::names::ServiceName;
use hostkeeper_steps::Clock;

use crate::key::{Entropy, HostKey};
use crate::validity::{self, WallClock};
use crate::wire::{HostId, Reason, TransportError};

pub use files::{FilesError, IdentityDir, IdentityFile, IdentityFiles, MAX_FILE_BYTES, Owner};
pub use trust::PkiTrust;
pub use verify::IdentityRefusal;

/// Seconds between two passes of [`ServiceIdentities::run_loop`].
pub const CHECK_SECS: u64 = 60;

/// Seconds a service whose last try failed is left alone.
pub const RETRY_AFTER_SECS: u64 = 300;

/// What the coordinator answered to a request for a service certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueAnswer {
    /// A certificate (DER, or PEM) and the CA certificates (PEM) the coordinator sent with it.
    Issued {
        cert_der: Vec<u8>,
        ca_bundle_pem: String,
    },
    /// A refusal, with its token (`service_not_assigned`, `identity_not_approved`, `denied`, ...).
    Refused(Reason),
    /// A coordinator that does not have the call.
    Unimplemented,
}

/// The coordinator's `IssueServiceCertificate`, over the host's mutual TLS
/// ([`GrpcIssuer`](crate::GrpcIssuer)).
pub trait ServiceCertIssuer: Send {
    /// Asks for a certificate of `service` for the key of `csr_der`.
    ///
    /// # Errors
    /// [`TransportError`]: the call did not complete, or its answer is not one this daemon reads.
    fn issue(&mut self, service: &str, csr_der: &[u8]) -> Result<IssueAnswer, TransportError>;
}

/// What the host says of itself that a service certificate is checked against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trust {
    /// The host's id, from its current certificate.
    pub host_id: HostId,
    /// The CA bundle the host trusts the coordinator by (the current `ca.pem`), PEM.
    pub ca_bundle_pem: String,
}

/// The host's own certificate and trusted CA bundle, read when a pass needs them.
pub trait HostTrust: Send {
    /// The host's [`Trust`], or why it has none (not enrolled yet, files unreadable).
    ///
    /// # Errors
    /// The reason, as the report says it.
    fn read(&self) -> Result<Trust, String>;
}

/// A service that wants an identity, and who owns its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wanted {
    pub service: ServiceName,
    /// The user and group of the manifest's account, or why the host cannot resolve it.
    pub account: Result<Owner, String>,
}

/// What one service reported last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceReport {
    pub service: String,
    /// The last second the installed certificate is valid; none while none is installed.
    pub not_after: Option<u64>,
    /// Why the last try failed; none once a try succeeds.
    pub last_error: Option<String>,
}

/// What each service of [`ServiceIdentities`] last reported, shared by every clone.
#[derive(Clone, Debug, Default)]
pub struct IdentityState(Arc<Mutex<BTreeMap<String, ServiceReport>>>);

impl IdentityState {
    /// The reports, by service name.
    #[must_use]
    pub fn get(&self) -> Vec<ServiceReport> {
        self.lock().values().cloned().collect()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, ServiceReport>> {
        // A writer never panics while it holds the lock: what it holds is whole.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Everything [`ServiceIdentities`] reaches outside itself.
pub struct IdentityPorts {
    pub issuer: Box<dyn ServiceCertIssuer>,
    pub files: Box<dyn IdentityFiles>,
    pub trust: Box<dyn HostTrust>,
    pub entropy: Box<dyn Entropy>,
    /// Read only for certificate validity.
    pub wall: Box<dyn WallClock>,
    /// Paces the passes and the retries.
    pub clock: Box<dyn Clock>,
}

/// Why one service's try failed; its text is the service's reported error.
#[derive(Debug)]
enum Failure {
    Account(String),
    Trust(String),
    Files(FilesError),
    Key(String),
    Asked(String),
    Refused(IdentityRefusal),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Account(why)
            | Failure::Trust(why)
            | Failure::Key(why)
            | Failure::Asked(why) => f.write_str(why),
            Failure::Files(e) => e.fmt(f),
            Failure::Refused(e) => e.fmt(f),
        }
    }
}

/// A failure to make or use the service's key, as `error` says it.
fn key_failure(error: impl fmt::Display) -> Failure {
    Failure::Key(error.to_string())
}

impl From<FilesError> for Failure {
    fn from(error: FilesError) -> Failure {
        Failure::Files(error)
    }
}

impl From<IdentityRefusal> for Failure {
    fn from(error: IdentityRefusal) -> Failure {
        Failure::Refused(error)
    }
}

/// The keeper of the certificates of a host's services; see the module documentation.
pub struct ServiceIdentities {
    ports: IdentityPorts,
    state: IdentityState,
    /// The monotonic second before which a service whose try failed is not tried again.
    retry_at: BTreeMap<String, u64>,
}

impl ServiceIdentities {
    /// A keeper that reports into `state`.
    pub fn new(ports: IdentityPorts, state: IdentityState) -> ServiceIdentities {
        ServiceIdentities {
            ports,
            state,
            retry_at: BTreeMap::new(),
        }
    }

    /// Makes a pass over `wanted()` and waits [`CHECK_SECS`] on the clock, while `keep_going()`.
    pub fn run_loop(
        &mut self,
        wanted: &mut dyn FnMut() -> Vec<Wanted>,
        keep_going: &mut dyn FnMut() -> bool,
    ) {
        while keep_going() {
            self.step(&wanted());
            self.ports.clock.sleep_secs(CHECK_SECS);
        }
    }

    /// One pass over the services that want an identity.
    pub fn step(&mut self, wanted: &[Wanted]) {
        let names: Vec<&str> = wanted.iter().map(|w| w.service.as_str()).collect();
        self.state
            .lock()
            .retain(|name, _| names.contains(&name.as_str()));
        self.retry_at
            .retain(|name, _| names.contains(&name.as_str()));
        for service in wanted {
            self.service_step(service);
        }
    }

    fn service_step(&mut self, wanted: &Wanted) {
        let name = wanted.service.as_str();
        let now = self.ports.clock.mono_secs();
        if self.retry_at.get(name).is_some_and(|at| now < *at) {
            return;
        }
        let mut not_after = self.state.lock().get(name).and_then(|r| r.not_after);
        let result = self.ensure(wanted, &mut not_after);
        let last_error = match result {
            Ok(()) => {
                self.retry_at.remove(name);
                None
            }
            Err(failure) => {
                self.retry_at
                    .insert(name.to_owned(), now.saturating_add(RETRY_AFTER_SECS));
                Some(failure.to_string())
            }
        };
        self.state.lock().insert(
            name.to_owned(),
            ServiceReport {
                service: name.to_owned(),
                not_after,
                last_error,
            },
        );
    }

    /// Brings the service's certificate to one that is valid and short of half its life.
    /// `not_after` is the end of the installed certificate, kept current as it is learned.
    fn ensure(&mut self, wanted: &Wanted, not_after: &mut Option<u64>) -> Result<(), Failure> {
        let service = wanted.service.as_str();
        let owner = wanted
            .account
            .clone()
            .map_err(|why| Failure::Account(format!("the account of the service: {why}")))?;
        let trust = self
            .ports
            .trust
            .read()
            .map_err(|why| Failure::Trust(format!("the host's certificate and CA: {why}")))?;
        self.ports.files.prepare(service, owner)?;
        let installed = self
            .ports
            .files
            .read(service, IdentityFile::Cert, owner)?
            .and_then(|bytes| verify::installed_window(&bytes));
        *not_after = installed.map(|(_, end)| end);
        if let Some((start, end)) = installed
            && !validity::service_due(self.ports.wall.as_ref(), start, end)
        {
            return Ok(());
        }
        let key = self.key(service, owner)?;
        let subject = format!("CN=service:{service}");
        let csr = key.csr_der_for(&subject).map_err(key_failure)?;
        let answer = self
            .ports
            .issuer
            .issue(service, &csr)
            .map_err(|e| Failure::Asked(e.to_string()))?;
        let (cert_der, ca_bundle_pem) = match answer {
            IssueAnswer::Issued {
                cert_der,
                ca_bundle_pem,
            } => (cert_der, ca_bundle_pem),
            IssueAnswer::Refused(reason) => return Err(Failure::Asked(reason.to_string())),
            IssueAnswer::Unimplemented => {
                return Err(Failure::Asked("unimplemented".to_owned()));
            }
        };
        let expected = verify::Expected {
            service,
            host: &trust.host_id,
            key: &key,
            trusted_pem: &trust.ca_bundle_pem,
        };
        let checked = verify::check(
            &cert_der,
            &ca_bundle_pem,
            &expected,
            self.ports.wall.as_ref(),
        )?;
        if let Some((start, _)) = installed
            && checked.not_before <= start
        {
            return Err(IdentityRefusal::NotNewer {
                not_before: checked.not_before,
                installed: start,
            }
            .into());
        }
        self.ports.files.write(
            service,
            IdentityFile::Ca,
            owner,
            trust.ca_bundle_pem.as_bytes(),
        )?;
        self.ports
            .files
            .write(service, IdentityFile::Cert, owner, checked.pem.as_bytes())?;
        *not_after = Some(checked.not_after);
        Ok(())
    }

    /// The service's key: the one in `key.pem`, or a new one written there.
    fn key(&mut self, service: &str, owner: Owner) -> Result<HostKey, Failure> {
        if let Some(pem) = self.ports.files.read(service, IdentityFile::Key, owner)? {
            return verify::key_from_pem(&pem).map_err(Failure::Key);
        }
        let key = HostKey::generate(self.ports.entropy.as_mut()).map_err(key_failure)?;
        let pem = verify::key_to_pem(&key).map_err(Failure::Key)?;
        self.ports
            .files
            .write(service, IdentityFile::Key, owner, pem.as_bytes())?;
        Ok(key)
    }
}
