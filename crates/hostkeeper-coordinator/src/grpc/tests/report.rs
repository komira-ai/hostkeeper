//! The mapping of the coordinator's status and convergence to `hostkeeper.operator.v1`
//! (`crate::grpc::report`), driven with report values built directly: no coordinator, no socket.

use std::collections::{BTreeMap, HashMap};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::hold_scope::Scope;
use hostkeeper_wire::wire::{
    Class, CommitState, DeferredReport, Health, Heartbeat, OrderId, Outcome, Phase, StepName,
    StepRun, Token, UpdateResult,
};

use crate::converge::{Convergence, HaltNote, RoleCounts, State};
use crate::decide::{Gate, Skip, Why};
use crate::desired::{Desired, RoleLimits};
use crate::force::{ForceRecord, ForceTargets};
use crate::grpc::report::{convergence as map_convergence, status as map_status};
use crate::holds::{HoldMark, HoldScope};
use crate::membership::{HostCounts, Member, Membership};
use crate::ops::{AuditEntry, FailureAck, Halt, HaltAck, HaltOrigin, OpsState, ServiceOps};
use crate::registry::Digest;
use crate::status::{AgentStatus, HostStatus, ServiceHostStatus, ServiceStatus, Status};
use crate::testutil::{beat, version};

const COMMIT: &str = "c0ffee";

fn conv(state: State) -> Convergence {
    Convergence {
        desired: None,
        quiet: false,
        state,
        roles: Vec::new(),
        halt: None,
        hosts: None,
        commit: COMMIT,
    }
}

fn svc_host(node: &str, role: &str, b: Heartbeat) -> ServiceHostStatus {
    ServiceHostStatus {
        host: HostStatus {
            node_id: node.to_owned(),
            agent: AgentStatus {
                name: "hostkeeper-agent".to_owned(),
                version: None,
            },
            role: role.to_owned(),
            live: true,
            up: true,
            holds_slot: false,
            silent_secs: 0,
            beat: b,
        },
        hold: None,
        held_on_bad_version: false,
    }
}

fn svc(hosts: Vec<ServiceHostStatus>) -> ServiceStatus {
    ServiceStatus {
        desired: None,
        roles: Vec::new(),
        hosts,
        members: Vec::new(),
        up: 0,
        slots_held: 0,
        convergence: conv(State::Converged),
    }
}

fn status_of(services: Vec<(&str, ServiceStatus)>, ops: OpsState) -> Status {
    Status {
        desired: None,
        services: BTreeMap::new(),
        quiet: false,
        services_dropped: 0,
        extras_dropped: BTreeMap::new(),
        hosts: Vec::new(),
        per_service: services
            .into_iter()
            .map(|(name, one)| (name.to_owned(), one))
            .collect(),
        ops,
        commit: COMMIT,
    }
}

/// The one service `web` of a status, mapped.
fn mapped(one: ServiceStatus, ops: OpsState) -> proto::ServiceStatus {
    let mut out = map_status(&status_of(vec![("web", one)], ops), "");
    assert_eq!(out.services.len(), 1);
    out.services.remove(0)
}

/// The one host of the service `web`, mapped, with `members` the service does not count.
fn mapped_host(host: ServiceHostStatus, members: Vec<Member>, ops: OpsState) -> proto::ServiceHost {
    let mut one = svc(vec![host]);
    one.members = members;
    let mut out = mapped(one, ops);
    assert_eq!(out.hosts.len(), 1);
    out.hosts.remove(0)
}

fn plain_host(b: Heartbeat) -> proto::ServiceHost {
    mapped_host(
        svc_host("d1", "storage", b),
        Vec::new(),
        OpsState::default(),
    )
}

fn ops_with(name: &str, marks: ServiceOps) -> OpsState {
    let mut ops = OpsState::default();
    ops.services.insert(name.to_owned(), marks);
    ops
}

