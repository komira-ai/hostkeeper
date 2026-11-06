//! The `install` order of the rollout gate (design 5.4 and 5.7): a service assigned to a host
//! that does not run it is ordered `install S V D` at its next heartbeat, under every rule an
//! update is under, taking and freeing the one live update slot as an update does; a service not
//! assigned to the host is never ordered; minimum versions per role and host. Every time is the
//! rig's clock; every transport is the in-process one, with the host's identity explicit.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::min_version::Scope as MinScope;
use hostkeeper_wire::wire::{OrderId, Outcome, ServiceBeat, ServiceReply, UpdateResult};
use serde_json::json;

use crate::assign_support::{
    absent, assign, install, order_to, set_min, unassign, uninstall_order, why_of,
};
use crate::decide::{Gate, Skip, Why};
use crate::marks_support::{
    DB, INSTALLER, NEW_DAEMON, WEB, ask, ask_typed, beat_of, digest, hold, operator, restart, rig,
    runs, scope_host, set, unhold, update,
};
use crate::orders::{INSTALL_FEATURE, Order};
use crate::testutil::{beat, id, version};

/// Catches: the old `absent` rule still in force (an absent copy skipped, or ordered `update`
/// over the JSON route), or an install sent as an update: a daemon that does not run the
/// service, or lists it absent, is ordered `install` with the desired version and its digest,
/// and the JSON reply, which carries no install, says nothing.
#[tokio::test]
async fn an_assigned_service_not_installed_gets_an_install_order() {
    let rig = rig("install-ordered", "");
    set(&rig, WEB, "2.0", 7).await;
    assert_eq!(
        ask(&rig, "d1", "daemon", beat_of(INSTALLER, vec![])),
        ServiceReply::Nothing
    );
    let order = order_to(&rig, "d1", vec![]).expect("an install");
    assert_eq!(order, install("2.0", 7));
    let typed = order.to_proto();
    assert_eq!(typed.order_id, format!("install-{}", "07".repeat(12)));
    assert_eq!(typed.r#try, 1);
    assert_eq!(
        typed.verb,
        Some(agent::order::Verb::Install(agent::Install {
            service: WEB.to_owned(),
            version: "2.0".to_owned(),
            package_digest: digest(7),
            index_digest: String::new(),
        }))
    );

    let rig = crate::marks_support::rig("install-absent", "");
    set(&rig, WEB, "2.0", 7).await;
    assert_eq!(
        ask(
            &rig,
            "d2",
            "daemon",
            beat_of(INSTALLER, vec![absent(WEB, "1.0")])
        ),
        ServiceReply::Nothing,
        "no update for an absent copy"
    );
    assert_eq!(
        order_to(&rig, "d2", vec![absent(WEB, "1.0")]),
        Some(install("2.0", 7))
    );
}

/// Catches: an install ordered to a daemon that cannot take it, of a version with no digest, or
/// a try that does not follow the verdicts: a daemon that did not announce `install-v1` is told
/// nothing and the gate says `unsupported`; a desired version CI set without a digest is
/// `no_digest`; a verdict of the install raises the try of the next.
#[tokio::test]
async fn an_install_needs_a_daemon_that_takes_it_and_a_digest() {
    let rig = rig("install-needs", "");
    rig.set(r#"{"service":"web","version":"2.0"}"#).await;
    assert_eq!(order_to(&rig, "d1", vec![absent(WEB, "1.0")]), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::NoDigest)));
    set(&rig, WEB, "2.1", 3).await;
    assert_eq!(
        ask_typed(
            &rig,
            "d1",
            "daemon",
            beat_of(NEW_DAEMON, vec![absent(WEB, "1.0")])
        ),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Skip(Skip::Unsupported)));
    assert!(!NEW_DAEMON.contains(&INSTALL_FEATURE));

    // An agent that does not list every service it runs is not told to install one it leaves
    // out.
    let mut old = beat("1.0");
    old.agent_features = vec![INSTALL_FEATURE.to_owned()];
    old.services = Some(Vec::new());
    assert_eq!(
        ask_typed(&rig, "d3", "daemon", old),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(why_of(&rig, WEB, "d3"), Some(Why::Skip(Skip::NotAMember)));

    let Some(Order::Install { order_id, .. }) = order_to(&rig, "d1", vec![absent(WEB, "1.0")])
    else {
        panic!("an install");
    };
    let mut reported = UpdateResult::new(version("2.1"), Outcome::Failed);
    reported.order_id = OrderId::parse(&order_id);
    reported.order_try = Some(4);
    let mut said = absent(WEB, "1.0");
    said.last_update = Some(reported);
    assert_eq!(rig.coordinator.lock_fleet().next_try(WEB, &id("d1")), 1);
    order_to(&rig, "d1", vec![said]);
    assert_eq!(rig.coordinator.lock_fleet().next_try(WEB, &id("d1")), 5);
}

