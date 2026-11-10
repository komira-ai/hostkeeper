//! `Hosts.Retire` as an operator call: a retired host is refused at its heartbeat and its
//! `Renew`, its own service assignments are dropped, its update slot is freed only once its
//! update is final, the first retirement is audited once, and a retirement cut short by a
//! failed write is finished by asking again.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1::{CoordinatorRefusal, RenewRequest};
use hostkeeper_proto::hostkeeper::operator::v1::{HostsRefusal, RetireRequest};
use hostkeeper_wire::wire::{HEARTBEAT_PATH, ServiceReply};
use hostkeeper_wire::{HostId, RoleName};
use hyper::{Request, StatusCode};

use super::heartbeat::{DONE, UPDATING, beat_json, denied, order, peer_of, slot_coordinator};
use super::support::{
    HostKey, PATIENCE, Rig, audit_after, audit_of, audited, coordinator_over, enrolled, host_id,
    identity, watched,
};
use crate::audit_file::{self, Appender, AuditLog, DEFAULT_ROTATION};
use crate::enrollment::{RpcError, os_random};
use crate::hosts::ACTION_RETIRE;
use crate::http::{Surface, handle};
use crate::ops::AuditEntry;
use crate::service::Coordinator;
use crate::testutil::{OPERATOR, beat, ci_token, ops_of, updating};

/// The retirement of `host`, for `reason`.
fn retirement(host: &str, reason: &str) -> RetireRequest {
    RetireRequest {
        node_id: host.to_owned(),
        reason: reason.to_owned(),
    }
}

/// `Hosts.Retire` of `host` by the operator.
async fn retire(
    coordinator: &Coordinator,
    host: &str,
    reason: &str,
) -> Result<hostkeeper_proto::hostkeeper::operator::v1::RetireResponse, HostsRefusal> {
    let operator = coordinator.caller(Some(OPERATOR)).await;
    coordinator.retire_host(&operator, &retirement(host, reason))
}

/// `kbf` assigned to `host` by the operator's own entry (`Services.Assign` of the host).
fn assign(coordinator: &Coordinator, host: &str) {
    let id: HostId = host.parse().expect("a host id");
    let role: RoleName = "storage".parse().expect("a role");
    coordinator
        .assign_host("operator", &id, Some(&role), &["kbf".to_owned()])
        .expect("assigned");
}

/// Whether the operator's assignments hold an entry of `host`.
fn names(coordinator: &Coordinator, host: &str) -> bool {
    coordinator
        .ops()
        .state()
        .assignments
        .values()
        .any(|entries| {
            entries.hosts.contains_key(host)
                || entries.host_min_versions.contains_key(host)
                || entries.uninstall.contains_key(host)
        })
}

/// The `host_retire` entries of the audit log of `coordinator` after the first `skip`.
fn retirements(coordinator: &Coordinator, skip: usize) -> Vec<super::support::Audited> {
    audit_after(coordinator, skip)
        .into_iter()
        .filter(|(_, action, _, _)| action == ACTION_RETIRE)
        .collect()
}

/// Proves: `Hosts.Retire` of an enrolled host with no update in flight answers its
/// `host_retire` entry and no update in flight; the audit log gains exactly that entry (by the
/// operator's token, about the host, with the operator's reason); the host's own assignment
/// of `kbf` is gone while another host's stays; its deny row is final at once, so its next
/// heartbeat is refused `denied` before its body is read; and its `Renew`, with a certificate
/// that has not expired, is refused `denied`.
///
/// Catches: a retired host still answered or renewing, a deny row left retiring for a host that
/// holds no slot (its heartbeats would be read forever), a retirement that leaves its
/// assignments or drops another host's, and a retirement with no audit entry.
#[tokio::test]
async fn a_retired_host_is_refused_at_handshake_and_renew_and_its_assignments_are_dropped() {
    let enrollment = Rig::new("retire-flow");
    let (key, other) = (HostKey::new(), HostKey::new());
    let certificate = enrolled(&enrollment.enrollment, &key, "machine-1", "s1");
    enrolled(&enrollment.enrollment, &other, "machine-2", "s2");
    let rig = coordinator_over("retire-flow-coordinator", &enrollment).await;
    let c = &rig.coordinator;
    let peer = peer_of(&certificate);
    assert_eq!(beat_json(c, &peer, DONE).await.0, StatusCode::OK);
    assign(c, "s1");
    assign(c, "s2");
    let skip = audit_of(c).len();

    let answer = retire(c, "s1", "board failed").await.expect("retired");
    assert!(!answer.update_in_flight);
    let entry = answer.entry.expect("the entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.node_id.as_deref(),
            entry.reason.as_deref()
        ),
        (ACTION_RETIRE, Some("s1"), Some("board failed"))
    );
    assert_eq!(
        audit_after(c, skip),
        [audited(
            "operator",
            ACTION_RETIRE,
            Some("s1"),
            "board failed"
        )]
    );
    assert!(!names(c, "s1"), "the host's own assignment is gone");
    assert!(names(c, "s2"), "another host's stays");
    let store = enrollment.enrollment.store();
    assert!(store.is_denied(&host_id("s1")).expect("read"));
    assert!(!store.is_retiring(&host_id("s1")).expect("read"), "final");

    let (body, read) = watched();
    let request = Request::post(HEARTBEAT_PATH).body(body).expect("a request");
    let response = handle(c, Surface::Heartbeat, &peer, request, PATIENCE).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        !read.load(Ordering::SeqCst),
        "a retired host's heartbeat was read"
    );
    assert_eq!(
        enrollment.enrollment.renew(
            Some(certificate.as_slice()),
            RenewRequest { csr_der: key.csr() }
        ),
        Err(RpcError::Refused(CoordinatorRefusal::Denied))
    );
}

