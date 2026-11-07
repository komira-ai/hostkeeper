//! The assignment calls of `Services` ([`crate::assign`]): what `List` shows, every refusal as
//! a token of the closed `ServicesRefusal`, the path an enrollment approval's services take, the
//! cap on the entries, and a host assigned a service its role does not run being counted and
//! ordered.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::assignment::Scope as AssignScope;
use hostkeeper_proto::hostkeeper::operator::v1::min_version::Scope as MinScope;
use hostkeeper_wire::wire::ServiceReply;

use crate::assign_support::{
    absent, assign, install, intents, order_to, set_min, told, unassign, why_of,
};
use crate::assignment::{Entry, MAX_ENTRIES, ServiceAssignments};
use crate::decide::{Gate, Skip, Why};
use crate::marks_support::{
    DB, INSTALLER, NEW_DAEMON, WEB, ask_typed, beat_of, hold, operator, rig, runs, scope_role, set,
    start, update,
};
use crate::operator::Caller;
use crate::ops::{OpsState, OpsStore};
use crate::rollback_tests::unwritable;
use crate::testutil::{id, role};

/// `Services.List` of the rig, as the operator.
fn listed(rig: &crate::testutil::Rig) -> proto::ListServicesResponse {
    rig.coordinator
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("listed")
}

