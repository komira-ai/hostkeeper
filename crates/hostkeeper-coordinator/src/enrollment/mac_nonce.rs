//! The Mac path of enrollment (design 5.9): a Mac that enrolls with the one-time nonce the MDM
//! policy gate gave it is approved without an operator, when every condition holds.
//!
//! - [`Enrollment::issue_mac_nonce`] is how the gate gets a nonce for a serial and the pending
//!   request it saw (docs/design/hostkeeper-mdm.md, section 5.2): 32 random bytes, of which the
//!   store keeps only the SHA-256, with the request's key hash, good for one hour and one
//!   approval of that request. The gate is a component linked into the coordinator, so this is
//!   a library call and no RPC.
//! - On an `Enroll` that carries a nonce, [`Enrollment::mac_auto_approve`] approves the request
//!   only when the serial the request claims is on the allowlist (`--mac-serial-allowlist`),
//!   the gate says the device is enrolled and supervised, and the nonce is known, unexpired,
//!   unused, issued for that serial and issued for that request, checked in that order (a
//!   nonce stored before the request field names none and is judged without it). The nonce is
//!   checked under the store's lock, in the approval itself, so two requests racing with one
//!   nonce cannot both win; the gate is asked only about a serial the allowlist names. It
//!   approves through the same recorded approval as an operator's, by [`MAC_APPROVER`], with
//!   the host id, role and services of the serial's allowlist entry, and the nonce is consumed
//!   in that approval's write. The coordinator then assigns the entry's services to the host
//!   as it does an operator approval's ([`crate::service::Coordinator::enroll`]).
//! - A Mac wiped after its approval comes back with a new key, and its host id is held by the
//!   old one. The nonce approval takes it over, audited `enroll_replace`, when the gate says
//!   the device's current enrollment began after the old key's approval and the old key's own
//!   request claimed the same serial ([`DeviceStatus::enrolled_since_unix`](super::DeviceStatus::enrolled_since_unix)); the old key's
//!   certificate then names no host, as after an operator's `replace`. A denied host id is
//!   never taken over: that is `denied`, and any other holder `host_id_taken`.
//! - Any condition false leaves the request pending, for an operator, and is audited
//!   `mac_auto_approve_declined` with its token ([`MacDecline`]). A declined attempt does not
//!   consume the nonce.
//!
//! The allowlist is a TOML file the coordinator reads at start:
//!
//! ```toml
//! [[mac]]
//! serial = "C02XK1ABJG5J"
//! host_id_prefix = "mac-"
//! role = "build"
//! services = ["builder"]
//! ```
//!
//! The host id is the prefix followed by the serial in lower case; each must make a host id,
//! the role and the services must follow their rules, and a serial is listed once, or the
//! start stops.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::enroll::{
    ApprovalRecord, ApproveError, Decision, MacNonceHash, MacNonceRefusal,
};
use hostkeeper_enrollment::names::{HostId, KeyHash, Role, ServiceName};
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;
use hostkeeper_wire::RoleName;
use serde::Deserialize;

use super::Enrollment;
use super::approve::{Checked, refusal_of};
use super::device_state::{DeviceState, NoDeviceState};
use crate::ops::AuditEntry;

/// The approver a Mac approved by its nonce is recorded under.
pub const MAC_APPROVER: &str = "mdm-nonce";

/// The audit action of a declined automatic approval.
pub const ACTION_DECLINED: &str = "mac_auto_approve_declined";

/// The length of a Mac nonce, in bytes.
pub const MAC_NONCE_BYTES: usize = 32;

/// The reason an automatic approval records.
const MAC_REASON: &str = "approved by its MDM nonce";

/// Why a Mac's request was not approved by its nonce. Each names its token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacDecline {
    NonceUnknown,
    NonceExpired,
    NonceUsed,
    NonceWrongSerial,
    NonceWrongRequest,
    DeviceNotEnrolled,
    DeviceNotSupervised,
    SerialNotAllowed,
    DeviceStateUnavailable,
}