/// A failed verdict with every optional fact set.
fn full_update() -> UpdateResult {
    UpdateResult {
        target: version("3.0"),
        outcome: Outcome::Failed,
        phase: Some(Phase::AfterCommit),
        class: Some(Class::Release),
        reason: Token::parse("drain_bound"),
        order_id: OrderId::parse("ord-7"),
        order_try: Some(2),
    }
}

fn full_update_proto() -> agent::UpdateResult {
    agent::UpdateResult {
        target: "3.0".to_owned(),
        outcome: agent::Outcome::Failed as i32,
        phase: agent::FailurePhase::AfterCommit as i32,
        class: agent::FailureClass::Release as i32,
        reason: "drain_bound".to_owned(),
        order_try: Some(2),
        order_id: Some("ord-7".to_owned()),
        ..Default::default()
    }
}

fn bare_update(reason: Option<&str>) -> UpdateResult {
    let mut r = UpdateResult::new(version("2.5"), Outcome::Succeeded);
    r.reason = reason.and_then(Token::parse);
    r
}

fn hold(service: &str, scope: HoldScope) -> HoldMark {
    HoldMark {
        service: service.to_owned(),
        scope,
        reason: "freeze".to_owned(),
        by: "alice".to_owned(),
        at_unix: 11,
    }
}

fn hold_proto(scope: Scope) -> proto::HoldMark {
    proto::HoldMark {
        scope: Some(proto::HoldScope { scope: Some(scope) }),
        service: "web".to_owned(),
        reason: "freeze".to_owned(),
        set_by: "alice".to_owned(),
        set_unix: 11,
    }
}

/// An empty filter lists every service in name order, and the status-wide facts pass through.
#[test]
fn status_lists_every_service_in_name_order_with_the_top_level_facts() {
    let mut audit = AuditEntry::new("alice", "hold");
    audit.seq = 4;
    audit.service = Some("web".to_owned());
    audit.node_id = Some("d1".to_owned());
    audit.reason = Some("freeze".to_owned());
    let mut ops = OpsState::default();
    ops.audit.push(audit);
    let mut s = status_of(vec![("zeta", svc(vec![])), ("alpha", svc(vec![]))], ops);
    s.quiet = true;
    s.services_dropped = 7;
    s.extras_dropped = BTreeMap::from([("step", 2), ("commit", 1)]);

    let out = map_status(&s, "");

    let names: Vec<&str> = out.services.iter().map(|s| s.service.as_str()).collect();
    assert_eq!(names, ["alpha", "zeta"]);
    assert!(out.quiet);
    assert_eq!(out.services_dropped, 7);
    assert_eq!(
        out.facts_dropped,
        HashMap::from([("step".to_owned(), 2), ("commit".to_owned(), 1)])
    );
    assert_eq!(
        out.audit,
        [proto::AuditEntry {
            seq: 4,
            by: proto::Actor::Operator as i32,
            action: "hold".to_owned(),
            service: Some("web".to_owned()),
            node_id: Some("d1".to_owned()),
            reason: Some("freeze".to_owned()),
        }]
    );
}

/// A coordinator with nothing dropped and no audit answers empty maps and no entries.
#[test]
fn status_with_nothing_dropped_has_no_facts_and_no_audit() {
    let out = map_status(&status_of(vec![], OpsState::default()), "");

    assert_eq!(out, proto::StatusResponse::default());
}

/// A filter that names a service answers that service alone.
#[test]
fn status_filter_keeps_only_the_named_service() {
    let s = status_of(
        vec![("alpha", svc(vec![])), ("zeta", svc(vec![]))],
        OpsState::default(),
    );

    let out = map_status(&s, "zeta");

    let names: Vec<&str> = out.services.iter().map(|s| s.service.as_str()).collect();
    assert_eq!(names, ["zeta"]);
}

