//! `Ssh.SignUserCert` and `Ssh.Revoke` over [`Coordinator`] (docs/design/ssh-through-
//! coordinator.md, section 5): who may have a certificate for which host, the one certificate the
//! CA then signs, and the revocation of certificates.
//!
//! The order of a signing is the design's, and each step refuses before the next runs:
//!
//! 1. the caller is an operator the operators file lists, and the host is one it may reach (a
//!    refusal after the caller is known is an audit line);
//! 2. the host is enrolled and approved, and the key is one the CA signs;
//! 3. the ledger records the certificate and its audit line in one write, and gives the serial;
//!    when it cannot, no certificate is made;
//! 4. the key signs: the principal is `hk-host-<host id>` and never a value of the request, the
//!    key id is `<operator>/<serial>`, the validity runs from a minute before the ledger's time to
//!    `--ssh-cert-ttl-s` after it, and the one extension is `permit-pty`.
//!
//! Any of the coordinator's instances signs: the key is a secret every instance reads, the
//! serial a sequence of the database, and nothing is kept in memory.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::EnrolledHost;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::SshRefusal as Refusal;

use crate::enrollment::Random;
use crate::operator::Caller;
use crate::service::Coordinator;
use crate::ssh_cert::{CaKey, Spec, UserKey};
use crate::ssh_ledger::{Issue, Ledger, LedgerError, Revocation};
use crate::ssh_operators::{Class, Operators, OperatorsError, Target};
use crate::tokens::TokenName;

/// How long before the ledger's time a certificate is already valid, for the clock of the host.
const BACKDATE_SECS: u64 = 60;

/// The most characters of the reason of a signing.
const MAX_REASON: usize = 200;

/// What signs, and under which rules.
pub struct Signing {
    /// The keys of the `fleet` CA secret: `current`, and `next` while a rotation runs.
    pub(crate) fleet: Vec<CaKey>,
    /// Which of [`Signing::fleet`] signs.
    pub(crate) signing: usize,
    /// The key of the `control-plane` CA, when the coordinator holds its private half.
    pub(crate) control_plane: Option<CaKey>,
    /// The public key of the `control-plane` CA the owner gave (`--ssh-cp-ca-public-file`), as
    /// `ssh-ed25519 <base64>`.
    pub(crate) control_plane_public: Option<String>,
    /// What every host is told to do with its password logins.
    pub(crate) password_auth: crate::config::PasswordAuth,
    /// The hosts of the `control-plane` class.
    pub(crate) control_plane_hosts: BTreeSet<String>,
    /// The operators file, read again at each call.
    pub(crate) operators: PathBuf,
    /// How long a certificate is valid, in seconds.
    pub(crate) ttl_secs: u64,
    /// The account a Linux host takes the certificate for.
    pub(crate) login_linux: String,
    /// The account of a Mac; empty when the host's daemon picks it.
    pub(crate) login_macos: String,
    pub(crate) ledger: Arc<dyn Ledger>,
    pub(crate) random: Random,
}

impl std::fmt::Debug for Signing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signing")
            .field("fleet", &self.fleet)
            .field("signing", &self.signing)
            .finish_non_exhaustive()
    }
}

/// Whether `reason` is at most [`MAX_REASON`] printable ASCII characters.
fn reason_ok(reason: &str) -> bool {
    reason.len() <= MAX_REASON && reason.bytes().all(|b| (0x20..0x7f).contains(&b))
}

/// The class of the host `id`.
pub(crate) fn class_of(ssh: &Signing, id: &str) -> Class {
    if ssh.control_plane_hosts.contains(id) {
        Class::ControlPlane
    } else {
        Class::Fleet
    }
}

/// The principal of a certificate for the host `id`, which the host's principals file lists.
pub(crate) fn principal(id: &str) -> String {
    format!("hk-host-{id}")
}

impl Signing {
    /// The key that signs for a host of `class`: none when the coordinator holds no key for it.
    fn key_for(&self, class: Class) -> Option<&CaKey> {
        match class {
            Class::Fleet => self.fleet.get(self.signing),
            Class::ControlPlane => self.control_plane.as_ref(),
        }
    }

    /// The account a host takes the certificate for: the Macs' when the host is a Mac.
    pub(crate) fn login_for(&self, host: &EnrolledHost) -> &str {
        if host.claims.mac_serial.is_empty() {
            &self.login_linux
        } else {
            &self.login_macos
        }
    }

    /// The operators file as it is now, or why it cannot be read.
    ///
    /// # Errors
    /// [`OperatorsError`] when the file cannot be read or parsed.
    pub(crate) fn read_operators(&self) -> Result<Operators, OperatorsError> {
        Operators::read(&self.operators)
    }

    /// The operators file as it is now; a file that cannot be read lists no one, so nothing is
    /// signed until it can be (default deny).
    pub(crate) fn operators(&self) -> Operators {
        self.read_operators().unwrap_or_else(|why| {
            tracing::error!(%why, "the operators file cannot be read: no one is permitted");
            Operators::default()
        })
    }

    /// Writes the line of a refusal; the refusal stands whether or not the line could be written.
    pub(crate) fn refused(&self, by: &str, host_id: &str, why: Refusal) {
        deny(&*self.ledger, by, host_id, why);
    }
}