/// Proves: a host retired while it updates is answered with its update in flight; its
/// heartbeats are refused `denied` from then on, and its slot stays held, so the other storage
/// host is ordered nothing, while it still reports the update; the heartbeat that reports the
/// update final frees the slot (the other host is ordered) and makes the deny row final.
/// Dropping the host's own assignment does not free the slot early.
///
/// Catches: a retirement that frees the slot of an update still running (two live updates),
/// and one that never frees it once the update is final.
#[tokio::test]
async fn a_host_retired_mid_update_keeps_its_slot_until_its_update_is_final() {
    let enrollment = Rig::new("retire-mid-update");
    let (one, two) = (HostKey::new(), HostKey::new());
    let first = enrolled(&enrollment.enrollment, &one, "machine-1", "s1");
    let second = enrolled(&enrollment.enrollment, &two, "machine-2", "s2");
    let rig = slot_coordinator("retire-mid-update-coordinator", &enrollment).await;
    let c = &rig.coordinator;
    let (s1, s2) = (identity(c, &first), identity(c, &second));
    assign(c, "s1");
    let heard =
        |who: &crate::identity::Identity, b| c.heartbeat_native(who.clone(), b).expect("heard");
    assert_eq!(heard(&s1, beat("1.0")).reply, order());
    heard(&s1, updating(beat("1.0"), "2.0"));

    let answer = retire(c, "s1", "").await.expect("retired");
    assert!(answer.update_in_flight, "the update holds its slot");
    let entry = answer.entry.expect("the entry");
    assert_eq!(entry.reason, None, "no reason was given");
    assert!(!names(c, "s1"));
    let s2_told = || heard(&s2, beat("1.0")).reply;
    assert_eq!(s2_told(), ServiceReply::Nothing, "the slot is held");

    let peer = peer_of(&first);
    assert_eq!(beat_json(c, &peer, UPDATING).await, denied());
    assert_eq!(s2_told(), ServiceReply::Nothing, "still updating");
    let store = enrollment.enrollment.store();
    assert!(store.is_retiring(&host_id("s1")).expect("read"));

    assert_eq!(beat_json(c, &peer, DONE).await, denied());
    assert_eq!(s2_told(), order(), "final: the slot is free");
    assert!(!store.is_retiring(&host_id("s1")).expect("read"));
    assert!(store.is_denied(&host_id("s1")).expect("read"));
}