/// A filter that names no service the coordinator manages answers no service.
#[test]
fn status_filter_naming_no_service_answers_none() {
    let s = status_of(vec![("alpha", svc(vec![]))], OpsState::default());

    let out = map_status(&s, "nope");

    assert!(out.services.is_empty());
}

/// A service with no desired state, no ops entry and no hosts has none of those in the answer.
#[test]
fn service_without_desired_or_marks_leaves_them_unset() {
    let out = mapped(svc(vec![]), OpsState::default());

    assert_eq!(out.service, "web");
    assert_eq!(out.desired, None);
    assert_eq!(out.marks, None);
    assert!(out.hosts.is_empty());
    let c = out.convergence.expect("convergence");
    assert_eq!(c.force, None);
    assert_eq!(c.record, None);
}

/// A service's counts pass through and a count above `u32::MAX` is the largest `uint32`.
#[test]
fn service_counts_pass_through_and_saturate_at_the_largest_uint32() {
    let mut one = svc(vec![]);
    one.up = usize::MAX;
    one.slots_held = 7;

    let out = mapped(one, OpsState::default());

    assert_eq!(out.up, u32::MAX);
    assert_eq!(out.slots_held, 7);
}

/// The desired state keeps its version, limits per role and package digest.
#[test]
fn desired_state_carries_version_limits_and_digest() {
    let mut one = svc(vec![]);
    one.desired = Some(Desired {
        version: version("3.0"),
        max_updating: RoleLimits::max_updating_default(),
        min_up: RoleLimits::from([("storage", 3), ("frontend", 2)]),
        release_digest: Some(Digest::parse(&"a".repeat(64)).expect("a digest")),
    });

    let desired = mapped(one, OpsState::default()).desired.expect("desired");

    assert_eq!(
        desired,
        proto::DesiredState {
            version: "3.0".to_owned(),
            max_updating: HashMap::from([
                ("frontend".to_owned(), 1),
                ("storage".to_owned(), 1),
                ("daemon".to_owned(), 2),
                ("mac-daemon".to_owned(), 2),
            ]),
            min_up: HashMap::from([("storage".to_owned(), 3), ("frontend".to_owned(), 2)]),
            package_digest: Some("a".repeat(64)),
        }
    );
}

/// A desired state with no digest and no limits has no package digest and empty maps.
#[test]
fn desired_state_without_digest_or_limits_leaves_them_empty() {
    let mut one = svc(vec![]);
    one.desired = Some(Desired {
        version: version("3.0"),
        max_updating: RoleLimits::default(),
        min_up: RoleLimits::default(),
        release_digest: None,
    });

    let desired = mapped(one, OpsState::default()).desired.expect("desired");

    assert_eq!(
        desired,
        proto::DesiredState {
            version: "3.0".to_owned(),
            ..Default::default()
        }
    );
}

/// The role limits list their roles in name order with their numbers.
#[test]
fn role_limits_iter_lists_roles_in_name_order() {
    let limits = RoleLimits::from([("storage", 3), ("frontend", 2)]);

    assert_eq!(
        limits.iter().collect::<Vec<_>>(),
        [("frontend", 2), ("storage", 3)]
    );
}

/// Each convergence state maps to its own proto state, and a bare convergence has no desired
/// version, halt, hosts, force or write record.
#[test]
fn convergence_maps_each_state_by_name() {
    for (state, expected) in [
        (State::Converged, proto::ConvergenceState::Converged),
        (State::Blocked, proto::ConvergenceState::Blocked),
        (State::Halted, proto::ConvergenceState::Halted),
        (State::InProgress, proto::ConvergenceState::InProgress),
    ] {
        assert_eq!(
            map_convergence(&conv(state)),
            proto::Convergence {
                state: expected as i32,
                coordinator_commit: COMMIT.to_owned(),
                ..Default::default()
            }
        );
    }
}