/// Catches: a `List` that leaves out the operator's assignments, the minimum versions, the
/// installed versions, the holds of an assigned host or the hosts ordered to uninstall, or
/// lists a host the service is not assigned to (taken off it, excluded or of another role).
#[tokio::test]
async fn services_list_shows_assignments_next_to_holds_and_installed_versions() {
    let rig = rig("assign-list", "");
    set(&rig, WEB, "2.0", 1).await;
    let old = |services| beat_of(NEW_DAEMON, services);
    ask_typed(&rig, "d1", "daemon", old(vec![runs(WEB, "1.0")]));
    ask_typed(&rig, "d2", "daemon", old(vec![runs(WEB, "1.0")]));
    ask_typed(&rig, "f1", "frontend", old(vec![]));
    ask_typed(&rig, "s1", "storage", old(vec![absent(WEB, "1.0")]));
    ask_typed(&rig, "s2", "storage", old(vec![runs(WEB, "1.0")]));
    hold(&rig, WEB, scope_role("storage")).expect("held");
    assign(&rig, WEB, "f1", "").expect("f1 by its own entry");
    unassign(&rig, WEB, "d2", "", true).expect("d2 taken off");
    set_min(&rig, WEB, MinScope::Role("daemon".to_owned()), "1.5").expect("the role's");
    set_min(&rig, WEB, MinScope::Host("d1".to_owned()), "1.8").expect("the host's");
    rig.coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: WEB.to_owned(),
                node_id: "s2".to_owned(),
                reason: "bad disk".to_owned(),
                abandon: false,
            },
        )
        .expect("excluded");

    let list = listed(&rig);
    let names: Vec<&str> = list.services.iter().map(|s| s.service.as_str()).collect();
    assert_eq!(names, [DB, WEB]);
    let web = &list.services[1];
    assert_eq!(web.desired_version.as_deref(), Some("2.0"));
    assert_eq!(web.roles, ["storage", "daemon"]);
    assert_eq!(web.hosts, ["d1", "f1", "s1"]);
    assert_eq!(web.uninstalling, ["d2"]);
    let scoped = |scope: AssignScope, removed: bool| proto::Assignment {
        scope: Some(scope),
        removed,
    };
    assert_eq!(
        web.assignments,
        [
            scoped(AssignScope::Host("d2".to_owned()), true),
            scoped(AssignScope::Host("f1".to_owned()), false),
        ]
    );
    let min = |scope: MinScope, version: &str| proto::MinVersion {
        scope: Some(scope),
        version: version.to_owned(),
    };
    assert_eq!(
        web.min_versions,
        [
            min(MinScope::Role("daemon".to_owned()), "1.5"),
            min(MinScope::Host("d1".to_owned()), "1.8"),
        ]
    );
    let seen: Vec<_> = web
        .assigned
        .iter()
        .map(|h| {
            (
                h.node_id.as_str(),
                h.role.as_str(),
                h.installed_version.as_deref(),
                h.min_version.as_deref(),
                h.holds.len(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("d1", "daemon", Some("1.0"), Some("1.8"), 0),
            ("f1", "frontend", None, None, 0),
            ("s1", "storage", None, None, 1),
        ]
    );
    assert_eq!(web.assigned[2].holds[0].scope, scope_role("storage"));
    assert_eq!(web.holds.len(), 1);
    assert_eq!(list.holds.len(), 1);

    let db = &list.services[0];
    assert_eq!(db.desired_version, None);
    assert_eq!(db.hosts, ["d1", "d2", "s1", "s2"]);
    assert!(db.assignments.is_empty() && db.min_versions.is_empty());
    assert!(db.uninstalling.is_empty() && db.holds.is_empty());
}

/// Catches: a host assigned a service its role does not run being left uncounted, so it is
/// never updated: a frontend host given `web` by its own entry is ordered the install, then,
/// once it runs an older version, the update.
#[tokio::test]
async fn a_host_assigned_a_service_its_role_does_not_run_is_installed_and_updated() {
    let rig = rig("assign-other-role", "");
    set(&rig, WEB, "2.0", 1).await;
    let f1 = |services| ask_typed(&rig, "f1", "frontend", beat_of(INSTALLER, services));
    assert_eq!(f1(vec![]), (ServiceReply::Nothing, None));
    assign(&rig, WEB, "f1", "").expect("assigned");
    assert_eq!(f1(vec![]), (ServiceReply::Nothing, Some(install("2.0", 1))));
    rig.at(2_200);
    assert_eq!(f1(vec![runs(WEB, "1.0")]), (update(WEB, "2.0"), None));
}

/// The token a refusal is spelled with on the wire: its name without the prefix, lower case.
fn token(refusal: proto::ServicesRefusal) -> String {
    refusal
        .as_str_name()
        .strip_prefix("SERVICES_REFUSAL_")
        .expect("the prefix")
        .to_ascii_lowercase()
}

/// Catches: a refusal outside the closed set of `ServicesRefusal`, or a refusal path that is
/// missing: each of `Assign`, `Unassign`, `SetMinVersion` and `List` is refused for each reason
/// its contract names, each refusal changes nothing, and together they are every token but
/// `not_held` (`Unhold`'s).
#[tokio::test]
async fn assignment_api_reasons_are_closed_tokens() {
    let mut rig = rig("assign-refusals", "");
    told(&rig, "d1", vec![runs(WEB, "1.0")]);
    ask_typed(&rig, "f1", "frontend", beat_of(INSTALLER, vec![]));
    set(&rig, WEB, "2.0", 1).await;
    let mut seen: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut note = |case: &'static str, refused: proto::ServicesRefusal| {
        seen.entry(token(refused)).or_default().push(case);
    };
    let c = &rig.coordinator;
    let request = |node: &str, service: &str, role: &str| proto::AssignRequest {
        node_id: node.to_owned(),
        service: service.to_owned(),
        role: role.to_owned(),
    };
    for (case, caller) in [("anonymous", Caller::Anonymous), ("ci", Caller::Ci)] {
        let refused = c.assign(&caller, request("d1", WEB, "")).expect_err(case);
        note(case, refused);
        let off = proto::UnassignRequest::default();
        note(case, c.unassign(&caller, off).expect_err(case));
        let min = proto::SetMinVersionRequest::default();
        note(case, c.set_min_version(&caller, min).expect_err(case));
        let all = proto::ListServicesRequest {};
        note(case, c.list_services(&caller, all).expect_err(case));
    }
    let assigns = [
        ("both", request("d1", WEB, "daemon")),
        ("neither", request("", WEB, "")),
        ("bad node", request("D1!", WEB, "")),
        ("bad role", request("", WEB, "Daemon")),
        ("unknown service", request("d1", "nope", "")),
        ("unknown host", request("d9", WEB, "")),
        ("unknown role", request("", WEB, "builder")),
    ];
    for (case, one) in assigns {
        note(case, c.assign(&operator(), one).expect_err(case));
    }
    note(
        "not assigned",
        unassign(&rig, WEB, "", "frontend", false).expect_err("not assigned"),
    );
    note(
        "unknown role off",
        unassign(&rig, WEB, "", "builder", true).expect_err("unknown"),
    );
    let mins = [
        ("no minimum", None),
        (
            "no scope",
            Some(proto::MinVersion {
                scope: None,
                version: "1.0".to_owned(),
            }),
        ),
        (
            "above desired",
            Some(proto::MinVersion {
                scope: Some(MinScope::Host("d1".to_owned())),
                version: "2.1".to_owned(),
            }),
        ),
        (
            "unknown service min",
            Some(proto::MinVersion {
                scope: Some(MinScope::Host("d1".to_owned())),
                version: "1.0".to_owned(),
            }),
        ),
    ];
    for (case, min_version) in mins {
        let service = if case == "unknown service min" {
            "nope"
        } else {
            WEB
        };
        let refused = c
            .set_min_version(
                &operator(),
                proto::SetMinVersionRequest {
                    service: service.to_owned(),
                    min_version,
                },
            )
            .expect_err(case);
        note(case, refused);
    }
    assert!(intents(&rig, WEB).is_empty());
    assert!(rig.coordinator.ops().read(|s| s.assignments.is_empty()));

    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(OpsState::default()));
    told(&rig, "d1", vec![runs(WEB, "1.0")]);
    let c = &rig.coordinator;
    note(
        "store failed",
        c.assign(&operator(), request("d1", WEB, ""))
            .expect_err("not written"),
    );
    note(
        "store failed min",
        set_min(&rig, WEB, MinScope::Role("daemon".to_owned()), "1.0").expect_err("not written"),
    );
    assert!(rig.coordinator.ops().read(|s| s.assignments.is_empty()));

    let closed: BTreeSet<String> = [
        "unauthorized",
        "forbidden",
        "invalid_request",
        "unknown_service",
        "unknown_host",
        "not_assigned",
        "not_held",
        "store_failed",
        "unknown_role",
        "below_min_version",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let given: BTreeSet<String> = seen.keys().cloned().collect();
    assert!(given.is_subset(&closed), "{seen:?}");
    let mut missing: Vec<&String> = closed.difference(&given).collect();
    missing.sort();
    assert_eq!(missing, [&"not_held".to_owned()], "{seen:?}");
    assert_eq!(seen["unauthorized"], ["anonymous"; 4]);
    assert_eq!(seen["forbidden"], ["ci"; 4]);
    assert_eq!(
        seen["invalid_request"],
        [
            "both",
            "neither",
            "bad node",
            "bad role",
            "no minimum",
            "no scope"
        ]
    );
    assert_eq!(seen["unknown_role"], ["unknown role", "unknown role off"]);
    assert_eq!(seen["unknown_host"], ["unknown host"]);
    assert_eq!(
        seen["unknown_service"],
        ["unknown service", "unknown service min"]
    );
    assert_eq!(seen["below_min_version"], ["above desired"]);
    assert_eq!(seen["store_failed"], ["store failed", "store failed min"]);
}

