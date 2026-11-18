//! What a host is sent of the SSH access, and what its daemon reported of it (feature
//! [`FEATURE`], `ssh-v1`; docs/design/ssh-through-coordinator.md, sections 9 and 10).
//!
//! # What a host is sent
//!
//! The answer to a heartbeat over gRPC from a daemon that announces [`FEATURE`] carries an
//! `SshAccess` ([`Coordinator::ssh_access_for`]) when this coordinator advertises the feature and
//! holds an SSH user CA (`--ssh-ca-secret-file`), the host is enrolled, and the ledger could be
//! read. It holds:
//!
//! - the CA public keys of the host's class only: a fleet host the keys of the CA secret, the
//!   current one and, while a rotation runs, the next; a control-plane host the key of
//!   `--ssh-cp-ca-public-file`, or, without it, the public half of `--ssh-cp-ca-secret-file`; a
//!   control-plane host the coordinator holds neither for is sent `enabled = false` and no key,
//!   which makes its daemon revert what it set up;
//! - the principal `hk-host-<host id>`, the account that takes certificates (the Linux or the Mac
//!   one, by the host's enrollment), and `--ssh-password-auth`;
//! - the serials of the host's revoked certificates that have not expired
//!   ([`crate::ssh_ledger::Ledger::revoked_serials`]).
//!
//! Its `generation` is a number made from every other field ([`generation`]), so it changes at any
//! change of them and is the same on every instance of the coordinator and after a restart. It is
//! sent in every answer until the host's last report says it applied or refused that generation
//! (`SshReport.applied_generation`). A heartbeat that arrives while the ledger cannot be read is
//! answered with nothing for the access: the daemon keeps what it has and the next heartbeat
//! tries again.
//!
//! # What a host reported
//!
//! The last [`agent::SshReport`] of each daemon that announces [`FEATURE`] is kept, in memory only:
//! after a restart the coordinator holds none until the next heartbeat of each host, which
//! carries it again. A report whose `state` is over 32 bytes, that lists more than 8 host key
//! fingerprints or one that is not `SHA256:` and 43 base64 characters is dropped, counted and
//! logged as a dropped fact, and the report read before is kept; a `state` outside the closed
//! set is kept as `other` and a `detail` over 128 bytes is cut at a character boundary.
//! `Hosts.List` and `Hosts.Get` show the report in the host's heartbeat
//! ([`Coordinator::show_ssh_report`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::enroll::EnrolledHost;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_wire::HostId;
use sha2::{Digest, Sha256};

use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::repository_push::cut;
use crate::service::Coordinator;
use crate::ssh_access::fingerprint_ok;
use crate::ssh_cert::CaKey;
use crate::ssh_operators::Class;
use crate::ssh_sign::{Signing, class_of, principal};

/// The feature a daemon announces to be sent the SSH access and to report it; the coordinator
/// advertises it ([`crate::features::FEATURES`]).
pub const FEATURE: &str = "ssh-v1";

/// The name an SSH report that breaks a rule is counted under.
pub(crate) const FACT: &str = "ssh";

/// The most bytes of a report's `state`.
const MAX_STATE: usize = 32;

/// The most bytes of a report's `detail`.
const MAX_DETAIL: usize = 128;

/// The most host key fingerprints of a report.
const MAX_FINGERPRINTS: usize = 8;

/// The states of a report: the closed set of `SshReport.state`.
const STATES: &[&str] = &[
    "applied",
    "disabled",
    "needs_full_disk_access",
    "blocked_by_allow_list",
    "sshd_missing",
    "no_include",
    "config_invalid",
    "reload_failed",
    "would_lock_out",
    "no_login_user",
    "cert_login_not_possible",
    "shadowed_by_stock",
    "blocked_by_access_group",
    "unsupported",
];

/// The last report of each host that announced [`FEATURE`], under a lock of its own that holds
/// nothing else.
#[derive(Debug, Default)]
pub(crate) struct SshReports {
    seen: Mutex<BTreeMap<HostId, agent::SshReport>>,
}

impl SshReports {
    /// The last report of the host `id`; none for a host that never announced the feature.
    pub(crate) fn report_of(&self, id: &HostId) -> Option<agent::SshReport> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Notes `report` as the host `id`'s last.
    fn note(&self, id: &HostId, report: agent::SshReport) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone(), report);
    }

    /// Forgets the host `id`'s report, when the host is retired.
    pub(crate) fn forget(&self, id: &HostId) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
}

/// The report `said` holds, or `None` when it breaks a rule: a `state` over [`MAX_STATE`] bytes,
/// more than [`MAX_FINGERPRINTS`] fingerprints, or one that is not `SHA256:` and 43 base64
/// characters. A `state` outside [`STATES`] is kept as `other`, and the `detail` is cut to
/// [`MAX_DETAIL`] bytes at a character boundary.
fn kept(said: &agent::SshReport) -> Option<agent::SshReport> {
    if said.state.len() > MAX_STATE
        || said.host_key_fingerprints.len() > MAX_FINGERPRINTS
        || !said.host_key_fingerprints.iter().all(|f| fingerprint_ok(f))
    {
        return None;
    }
    let state = if STATES.contains(&said.state.as_str()) {
        said.state.clone()
    } else {
        "other".to_owned()
    };
    Some(agent::SshReport {
        applied_generation: said.applied_generation,
        state,
        host_key_fingerprints: said.host_key_fingerprints.clone(),
        detail: cut(&said.detail, MAX_DETAIL).to_owned(),
    })
}

/// Whether a heartbeat over gRPC announces [`FEATURE`].
fn announces(request: &agent::HeartbeatRequest) -> bool {
    request.features.iter().any(|f| f == FEATURE)
}

