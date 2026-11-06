//! `Releases.UnmarkBad`: the inverse of `Releases.MarkBad`, audited under the operator, with
//! the failure reports of the version acked in the same write, and each refusal leaving the
//! ops state as it was.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Class, Heartbeat, Outcome, Phase, ServiceReply, UpdateResult};

use crate::marks_support::{
    NEW_DAEMON, WEB, ask, beat_of, operator, ops_bytes, rig, runs, set, start, update,
};
use crate::operator::Caller;
use crate::ops::{AuditEntry, FailureAck, OpsState, ServiceOps, Write};
use crate::rollback_tests::unwritable;
use crate::testutil::{Rig, ops_of, version};

/// A heartbeat of a new daemon running `running` of `web`.
fn daemon(running: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, running)])
}

/// A daemon that runs `web` 1.0 after its update to `target` failed in `phase`, blamed on
/// `class`.
fn failed(target: &str, phase: Phase, class: Option<Class>) -> Heartbeat {
    let mut one = runs(WEB, "1.0");
    one.last_update = Some(report(target, phase, class));
    beat_of(NEW_DAEMON, vec![one])
}

/// A failed update to `target` in `phase`, blamed on `class`.
fn report(target: &str, phase: Phase, class: Option<Class>) -> UpdateResult {
    UpdateResult {
        target: version(target),
        outcome: Outcome::Failed,
        phase: Some(phase),
        class,
        reason: None,
        order_id: None,
        order_try: None,
    }
}

fn mark_of(service: &str, version: &str) -> proto::MarkBadRequest {
    proto::MarkBadRequest {
        service: service.to_owned(),
        version: version.to_owned(),
        reason: "crashes on start".to_owned(),
    }
}

fn unmark_of(service: &str, version: &str) -> proto::UnmarkBadRequest {
    proto::UnmarkBadRequest {
        service: service.to_owned(),
        version: version.to_owned(),
        reason: "the crash was the disk".to_owned(),
    }
}

/// The actions of the audit file, oldest first.
fn actions(rig: &Rig) -> Vec<String> {
    rig.coordinator
        .ops
        .audit_log()
        .expect("the audit file")
        .into_iter()
        .map(|entry| entry.action)
        .collect()
}

/// Proves: `MarkBad` followed by `UnmarkBad` leaves the marks of the service as they were
/// before the mark, the version is ordered again and CI may set it, and the lift is one audit
/// entry `unmark_bad` under the operator whose reason is the version and the operator's reason.
///
/// Catches: an unmark that leaves the version in `bad`, that is audited as another action or
/// under another actor, or that drops the operator's reason.
#[tokio::test]
async fn unmark_bad_undoes_mark_bad_and_is_audited() {
    let rig = rig("unmark-inverse", "");
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    set(&rig, WEB, "2.0", 1).await;
    let before = ops_of(&rig.dir).marks(WEB);

    rig.coordinator
        .mark_bad(&operator(), mark_of(WEB, "2.0"))
        .expect("marked");
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    let done = rig
        .coordinator
        .unmark_bad(&operator(), unmark_of(WEB, "2.0"))
        .expect("unmarked");

    assert_eq!(ops_of(&rig.dir).marks(WEB), before, "on disk");
    assert_eq!(rig.coordinator.ops.marks(WEB), before, "in memory");
    assert_eq!(
        done.entry,
        Some(proto::AuditEntry {
            seq: done.entry.as_ref().map_or(0, |entry| entry.seq),
            by: proto::Actor::Operator.into(),
            action: "unmark_bad".to_owned(),
            service: Some(WEB.to_owned()),
            node_id: None,
            reason: Some("2.0: the crash was the disk".to_owned()),
            at_unix: None,
            by_name: Some("operator".to_owned()),
        })
    );
    assert_eq!(
        actions(&rig).iter().rev().take(2).collect::<Vec<_>>(),
        ["unmark_bad", "mark_bad"]
    );
    assert_eq!(ask(&rig, "d1", "daemon", daemon("1.0")), update(WEB, "2.0"));
    set(&rig, WEB, "2.0", 1).await;
}

