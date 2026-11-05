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
//!   coordinator's server certificate.
//!
//! The heartbeat path asks [`Enrollment::identity_of`] who a certificate the CA issued names,
//! [`Enrollment::admission`] whether its host is denied, retiring or neither,
//! [`Enrollment::report_live`] which machine speaks for it, and [`Enrollment::is_duplicate`]
//! whether it is ordered nothing ([`crate::service::Coordinator`]).

mod nonce;
mod refusal;
mod rpc;
mod setup;

use std::sync::Arc;

use hostkeeper_enrollment::ca::{Ca, CaBundle, Signer};
use hostkeeper_enrollment::enroll::EnrollmentStore;
use hostkeeper_enrollment::names::HostId;
use hostkeeper_enrollment::validity::UnixClock;
pub use hostkeeper_proto::hostkeeper::agent::v1::CoordinatorRefusal;

pub use nonce::{NONCE_BYTES, NONCES_PER_REQUEST, Nonces, Random, os_random};
pub use refusal::{RpcError, refusal_of};
pub use rpc::{MAX_MESSAGE_BYTES, Method, call};
pub use setup::{CA_CERT_SUFFIX, Setup, SetupError, ca_certificate_path, pem, server_due, setup};

use crate::identity::Identity;

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

/// The coordinator's enrollment: its store, its CA, its wall clock and its poll nonces.
pub struct Enrollment {
    store: EnrollmentStore,
    ca: Ca,
    clock: Arc<dyn UnixClock>,
    nonces: Nonces,
}

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
    /// poll nonces drawn from `random`.
    #[must_use]
    pub fn new(store: EnrollmentStore, ca: Ca, clock: Arc<dyn UnixClock>, random: Random) -> Self {
        Self {
            store,
            ca,
            clock,
            nonces: Nonces::new(random),
        }
    }

    /// The enrollment store.
    #[must_use]
    pub fn store(&self) -> &EnrollmentStore {
        &self.store
    }

    /// The CA certificate and its SHA-256.
    #[must_use]
    pub fn ca_bundle(&self) -> &CaBundle {
        self.ca.bundle()
    }

    /// Who a client certificate this CA issued names: the host of its subject `host:<id>`,
    /// with the role the host was approved for. `None` for a certificate the CA did not sign
    /// as a host's, a host the store holds no approval of, a role the heartbeat's grammar does
    /// not read, or a host file that cannot be read (logged).
    #[must_use]
    pub fn identity_of(&self, der: &[u8]) -> Option<Identity> {
        let host = self.ca.bundle().host_certificate(der)?.host_id;
        let role = match self.store.host_role(&host) {
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
