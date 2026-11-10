//! Which services' rollouts take the farm's `--live-rollouts` (`live.rs`): only a service that
//! is moving, one of its hosts updating, under an order or baking, or its plan under way. A
//! failed host keeps a slot of its own service, against its role's `max_updating`, but a halted
//! service, or one whose only occupants are failed hosts with nothing ordered or updating, holds
//! no other service's rollout. What `Releases.Status` names when a plan waits
//! `rollout_elsewhere`. Every time is explicit; the fleets are built beat by beat.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, Outcome};

use crate::decide::{Decision, Gate};
use crate::desired::Desired;
use crate::fleet::{Fleet, Marks};
use crate::holds::{HoldMark, HoldScope};
use crate::rollouts::plan_proto;
use crate::services::Services;
use crate::stages::{HostState, View, is_live};
use crate::testutil::{
    beat, decide_why, id, last, native, role, secs, unhealthy, updating, version,
};

/// `a` and `b` run on daemons; `a` bakes for two beats, `b` for none. `c` runs on daemons too.
/// All have the default plan.
const SERVICES: &str = "[[service]]\nname = \"a\"\nbake_beats = 2\nroles = [\"daemon\"]\n\
                        [[service]]\nname = \"b\"\nroles = [\"daemon\"]\n\
                        [[service]]\nname = \"c\"\nroles = [\"daemon\"]\n";

fn services() -> Services {
    Services::parse(SERVICES).expect("services")
}

/// A desired state of `target` with the `max_updating` of the daemons given.
fn want(target: &str, max_updating: u32) -> Desired {
    let text = format!(
        r#"{{"version":"{target}","max_updating":{{"daemon":{max_updating}}},"min_up":{{"daemon":0}}}}"#
    );
    Desired::parse(text.as_bytes()).expect("parsed")
}

/// `a` wants 2.0 and `b` wants 5.0; `c` has no desired state.
fn all() -> BTreeMap<String, Desired> {
    BTreeMap::from([
        ("a".to_owned(), want("2.0", 2)),
        ("b".to_owned(), want("5.0", 2)),
    ])
}

/// A fleet past its quiet start with `--live-rollouts` 1, in which `a` runs on `a1` as `a_beat`
/// says and `b` runs 1.0 on `b1` and `b2`, all heard at 100 s.
fn farm(a_beat: Heartbeat) -> Fleet {
    let mut f = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    f.record_in("a", id("a1"), role("daemon"), a_beat, secs(100));
    for host in ["b1", "b2"] {
        f.record_in("b", id(host), role("daemon"), beat("1.0"), secs(100));
    }
    f
}

/// The decision for `host` of `service` at 100 s.
fn why(f: &Fleet, service: &str, host: &str) -> Decision {
    decide_why(&services(), &all(), service, f, &id(host), secs(100))
}

fn go_b() -> Decision {
    Decision::Update {
        version: version("5.0"),
    }
}

fn elsewhere() -> Decision {
    Decision::Gate(Gate::RolloutElsewhere)
}

/// Whether `a`'s rollout is live at 100 s.
fn a_live(f: &Fleet) -> bool {
    let s = services();
    is_live(
        s.get("a").expect("a"),
        all().get("a"),
        &native(f, &all()),
        secs(100),
    )
}

/// The plan `Releases.Status` answers for `service`, halted or not.
fn plan(f: &Fleet, service: &str, halted: bool) -> proto::RolloutPlan {
    let s = services();
    let all = all();
    let def = s.get(service).expect("the service");
    plan_proto(
        &s,
        &all,
        def,
        &all[service],
        halted,
        &native(f, &all),
        secs(100),
    )
}

/// A heartbeat of a host whose update or install of `a` to 2.0 failed and that is unhealthy,
/// running `running`: 1.0 after a rollback, 2.0 when the failure halted and left it installed.
fn failed(running: &str) -> Heartbeat {
    unhealthy(last(beat(running), "2.0", Outcome::Failed))
}

fn halt(f: &mut Fleet, service: &str) {
    f.set_marks(Marks {
        halted: BTreeSet::from([service.to_owned()]),
        ..Marks::default()
    });
}

/// Behaviour: `a`'s only host failed its update and `a` is halted; `b`'s desired state is set and
/// both its hosts are ordered, and its plan waits for nothing elsewhere. Held both ways the
/// failure leaves the host: rolled back to 1.0, or halted at 2.0 with the service installed and
/// never started (unhealthy at the version it failed). A halt also ends a bake's hold: a host
/// healthy at 2.0 that has not finished baking holds `b` until `a` is halted. Catches a halted
/// rollout that orders nothing keeping the farm's one live rollout from every other service.
#[test]
fn a_halted_service_whose_only_host_failed_holds_no_other_services_rollout() {
    for running in ["1.0", "2.0"] {
        let mut f = farm(failed(running));
        halt(&mut f, "a");
        assert!(!a_live(&f), "a at {running}");
        assert_eq!(why(&f, "b", "b1"), go_b(), "b1, a at {running}");
        assert_eq!(why(&f, "b", "b2"), go_b(), "b2, a at {running}");
        let b = plan(&f, "b", false);
        assert_eq!(
            b.waiting,
            i32::from(proto::PlanWait::None),
            "a at {running}"
        );
        assert_eq!(b.waiting_on, Vec::<String>::new(), "a at {running}");
    }
    let mut f = farm(beat("2.0"));
    assert!(a_live(&f), "a1 bakes 2.0");
    halt(&mut f, "a");
    assert!(!a_live(&f), "a halted while a1 bakes");
    assert_eq!(why(&f, "b", "b1"), go_b(), "a halted while a1 bakes");
}