/// Proves: the lift acks, in the same write, the release failure of the version each host of
/// the service stands on (and only that: a host that failed for its environment, one that
/// failed before the commit, and one that failed another version are not acked), lifts the
/// mark a force left as well, replaces a host's older ack, and keeps the other versions' marks.
///
/// Catches: an override the next heartbeat undoes because the failures it overrode count
/// again, an ack of a report that would never count, or a force's mark left behind.
#[tokio::test]
async fn unmark_bad_acks_the_release_failures_of_the_version_and_lifts_a_force_mark() {
    let rig = rig("unmark-acks", "");
    let release = report("2.0", Phase::AfterCommit, Some(Class::Release));
    let beats = [
        (
            "d1",
            failed("2.0", Phase::AfterCommit, Some(Class::Release)),
        ),
        ("d2", failed("2.0", Phase::GateRelease, None)),
        (
            "d3",
            failed("2.0", Phase::AfterCommit, Some(Class::Environment)),
        ),
        ("d4", failed("2.0", Phase::BeforeCommit, None)),
        ("d5", failed("3.0", Phase::AfterCommit, None)),
        ("d6", daemon("1.0")),
    ];
    for (host, beat) in beats {
        ask(&rig, host, "daemon", beat);
    }
    let older = FailureAck {
        node_id: "d1".to_owned(),
        report: report("1.5", Phase::AfterCommit, None),
    };
    rig.coordinator
        .ops
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "seed"),
            |state| {
                let marks = state.services.entry(WEB.to_owned()).or_default();
                marks.bad = vec![version("2.0"), version("3.0")];
                marks.bad_by_force = vec![version("2.0")];
                marks.failure_acked = vec![older];
            },
        )
        .expect("seeded");

    rig.coordinator
        .unmark_bad(&operator(), unmark_of(WEB, "2.0"))
        .expect("unmarked");

    let expected = ServiceOps {
        bad: vec![version("3.0")],
        failure_acked: vec![
            FailureAck {
                node_id: "d1".to_owned(),
                report: release,
            },
            FailureAck {
                node_id: "d2".to_owned(),
                report: report("2.0", Phase::GateRelease, None),
            },
        ],
        ..ServiceOps::default()
    };
    assert_eq!(ops_of(&rig.dir).marks(WEB), expected);
}

/// Proves: each refusal of `UnmarkBad` is the value its cause names and writes nothing: no
/// caller `unauthorized`, CI `forbidden`, a service it does not manage `unknown_service`, a
/// version that is no version or a blank reason `invalid_request`, a version that is not
/// marked bad (never marked, or unmarked already) `not_marked_bad`.
///
/// Catches: a refusal that still lifts a mark or writes an audit entry, or an unmark of a
/// version that was never marked that answers success.
#[test]
fn unmark_bad_refuses_with_the_value_of_its_cause_and_changes_nothing() {
    let rig = rig("unmark-refusals", "");
    rig.coordinator
        .mark_bad(&operator(), mark_of(WEB, "2.0"))
        .expect("marked");
    let marked = ops_bytes(&rig);
    let audited = actions(&rig);
    let mut blank = unmark_of(WEB, "2.0");
    blank.reason = "   ".to_owned();
    let refusals = [
        (
            Caller::Anonymous,
            unmark_of(WEB, "2.0"),
            proto::ReleasesRefusal::Unauthorized,
        ),
        (
            Caller::Ci,
            unmark_of(WEB, "2.0"),
            proto::ReleasesRefusal::Forbidden,
        ),
        (
            operator(),
            unmark_of("nope", "2.0"),
            proto::ReleasesRefusal::UnknownService,
        ),
        (
            operator(),
            unmark_of(WEB, "not a version"),
            proto::ReleasesRefusal::InvalidRequest,
        ),
        (operator(), blank, proto::ReleasesRefusal::InvalidRequest),
        (
            operator(),
            unmark_of(WEB, "2.1"),
            proto::ReleasesRefusal::NotMarkedBad,
        ),
    ];
    for (caller, request, refusal) in refusals {
        let said = format!("{caller:?} {request:?}");
        assert_eq!(
            rig.coordinator.unmark_bad(&caller, request),
            Err(refusal),
            "{said}"
        );
        assert_eq!(ops_bytes(&rig), marked, "{said}");
        assert_eq!(actions(&rig), audited, "{said}");
    }

    rig.coordinator
        .unmark_bad(&operator(), unmark_of(WEB, "2.0"))
        .expect("unmarked");
    let unmarked = ops_bytes(&rig);
    assert_eq!(
        rig.coordinator
            .unmark_bad(&operator(), unmark_of(WEB, "2.0")),
        Err(proto::ReleasesRefusal::NotMarkedBad),
        "a second unmark"
    );
    assert_eq!(ops_bytes(&rig), unmarked);
}

/// Proves: an ops file that cannot be written is `store_failed`, and the mark stands in memory.
///
/// Catches: a lift that reaches memory before the file, so a restart brings back a mark the
/// coordinator had stopped holding.
#[test]
fn unmark_bad_whose_write_fails_is_store_failed_and_keeps_the_mark() {
    let mut rig = rig("unmark-store", "");
    let mut state = OpsState::default();
    state.services.entry(WEB.to_owned()).or_default().bad = vec![version("2.0")];
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(state));
    assert_eq!(
        rig.coordinator
            .unmark_bad(&operator(), unmark_of(WEB, "2.0")),
        Err(proto::ReleasesRefusal::StoreFailed)
    );
    assert_eq!(rig.coordinator.ops.marks(WEB).bad, vec![version("2.0")]);
}