/// Appends `bytes` to `out` with their length in eight big-endian bytes in front.
fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// The generation of the access `access` (whatever its own `generation` says): the first eight
/// bytes, big-endian, of the SHA-256 of a canonical encoding of every other field, each text
/// and each count preceded by its length in eight big-endian bytes (`enabled` is one byte, a
/// serial eight big-endian bytes), with 0, which a daemon that has seen none reports, made 1.
/// Nothing in it depends on the instance, the clock or the restart.
pub(crate) fn generation(access: &agent::SshAccess) -> u64 {
    let mut bytes = vec![u8::from(access.enabled)];
    bytes.extend_from_slice(&(access.ca_public_keys.len() as u64).to_be_bytes());
    for key in &access.ca_public_keys {
        put(&mut bytes, key.as_bytes());
    }
    put(&mut bytes, access.login_user.as_bytes());
    put(&mut bytes, access.principal.as_bytes());
    put(&mut bytes, access.password_auth.as_bytes());
    bytes.extend_from_slice(&(access.revoked_serials.len() as u64).to_be_bytes());
    for serial in &access.revoked_serials {
        bytes.extend_from_slice(&serial.to_be_bytes());
    }
    let digest = Sha256::digest(&bytes);
    let mut first = [0; 8];
    first.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first).max(1)
}

/// The CA public keys a host of `class` is told to trust: none when the coordinator holds no key
/// for the class.
fn ca_public_keys(ssh: &Signing, class: Class) -> Vec<String> {
    match class {
        Class::Fleet => ssh.fleet.iter().map(CaKey::public_line).collect(),
        Class::ControlPlane => ssh
            .control_plane_public
            .clone()
            .or_else(|| ssh.control_plane.as_ref().map(CaKey::public_line))
            .into_iter()
            .collect(),
    }
}

/// The access of the host `id`, enrolled as `host`, whose unexpired revoked serials are `revoked`.
fn access_of(ssh: &Signing, id: &str, host: &EnrolledHost, revoked: Vec<u64>) -> agent::SshAccess {
    let ca_public_keys = ca_public_keys(ssh, class_of(ssh, id));
    let mut access = agent::SshAccess {
        generation: 0,
        enabled: !ca_public_keys.is_empty(),
        ca_public_keys,
        login_user: ssh.login_for(host).to_owned(),
        principal: principal(id),
        password_auth: ssh.password_auth.as_str().to_owned(),
        revoked_serials: revoked,
    };
    access.generation = generation(&access);
    access
}

impl Coordinator {
    /// Notes what a heartbeat over gRPC from `who` said of the SSH access, when its daemon
    /// announced [`FEATURE`]: the report, or the empty one when it sent none; a report that breaks
    /// a rule is counted and logged as a dropped fact, and the one read before is kept.
    pub(crate) fn hear_ssh_report(&self, who: &Identity, request: &agent::HeartbeatRequest) {
        if !announces(request) {
            return;
        }
        let report = match &request.ssh {
            None => Some(agent::SshReport::default()),
            Some(said) => kept(said),
        };
        match report {
            Some(report) => self.ssh_reports.note(&who.host_id, report),
            None => {
                self.note_extras_dropped(who, "the ssh report breaks a rule", &[FACT]);
            }
        }
    }

    /// The SSH access the answer to a heartbeat over gRPC from `who` carries, if it carries it:
    /// when the daemon announced [`FEATURE`], this coordinator advertises it and holds an SSH user
    /// CA, the host is enrolled and the ledger could be read, and the host's last report does not
    /// hold the access' generation. A host that is not enrolled, whose record cannot be read, or
    /// when the ledger cannot be read, is sent none, and the reason is logged as a warning: the
    /// next heartbeat tries again.
    pub(crate) async fn ssh_access_for(
        &self,
        who: &Identity,
        request: &agent::HeartbeatRequest,
    ) -> Option<agent::SshAccess> {
        if !announces(request) || !self.features.iter().any(|f| f == FEATURE) {
            return None;
        }
        let ssh = self.ssh.as_ref()?;
        let id = who.host_id.as_str();
        let host = match self.enrolled_host(id) {
            Ok(Some(host)) => host,
            Ok(None) => {
                tracing::warn!(host_id = %id, "the ssh access is not sent: the host is not enrolled");
                return None;
            }
            Err(why) => {
                tracing::warn!(host_id = %id, %why, "the ssh access is not sent: the host cannot be read");
                return None;
            }
        };
        let (ledger, owned) = (Arc::clone(&ssh.ledger), id.to_owned());
        let revoked = match tokio::task::spawn_blocking(move || ledger.revoked_serials(&owned))
            .await
        {
            Ok(Ok(revoked)) => revoked,
            Ok(Err(why)) => {
                tracing::warn!(host_id = %id, %why, "the ssh access is not sent: the revocations cannot be read");
                return None;
            }
            Err(why) => {
                tracing::warn!(host_id = %id, %why, "the ssh access is not sent: reading the revocations failed");
                return None;
            }
        };
        let access = access_of(ssh, id, &host, revoked);
        let held = self.ssh_reports.report_of(&who.host_id).unwrap_or_default();
        (held.applied_generation != access.generation).then_some(access)
    }

    /// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id` (`fleet` is the
    /// fleet as they read it), with the last SSH report of its daemon, in its heartbeat, when the
    /// daemon announced [`FEATURE`].
    pub(crate) fn show_ssh_report(&self, fleet: &Fleet, id: &HostId, host: &mut operator::Host) {
        if !fleet.announced(id, FEATURE) {
            return;
        }
        if let Some(report) = self.ssh_reports.report_of(id) {
            host.heartbeat.get_or_insert_default().ssh = Some(report);
        }
    }
}

#[cfg(test)]
#[path = "ssh_heartbeat_tests.rs"]
pub(crate) mod tests;