/// Writes the line of a refusal to `ledger`, whose token is the refusal's name in lower case; the
/// refusal stands whether or not the line could be written.
pub(crate) fn deny(ledger: &dyn Ledger, by: &str, host_id: &str, why: Refusal) {
    let token = why
        .as_str_name()
        .trim_start_matches("SSH_REFUSAL_")
        .to_ascii_lowercase();
    if let Err(error) = ledger.denied(by, host_id, &token) {
        tracing::warn!(%error, "the line of a refused SSH call could not be written");
    }
}

impl Coordinator {
    /// `Ssh.SignUserCert`: the certificate for the caller's key, for one host.
    ///
    /// # Errors
    /// [`Refusal`]: the call is refused and nothing is signed.
    pub(crate) fn ssh_sign_user_cert(
        &self,
        caller: &Caller,
        request: &proto::SignUserCertRequest,
    ) -> Result<proto::SignUserCertResponse, Refusal> {
        let operator = caller.operator().map_err(Refusal::from)?;
        let ssh = self.ssh.as_ref().ok_or(Refusal::NotConfigured)?;
        let host_id = hostkeeper_enrollment::names::HostId::parse(&request.host_id)
            .map_err(|_| Refusal::InvalidRequest)?;
        let host_id = host_id.as_str();
        let operators = ssh.operators();
        if !operators.lists(operator) {
            ssh.refused(operator, host_id, Refusal::NotPermitted);
            return Err(Refusal::NotPermitted);
        }
        let host = match self.enrolled_host(host_id) {
            Ok(Some(host)) => host,
            Ok(None) => {
                ssh.refused(operator, host_id, Refusal::UnknownHost);
                return Err(Refusal::UnknownHost);
            }
            Err(why) => {
                tracing::error!(%why, "the enrolled host could not be read");
                return Err(Refusal::StoreFailed);
            }
        };
        let class = class_of(ssh, host_id);
        let target = Target {
            id: host_id,
            group: &host.host_group,
            class,
        };
        if !operators.permits(operator, &target) {
            ssh.refused(operator, host_id, Refusal::NotPermitted);
            return Err(Refusal::NotPermitted);
        }
        let key = ssh.key_for(class).ok_or(Refusal::NotConfigured)?;
        if !reason_ok(&request.reason) {
            ssh.refused(operator, host_id, Refusal::BadReason);
            return Err(Refusal::BadReason);
        }
        let Ok(parsed) = UserKey::parse(&request.public_key) else {
            ssh.refused(operator, host_id, Refusal::BadKey);
            return Err(Refusal::BadKey);
        };
        let issued = ssh
            .ledger
            .issue(&Issue {
                operator,
                host_id,
                fingerprint: &parsed.fingerprint,
                ttl_secs: ssh.ttl_secs,
                reason: &request.reason,
            })
            .map_err(|why| {
                tracing::error!(%why, "a certificate was not signed: the ledger did not write");
                Refusal::AuditUnavailable
            })?;
        let expires = issued.issued_unix.saturating_add(ssh.ttl_secs);
        let key_id = format!("{operator}/{}", issued.serial);
        let principal = principal(host_id);
        let certificate = key
            .sign_user(
                &Spec {
                    key: &parsed.key,
                    serial: issued.serial,
                    key_id: &key_id,
                    principal: &principal,
                    valid_after: issued.issued_unix.saturating_sub(BACKDATE_SECS),
                    // The last valid second is `expires`; the certificate's end is exclusive.
                    valid_before: expires.saturating_add(1),
                },
                &*ssh.random,
            )
            .map_err(|why| {
                tracing::error!(%why, "a certificate could not be made");
                Refusal::StoreFailed
            })?;
        Ok(proto::SignUserCertResponse {
            certificate,
            login_user: ssh.login_for(&host).to_owned(),
            expires_unix: expires,
            key_id,
        })
    }

    /// `Ssh.Revoke`: the certificates of an operator, or one serial. Only an operator the file
    /// lists may revoke.
    ///
    /// # Errors
    /// [`Refusal`]: the call is refused and nothing is revoked.
    pub(crate) fn ssh_revoke(
        &self,
        caller: &Caller,
        request: &proto::RevokeRequest,
    ) -> Result<proto::RevokeResponse, Refusal> {
        let by = caller.operator().map_err(Refusal::from)?;
        let ssh = self.ssh.as_ref().ok_or(Refusal::NotConfigured)?;
        if !ssh.operators().lists(by) {
            ssh.refused(by, "", Refusal::NotPermitted);
            return Err(Refusal::NotPermitted);
        }
        let target = match (request.operator.as_str(), request.serial) {
            ("", serial) if serial != 0 => Revocation::Serial(serial),
            (name, 0) if name.parse::<TokenName>().is_ok() => Revocation::Operator(name),
            _ => return Err(Refusal::InvalidRequest),
        };
        match ssh.ledger.revoke(by, target) {
            Ok(revoked) => Ok(proto::RevokeResponse { revoked }),
            Err(LedgerError::UnknownCert) => Err(Refusal::UnknownCert),
            Err(why) => {
                tracing::error!(%why, "a revocation was not written: nothing changed");
                Err(Refusal::AuditUnavailable)
            }
        }
    }
}

#[cfg(test)]
#[path = "ssh_sign_tests.rs"]
pub(crate) mod tests;