/// A convergence with a desired version, a halt, host counts and role counts carries each into
/// its proto field; the pinned count has no source and stays 0.
#[test]
fn convergence_carries_desired_halt_hosts_and_roles() {
    let mut c = conv(State::Halted);
    c.desired = Some(version("3.0"));
    c.quiet = true;
    c.roles = vec![
        RoleCounts {
            role: "storage".to_owned(),
            at_desired: 4,
            updating: 3,
            failed: 2,
            down: 1,
        },
        RoleCounts {
            role: "frontend".to_owned(),
            at_desired: usize::MAX,
            updating: 0,
            failed: 0,
            down: 0,
        },
    ];
    c.halt = Some(HaltNote {
        node_id: "d1".to_owned(),
        check: "disk_full".to_owned(),
        target: version("3.0"),
    });
    c.hosts = Some(HostCounts {
        missing: 1,
        absent: 2,
        excluded: 3,
        unmanaged: 4,
        deferred: usize::MAX,
    });

    assert_eq!(
        map_convergence(&c),
        proto::Convergence {
            desired: Some("3.0".to_owned()),
            quiet: true,
            state: proto::ConvergenceState::Halted as i32,
            roles: vec![
                proto::RoleCounts {
                    role: "storage".to_owned(),
                    at_desired: 4,
                    updating: 3,
                    failed: 2,
                    down: 1,
                    pinned: 0,
                },
                proto::RoleCounts {
                    role: "frontend".to_owned(),
                    at_desired: u32::MAX,
                    ..Default::default()
                },
            ],
            halt: Some(proto::HaltRecord {
                node_id: "d1".to_owned(),
                check: "disk_full".to_owned(),
                target: "3.0".to_owned(),
                report: None,
            }),
            hosts: Some(proto::HostCounts {
                missing: 1,
                absent: 2,
                excluded: 3,
                unmanaged: 4,
                deferred: u32::MAX,
            }),
            coordinator_commit: COMMIT.to_owned(),
            record: None,
            force: None,
        }
    );
}

/// An ops entry for the service with no force gives marks and no force on the convergence.
#[test]
fn service_with_an_ops_entry_and_no_force_has_default_marks() {
    let out = mapped(svc(vec![]), ops_with("web", ServiceOps::default()));

    assert_eq!(out.marks, Some(proto::ServiceMarks::default()));
    assert_eq!(out.convergence.expect("convergence").force, None);
}

/// An ops entry of another service gives this one no marks.
#[test]
fn ops_entry_of_another_service_gives_no_marks() {
    let out = mapped(svc(vec![]), ops_with("other", ServiceOps::default()));

    assert_eq!(out.marks, None);
}

/// A standing force is put into the convergence of its service, with version, generation and
/// reason.
#[test]
fn standing_force_is_named_in_the_convergence() {
    let marks = ServiceOps {
        force: Some(ForceRecord {
            version: version("2.9"),
            generation: 5,
            reason: "rollback".to_owned(),
            by: "alice".to_owned(),
            digest: Digest::parse(&"b".repeat(64)).expect("a digest"),
            halt_generation: 0,
            targets: ForceTargets::Fleet,
            include_held: false,
            hosts: Vec::new(),
        }),
        force_generation: 5,
        ..Default::default()
    };

    let out = mapped(svc(vec![]), ops_with("web", marks));

    assert_eq!(
        out.convergence.expect("convergence").force,
        Some(proto::ForceRecord {
            version: "2.9".to_owned(),
            generation: 5,
            reason: "rollback".to_owned(),
        })
    );
    assert_eq!(out.marks.expect("marks").force_generation, 5);
}

