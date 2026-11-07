//! The operator's calls that mark or steer the rollout, from the outside: `Hosts.Abort` (queued
//! for a typed heartbeat, sent once, no slot), `Releases.Halt` and `Releases.MarkBad` (every host
//! told nothing, audited, CI cannot undo them), `Hosts.Exclude` with and without `abandon` (what
//! the update slot does), `Audit.List` (paging, filters, the cap on the page) and the refusals every
//! one of these calls makes for a caller that is not the operator. Every time is the rig's clock;
//! nothing sleeps.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceReply};

use crate::decide::{Gate, Why};
use crate::install_tests::{ONE_STAGE, set_one_slot};
use crate::marks_support::{
    NEW_DAEMON, OLD_DAEMON, WEB, ask, ask_typed, beat_of, digest, fleet_target, force_request,
    hold, operator, ops_bytes, rig, runs, scope_fleet, set, start, update, who,
};
use crate::operator::Caller;
use crate::ops::{AuditEntry, Exclusion, HaltOrigin, MAX_EXCLUDED, OpsState, OpsStore, Write};
use crate::orders::Order;
use crate::rollback_tests::{status, unwritable};
use crate::service::SetError;
use crate::testutil::{Log, OPERATOR, Rig, ci_token, ops_of, version};

/// A heartbeat of a new daemon running `running` of `web`.
fn daemon(running: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, running)])
}

/// A heartbeat of a daemon that predates the abort, running `running` of `web`.
fn old_daemon(running: &str) -> Heartbeat {
    beat_of(OLD_DAEMON, vec![runs(WEB, running)])
}

/// A heartbeat of a new daemon running `running` of `web` and updating it to `target`.
fn updating_to(running: &str, target: &str) -> Heartbeat {
    let mut one = runs(WEB, running);
    one.updating = Some(version(target));
    beat_of(NEW_DAEMON, vec![one])
}

/// Why the status says `host` is told nothing for `web`, or `None` when it would be ordered.
fn hold_of(rig: &Rig, host: &str) -> Option<Why> {
    let shown = status(rig);
    shown.per_service[WEB]
        .hosts
        .iter()
        .find(|h| h.host.node_id == host)
        .map(|h| h.hold)
        .expect("a host the coordinator heard from")
}

/// The request that aborts `service` on `node`.
fn abort_of(node: &str, service: &str) -> proto::AbortRequest {
    proto::AbortRequest {
        node_id: node.to_owned(),
        service: service.to_owned(),
        reason: "stuck step".to_owned(),
    }
}

/// The request that excludes `node` from `service`, abandoning it or not.
fn exclude_of(node: &str, service: &str, abandon: bool) -> proto::ExcludeRequest {
    proto::ExcludeRequest {
        service: service.to_owned(),
        node_id: node.to_owned(),
        reason: "host gone".to_owned(),
        abandon,
    }
}

/// The request that marks `version` of `service` bad.
fn mark_of(service: &str, version: &str) -> proto::MarkBadRequest {
    proto::MarkBadRequest {
        service: service.to_owned(),
        version: version.to_owned(),
        reason: "crashes on start".to_owned(),
    }
}

/// The request that halts `service`.
fn halt_of(service: &str) -> proto::HaltRequest {
    proto::HaltRequest {
        service: service.to_owned(),
        reason: "disk replaced soon".to_owned(),
    }
}

/// Records one change of the audit with the given facts, as a call of the store would.
fn record(rig: &Rig, by: &str, action: &str, service: &str, node: Option<&str>) {
    let mut entry = AuditEntry::new(by, action);
    entry.service = Some(service.to_owned());
    entry.node_id = node.map(str::to_owned);
    rig.coordinator
        .ops()
        .change(Write::BeforeMemory, entry, |_| {})
        .expect("a recorded change");
}

/// The page `request` lists.
fn page(rig: &Rig, request: proto::ListAuditRequest) -> proto::ListAuditResponse {
    rig.coordinator
        .audit_list(&operator(), request)
        .expect("a page")
}

