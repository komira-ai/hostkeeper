//! Enrollment at the coordinator (docs/design/hostkeeper-design.md, sections 4.4, 5.7, 6.1 and
//! 8.8): how a host gets and keeps its certificate, and what the heartbeat path asks of the
//! enrollment store.
//!
//! The certificate authority and the store are `hostkeeper_enrollment`'s; this module is the
//! coordinator's side of them:
//!
//! - [`Enrollment`]: the store, the CA, the clock and the poll nonces of one coordinator;
//! - `rpc`: `Enroll`, `Poll` and `Renew` of `hostkeeper.agent.v1.Coordinator` as handlers over
//!   the protobuf messages, and [`call`], which answers one encoded call from a peer, refusing a
//!   `Renew` without a client certificate before its body is read;
//! - `refusal`: the one place an error of the store or the CA becomes a [`CoordinatorRefusal`]
//!   (or [`RpcError::Internal`]), and the gRPC status of each;
//! - `nonce`: the single-use nonces a `Poll` is signed over;
//! - `setup`: the start: the CA key file (made with `--ca-init`), the store, and the
//!   coordinator's server certificate, which [`Enrollment::renew_server`] makes again while the
//!   coordinator runs;
//! - `approve` and `list`: `Enrollment.Approve` and `Enrollment.List` of
//!   `hostkeeper.operator.v1` as plain functions over the store, and the audit of each
//!   approval in the coordinator's ops store ([`Enrollment::audit_to`]);
//! - `by_record`: a pending request that matches an open inventory record exactly is approved by
//!   the coordinator itself ([`Enrollment::approve_by_record`]), audited
//!   `enrollment.auto_approved` and marking the record used;
//! - `reject`: `Enrollment.Reject` ([`Enrollment::reject`]), and the deny row `Hosts.Retire`
//!   writes ([`Enrollment::deny_host`]);
//! - `expiry`: a request no operator approved within the request TTL expires
//!   ([`Enrollment::expire_requests`], run by the reload tick), audited `request_expired`;
//! - `mac_nonce` and `device_state`: the Mac path of design 5.9, a request approved by its MDM
//!   nonce when the allowlist and the gate's [`DeviceState`] port agree;
//! - `gate_port`: the enrollment as the gate's enrollment side asks it
//!   ([`hostkeeper_mdm_gate::Enrollments`]): the allowlist, the waiting requests of a serial,
//!   its last approval, a nonce bound to a request, and the audit of a push; a request posted
//!   is told to the gate ([`Enrollment::with_posted`]).
//!
//! The heartbeat path asks [`Enrollment::identity_of`] who a certificate the CA issued names,
//! [`Enrollment::admission`] whether its host is denied, retiring or neither,
//! [`Enrollment::note_heartbeat`] to keep the first heartbeat since its approval,
//! [`Enrollment::note_serving`] to keep the first that reported a managed service serving,
//! [`Enrollment::report_live`] which machine speaks for it, and [`Enrollment::is_duplicate`]
//! whether it is ordered nothing ([`crate::service::Coordinator`]).

mod approve;
mod by_record;
mod device_state;
mod expiry;
mod gate_port;
mod list;
mod mac_nonce;
mod nonce;
mod refusal;
mod reject;
mod rpc;
mod setup;

use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::ca::{Ca, CaBundle, Signer};
use hostkeeper_enrollment::enroll::EnrollmentStore;
use hostkeeper_enrollment::names::HostId;
use hostkeeper_enrollment::validity::UnixClock;
pub use hostkeeper_proto::hostkeeper::agent::v1::CoordinatorRefusal;