/// Every mark of the ops state passes into the service's marks, a halt with its report and an
/// acknowledgement with the report's reason as its check.
#[test]
fn service_marks_carry_halts_acks_versions_and_generations() {
    let marks = ServiceOps {
        halt: Some(Halt {
            node_id: "d1".to_owned(),
            check: "disk_full".to_owned(),
            target: version("3.0"),
            report: Some(full_update()),
            origin: HaltOrigin::Updater,
        }),
        halt_acked: vec![
            HaltAck {
                node_id: "d2".to_owned(),
                target: version("2.5"),
                report: Some(bare_update(Some("bad_disk"))),
            },
            HaltAck {
                node_id: "d3".to_owned(),
                target: version("2.5"),
                report: Some(bare_update(None)),
            },
            HaltAck {
                node_id: "d4".to_owned(),
                target: version("2.5"),
                report: None,
            },
        ],
        halt_seen: vec![HaltAck {
            node_id: "d5".to_owned(),
            target: version("2.6"),
            report: Some(bare_update(Some("seen_it"))),
        }],
        halt_generation: 9,
        last_good: Some(version("2.0")),
        bad: vec![version("2.7"), version("2.8")],
        failure_acked: vec![FailureAck {
            node_id: "d6".to_owned(),
            report: full_update(),
        }],
        force: None,
        force_generation: 3,
        bad_by_force: vec![version("2.9")],
    };

    let out = mapped(svc(vec![]), ops_with("web", marks))
        .marks
        .expect("marks");

    let acked = |node: &str, check: &str, report: Option<agent::UpdateResult>| proto::HaltRecord {
        node_id: node.to_owned(),
        check: check.to_owned(),
        target: "2.5".to_owned(),
        report,
    };
    let bare = |reason: &str| agent::UpdateResult {
        target: "2.5".to_owned(),
        outcome: agent::Outcome::Succeeded as i32,
        reason: reason.to_owned(),
        ..Default::default()
    };
    assert_eq!(
        out,
        proto::ServiceMarks {
            halt: Some(proto::HaltRecord {
                node_id: "d1".to_owned(),
                check: "disk_full".to_owned(),
                target: "3.0".to_owned(),
                report: Some(full_update_proto()),
            }),
            halt_acked: vec![
                acked("d2", "bad_disk", Some(bare("bad_disk"))),
                acked("d3", "", Some(bare(""))),
                acked("d4", "", None),
            ],
            halt_seen: vec![proto::HaltRecord {
                node_id: "d5".to_owned(),
                check: "seen_it".to_owned(),
                target: "2.6".to_owned(),
                report: Some(agent::UpdateResult {
                    target: "2.5".to_owned(),
                    outcome: agent::Outcome::Succeeded as i32,
                    reason: "seen_it".to_owned(),
                    ..Default::default()
                }),
            }],
            halt_generation: 9,
            last_good: Some("2.0".to_owned()),
            bad: vec!["2.7".to_owned(), "2.8".to_owned()],
            bad_by_force: vec!["2.9".to_owned()],
            failure_acked: vec![proto::FailureAck {
                node_id: "d6".to_owned(),
                report: Some(full_update_proto()),
            }],
            force_generation: 3,
        }
    );
}

/// A standing halt that carries no report maps with no report.
#[test]
fn standing_halt_without_a_report_has_none() {
    let marks = ServiceOps {
        halt: Some(Halt {
            node_id: "d1".to_owned(),
            check: "disk_full".to_owned(),
            target: version("3.0"),
            report: None,
            origin: HaltOrigin::Hook,
        }),
        ..Default::default()
    };

    let halt = mapped(svc(vec![]), ops_with("web", marks))
        .marks
        .expect("marks")
        .halt;

    assert_eq!(
        halt,
        Some(proto::HaltRecord {
            node_id: "d1".to_owned(),
            check: "disk_full".to_owned(),
            target: "3.0".to_owned(),
            report: None,
        })
    );
}