/// The seqs of the entries of `page`.
fn seqs(page: &proto::ListAuditResponse) -> Vec<u64> {
    page.entries.iter().map(|e| e.seq).collect()
}

/// Asserts that every operator call refuses `caller` with the given refusals and changes nothing.
fn assert_all_refused(
    rig: &Rig,
    caller: &Caller,
    releases: proto::ReleasesRefusal,
    hosts: proto::HostsRefusal,
    services: proto::ServicesRefusal,
    audit: proto::AuditRefusal,
) {
    let c = &rig.coordinator;
    let scope = || crate::marks_support::scope_host("d1");
    assert_eq!(c.mark_bad(caller, mark_of(WEB, "2.0")), Err(releases));
    assert_eq!(c.halt(caller, halt_of(WEB)), Err(releases));
    assert_eq!(c.abort(caller, abort_of("d1", WEB)), Err(hosts));
    assert_eq!(c.exclude(caller, exclude_of("d1", WEB, true)), Err(hosts));
    assert_eq!(
        c.hold(
            caller,
            proto::HoldRequest {
                scope: scope(),
                service: WEB.to_owned(),
                reason: "looking".to_owned(),
            }
        ),
        Err(services)
    );
    assert_eq!(
        c.unhold(
            caller,
            proto::UnholdRequest {
                scope: scope(),
                service: WEB.to_owned(),
                reason: "done".to_owned(),
            }
        ),
        Err(services)
    );
    assert_eq!(
        c.list_services(caller, proto::ListServicesRequest {}),
        Err(services)
    );
    assert_eq!(
        c.audit_list(caller, proto::ListAuditRequest::default()),
        Err(audit)
    );
    assert_eq!(
        c.force(caller, force_request(WEB, "2.0", fleet_target(WEB), false)),
        Err(releases)
    );
}

/// An abort is queued for the host's next typed heartbeat, takes no update slot, is sent once and
/// only to a daemon that announced `abort-v1`; the JSON heartbeat never carries it, asking twice
/// queues one, and a host never heard from is refused.
///
/// Catches: an abort that takes a slot, is sent on every heartbeat or twice, is sent to a daemon
/// that cannot read it, is lost by a JSON heartbeat, is queued twice with two order ids, or is
/// queued for a host nobody heard from.
#[tokio::test]
async fn abort_is_queued_for_the_next_heartbeat_takes_no_slot_and_is_sent_once() {
    let rig = rig("marks-abort", "");
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("2.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d3", "daemon", old_daemon("2.0")),
        ServiceReply::Nothing
    );
    set(&rig, WEB, "2.0", 1).await;

    assert_eq!(
        rig.coordinator.abort(&operator(), abort_of("d9", WEB)),
        Err(proto::HostsRefusal::UnknownHost)
    );

    let first = rig
        .coordinator
        .abort(&operator(), abort_of("d1", WEB))
        .expect("queued");
    assert!(first.queued);
    let entry = first.entry.expect("an entry");
    assert_eq!(entry.action, "abort");
    assert_eq!(entry.service.as_deref(), Some(WEB));
    assert_eq!(entry.node_id.as_deref(), Some("d1"));
    let again = rig
        .coordinator
        .abort(&operator(), abort_of("d1", WEB))
        .expect("queued");
    assert!(again.queued);
    assert!(again.entry.expect("an entry").seq > entry.seq);

    // The JSON route has nowhere to put an order: the abort waits for a typed heartbeat.
    let json = rig
        .coordinator
        .heartbeat(who("d1", "daemon"), daemon("2.0"))
        .expect("answered")
        .reply;
    assert_eq!(json, ServiceReply::Nothing);

    let (reply, order) = ask_typed(&rig, "d1", "daemon", daemon("2.0"));
    assert_eq!(reply, ServiceReply::Nothing);
    let sent = Order::Abort {
        service: WEB.to_owned(),
        order_id: format!("abort-{}", entry.seq),
    };
    assert_eq!(order, Some(sent.clone()));
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", daemon("2.0")),
        (ServiceReply::Nothing, None)
    );

    // The abort took no slot: the host behind is still ordered.
    assert_eq!(ask(&rig, "d2", "daemon", daemon("1.0")), update(WEB, "2.0"));

    // A daemon that predates the abort is not sent it; it waits for one that can read it.
    rig.coordinator
        .abort(&operator(), abort_of("d3", WEB))
        .expect("queued");
    assert_eq!(
        ask_typed(&rig, "d3", "daemon", old_daemon("2.0")),
        (ServiceReply::Nothing, None)
    );
    let (_, later) = ask_typed(&rig, "d3", "daemon", daemon("2.0"));
    assert!(matches!(later, Some(Order::Abort { .. })), "{later:?}");

    assert_eq!(
        sent.to_proto(),
        agent::Order {
            order_id: format!("abort-{}", entry.seq),
            r#try: 1,
            verb: Some(agent::order::Verb::Abort(agent::Abort {
                service: WEB.to_owned()
            })),
        }
    );
}