/// Proves: a second `Retire` of a retired host succeeds with no entry, no update in flight, and
/// the audit log still holds one `host_retire`; and `Retire` refuses, auditing and changing
/// nothing: no token (`unauthorized`), CI (`forbidden`), a node id or a reason that breaks its
/// rule (`invalid_request`), a host id no host was enrolled as (`unknown_host`, NOT_FOUND), on a
/// coordinator with enrollment and on one without; and a store whose write fails
/// (`store_failed`).
///
/// Catches: a repeat that audits twice, a retirement reachable without the operator token, an
/// unknown host answered as retired (a deny row for a host id nobody holds), and a failed deny
/// answered as success.
#[tokio::test]
async fn retire_is_audited_once_and_refuses_what_it_cannot_retire() {
    let enrollment = Rig::new("retire-refusals");
    enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let rig = coordinator_over("retire-refusals-coordinator", &enrollment).await;
    let c = &rig.coordinator;
    let skip = audit_of(c).len();
    retire(c, "s1", "lost").await.expect("retired");
    let again = retire(c, "s1", "lost").await.expect("retired again");
    assert_eq!(
        (again.entry, again.update_in_flight),
        (None, false),
        "a repeat is answered as retired"
    );
    assert_eq!(
        retirements(c, skip),
        [audited("operator", ACTION_RETIRE, Some("s1"), "lost")]
    );

    let before = audit_of(c);
    let ci = ci_token();
    for (bearer, refusal) in [
        (None, HostsRefusal::Unauthorized),
        (Some(ci.as_str()), HostsRefusal::Forbidden),
    ] {
        let caller = c.caller(bearer).await;
        assert_eq!(c.retire_host(&caller, &retirement("s9", "")), Err(refusal));
    }
    for (host, reason, refusal) in [
        ("S1!", "", HostsRefusal::InvalidRequest),
        ("s9", "line\nbreak", HostsRefusal::InvalidRequest),
        ("s9", "", HostsRefusal::UnknownHost),
    ] {
        assert_eq!(retire(c, host, reason).await, Err(refusal), "{host}");
    }
    assert_eq!(audit_of(c), before, "nothing is audited");
    let store = enrollment.enrollment.store();
    assert!(
        !store.is_denied(&host_id("s9")).expect("read"),
        "no row for s9"
    );

    let without = crate::testutil::Rig::new("retire-no-enrollment");
    assert_eq!(
        retire(&without.coordinator, "s1", "").await,
        Err(HostsRefusal::UnknownHost)
    );

    let fs = Arc::new(FaultFs::new());
    let faulty = Rig::over("retire-store-fault", fs.clone(), os_random());
    enrolled(&faulty.enrollment, &HostKey::new(), "machine-1", "s1");
    let rig = coordinator_over("retire-store-fault-coordinator", &faulty).await;
    fs.fail_at(fs.count() + 1);
    assert_eq!(
        retire(&rig.coordinator, "s1", "").await,
        Err(HostsRefusal::StoreFailed)
    );
    assert!(
        !faulty
            .enrollment
            .store()
            .is_denied(&host_id("s1"))
            .expect("read"),
        "nothing was denied"
    );
}

/// An appender that fails with a full disk while `down` is set, and otherwise appends to the
/// audit file of the ops file `ops`.
fn switchable(ops: &std::path::Path, down: &Arc<AtomicBool>) -> Appender {
    let log = AuditLog::open(audit_file::path_of(ops), DEFAULT_ROTATION).expect("opened");
    let down = Arc::clone(down);
    Box::new(move |line| {
        if down.load(Ordering::SeqCst) {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        log.append(line)
    })
}

/// Proves: a `Retire` whose audit line cannot be appended is refused `audit_unavailable`, with
/// the host denied (the deny row is written first) and its assignment kept, unaudited; asking
/// again once the file takes lines drops the assignment and audits the retirement, once. The
/// same holds for a host with no assignment at all, whose repeat finds nothing to drop: it is
/// audited once, and a third call audits nothing.
///
/// Catches: a retirement cut short that a repeat answers as done, leaving the host's
/// assignments or no audit entry (whether or not the host had assignments), and an assignment
/// dropped with no audit line.
#[tokio::test]
async fn a_retirement_whose_audit_fails_is_finished_by_asking_again() {
    let enrollment = Rig::new("retire-audit-fault");
    enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    enrolled(&enrollment.enrollment, &HostKey::new(), "machine-2", "s2");
    let mut rig = coordinator_over("retire-audit-fault-coordinator", &enrollment).await;
    assign(&rig.coordinator, "s1");
    let down = Arc::new(AtomicBool::new(true));
    let ops =
        ops_of(&rig.dir).with_append(switchable(&rig.dir.path().join("desired.json.ops"), &down));
    rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_ops(ops)
        .with_enrollment(Arc::clone(&enrollment.enrollment));
    let c = &rig.coordinator;
    let skip = audit_of(c).len();

    assert_eq!(
        retire(c, "s1", "lost").await,
        Err(HostsRefusal::AuditUnavailable)
    );
    let store = enrollment.enrollment.store();
    assert!(
        store.is_denied(&host_id("s1")).expect("read"),
        "denied first"
    );
    assert!(names(c, "s1"), "the assignment is kept");
    assert_eq!(
        retire(c, "s2", "dead").await,
        Err(HostsRefusal::AuditUnavailable)
    );
    assert!(!names(c, "s2"), "s2 has no assignment");
    assert_eq!(retirements(c, skip), []);

    down.store(false, Ordering::SeqCst);
    let finished = retire(c, "s1", "lost").await.expect("retired");
    let entry: Option<AuditEntry> = c.ops().audit_log().expect("the log").pop();
    assert_eq!(
        finished.entry.map(|e| e.seq),
        entry.map(|e| e.seq),
        "the answer is the entry written"
    );
    assert!(!names(c, "s1"), "the assignment is dropped");
    let s2 = retire(c, "s2", "dead").await.expect("retired");
    assert!(
        s2.entry.is_some(),
        "a host with no assignment is audited too"
    );
    let third = retire(c, "s2", "dead").await.expect("retired");
    assert_eq!(third.entry, None, "and only once");
    assert_eq!(
        retirements(c, skip),
        [
            audited("operator", ACTION_RETIRE, Some("s1"), "lost"),
            audited("operator", ACTION_RETIRE, Some("s2"), "dead"),
        ]
    );
}