/// A host not named among the members is present; the host's role, liveness, slot and the
/// service's own health and version pass through.
#[test]
fn host_not_among_the_members_is_present_with_its_own_facts() {
    let mut h = svc_host("d1", "storage", beat("2.0"));
    h.host.live = false;
    h.host.up = true;
    h.host.holds_slot = true;
    let others = vec![Member {
        node_id: "d9".to_owned(),
        role: "storage".to_owned(),
        membership: Membership::Down,
        reason: Some("not mine".to_owned()),
    }];

    let out = mapped_host(h, others, OpsState::default());

    assert_eq!(out.node_id, "d1");
    assert_eq!(out.role, "storage");
    assert!(!out.live);
    assert_eq!(out.excluded_reason, None);
    let view = out.view.expect("view");
    assert_eq!(view.service, "web");
    assert!(view.up);
    assert!(view.holds_slot);
    assert_eq!(view.gate_reason, proto::GateReason::None as i32);
    assert_eq!(view.membership, proto::Membership::Present as i32);
    assert!(view.holds.is_empty());
    assert_eq!(view.installed_version, Some("2.0".to_owned()));
    assert_eq!(view.health, agent::Health::Healthy as i32);
    assert_eq!(view.last_update, None);
    assert_eq!(view.excluded_reason, None);
}

/// A member of the service names its membership and its reason on both the host and its view.
#[test]
fn member_host_carries_its_membership_and_excluded_reason() {
    let members = vec![Member {
        node_id: "d1".to_owned(),
        role: "storage".to_owned(),
        membership: Membership::Excluded,
        reason: Some("maintenance".to_owned()),
    }];

    let out = mapped_host(
        svc_host("d1", "storage", beat("2.0")),
        members,
        OpsState::default(),
    );

    assert_eq!(out.excluded_reason, Some("maintenance".to_owned()));
    let view = out.view.expect("view");
    assert_eq!(view.membership, proto::Membership::Excluded as i32);
    assert_eq!(view.excluded_reason, Some("maintenance".to_owned()));
}

/// A member with no reason has no excluded reason.
#[test]
fn member_without_a_reason_has_no_excluded_reason() {
    let members = vec![Member {
        node_id: "d1".to_owned(),
        role: "storage".to_owned(),
        membership: Membership::Missing,
        reason: None,
    }];

    let out = mapped_host(
        svc_host("d1", "storage", beat("2.0")),
        members,
        OpsState::default(),
    );

    assert_eq!(out.excluded_reason, None);
    assert_eq!(out.view.expect("view").excluded_reason, None);
}

/// Each membership maps to its proto name.
#[test]
fn every_membership_maps_to_its_proto_name() {
    for (membership, expected) in [
        (Membership::Excluded, proto::Membership::Excluded),
        (Membership::NotSelected, proto::Membership::NotSelected),
        (Membership::Down, proto::Membership::Down),
        (Membership::Absent, proto::Membership::Absent),
        (Membership::Present, proto::Membership::Present),
        (Membership::Missing, proto::Membership::Missing),
        (Membership::Unmanaged, proto::Membership::Unmanaged),
    ] {
        let members = vec![Member {
            node_id: "d1".to_owned(),
            role: "storage".to_owned(),
            membership,
            reason: None,
        }];

        let out = mapped_host(
            svc_host("d1", "storage", beat("2.0")),
            members,
            OpsState::default(),
        );

        assert_eq!(out.view.expect("view").membership, expected as i32);
    }
}