/// An operator's halt tells every host nothing, is kept in the ops file as the operator's, is
/// logged as an alert, is refused twice and for a service with no desired version, survives a CI
/// set of another version, and `clear_halt` lifts it.
///
/// Catches: a halt that lets a host through, that is recorded as a host's or the updater's (so CI
/// lifts it), that is not alerted, that is raised twice, or that is raised without a version.
#[tokio::test]
async fn halt_raised_by_an_operator_blocks_every_host_until_clear_halt() {
    let rig = rig("marks-halt", "");
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    set(&rig, WEB, "2.0", 1).await;
    let log = Log::default();
    let _guard = log.install();

    let done = rig
        .coordinator
        .halt(&operator(), halt_of(WEB))
        .expect("halted");
    assert_eq!(done.entry.expect("an entry").action, "halt");
    rig.coordinator.observe_alerts().expect("observed");
    assert!(log.text().contains(r#"alert="halt""#), "{}", log.text());

    for host in ["d1", "d2"] {
        assert_eq!(
            ask(&rig, host, "daemon", daemon("1.0")),
            ServiceReply::Nothing
        );
        assert_eq!(hold_of(&rig, host), Some(Why::Gate(Gate::Halted)));
    }
    let halt = ops_of(&rig.dir)
        .marks(WEB)
        .halt
        .expect("a halt in the file");
    assert_eq!(halt.check, "operator");
    assert_eq!(halt.origin, HaltOrigin::Operator);
    assert_eq!(halt.target, version("2.0"));

    assert_eq!(
        rig.coordinator.halt(&operator(), halt_of(WEB)),
        Err(proto::ReleasesRefusal::AlreadyHalted)
    );
    assert_eq!(
        rig.coordinator.halt(&operator(), halt_of("db")),
        Err(proto::ReleasesRefusal::InvalidRequest)
    );

    // CI's next version does not lift an operator's halt.
    set(&rig, WEB, "3.0", 2).await;
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(hold_of(&rig, "d2"), Some(Why::Gate(Gate::Halted)));
    assert!(ops_of(&rig.dir).marks(WEB).halt.is_some());

    let cleared = rig
        .coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"clear_halt","service":"web","reason":"disk replaced"}"#,
        )
        .expect("cleared");
    assert_eq!(cleared.node_id, None, "an operator's halt names no host");
    assert!(ops_of(&rig.dir).marks(WEB).halt.is_none());
    assert_eq!(ask(&rig, "d1", "daemon", daemon("1.0")), update(WEB, "3.0"));
}