/// Catches: a service ordered to a host it is not assigned to, installed or not, or one ordered
/// to the hosts of a role the operator took it off: only the hosts it is assigned to are told.
#[tokio::test]
async fn a_service_not_assigned_to_the_host_is_never_ordered() {
    let rig = rig("install-unassigned", "");
    set(&rig, WEB, "2.0", 1).await;
    set(&rig, DB, "2.0", 2).await;
    for services in [Vec::new(), vec![runs(WEB, "1.0"), runs(DB, "1.0")]] {
        assert_eq!(
            ask_typed(&rig, "f1", "frontend", beat_of(INSTALLER, services.clone())),
            (ServiceReply::Nothing, None)
        );
        assert_eq!(
            ask(&rig, "f1", "frontend", beat_of(INSTALLER, services)),
            ServiceReply::Nothing
        );
    }
    assert_eq!(why_of(&rig, WEB, "f1"), Some(Why::Skip(Skip::NotAMember)));

    unassign(&rig, WEB, "", "daemon", false).expect("taken off the daemons");
    assert_eq!(order_to(&rig, "d1", vec![runs(DB, "2.0")]), None);
    assert_eq!(
        order_to(&rig, "d2", vec![runs(WEB, "1.0"), runs(DB, "2.0")]),
        None
    );
    assert_eq!(
        ask(
            &rig,
            "d2",
            "daemon",
            beat_of(INSTALLER, vec![runs(WEB, "1.0")])
        ),
        ServiceReply::Nothing
    );
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Skip(Skip::NotAMember)));
    let (_, order) = ask_typed(
        &rig,
        "s1",
        "storage",
        beat_of(INSTALLER, vec![runs(DB, "2.0")]),
    );
    assert_eq!(
        order,
        Some(install("2.0", 1)),
        "the storage host is assigned"
    );
}

/// Why the gate holds `web` on `d1` under the rule named `rule`, when `d1` runs web 1.0
/// (`installed`) and when it does not (it lists web absent where the rule needs a report of it,
/// and leaves it out otherwise), with `d1`'s own heartbeat ordered nothing.
async fn judged(rule: &str, installed: bool) -> Option<Why> {
    let extra = match rule {
        "canary_wait" => "canary = { count = 1 }",
        "baking" => "bake_beats = 2",
        _ => "",
    };
    let mut rig = rig(&format!("install-rule-{rule}-{installed}"), extra);
    if rule == "below_min_version" {
        set_min(&rig, WEB, MinScope::Role("daemon".to_owned()), "3.0").expect("a minimum");
    }
    set(&rig, WEB, "2.0", 1).await;
    let reported = installed || matches!(rule, "updating" | "failed_here" | "deferred");
    let mut web: ServiceBeat = if installed {
        runs(WEB, "1.0")
    } else {
        absent(WEB, "1.0")
    };
    let mut others = Vec::new();
    let busy = || {
        let mut db = runs(DB, "1.0");
        db.updating = Some(version("2.0"));
        db
    };
    match rule {
        "halted" => {
            rig.coordinator
                .halt(
                    &operator(),
                    proto::HaltRequest {
                        service: WEB.to_owned(),
                        reason: "disk tainted".to_owned(),
                    },
                )
                .expect("halted");
        }
        "excluded" => {
            rig.coordinator
                .exclude(
                    &operator(),
                    proto::ExcludeRequest {
                        service: WEB.to_owned(),
                        node_id: "d1".to_owned(),
                        reason: "bad disk".to_owned(),
                        abandon: false,
                    },
                )
                .expect("excluded");
        }
        "held" => {
            hold(&rig, WEB, scope_host("d1")).expect("held");
        }
        "bad" => {
            rig.coordinator
                .mark_bad(
                    &operator(),
                    proto::MarkBadRequest {
                        service: WEB.to_owned(),
                        version: "2.0".to_owned(),
                        reason: "broken".to_owned(),
                    },
                )
                .expect("marked");
        }
        "updating" => web.updating = Some(version("2.0")),
        "busy_elsewhere" => others.push(busy()),
        "no_slot" => {
            order_to(&rig, "d2", vec![runs(WEB, "2.0"), busy()]);
        }
        "quiet" => restart(&mut rig, extra),
        "canary_wait" => {
            let canary = beat_of(INSTALLER, vec![runs(WEB, "1.0")]);
            let (reply, _) = ask_typed(&rig, "d0", "daemon", canary);
            assert_eq!(reply, update(WEB, "2.0"), "d0 is the canary");
        }
        "baking" => {
            order_to(&rig, "d2", vec![runs(WEB, "2.0")]);
        }
        "failed_here" => {
            web.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Failed));
        }
        "deferred" => {
            web.deferred = Some(
                serde_json::from_value(json!({
                    "target": "2.0", "reason": "drain_bound", "retry_after_secs": 300
                }))
                .expect("a deferral"),
            );
        }
        "below_min_version" => {}
        other => panic!("no rule {other}"),
    }
    if reported {
        others.insert(0, web);
    }
    assert_eq!(order_to(&rig, "d1", others), None, "{rule}");
    why_of(&rig, WEB, "d1")
}