/// Catches: an enrollment approval's services taking another path than `Assign` (no entry, no
/// audit, an unknown service half assigned): one call assigns each service to the new host by
/// its own entry, one audit entry each, and the host, at its first heartbeat, is told to install
/// them; a service the rollout file does not define assigns nothing. The install takes the first
/// place of the plan's stage: the next host waits for the stage after.
#[tokio::test]
async fn an_approvals_services_are_assigned_by_the_path_assign_takes() {
    let rig = rig("assign-approve", "");
    set(&rig, WEB, "2.0", 1).await;
    let both = [DB.to_owned(), WEB.to_owned()];
    let refused = rig
        .coordinator
        .assign_host(
            "operator",
            &id("n1"),
            Some(&role("builder")),
            &[WEB.to_owned(), "nope".to_owned()],
        )
        .expect_err("an unknown service");
    assert_eq!(refused, proto::ServicesRefusal::UnknownService);
    assert!(rig.coordinator.ops().read(|s| s.assignments.is_empty()));

    let entries = rig
        .coordinator
        .assign_host("operator", &id("n1"), Some(&role("builder")), &both)
        .expect("assigned");
    let said: Vec<_> = entries
        .iter()
        .map(|e| {
            (
                e.action.as_str(),
                e.service.as_deref(),
                e.node_id.as_deref(),
                e.reason.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        said,
        [
            ("assign", Some(DB), Some("n1"), Some("host n1")),
            ("assign", Some(WEB), Some("n1"), Some("host n1")),
        ]
    );
    assert_eq!(entries[1].seq, entries[0].seq + 1);
    let (_, order) = ask_typed(&rig, "n1", "builder", beat_of(INSTALLER, vec![]));
    assert_eq!(order, Some(install("2.0", 1)), "db has no desired version");
    assert_eq!(why_of(&rig, DB, "n1"), Some(Why::Skip(Skip::NoDesired)));
    assert_eq!(order_to(&rig, "n2", vec![]), None);
    assert_eq!(
        why_of(&rig, WEB, "n2"),
        Some(Why::Gate(Gate::StageWait)),
        "n1's install takes the one place of the first stage"
    );
}

/// Catches: no cap on the assignments the ops file keeps: a change past [`MAX_ENTRIES`] is
/// `invalid_request` and changes nothing; one that removes entries still goes through.
#[tokio::test]
async fn a_change_past_the_cap_on_entries_is_refused() {
    let mut rig = rig("assign-cap", "");
    let mut full = ServiceAssignments::default();
    for n in 0..MAX_ENTRIES {
        full.hosts.insert(format!("h{n}"), Entry::Include);
    }
    let mut state = OpsState::default();
    state.assignments.insert(WEB.to_owned(), full);
    rig.coordinator =
        start(&rig.dir, &rig.clock, "").with_ops(OpsStore::refusing(state, Box::new(|_| Ok(()))));
    assert_eq!(
        assign(&rig, DB, "h1", "").expect_err("past the cap"),
        proto::ServicesRefusal::InvalidRequest
    );
    assert_eq!(
        set_min(&rig, WEB, MinScope::Host("h1".to_owned()), "1.0").expect_err("past the cap"),
        proto::ServicesRefusal::InvalidRequest
    );
    assert!(
        rig.coordinator
            .ops()
            .read(|s| !s.assignments.contains_key(DB))
    );
    unassign(&rig, WEB, "h1", "", false).expect("a removal goes through");
    assert_eq!(
        rig.coordinator
            .ops()
            .read(|s| s.assignments[WEB].hosts.len()),
        MAX_ENTRIES - 1
    );
}