/// Marking the desired version bad tells every host nothing, is audited under the operator with
/// the version in its reason, is refused twice, cannot be undone by CI, takes any version the
/// scheme reads, and refuses a version or reason that is no such thing.
///
/// Catches: a mark that lets a host through, that CI's set walks over, that is audited under
/// another actor, that is recorded twice, or that takes a version the scheme cannot read.
#[tokio::test]
async fn mark_bad_stops_ordering_the_version_and_is_audited() {
    let rig = rig("marks-bad", "");
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(hold_of(&rig, "d1"), None);

    let done = rig
        .coordinator
        .mark_bad(&operator(), mark_of(WEB, "2.0"))
        .expect("marked");
    let entry = done.entry.expect("an entry");
    assert_eq!(entry.action, "mark_bad");
    assert_eq!(entry.by, i32::from(proto::Actor::Operator));
    assert_eq!(entry.service.as_deref(), Some(WEB));
    let said = entry.reason.expect("a reason");
    assert!(
        said.contains("2.0") && said.contains("crashes on start"),
        "{said}"
    );

    for host in ["d1", "d2"] {
        assert_eq!(
            ask(&rig, host, "daemon", daemon("1.0")),
            ServiceReply::Nothing
        );
        assert_eq!(hold_of(&rig, host), Some(Why::Gate(Gate::Bad)));
    }
    assert_eq!(
        rig.coordinator.mark_bad(&operator(), mark_of(WEB, "2.0")),
        Err(proto::ReleasesRefusal::AlreadyBad)
    );

    let body = format!(
        r#"{{"service":"web","version":"2.0","release_digest":"{}"}}"#,
        digest(1)
    );
    let token = ci_token();
    let refused = rig
        .coordinator
        .set_desired(Some(token.as_str()), body.as_bytes())
        .await;
    assert!(matches!(refused, Err(SetError::BadVersion)), "{refused:?}");

    // A version CI never set may be marked too.
    rig.coordinator
        .mark_bad(&operator(), mark_of(WEB, "9.9"))
        .expect("marked");
    assert!(ops_of(&rig.dir).marks(WEB).bad.contains(&version("9.9")));

    assert_eq!(
        rig.coordinator
            .mark_bad(&operator(), mark_of(WEB, "not a version")),
        Err(proto::ReleasesRefusal::InvalidRequest)
    );
    let mut blank = mark_of(WEB, "3.0");
    blank.reason = "  ".to_owned();
    assert_eq!(
        rig.coordinator.mark_bad(&operator(), blank),
        Err(proto::ReleasesRefusal::InvalidRequest)
    );
    assert_eq!(
        rig.coordinator
            .mark_bad(&operator(), mark_of("nope", "2.0")),
        Err(proto::ReleasesRefusal::UnknownService)
    );
}