/// Catches: a rule of the gate skipped for an install: each rule that holds an update of a host
/// holds the install of the same host, with the same reason, the host's role, its slot and its
/// reports alike. (The coordinator has no pending or cloned-disk host to order: enrollment is
/// not wired into it yet.)
#[tokio::test]
async fn install_obeys_every_rule_an_update_obeys() {
    let rules = [
        ("halted", Why::Gate(Gate::Halted)),
        ("excluded", Why::Skip(Skip::NotAMember)),
        ("held", Why::Gate(Gate::Held)),
        ("bad", Why::Gate(Gate::Bad)),
        ("below_min_version", Why::Gate(Gate::BelowMinVersion)),
        ("updating", Why::Gate(Gate::Updating)),
        ("busy_elsewhere", Why::Gate(Gate::BusyElsewhere)),
        ("no_slot", Why::Gate(Gate::NoSlot)),
        ("quiet", Why::Gate(Gate::Quiet)),
        ("canary_wait", Why::Gate(Gate::CanaryWait)),
        ("baking", Why::Gate(Gate::Baking)),
        ("failed_here", Why::Skip(Skip::FailedHere)),
        ("deferred", Why::Skip(Skip::Deferred)),
    ];
    for (rule, why) in rules {
        assert_eq!(
            judged(rule, true).await,
            Some(why),
            "an update under {rule}"
        );
        assert_eq!(
            judged(rule, false).await,
            Some(why),
            "an install under {rule}"
        );
    }
}

/// Catches: an install that does not hold the one live update, or never frees it: while `d1`
/// has an install out, `d2` is held at `no_slot` and `d1`'s other service at `busy_elsewhere`;
/// asked again, `d1` is told the same install; once `d1` runs the version, healthy, `d2` is
/// ordered.
#[tokio::test]
async fn an_install_takes_and_frees_the_one_live_update_slot() {
    let rig = rig("install-slot", "");
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(order_to(&rig, "d1", vec![]), Some(install("2.0", 1)));
    assert_eq!(order_to(&rig, "d2", vec![runs(WEB, "1.0")]), None);
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::NoSlot)));
    set(&rig, DB, "2.0", 2).await;
    assert_eq!(
        order_to(&rig, "d1", vec![runs(DB, "1.0")]),
        Some(install("2.0", 1)),
        "asked again under its own order, the host is told the same"
    );
    assert_eq!(why_of(&rig, DB, "d1"), Some(Why::Gate(Gate::BusyElsewhere)));

    let mut done = runs(WEB, "2.0");
    done.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Succeeded));
    assert_eq!(order_to(&rig, "d1", vec![done, runs(DB, "2.0")]), None);
    let (reply, order) = ask_typed(
        &rig,
        "d2",
        "daemon",
        beat_of(INSTALLER, vec![runs(WEB, "1.0"), runs(DB, "2.0")]),
    );
    assert_eq!(order, None);
    assert_eq!(reply, update(WEB, "2.0"));
}