impl MacDecline {
    /// The token of the decline.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            MacDecline::NonceUnknown => "nonce_unknown",
            MacDecline::NonceExpired => "nonce_expired",
            MacDecline::NonceUsed => "nonce_used",
            MacDecline::NonceWrongSerial => "nonce_wrong_serial",
            MacDecline::NonceWrongRequest => "nonce_wrong_request",
            MacDecline::DeviceNotEnrolled => "device_not_enrolled",
            MacDecline::DeviceNotSupervised => "device_not_supervised",
            MacDecline::SerialNotAllowed => "serial_not_allowed",
            MacDecline::DeviceStateUnavailable => "device_state_unavailable",
        }
    }
}

impl From<MacNonceRefusal> for MacDecline {
    fn from(refusal: MacNonceRefusal) -> Self {
        match refusal {
            MacNonceRefusal::Unknown => MacDecline::NonceUnknown,
            MacNonceRefusal::Expired => MacDecline::NonceExpired,
            MacNonceRefusal::Used => MacDecline::NonceUsed,
            MacNonceRefusal::WrongSerial => MacDecline::NonceWrongSerial,
            MacNonceRefusal::WrongRequest => MacDecline::NonceWrongRequest,
        }
    }
}

/// What became of a nonce-bearing request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MacOutcome {
    /// Approved as the allowlist entry says: its host id, role and services.
    Approved(MacAllowEntry),
    /// A condition was false: the request stays pending.
    Declined(MacDecline),
    /// The approval was refused as an operator's would be (the host id is taken, the request
    /// is a duplicate claim, ...), or the store failed: the request stays pending.
    Refused(EnrollmentRefusal),
}

/// A nonce as the gate hands it to a Mac. Its `Debug` shows no byte of it.
#[derive(Clone, PartialEq, Eq)]
pub struct MacNonce(Vec<u8>);

impl MacNonce {
    /// The bytes, for the gate to put in the device's profile.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for MacNonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MacNonce(..)")
    }
}

/// Why no nonce was issued.
#[derive(Debug, thiserror::Error)]
pub enum MacNonceError {
    /// The serial is not 1 to 64 ASCII letters and digits.
    #[error("a Mac serial is 1 to 64 ASCII letters and digits")]
    InvalidSerial,
    /// No random bytes could be had, or the ones drawn make a nonce issued before.
    #[error("no fresh random bytes for a Mac nonce")]
    NoRandom,
    /// The store could not record it.
    #[error(transparent)]
    Store(#[from] hostkeeper_enrollment::enroll::StoreError),
}

/// Whether `serial` could be a Mac serial number: 1 to 64 ASCII letters and digits.
fn is_serial(serial: &str) -> bool {
    (1..=64).contains(&serial.len()) && serial.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// What the allowlist says a serial is approved as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacAllowEntry {
    /// The host id: the entry's prefix and the serial in lower case.
    pub host_id: HostId,
    pub role: Role,
    pub services: Vec<ServiceName>,
}

/// The serials a Mac may be approved by its nonce for, and as what.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MacAllowlist(BTreeMap<String, MacAllowEntry>);

/// Why the allowlist was refused.
#[derive(Debug, thiserror::Error)]
pub enum AllowlistError {
    #[error("--mac-serial-allowlist {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("--mac-serial-allowlist: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowFile {
    #[serde(default)]
    mac: Vec<AllowRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowRow {
    serial: String,
    host_id_prefix: String,
    role: String,
    #[serde(default)]
    services: Vec<String>,
}

impl MacAllowlist {
    /// The allowlist the TOML `text` says.
    ///
    /// # Errors
    /// [`AllowlistError::Invalid`]: not the file's shape, a serial listed twice, or a serial,
    /// host id, role or service that breaks its rule.
    pub fn parse(text: &str) -> Result<MacAllowlist, AllowlistError> {
        let invalid = AllowlistError::Invalid;
        let file: AllowFile = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
        let mut entries = BTreeMap::new();
        for row in file.mac {
            if !is_serial(&row.serial) {
                return Err(invalid(format!(
                    "serial {:?} is not 1 to 64 letters and digits",
                    row.serial
                )));
            }
            let host = format!("{}{}", row.host_id_prefix, row.serial.to_ascii_lowercase());
            let host_id = HostId::parse(&host).map_err(|e| invalid(e.to_string()))?;
            let role = row
                .role
                .parse::<RoleName>()
                .ok()
                .and_then(|_| Role::parse(&row.role).ok())
                .ok_or_else(|| invalid(format!("role {:?} breaks its rule", row.role)))?;
            let services = row
                .services
                .iter()
                .map(|name| ServiceName::parse(name).map_err(|e| invalid(e.to_string())))
                .collect::<Result<Vec<_>, _>>()?;
            let entry = MacAllowEntry {
                host_id,
                role,
                services,
            };
            if entries.insert(row.serial.clone(), entry).is_some() {
                return Err(invalid(format!("serial {} is listed twice", row.serial)));
            }
        }
        Ok(MacAllowlist(entries))
    }