pub use approve::{ACTION_APPROVE, ACTION_REFUSED, ACTION_REPLACE, Approved, token_of};
pub use by_record::{ACTION_AUTO_APPROVED, ByRecord, INVENTORY_APPROVER, Waits};
#[cfg(test)]
pub use device_state::FakeDeviceState;
pub use device_state::{DeviceState, DeviceStateError, DeviceStatus, NoDeviceState};
pub use expiry::DEFAULT_REQUEST_TTL_SECS;
pub use gate_port::GATE_AUDITOR;
pub use list::MAX_PENDING_PAGE;
pub use mac_nonce::{
    ACTION_DECLINED, AllowlistError, MAC_APPROVER, MAC_NONCE_BYTES, MacAllowEntry, MacAllowlist,
    MacDecline, MacNonce, MacNonceError, MacOutcome, MacPolicy,
};
pub use nonce::{NONCE_BYTES, NONCES_PER_REQUEST, Nonces, Random, os_random};
pub use refusal::{RpcError, refusal_of};
pub use reject::ACTION_REJECT;
pub use rpc::{Enrolled, MAX_MESSAGE_BYTES, Method, call};
pub use setup::{
    CA_CERT_SUFFIX, Kept, ServerFile, Setup, SetupError, StoreAt, ca_certificate_path, pem,
    server_due, setup,
};

use crate::identity::Identity;
use crate::ops::OpsStore;

/// What the heartbeat path does with a caller, by its host's deny row (design 6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Not denied: the heartbeat is read and answered.
    Admitted,
    /// Denied while an update may still be in flight: the heartbeat is read for its final
    /// report, then refused `denied`.
    Retiring,
    /// Denied and final, or the deny row cannot be read: refused `denied` before the body is
    /// read.
    Denied,
}

/// The coordinator's server certificate as the status shows it (`server_certificate`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ServerCertificate {
    /// The end of the validity of the certificate the server certificate file started with at
    /// the last look that read it, in unix seconds.
    pub not_after: u64,
    /// Why the last look could not renew it; `None` when the last look found it not due or
    /// renewed it. The certificate in use is still served, and the next look tries again.
    pub renewal_error: Option<String>,
}

/// The coordinator's enrollment: its store, its CA, its server certificate file, its wall
/// clock, its poll nonces, the Mac path's policy, the request TTL, and the ops store it audits
/// approvals in.
pub struct Enrollment {
    store: EnrollmentStore,
    ca: Ca,
    server: ServerFile,
    /// What the last look at `server` found.
    server_state: Mutex<ServerCertificate>,
    clock: Arc<dyn UnixClock>,
    nonces: Nonces,
    mac: MacPolicy,
    /// How long a request waits for an operator before it expires, in seconds.
    request_ttl_secs: u64,
    /// Set when a coordinator takes this enrollment ([`Enrollment::audit_to`]).
    audit: Mutex<Option<Arc<OpsStore>>>,
    /// Called after each new request is recorded ([`Enrollment::with_posted`]).
    posted: Option<Posted>,
}

/// What [`Enrollment::with_posted`] calls.
pub type Posted = Arc<dyn Fn() + Send + Sync>;

impl std::fmt::Debug for Enrollment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Enrollment")
            .field("ca", &self.ca)
            .finish_non_exhaustive()
    }
}

/// The store's spelling of a host id the heartbeat path read from a certificate: the two
/// grammars are one (a DNS label), so every id the wire reads is one the store reads.
fn store_host(host: &hostkeeper_wire::HostId) -> Option<HostId> {
    HostId::parse(host.as_str()).ok()
}

impl Enrollment {
    /// The enrollment of `store` and `ca`, posting requests at the time `clock` reads, with
    /// poll nonces drawn from `random`. It keeps the server certificate file `server`, whose
    /// certificate is valid until `not_after` (unix seconds).
    #[must_use]
    pub fn new(
        store: EnrollmentStore,
        ca: Ca,
        server: ServerFile,
        not_after: u64,
        clock: Arc<dyn UnixClock>,
        random: Random,
    ) -> Self {
        Self {
            store,
            ca,
            server,
            server_state: Mutex::new(ServerCertificate {
                not_after,
                renewal_error: None,
            }),
            clock,
            nonces: Nonces::new(random),
            mac: MacPolicy::default(),
            request_ttl_secs: DEFAULT_REQUEST_TTL_SECS,
            audit: Mutex::new(None),
            posted: None,
        }
    }