/// Each closed gate, and each question that does not apply, maps to its proto reason; the
/// floor gate, and a host that cannot take an install or an uninstall or a version with no
/// package digest to install, have no spelling and are unspecified; no hold at all is `none`.
#[test]
fn every_rollout_hold_maps_to_its_proto_gate_reason() {
    use proto::GateReason as R;
    let cases = [
        (Some(Why::Gate(Gate::Halted)), R::Halted),
        (Some(Why::Gate(Gate::Held)), R::Held),
        (Some(Why::Gate(Gate::Bad)), R::Bad),
        (Some(Why::Gate(Gate::BelowMinVersion)), R::Unspecified),
        (Some(Why::Gate(Gate::Updating)), R::Updating),
        (Some(Why::Gate(Gate::BusyElsewhere)), R::BusyElsewhere),
        (Some(Why::Gate(Gate::NoSlot)), R::NoSlot),
        (Some(Why::Gate(Gate::Quiet)), R::Quiet),
        (Some(Why::Gate(Gate::CanaryWait)), R::CanaryWait),
        (Some(Why::Gate(Gate::Baking)), R::Baking),
        (Some(Why::Skip(Skip::NoDesired)), R::NoDesired),
        (Some(Why::Skip(Skip::NotAMember)), R::NotAMember),
        (Some(Why::Skip(Skip::UpToDate)), R::UpToDate),
        (Some(Why::Skip(Skip::FailedHere)), R::FailedHere),
        (Some(Why::Skip(Skip::Deferred)), R::Deferred),
        (Some(Why::Skip(Skip::Unsupported)), R::Unspecified),
        (Some(Why::Skip(Skip::NoDigest)), R::Unspecified),
        (None, R::None),
    ];
    for (why, expected) in cases {
        let mut h = svc_host("d1", "storage", beat("2.0"));
        h.hold = why;

        let out = mapped_host(h, Vec::new(), OpsState::default());

        assert_eq!(
            out.view.expect("view").gate_reason,
            expected as i32,
            "{why:?}"
        );
    }
}

/// Only the holds of this service that cover the host (by node, by role or the whole fleet) are
/// listed, in the order the ops state keeps them.
#[test]
fn host_lists_only_the_holds_of_its_service_that_cover_it() {
    let mut ops = OpsState::default();
    ops.holds.extend([
        hold("web", HoldScope::Host("d1".to_owned())),
        hold("other", HoldScope::Host("d1".to_owned())),
        hold("web", HoldScope::Host("d2".to_owned())),
        hold("web", HoldScope::Role("storage".to_owned())),
        hold("web", HoldScope::Role("frontend".to_owned())),
        hold("web", HoldScope::Fleet),
    ]);

    let out = mapped_host(svc_host("d1", "storage", beat("2.0")), Vec::new(), ops);

    assert_eq!(
        out.view.expect("view").holds,
        [
            hold_proto(Scope::Host("d1".to_owned())),
            hold_proto(Scope::Role("storage".to_owned())),
            hold_proto(Scope::Fleet(proto::WholeFleet {})),
        ]
    );
}

/// An unhealthy host has its running version installed; an absent one has none, whatever the
/// heartbeat's version says.
#[test]
fn installed_version_is_unset_only_when_the_service_is_absent() {
    for (health, installed, expected_health) in [
        (Health::Healthy, Some("2.0"), agent::Health::Healthy),
        (Health::Unhealthy, Some("2.0"), agent::Health::Unhealthy),
        (Health::Absent, None, agent::Health::Absent),
    ] {
        let mut b = beat("2.0");
        b.health = health;

        let out = plain_host(b);

        let view = out.view.expect("view");
        assert_eq!(view.installed_version.as_deref(), installed);
        assert_eq!(view.health, expected_health as i32);
        let beat = out.beat.expect("beat");
        assert_eq!(beat.health, expected_health as i32);
        assert_eq!(beat.version, "2.0");
    }
}

/// A host that reports nothing beyond its version, health and platform has a bare service beat
/// named for the service.
#[test]
fn bare_heartbeat_maps_to_a_bare_service_beat() {
    let out = plain_host(beat("2.0"));

    assert_eq!(
        out.beat,
        Some(agent::ServiceBeat {
            name: "web".to_owned(),
            version: "2.0".to_owned(),
            health: agent::Health::Healthy as i32,
            commit: agent::CommitState::Unspecified as i32,
            ..Default::default()
        })
    );
}