    /// The allowlist in the file `path`.
    ///
    /// # Errors
    /// [`AllowlistError`]: the file cannot be read, or [`Self::parse`] refuses it.
    pub fn load(path: &Path) -> Result<MacAllowlist, AllowlistError> {
        let text = std::fs::read_to_string(path).map_err(|source| AllowlistError::Read {
            path: path.display().to_string(),
            source,
        })?;
        MacAllowlist::parse(&text)
    }

    /// The entry of `serial`.
    #[must_use]
    pub fn get(&self, serial: &str) -> Option<&MacAllowEntry> {
        self.0.get(serial)
    }

    /// Every service an entry names, for the start to check that the coordinator manages each.
    pub fn services(&self) -> impl Iterator<Item = &ServiceName> {
        self.0.values().flat_map(|entry| entry.services.iter())
    }
}

/// The Mac path's configuration: the gate's port and the allowlist.
#[derive(Clone)]
pub struct MacPolicy {
    pub device_state: Arc<dyn DeviceState>,
    pub allowlist: MacAllowlist,
}

impl Default for MacPolicy {
    /// No gate and an empty allowlist: no Mac is approved by its nonce.
    fn default() -> Self {
        MacPolicy {
            device_state: Arc::new(NoDeviceState),
            allowlist: MacAllowlist::default(),
        }
    }
}

impl fmt::Debug for MacPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacPolicy")
            .field("allowlist", &self.allowlist)
            .finish_non_exhaustive()
    }
}

impl Enrollment {
    /// A new nonce for the Mac of `serial` and its pending request `request_id` (the request's
    /// key hash), issued now and good for one hour and one approval of that request. Only its
    /// SHA-256 is recorded, with the serial and the request id; the bytes returned are the only
    /// copy.
    ///
    /// # Errors
    /// [`MacNonceError`].
    pub fn issue_mac_nonce(
        &self,
        serial: &str,
        request_id: &KeyHash,
    ) -> Result<MacNonce, MacNonceError> {
        if !is_serial(serial) {
            return Err(MacNonceError::InvalidSerial);
        }
        let mut nonce = vec![0; MAC_NONCE_BYTES];
        if !self.nonces.fill(&mut nonce) {
            return Err(MacNonceError::NoRandom);
        }
        let hash = MacNonceHash::of(&nonce);
        if !self
            .store
            .issue_mac_nonce(serial, request_id, &hash, self.clock.now_unix())?
        {
            return Err(MacNonceError::NoRandom);
        }
        Ok(MacNonce(nonce))
    }