    /// The same enrollment, calling `posted` after each new request is recorded (a repost of a
    /// request is none): the MDM gate is told of it, so a Mac's nonce is pushed without waiting
    /// for its tick.
    #[must_use]
    pub fn with_posted(mut self, posted: Posted) -> Self {
        self.posted = Some(posted);
        self
    }

    /// The same enrollment, letting at most `max` requests wait for an operator
    /// (`--max-pending-enrollments`); a request over it is refused `pending_full`.
    #[must_use]
    pub fn with_max_pending(mut self, max: usize) -> Self {
        self.store = self.store.with_max_pending(max);
        self
    }

    /// The same enrollment, approving a Mac by its nonce as `mac` says. Without it no Mac is
    /// ([`MacPolicy::default`]).
    #[must_use]
    pub fn with_mac(mut self, mac: MacPolicy) -> Self {
        self.mac = mac;
        self
    }

    /// The enrollment store.
    #[must_use]
    pub fn store(&self) -> &EnrollmentStore {
        &self.store
    }

    /// One look at the server certificate file, at the time the clock reads: it is made again
    /// when it is due ([`ServerFile::keep`]: a third of its validity or less left) or names
    /// other names than the flags (`--server-name`, the address of `--lan-listen`). The reload
    /// loop calls this before it looks at the TLS files, so a new file is served from the same
    /// tick. A look that fails is logged at error level and kept for the status
    /// ([`Enrollment::server_certificate`]) until a later look succeeds; the file in use is left
    /// as it was, and the listener keeps serving it.
    pub fn renew_server(&self) {
        let kept = self
            .server
            .keep(&self.ca, &self.store, self.clock.now_unix());
        let mut state = self
            .server_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match kept {
            Ok(kept) => {
                if kept.renewed {
                    tracing::info!(
                        path = %self.server.path().display(),
                        not_after = kept.not_after,
                        "renewed the coordinator's server certificate"
                    );
                }
                *state = ServerCertificate {
                    not_after: kept.not_after,
                    renewal_error: None,
                };
            }
            Err(why) => {
                tracing::error!(
                    path = %self.server.path().display(),
                    %why,
                    "cannot renew the coordinator's server certificate; the one in use is still served"
                );
                state.renewal_error = Some(why);
            }
        }
    }