/// Behaviour: as above with `a` not halted: its failed host is still unhealthy and nothing is
/// ordered or updating. `b`'s hosts are ordered all the same, and `a`'s failed host still
/// counts against `a`'s own `max_updating`: with a limit of 1 and a newer version, `a`'s other
/// host is held with `no_slot`. Catches a failed host that holds the others' rollouts while its
/// own service is not halted, and a failure that stops counting against its own service.
#[test]
fn a_failed_unhealthy_host_with_nothing_ordered_holds_no_other_services_rollout() {
    for running in ["1.0", "2.0"] {
        let f = farm(failed(running));
        assert!(!a_live(&f), "a at {running}");
        assert_eq!(why(&f, "b", "b1"), go_b(), "a at {running}");
        assert_eq!(why(&f, "b", "b2"), go_b(), "a at {running}");
    }
    let mut f = farm(failed("1.0"));
    f.record_in("a", id("a2"), role("daemon"), beat("1.0"), secs(100));
    let newer = BTreeMap::from([("a".to_owned(), want("3.0", 1))]);
    assert_eq!(
        decide_why(&services(), &newer, "a", &f, &id("a2"), secs(100)),
        Decision::Gate(Gate::NoSlot)
    );
}

/// Behaviour, the converse that stays: while `a` genuinely moves a host, `b` waits with
/// `rollout_elsewhere` and its plan names `a`: `a1` under an order not yet reported, updating,
/// with its promotion pending, or baking 2.0 (one healthy beat of two). A failed host beside a
/// host that updates does not free the slot either, nor does a halt while an update runs.
/// Catches the fix freeing the farm's slot from a service that is in the middle of an update.
#[test]
fn while_a_genuinely_updates_or_bakes_a_host_b_waits_and_its_plan_names_a() {
    let mut pending = beat("2.0");
    pending.promotion_pending = true;
    let cases: [(&str, Heartbeat); 3] = [
        ("updating", updating(beat("1.0"), "2.0")),
        ("pending", pending),
        ("baking", beat("2.0")),
    ];
    let mut fleets: Vec<(&str, Fleet)> = cases.into_iter().map(|(c, b)| (c, farm(b))).collect();
    let mut ordered = farm(beat("1.0"));
    ordered.order("a", id("a1"), version("2.0"), secs(100));
    fleets.push(("ordered", ordered));
    let mut beside = farm(failed("1.0"));
    beside.record_in(
        "a",
        id("a2"),
        role("daemon"),
        updating(beat("1.0"), "2.0"),
        secs(100),
    );
    fleets.push(("failed beside an update", beside.clone()));
    halt(&mut beside, "a");
    fleets.push(("halted mid-update", beside));
    for (case, f) in fleets {
        assert!(a_live(&f), "{case}");
        assert_eq!(why(&f, "b", "b1"), elsewhere(), "{case}");
        let b = plan(&f, "b", false);
        assert_eq!(
            (b.waiting, b.waiting_on),
            (
                i32::from(proto::PlanWait::RolloutElsewhere),
                vec!["a".to_owned()]
            ),
            "{case}"
        );
    }
}

/// Behaviour: the host of the incident, which failed its first install of `a` at 2.0 and
/// reports it unhealthy at 2.0, is `failed` in `a`'s plan (not `updating`), so `a`'s plan, with a
/// host left to roll, is held by it (`stage_failed`) and is not live: `b` is ordered. Once it
/// beats healthy it is no longer `failed`. Catches a halted install read as an update still
/// running or a bake, which holds every other service until an operator abandons the host.
#[test]
fn a_host_left_unhealthy_at_the_version_it_failed_is_failed_and_bakes_nothing() {
    let mut f = farm(failed("2.0"));
    f.record_in("a", id("a2"), role("daemon"), beat("1.0"), secs(100));
    let s = services();
    let def = s.get("a").expect("a");
    let view = View::effective(def, &all()["a"], None, &native(&f, &all()), secs(100));
    let a1 = view.members.iter().find(|m| m.id == id("a1")).expect("a1");
    assert_eq!(a1.state, HostState::Failed);
    assert_eq!(why(&f, "a", "a2"), Decision::Gate(Gate::StageFailed));
    assert!(!a_live(&f));
    assert_eq!(why(&f, "b", "b1"), go_b());
    assert_eq!(
        plan(&f, "a", false).waiting,
        i32::from(proto::PlanWait::FailedHost)
    );

    let mut healthy = failed("2.0");
    healthy.health = hostkeeper_wire::wire::Health::Healthy;
    let f = farm(healthy);
    let view = View::effective(def, &all()["a"], None, &native(&f, &all()), secs(100));
    assert_ne!(view.members[0].state, HostState::Failed, "healthy again");
}