/// A heartbeat with every optional fact maps each into the service beat and the view's last
/// update.
#[test]
fn full_heartbeat_maps_every_fact_into_the_service_beat() {
    let mut b = beat("2.0");
    b.last_update = Some(full_update());
    b.updating = Some(version("3.0"));
    b.step = Some(StepRun {
        name: StepName::parse("cutover").expect("a step"),
        attempt: 2,
    });
    b.commit = Some(CommitState::Committed);
    b.step_secs = BTreeMap::from([
        (StepName::parse("prepare").expect("a step"), 4),
        (StepName::parse("cutover").expect("a step"), 9),
    ]);
    b.promotion_pending = true;
    b.deferred = Some(DeferredReport {
        target: version("3.0"),
        reason: Token::parse("busy").expect("a token"),
        retry_after_secs: 30,
    });

    let out = plain_host(b);

    assert_eq!(
        out.beat,
        Some(agent::ServiceBeat {
            name: "web".to_owned(),
            version: "2.0".to_owned(),
            health: agent::Health::Healthy as i32,
            last_update: Some(full_update_proto()),
            updating: Some("3.0".to_owned()),
            step: Some(agent::StepRun {
                name: "cutover".to_owned(),
                attempt: 2,
            }),
            commit: agent::CommitState::Committed as i32,
            step_secs: HashMap::from([("prepare".to_owned(), 4), ("cutover".to_owned(), 9)]),
            promotion_pending: true,
            deferred: Some(agent::DeferredReport {
                target: "3.0".to_owned(),
                reason: "busy".to_owned(),
                retry_after_secs: 30,
            }),
            pinned: None,
        })
    );
    assert_eq!(
        out.view.expect("view").last_update,
        Some(full_update_proto())
    );
}

/// Each commit state of a beat maps to its proto name; none is unspecified.
#[test]
fn every_commit_state_maps_to_its_proto_name() {
    for (state, expected) in [
        (None, agent::CommitState::Unspecified),
        (
            Some(CommitState::Uncommitted),
            agent::CommitState::Uncommitted,
        ),
        (Some(CommitState::Committed), agent::CommitState::Committed),
    ] {
        let mut b = beat("2.0");
        b.commit = state;

        assert_eq!(plain_host(b).beat.expect("beat").commit, expected as i32);
    }
}

/// A verdict with no reason, order or try maps to an empty reason and no order; each outcome,
/// phase and class maps to its proto name, and none is unspecified.
#[test]
fn every_outcome_phase_and_class_maps_to_its_proto_name() {
    let phases = [
        (None, agent::FailurePhase::Unspecified),
        (Some(Phase::BeforeCommit), agent::FailurePhase::BeforeCommit),
        (Some(Phase::GateRelease), agent::FailurePhase::GateRelease),
        (
            Some(Phase::GateEnvironment),
            agent::FailurePhase::GateEnvironment,
        ),
        (Some(Phase::AfterCommit), agent::FailurePhase::AfterCommit),
        (Some(Phase::Halt), agent::FailurePhase::Halt),
    ];
    let classes = [
        (None, agent::FailureClass::Unspecified),
        (Some(Class::Release), agent::FailureClass::Release),
        (Some(Class::Environment), agent::FailureClass::Environment),
    ];
    let outcomes = [
        (Outcome::Succeeded, agent::Outcome::Succeeded),
        (Outcome::Failed, agent::Outcome::Failed),
    ];
    for (outcome, expected_outcome) in outcomes {
        for (phase, expected_phase) in phases {
            for (class, expected_class) in classes {
                let mut r = UpdateResult::new(version("3.0"), outcome);
                r.phase = phase;
                r.class = class;
                let mut b = beat("2.0");
                b.last_update = Some(r);

                let got = plain_host(b).beat.expect("beat").last_update;

                assert_eq!(
                    got,
                    Some(agent::UpdateResult {
                        target: "3.0".to_owned(),
                        outcome: expected_outcome as i32,
                        phase: expected_phase as i32,
                        class: expected_class as i32,
                        ..Default::default()
                    })
                );
            }
        }
    }
}