    /// The server certificate as the last look at its file found it.
    #[must_use]
    pub fn server_certificate(&self) -> ServerCertificate {
        self.server_state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The CA certificate and its SHA-256.
    #[must_use]
    pub fn ca_bundle(&self) -> &CaBundle {
        self.ca.bundle()
    }

    /// Who a client certificate this CA issued names: the host of its subject `host:<id>`,
    /// with the role the host was approved for. `None` for a certificate the CA did not sign
    /// as a host's, a host the store holds no approval of, a certificate of a key the host id
    /// is no longer approved for (an approval with `replace` gave it to another key), a role
    /// the heartbeat's grammar does not read, or a host file that cannot be read (logged).
    #[must_use]
    pub fn identity_of(&self, der: &[u8]) -> Option<Identity> {
        let certificate = self.ca.bundle().host_certificate(der)?;
        let host = certificate.host_id;
        let role = match self.store.host_role_for(&host, &certificate.key.key_hash()) {
            Ok(role) => role?,
            Err(why) => {
                tracing::error!(%why, host_id = %host, "cannot read an enrolled host");
                return None;
            }
        };
        Some(Identity {
            host_id: host.as_str().parse().ok()?,
            role: role.as_str().parse().ok()?,
        })
    }

    /// Whether the heartbeat of `host` is answered, read for its final report, or refused
    /// (design 6.1: a denied host is refused at once; an update in flight on it still frees
    /// its slot when its record is final). A deny row that cannot be read refuses (logged).
    #[must_use]
    pub fn admission(&self, host: &hostkeeper_wire::HostId) -> Admission {
        store_host(host).map_or(Admission::Admitted, |host| self.admission_of(&host))
    }

    fn admission_of(&self, host: &HostId) -> Admission {
        match (self.store.is_denied(host), self.store.is_retiring(host)) {
            (Ok(false), _) => Admission::Admitted,
            (Ok(true), Ok(true)) => Admission::Retiring,
            (Ok(true), Ok(false)) => Admission::Denied,
            (Err(why), _) | (_, Err(why)) => {
                tracing::error!(%why, host_id = %host, "cannot read a deny row: refused");
                Admission::Denied
            }
        }
    }

    /// Records that the daemon on machine `machine_id` speaks for `host` now
    /// ([`EnrollmentStore::report_live`]): two machines within one window mark the host
    /// duplicate. A host the store never approved (a certificate of the farm CA) is not
    /// tracked.
    pub fn report_live(&self, host: &hostkeeper_wire::HostId, machine_id: &str) {
        let now = self.clock.now_unix();
        if let Some(Err(why)) =
            store_host(host).map(|host| self.store.report_live(&host, machine_id, now))
        {
            tracing::debug!(%why, host_id = %host, "a live report was not recorded");
        }
    }

    /// Records that a heartbeat of `host` was accepted now, by the enrollment's clock: the first
    /// since its approval is kept with the host ([`EnrollmentStore::note_heartbeat`]) and
    /// answered as its `first_heartbeat_unix`. A host the store never approved (a certificate
    /// of the farm CA) is not tracked; a store that cannot be written is logged, and the next
    /// heartbeat tries again.
    pub fn note_heartbeat(&self, host: &hostkeeper_wire::HostId) {
        let now = self.clock.now_unix();
        if let Some(Err(why)) = store_host(host).map(|host| self.store.note_heartbeat(&host, now)) {
            tracing::warn!(%why, host_id = %host, "a first heartbeat was not recorded");
        }
    }

    /// Records that a heartbeat of `host` accepted now, by the enrollment's clock, reported a
    /// service the coordinator manages for the host installed and healthy: the first since its
    /// approval is kept with the host ([`EnrollmentStore::note_serving`]) and answered as its
    /// `first_serving_unix`. A host the store never approved is not tracked; a store that cannot
    /// be written is logged, and the next such heartbeat tries again.
    pub fn note_serving(&self, host: &hostkeeper_wire::HostId) {
        let now = self.clock.now_unix();
        if let Some(Err(why)) = store_host(host).map(|host| self.store.note_serving(&host, now)) {
            tracing::warn!(%why, host_id = %host, "a first serving heartbeat was not recorded");
        }
    }

    /// Whether `host` is marked duplicate (two live daemons on one host id): it is ordered
    /// nothing. A host file that cannot be read counts as duplicate (logged), so a broken file
    /// orders nothing rather than everything.
    #[must_use]
    pub fn is_duplicate(&self, host: &hostkeeper_wire::HostId) -> bool {
        store_host(host).is_some_and(|host| {
            self.store.is_duplicate(&host).unwrap_or_else(|why| {
                tracing::error!(%why, host_id = %host, "cannot read a host: ordered nothing");
                true
            })
        })
    }

    /// Records that the retiring `host` reported its update record final and its slot was
    /// freed: its deny row becomes final, and its heartbeats are refused before they are read.
    pub fn retire(&self, host: &hostkeeper_wire::HostId) {
        if let Some(Err(why)) = store_host(host).map(|host| self.store.retire(&host)) {
            tracing::error!(%why, host_id = %host, "cannot mark a retired host final");
        }
    }

    /// The CA certificate as PEM: the bundle `Poll` and `Renew` return.
    fn ca_pem(&self) -> String {
        pem("CERTIFICATE", self.ca.bundle().certificate_der())
    }
}

#[cfg(test)]
#[path = "tests/mod.rs"]
pub(crate) mod tests;