    /// Approves the pending request `request_id`, which presented `nonce`, when every condition
    /// of the Mac path holds for the serial the request was posted with; otherwise it stays
    /// pending, and the reason is logged and audited.
    pub fn mac_auto_approve(&self, request_id: &KeyHash, nonce: &[u8]) -> MacOutcome {
        // The serial is the one the request was first posted with: the one the store checks
        // the nonce against, never one a later post claims.
        let pending = match self.store.request(request_id) {
            Ok(Some(pending)) => pending,
            Ok(None) => {
                return self.mac_refused(request_id, None, EnrollmentRefusal::UnknownRequest);
            }
            Err(why) => {
                tracing::error!(%why, "cannot read a Mac's request: it stays pending");
                return self.mac_refused(request_id, None, EnrollmentRefusal::StoreFailed);
            }
        };
        let serial = pending.claims.mac_serial.as_str();
        let Some(entry) = self.mac.allowlist.get(serial) else {
            return self.decline(request_id, serial, MacDecline::SerialNotAllowed);
        };
        let status = match self.mac.device_state.status(serial) {
            Err(why) => {
                tracing::warn!(%why, "the device-state gate gave no answer");
                return self.decline(request_id, serial, MacDecline::DeviceStateUnavailable);
            }
            Ok(status) if !status.enrolled => {
                return self.decline(request_id, serial, MacDecline::DeviceNotEnrolled);
            }
            Ok(status) if !status.supervised => {
                return self.decline(request_id, serial, MacDecline::DeviceNotSupervised);
            }
            Ok(status) => status,
        };
        let mut checked = Checked {
            request_id: *request_id,
            key_sha256: *request_id,
            decision: Decision {
                host_id: entry.host_id.clone(),
                role: entry.role.clone(),
                services: entry.services.clone(),
                replace: false,
            },
            reason: MAC_REASON.to_owned(),
        };
        let record = ApprovalRecord {
            approver: MAC_APPROVER.to_owned(),
            approved_unix: self.clock.now_unix(),
            reason: MAC_REASON.to_owned(),
            mac_nonce: Some(MacNonceHash::of(nonce)),
            mac_enrolled_since_unix: status.enrolled_since_unix,
        };
        let approved =
            match self
                .store
                .approve_recorded(request_id, request_id, &checked.decision, &record)
            {
                Ok(approval) => {
                    // Recorded as replacing when it took the Mac's own host id over.
                    checked.decision.replace = approval.replace;
                    Ok(Ok(self.audited(&checked, &record, pending.claims.clone())))
                }
                Err(ApproveError::MacNonce(refusal)) => Ok(Err(MacDecline::from(refusal))),
                Err(other) => Err(refusal_of(&other)),
            };
        match approved {
            Ok(Ok(approved)) => {
                tracing::info!(
                    request_id = %request_id,
                    host_id = %approved.host_id,
                    "a Mac was approved by its nonce"
                );
                MacOutcome::Approved(entry.clone())
            }
            Ok(Err(why)) => self.decline(request_id, serial, why),
            Err(refusal) => self.mac_refused(request_id, Some(&entry.host_id), refusal),
        }
    }

    /// The automatic approval of `request_id` (as `host_id`, when one was chosen) refused as an
    /// operator's would be: audited by [`MAC_APPROVER`], and the request stays as it was.
    fn mac_refused(
        &self,
        request_id: &KeyHash,
        host_id: Option<&HostId>,
        refusal: EnrollmentRefusal,
    ) -> MacOutcome {
        self.audit_refused(MAC_APPROVER, refusal, Some(request_id), host_id);
        MacOutcome::Refused(refusal)
    }

    /// Declines the automatic approval of `request_id` for `why`: logged and audited.
    fn decline(&self, request_id: &KeyHash, serial: &str, why: MacDecline) -> MacOutcome {
        tracing::warn!(request_id = %request_id, reason = why.token(), "a Mac was not approved by its nonce");
        let mut entry = AuditEntry::new(MAC_APPROVER, ACTION_DECLINED);
        let serial: String = serial
            .chars()
            .take(64)
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '?' })
            .collect();
        entry.reason = Some(format!(
            "{}; request {request_id}; serial {serial}",
            why.token()
        ));
        self.audit(entry);
        MacOutcome::Declined(why)
    }
}