/// Behaviour: a host left unhealthy at the version it failed is `updating` again, and `a` is
/// live again, once it runs that version's update a second time: under a new order of it, or
/// reporting it updating. Catches a failed host read as failed while it runs an update, which
/// would let another service roll beside a live update.
#[test]
fn a_host_left_at_the_version_it_failed_updates_again_under_a_new_order() {
    let s = services();
    let def = s.get("a").expect("a");
    let mut ordered = farm(failed("2.0"));
    ordered.order("a", id("a1"), version("2.0"), secs(100));
    let again = farm(updating(failed("2.0"), "2.0"));
    for (case, f) in [("ordered", ordered), ("updating", again)] {
        let view = View::effective(def, &all()["a"], None, &native(&f, &all()), secs(100));
        assert_eq!(view.members[0].state, HostState::Updating, "{case}");
        assert!(a_live(&f), "{case}");
        assert_eq!(why(&f, "b", "b1"), elsewhere(), "{case}");
    }
}

/// Behaviour: with `--live-rollouts` 2, `c` (no desired state, a host updating) and `a` (a host
/// updating) are both live; `b` waits and its plan names both, in name order. With one of them
/// live, `b` does not wait and names none; a halted plan of `b` names none either, whatever is
/// live. Catches a name list that is set while the plan does not wait elsewhere, and one that
/// leaves a live service out.
#[test]
fn a_plan_that_waits_elsewhere_names_every_live_service_and_no_other_does() {
    let mut f = farm(updating(beat("1.0"), "2.0"));
    f.set_live_rollouts(2);
    f.record_in(
        "c",
        id("c1"),
        role("daemon"),
        updating(beat("1.0"), "2.0"),
        secs(100),
    );
    let b = plan(&f, "b", false);
    assert_eq!(
        (b.waiting, b.waiting_on),
        (
            i32::from(proto::PlanWait::RolloutElsewhere),
            vec!["a".to_owned(), "c".to_owned()]
        )
    );
    assert_eq!(plan(&f, "b", true).waiting_on, Vec::<String>::new());

    f.record_in("c", id("c1"), role("daemon"), beat("2.0"), secs(100));
    let b = plan(&f, "b", false);
    assert_eq!(b.waiting, i32::from(proto::PlanWait::None));
    assert_eq!(b.waiting_on, Vec::<String>::new());
}

/// Behaviour: `a1` is healthy at 2.0 with its bake not finished, so `a` is live and `b` waits;
/// the bake no longer holds `b` once an operator excludes `a1` from `a`, holds `a` on it, or
/// marks 2.0 of `a` bad. A held host whose promotion is pending holds `b` no more than a held
/// bake does. Catches a bake an operator took out of the plan, or of a version marked bad, and
/// a held host's pending promotion, keeping the farm's one live rollout.
#[test]
fn a_bake_an_operator_took_out_of_the_plan_or_marked_bad_holds_no_other_service() {
    let baking = farm(beat("2.0"));
    assert!(a_live(&baking));
    assert_eq!(why(&baking, "b", "b1"), elsewhere());
    let marks = [
        (
            "excluded",
            Marks {
                excluded: BTreeSet::from([("a".to_owned(), "a1".to_owned())]),
                ..Marks::default()
            },
        ),
        (
            "held",
            Marks {
                holds: vec![hold_of_a1()],
                ..Marks::default()
            },
        ),
        (
            "marked bad",
            Marks {
                bad: BTreeMap::from([("a".to_owned(), vec![version("2.0")])]),
                ..Marks::default()
            },
        ),
    ];
    for (case, marks) in marks {
        let mut f = baking.clone();
        f.set_marks(marks);
        assert!(!a_live(&f), "{case}");
        assert_eq!(why(&f, "b", "b1"), go_b(), "{case}");
    }

    let mut pending = beat("2.0");
    pending.promotion_pending = true;
    let mut f = farm(pending);
    assert!(a_live(&f), "a1's promotion pending");
    f.set_marks(Marks {
        holds: vec![hold_of_a1()],
        ..Marks::default()
    });
    assert!(!a_live(&f), "a1 held with its promotion pending");
    assert_eq!(
        why(&f, "b", "b1"),
        go_b(),
        "a1 held with its promotion pending"
    );
}

/// A hold of `a` on `a1`, as the ops file keeps it.
fn hold_of_a1() -> HoldMark {
    HoldMark {
        service: "a".to_owned(),
        scope: HoldScope::Host("a1".to_owned()),
        reason: "investigating".to_owned(),
        by: "operator".to_owned(),
        at_unix: 1_800_000_000,
    }
}