/// Excluding a host with `abandon` frees the update slot of a host that was mid-update, so the
/// next host is ordered; the entry says `possibly_mid_update`, the log carries the alert, and the
/// exclusion stands in the status.
///
/// Catches: an abandon that keeps the slot of a vanished host (the role stalls), that does not
/// say the host may have been mid-update, that is not alerted, or that does not exclude. (The
/// daemons update one at a time here: `max_updating` of 1 and one stage.)
#[tokio::test]
async fn exclude_abandon_frees_the_slot_of_a_mid_update_host_and_audits_possibly_mid_update() {
    let rig = rig("marks-abandon", ONE_STAGE);
    set_one_slot(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        ask(&rig, "d1", "daemon", updating_to("1.0", "2.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(hold_of(&rig, "d2"), Some(Why::Gate(Gate::NoSlot)));
    let log = Log::default();
    let _guard = log.install();

    let done = rig
        .coordinator
        .exclude(&operator(), exclude_of("d1", WEB, true))
        .expect("excluded");
    let entry = done.entry.expect("an entry");
    assert_eq!(entry.action, "exclude_abandon");
    assert_eq!(entry.node_id.as_deref(), Some("d1"));
    assert!(
        entry
            .reason
            .expect("a reason")
            .contains("possibly_mid_update")
    );
    rig.coordinator.observe_alerts().expect("observed");
    assert!(
        log.text().contains(r#"alert="exclude_abandon""#),
        "{}",
        log.text()
    );
    assert!(
        status(&rig)
            .ops
            .excluded
            .iter()
            .any(|e| e.service == WEB && e.node_id == "d1")
    );

    assert_eq!(ask(&rig, "d2", "daemon", daemon("1.0")), update(WEB, "2.0"));
}

/// Excluding a host without `abandon` leaves the slot it holds: the next host is still told
/// nothing, and the entry is a plain `exclude` with no alert.
///
/// Catches: an exclusion that frees a slot a host may still be using, or that is audited as an
/// abandon. (The daemons update one at a time here: `max_updating` of 1 and one stage.)
#[tokio::test]
async fn exclude_without_abandon_keeps_the_slot() {
    let rig = rig("marks-exclude", ONE_STAGE);
    set_one_slot(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        ask(&rig, "d1", "daemon", updating_to("1.0", "2.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    let log = Log::default();
    let _guard = log.install();

    let done = rig
        .coordinator
        .exclude(&operator(), exclude_of("d1", WEB, false))
        .expect("excluded");
    let entry = done.entry.expect("an entry");
    assert_eq!(entry.action, "exclude");
    assert_eq!(entry.reason.as_deref(), Some("host gone"));
    assert!(!log.text().contains("exclude_abandon"), "{}", log.text());

    assert_eq!(
        ask(&rig, "d2", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(hold_of(&rig, "d2"), Some(Why::Gate(Gate::NoSlot)));
}

/// A heartbeat of a new daemon at `running` of `web` whose last update, to `target`, failed, and
/// which reports `web` unhealthy: the shape of a host the farm excluded after its update stalled.
fn failed_unhealthy(running: &str, target: &str) -> Heartbeat {
    let mut one = runs(WEB, running);
    one.health = agent_health_unhealthy();
    one.last_update = Some(hostkeeper_wire::wire::UpdateResult {
        reason: hostkeeper_wire::wire::Token::parse("protocol_unreadable"),
        order_id: hostkeeper_wire::wire::OrderId::parse("install-1"),
        order_try: Some(1),
        ..hostkeeper_wire::wire::UpdateResult::new(
            version(target),
            hostkeeper_wire::wire::Outcome::Failed,
        )
    });
    beat_of(NEW_DAEMON, vec![one])
}

fn agent_health_unhealthy() -> hostkeeper_wire::wire::Health {
    hostkeeper_wire::wire::Health::Unhealthy
}

/// Excluding a host whose last update failed and that stays unhealthy frees its role's update
/// slot, with or without `abandon`, and it stays free at that host's later heartbeats: the
/// service no longer orders it and nothing of an update runs there. Before, the excluded host's
/// next heartbeat (failed, not healthy) took the slot again, so the next host of the role sat at
/// `no_slot` until an operator raised `max_updating`.
///
/// Catches: an excluded host that counts against its role's `max_updating` while it runs no
/// update, after an abandon or without one. (One slot, one stage.)
#[tokio::test]
async fn an_excluded_host_that_failed_and_is_unhealthy_holds_no_slot() {
    for abandon in [true, false] {
        let rig = rig(&format!("marks-excluded-failed-{abandon}"), ONE_STAGE);
        set_one_slot(&rig, WEB, "2.0", 1).await;
        assert_eq!(
            ask(&rig, "d1", "daemon", failed_unhealthy("2.0", "1.5")),
            ServiceReply::Nothing
        );
        assert_eq!(
            ask(&rig, "d2", "daemon", daemon("1.0")),
            ServiceReply::Nothing
        );
        assert_eq!(hold_of(&rig, "d2"), Some(Why::Gate(Gate::NoSlot)));

        rig.coordinator
            .exclude(&operator(), exclude_of("d1", WEB, abandon))
            .expect("excluded");
        assert_eq!(
            ask(&rig, "d1", "daemon", failed_unhealthy("2.0", "1.5")),
            ServiceReply::Nothing,
            "abandon: {abandon}"
        );
        assert_eq!(
            ask(&rig, "d2", "daemon", daemon("1.0")),
            update(WEB, "2.0"),
            "abandon: {abandon}"
        );
    }
}

/// `Audit.List` pages the audit file in order: a limit of 0 or above 256 is 256, `since_seq`
/// continues from `next_seq`, each filter keeps only its entries, the coordinator's and CI's
/// entries read as their actors, and a filter that is no actor, action, service name or node id is
/// refused.
///
/// Catches: a page past the cap, a `next_seq` that repeats or skips entries, a filter that lets
/// other entries through, an actor mapped wrong, or a bad filter taken as "no filter".
#[test]
fn audit_list_pages_filters_and_caps_the_limit() {
    let rig = rig("marks-audit", "");
    for _ in 0..300 {
        record(&rig, "operator", "exclude", WEB, Some("d1"));
    }
    record(&rig, "ci", "lift_halt", "db", None);
    record(&rig, "coordinator", "last_good", WEB, None);
    record(&rig, "operator", "abort", WEB, Some("d2"));
    let req = proto::ListAuditRequest::default;

    let all = page(&rig, req());
    assert_eq!(all.entries.len(), 256);
    assert_eq!(all.next_seq, 256);
    assert_eq!(all.entries[0].seq, 1);
    let wide = page(
        &rig,
        proto::ListAuditRequest {
            limit: 1000,
            ..req()
        },
    );
    assert_eq!(wide.entries.len(), 256);
    let few = page(&rig, proto::ListAuditRequest { limit: 10, ..req() });
    assert_eq!(seqs(&few), (1..=10).collect::<Vec<u64>>());
    assert_eq!(few.next_seq, 10);

    let rest = page(
        &rig,
        proto::ListAuditRequest {
            since_seq: all.next_seq,
            ..req()
        },
    );
    assert_eq!(seqs(&rest), (257..=303).collect::<Vec<u64>>());
    assert_eq!(rest.next_seq, 303);
    let end = page(
        &rig,
        proto::ListAuditRequest {
            since_seq: rest.next_seq,
            ..req()
        },
    );
    assert!(end.entries.is_empty());
    assert_eq!(end.next_seq, 303);

    let ci = page(
        &rig,
        proto::ListAuditRequest {
            actor: Some("ci".to_owned()),
            ..req()
        },
    );
    assert_eq!(seqs(&ci), vec![301]);
    assert_eq!(ci.entries[0].by, i32::from(proto::Actor::Ci));
    assert_eq!(ci.entries[0].action, "lift_halt");
    let coordinator = page(
        &rig,
        proto::ListAuditRequest {
            actor: Some("coordinator".to_owned()),
            ..req()
        },
    );
    assert_eq!(seqs(&coordinator), vec![302]);
    assert_eq!(
        coordinator.entries[0].by,
        i32::from(proto::Actor::Coordinator)
    );
    let operators = page(
        &rig,
        proto::ListAuditRequest {
            actor: Some("operator".to_owned()),
            ..req()
        },
    );
    assert_eq!(operators.entries.len(), 256);
    assert!(
        operators
            .entries
            .iter()
            .all(|e| e.by == i32::from(proto::Actor::Operator))
    );

    let aborts = page(
        &rig,
        proto::ListAuditRequest {
            action: Some("abort".to_owned()),
            ..req()
        },
    );
    assert_eq!(seqs(&aborts), vec![303]);
    let db = page(
        &rig,
        proto::ListAuditRequest {
            service: Some("db".to_owned()),
            ..req()
        },
    );
    assert_eq!(seqs(&db), vec![301]);
    let d2 = page(
        &rig,
        proto::ListAuditRequest {
            node_id: Some("d2".to_owned()),
            ..req()
        },
    );
    assert_eq!(seqs(&d2), vec![303]);
    let d1 = page(
        &rig,
        proto::ListAuditRequest {
            node_id: Some("d1".to_owned()),
            since_seq: 256,
            ..req()
        },
    );
    assert_eq!(seqs(&d1), (257..=300).collect::<Vec<u64>>());
    let narrow = page(
        &rig,
        proto::ListAuditRequest {
            actor: Some("operator".to_owned()),
            service: Some(WEB.to_owned()),
            node_id: Some("d2".to_owned()),
            ..req()
        },
    );
    assert_eq!(seqs(&narrow), vec![303]);

    for bad in [
        proto::ListAuditRequest {
            actor: Some("root".to_owned()),
            ..req()
        },
        proto::ListAuditRequest {
            action: Some("Mark_Bad".to_owned()),
            ..req()
        },
        proto::ListAuditRequest {
            action: Some(String::new()),
            ..req()
        },
        proto::ListAuditRequest {
            action: Some("a".repeat(65)),
            ..req()
        },
        proto::ListAuditRequest {
            service: Some("bad name".to_owned()),
            ..req()
        },
        proto::ListAuditRequest {
            node_id: Some("Bad_Node".to_owned()),
            ..req()
        },
    ] {
        assert_eq!(
            rig.coordinator.audit_list(&operator(), bad),
            Err(proto::AuditRefusal::InvalidRequest)
        );
    }
}

/// A coordinator whose store has no audit file lists the ring it keeps.
///
/// Catches: a listing that fails, or is empty, for a store without a file.
#[test]
fn audit_list_of_a_store_with_no_file_lists_its_ring() {
    let mut rig = rig("marks-audit-ring", "");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(OpsStore::volatile());
    rig.coordinator
        .mark_bad(&operator(), mark_of(WEB, "1.0"))
        .expect("marked");
    hold(&rig, WEB, scope_fleet()).expect("held");

    let listed = page(&rig, proto::ListAuditRequest::default());
    let actions: Vec<&str> = listed.entries.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, vec!["mark_bad", "hold"]);
    assert_eq!(listed.next_seq, 2);
}

/// An audit file that cannot be read is `store_failed`, not an empty page.
///
/// Catches: an unreadable audit file answered as "no entries".
#[test]
fn audit_list_of_an_unreadable_audit_file_is_store_failed() {
    let rig = rig("marks-audit-unreadable", "");
    record(&rig, "operator", "exclude", WEB, Some("d1"));
    let file = rig.dir.path().join("desired.json.ops.audit");
    std::fs::remove_file(&file).expect("the audit file exists after a change");
    std::fs::create_dir(&file).expect("a directory in its place");

    assert_eq!(
        rig.coordinator
            .audit_list(&operator(), proto::ListAuditRequest::default()),
        Err(proto::AuditRefusal::StoreFailed)
    );
}

/// Every operator call refuses a CI token as `forbidden` and no token or a bad one as
/// `unauthorized`, changing nothing; and the bearer is mapped to the right caller.
///
/// Catches: a call that lets CI or an anonymous caller steer the rollout, or that answers one with
/// the other's refusal, or a bearer taken for the wrong caller.
#[tokio::test]
async fn every_new_operator_method_refuses_a_ci_token_and_no_token() {
    let rig = rig("marks-callers", "");
    hold(&rig, WEB, scope_fleet()).expect("held");
    let before = ops_bytes(&rig);
    assert!(before.is_some());

    let token = ci_token();
    let ci = rig.coordinator.caller(Some(token.as_str())).await;
    assert!(matches!(ci, Caller::Ci), "{ci:?}");
    assert_all_refused(
        &rig,
        &ci,
        proto::ReleasesRefusal::Forbidden,
        proto::HostsRefusal::Forbidden,
        proto::ServicesRefusal::Forbidden,
        proto::AuditRefusal::Forbidden,
    );
    assert_all_refused(
        &rig,
        &Caller::Anonymous,
        proto::ReleasesRefusal::Unauthorized,
        proto::HostsRefusal::Unauthorized,
        proto::ServicesRefusal::Unauthorized,
        proto::AuditRefusal::Unauthorized,
    );
    assert_eq!(ops_bytes(&rig), before);

    match rig.coordinator.caller(Some(OPERATOR)).await {
        Caller::Operator(name) => assert_eq!(name.as_str(), "operator"),
        other => panic!("the operator token is {other:?}"),
    }
    for bearer in [Some("not-a-token"), None] {
        let caller = rig.coordinator.caller(bearer).await;
        assert!(matches!(caller, Caller::Anonymous), "{caller:?}");
    }
}

/// A store that cannot be written refuses `mark_bad`, `abort`, `exclude` and `hold` as
/// `store_failed`.
///
/// Catches: a failed write answered as done, or as a different refusal.
#[test]
fn a_store_that_cannot_be_written_refuses_as_store_failed() {
    let mut rig = rig("marks-store-failed", "");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(OpsState::default()));
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );

    assert_eq!(
        rig.coordinator.mark_bad(&operator(), mark_of(WEB, "2.0")),
        Err(proto::ReleasesRefusal::StoreFailed)
    );
    assert_eq!(
        rig.coordinator.abort(&operator(), abort_of("d1", WEB)),
        Err(proto::HostsRefusal::StoreFailed)
    );
    assert_eq!(
        rig.coordinator
            .exclude(&operator(), exclude_of("d1", WEB, false)),
        Err(proto::HostsRefusal::StoreFailed)
    );
    assert_eq!(
        hold(&rig, WEB, scope_fleet()),
        Err(proto::ServicesRefusal::StoreFailed)
    );
}

/// A store already holding `MAX_EXCLUDED` other exclusions refuses another host as `capacity`,
/// and still takes a change to a host it already excludes.
///
/// Catches: a cap that is not checked (the file grows without bound) or that refuses the edit of an
/// exclusion that stands.
#[test]
fn exclude_past_the_cap_is_capacity() {
    let rig = rig("marks-capacity", "");
    rig.coordinator
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("operator", "exclude"),
            |state| {
                state.excluded = (0..MAX_EXCLUDED)
                    .map(|n| Exclusion {
                        service: WEB.to_owned(),
                        node_id: format!("x{n}"),
                        reason: "seed".to_owned(),
                    })
                    .collect();
            },
        )
        .expect("seeded");

    assert_eq!(
        rig.coordinator
            .exclude(&operator(), exclude_of("d1", WEB, false)),
        Err(proto::HostsRefusal::Capacity)
    );
    assert_eq!(status(&rig).ops.excluded.len(), MAX_EXCLUDED);
    rig.coordinator
        .exclude(&operator(), exclude_of("x0", WEB, false))
        .expect("an exclusion that stands may be changed");
}

/// `exclude` and `abort` refuse a service that is not managed, a node id that is none and a
/// reason that is blank, each with its own refusal, and change nothing.
///
/// Catches: a request taken before it is checked, or a refusal that names the wrong field.
#[test]
fn exclude_and_abort_refuse_a_bad_request_and_change_nothing() {
    let rig = rig("marks-validation", "");
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    let before = ops_bytes(&rig);
    let c = &rig.coordinator;

    assert_eq!(
        c.exclude(&operator(), exclude_of("d1", "nope", false)),
        Err(proto::HostsRefusal::UnknownService)
    );
    assert_eq!(
        c.exclude(&operator(), exclude_of("Bad_Node", WEB, false)),
        Err(proto::HostsRefusal::InvalidRequest)
    );
    let mut blank = exclude_of("d1", WEB, false);
    blank.reason = String::new();
    assert_eq!(
        c.exclude(&operator(), blank),
        Err(proto::HostsRefusal::InvalidRequest)
    );

    assert_eq!(
        c.abort(&operator(), abort_of("Bad_Node", WEB)),
        Err(proto::HostsRefusal::InvalidRequest)
    );
    assert_eq!(
        c.abort(&operator(), abort_of("d1", "nope")),
        Err(proto::HostsRefusal::UnknownService)
    );
    let mut blank = abort_of("d1", WEB);
    blank.reason = "\u{7}".to_owned();
    assert_eq!(
        c.abort(&operator(), blank),
        Err(proto::HostsRefusal::InvalidRequest)
    );
    assert_eq!(ops_bytes(&rig), before);
}