/// Catches: an install or an uninstall ordered to a pair an operator holds, or a release that
/// does not go to the version desired at its time: a held host that does not run the service is
/// told nothing, and one ordered to uninstall it is held too; released, each is told the then
/// desired version, or its uninstall.
#[tokio::test]
async fn a_hold_stops_install_and_uninstall_orders() {
    let rig = rig("install-held", "");
    set(&rig, WEB, "2.0", 1).await;
    hold(&rig, WEB, scope_host("d1")).expect("held");
    assert_eq!(order_to(&rig, "d1", vec![]), None);
    assert_eq!(why_of(&rig, WEB, "d1"), Some(Why::Gate(Gate::Held)));
    set(&rig, WEB, "3.0", 2).await;
    assert_eq!(order_to(&rig, "d1", vec![]), None);
    unhold(&rig, WEB, scope_host("d1")).expect("released");
    assert_eq!(order_to(&rig, "d1", vec![]), Some(install("3.0", 2)));

    let rig = crate::marks_support::rig("uninstall-held", "");
    assert_eq!(order_to(&rig, "d2", vec![runs(WEB, "1.0")]), None);
    hold(&rig, WEB, scope_host("d2")).expect("held");
    unassign(&rig, WEB, "d2", "", true).expect("unassigned");
    assert_eq!(order_to(&rig, "d2", vec![runs(WEB, "1.0")]), None);
    assert_eq!(why_of(&rig, WEB, "d2"), Some(Why::Gate(Gate::Held)));
    unhold(&rig, WEB, scope_host("d2")).expect("released");
    assert_eq!(
        order_to(&rig, "d2", vec![runs(WEB, "1.0")]),
        Some(uninstall_order(WEB, "uninstall-1", 1))
    );
}

/// Catches: a host's minimum that does not stand before its role's, one that cannot be removed,
/// a minimum above the desired version accepted, or a minimum that does not hold back an
/// install: the rollout gate holds each host below the minimum that applies to it.
#[tokio::test]
async fn set_min_version_per_host_overrides_the_role_value_and_refuses_above_desired() {
    let rig = rig("install-min", "");
    let role = || MinScope::Role("daemon".to_owned());
    let host = |id: &str| MinScope::Host(id.to_owned());
    order_to(&rig, "d1", vec![runs(WEB, "1.0")]);
    order_to(&rig, "d2", vec![runs(WEB, "1.0")]);
    set(&rig, WEB, "2.0", 1).await;
    assert_eq!(
        set_min(&rig, WEB, role(), "2.5").expect_err("above the desired version"),
        proto::ServicesRefusal::BelowMinVersion
    );
    set_min(&rig, WEB, role(), "1.5").expect("the role's");
    let entry = set_min(&rig, WEB, host("d1"), "2.0")
        .expect("the host's")
        .entry
        .expect("audited");
    assert_eq!(entry.action, "set_min_version");
    assert_eq!(entry.reason.as_deref(), Some("host d1: 2.0"));
    assert_eq!(entry.node_id.as_deref(), Some("d1"));

    set(&rig, WEB, "1.8", 2).await;
    assert_eq!(order_to(&rig, "d1", vec![runs(WEB, "1.0")]), None);
    assert_eq!(
        why_of(&rig, WEB, "d1"),
        Some(Why::Gate(Gate::BelowMinVersion))
    );
    assert_eq!(
        why_of(&rig, WEB, "d2"),
        None,
        "the role's minimum lets 1.8 through"
    );
    assert_eq!(why_of(&rig, WEB, "d9"), Some(Why::Skip(Skip::NotAMember)));

    let removed = set_min(&rig, WEB, host("d1"), "")
        .expect("removed")
        .entry
        .expect("audited");
    assert_eq!(removed.reason.as_deref(), Some("host d1: none"));
    assert_eq!(why_of(&rig, WEB, "d1"), None, "back to the role's minimum");

    for bad in ["1.x", "not a version"] {
        assert_eq!(
            set_min(&rig, WEB, host("d1"), bad).expect_err("refused"),
            proto::ServicesRefusal::InvalidRequest,
            "{bad}"
        );
    }
    // An install is held below the floor as an update is.
    assert_eq!(
        set_min(&rig, WEB, host("d3"), "1.9").expect_err("d3 is unknown"),
        proto::ServicesRefusal::UnknownHost
    );
    assign(&rig, WEB, "", "storage").expect("assigned already");
    order_to(&rig, "d3", vec![runs(DB, "1.0")]);
    set_min(&rig, WEB, host("d3"), "1.8").expect("known now");
    set(&rig, WEB, "1.7", 3).await;
    assert_eq!(order_to(&rig, "d3", vec![runs(DB, "1.0")]), None);
    assert_eq!(
        why_of(&rig, WEB, "d3"),
        Some(Why::Gate(Gate::BelowMinVersion))
    );
}
