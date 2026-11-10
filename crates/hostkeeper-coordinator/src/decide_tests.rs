//! One test per rule of the decision, each with the hosts that make the rule matter.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::RoleName;
use hostkeeper_wire::wire::{Outcome, Reply};

use crate::decide::{Decision, Gate};
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::services::{ServiceDef, Services};
use crate::testutil::{
    KBF_ENTRY, LEGACY, beat, decide, decide_in, decide_why, desired, fleet, id, kbf_def,
    kbf_services, last, role, secs, unhealthy, updating, version,
};

const NOW: Duration = Duration::from_secs(1_000);

fn update(target: &str) -> Reply {
    Reply::Update {
        version: version(target),
    }
}

fn ask(d: &Desired, f: &Fleet, host: &str) -> Reply {
    decide(d, f, &id(host), NOW)
}

/// The recorded kbf service rolled in one stage (`stages = [100]`): every eligible host is in
/// the stage at once, so only the desired state's per-role limits (`max_updating`, `min_up`)
/// hold an order.
fn kbf_whole() -> Services {
    Services::parse(&format!("{KBF_ENTRY}\nstages = [100]\n")).expect("services")
}

fn whole_wants(d: &Desired) -> BTreeMap<String, Desired> {
    BTreeMap::from([(LEGACY.to_owned(), d.clone())])
}

/// What `host` is told under `d` when kbf rolls in one stage.
fn ask_whole(d: &Desired, f: &Fleet, host: &str) -> Reply {
    decide_in(&kbf_whole(), &whole_wants(d), LEGACY, f, &id(host), NOW)
}

/// Why `host` is held under `d` when kbf rolls in one stage.
fn why_whole(d: &Desired, f: &Fleet, host: &str) -> Decision {
    decide_why(&kbf_whole(), &whole_wants(d), LEGACY, f, &id(host), NOW)
}

/// Catches: a host that runs an older version than the desired one not being told to update,
/// and a reply that carries anything but the desired version.
#[test]
fn a_host_behind_the_desired_version_is_told_to_update() {
    let f = fleet(NOW, &[("d1", role("daemon"), beat("1.0"))]);
    assert_eq!(ask(&desired("2.0", 1, 1), &f, "d1"), update("2.0"));
}

/// Catches: a host that already runs the desired version being told to update.
#[test]
fn a_host_at_the_desired_version_is_told_nothing() {
    let f = fleet(NOW, &[("d1", role("daemon"), beat("2.0"))]);
    assert_eq!(ask(&desired("2.0", 1, 1), &f, "d1"), Reply::Nothing);
}

/// Catches: a host that is in the middle of an update being told to update again.
#[test]
fn a_host_that_is_updating_is_told_nothing() {
    let f = fleet(NOW, &[("d1", role("daemon"), updating(beat("1.0"), "2.0"))]);
    assert_eq!(ask(&desired("2.0", 1, 1), &f, "d1"), Reply::Nothing);
}

/// Catches: a host the coordinator has never heard from being told anything.
#[test]
fn a_host_that_has_not_heartbeated_is_told_nothing() {
    let f = fleet(NOW, &[("d1", role("daemon"), beat("1.0"))]);
    assert_eq!(ask(&desired("2.0", 1, 1), &f, "d2"), Reply::Nothing);
}

/// Catches: the hosts that are updating not being counted, so a second daemon starts while
/// one is updating under a daemon limit of one; and the slot not freeing when the update ends.
/// With the default plan the second daemon also waits, for the first stage, which one host
/// fills.
#[test]
fn a_host_updating_holds_the_slot_of_its_role_until_it_is_done() {
    let with = |d1: hostkeeper_wire::wire::Heartbeat| {
        fleet(
            NOW,
            &[
                ("d1", role("daemon"), d1),
                ("d2", role("daemon"), beat("1.0")),
            ],
        )
    };
    let mut d = desired("2.0", 1, 1);
    assert_eq!(
        ask(&d, &with(updating(beat("1.0"), "2.0")), "d2"),
        Reply::Nothing,
        "the first stage holds one of the two hosts"
    );
    d.max_updating.set("daemon", 1);
    assert_eq!(
        ask_whole(&d, &with(updating(beat("1.0"), "2.0")), "d2"),
        Reply::Nothing,
        "one daemon updating fills a daemon limit of one"
    );
    assert_eq!(ask_whole(&d, &with(beat("2.0")), "d2"), update("2.0"));
    d.max_updating.set("daemon", 2);
    assert_eq!(
        ask_whole(&d, &with(updating(beat("1.0"), "2.0")), "d2"),
        update("2.0"),
        "a daemon limit of two has a place left"
    );
}

/// Catches: the slot counted across roles (a storage host updating holding a daemon back), a
/// limit above one not letting a second host of its role go, a limit reached not holding the
/// role, and a limit of zero not holding its role. The plan is of one stage, so only the limits
/// decide.
#[test]
fn max_updating_caps_a_role_within_a_stage() {
    let busy = fleet(
        NOW,
        &[
            ("s1", role("storage"), updating(beat("1.0"), "2.0")),
            ("s2", role("storage"), beat("1.0")),
            ("s3", role("storage"), beat("1.0")),
            ("d1", role("daemon"), beat("1.0")),
            ("m1", role("mac-daemon"), beat("1.0")),
        ],
    );
    let mut d = desired("2.0", 0, 0);
    assert_eq!(
        ask_whole(&d, &busy, "d1"),
        update("2.0"),
        "a storage host updating holds no slot of the daemons"
    );
    assert_eq!(ask_whole(&d, &busy, "m1"), update("2.0"));
    assert_eq!(
        why_whole(&d, &busy, "s2"),
        Decision::Gate(Gate::NoSlot),
        "the storage limit of one is reached"
    );
    d.max_updating.set("storage", 2);
    assert_eq!(
        ask_whole(&d, &busy, "s2"),
        update("2.0"),
        "a storage limit of two has a place left"
    );
    let full = fleet(
        NOW,
        &[
            ("s1", role("storage"), updating(beat("1.0"), "2.0")),
            ("s2", role("storage"), updating(beat("1.0"), "2.0")),
            ("s3", role("storage"), beat("1.0")),
        ],
    );
    assert_eq!(
        why_whole(&d, &full, "s3"),
        Decision::Gate(Gate::NoSlot),
        "two storage hosts updating fill a limit of two"
    );
    d.max_updating.set("daemon", 0);
    assert_eq!(
        why_whole(&d, &busy, "d1"),
        Decision::Gate(Gate::NoSlot),
        "a limit of zero holds the role"
    );
    assert_eq!(
        ask_whole(&d, &busy, "m1"),
        update("2.0"),
        "and no other role"
    );
}

/// Catches: the last frontend that is up held back by a floor of one or by a
/// minimum: with the desired state naming no frontend up, a lone frontend, one beside a silent
/// frontend and one beside an unhealthy frontend are each told to update; only a frontend that
/// is updating beside it holds the slot.
#[test]
fn the_last_frontend_that_is_up_is_told_to_update_like_any_host() {
    let d = desired("2.0", 1, 0);
    let with = |other: hostkeeper_wire::wire::Heartbeat, seen| {
        let mut f = fleet(NOW, &[("f1", role("frontend"), beat("1.0"))]);
        f.record_in(LEGACY, id("f2"), role("frontend"), other, seen);
        f
    };
    assert_eq!(ask(&d, &with(beat("1.0"), NOW), "f1"), update("2.0"));
    assert_eq!(
        ask(&d, &with(beat("1.0"), secs(900)), "f1"),
        update("2.0"),
        "f2 silent"
    );
    assert_eq!(
        ask(&d, &with(unhealthy(beat("1.0")), NOW), "f1"),
        update("2.0"),
        "f2 unhealthy"
    );
    let busy = updating(beat("1.0"), "2.0");
    assert_eq!(
        ask(&d, &with(busy, NOW), "f1"),
        Reply::Nothing,
        "f2 holds the slot"
    );
    let alone = fleet(NOW, &[("f1", role("frontend"), beat("1.0"))]);
    assert_eq!(ask(&d, &alone, "f1"), update("2.0"), "the only frontend");
}

/// Catches: the desired state's `min_up` of a role not read: an order that would leave fewer of
/// the role's live hosts up than the minimum given anyway, with the hosts alone, silent or
/// unhealthy; and the hosts that stay up not counted, so an order the minimum allows is held.
/// The numbers are kept in the state.
#[test]
fn min_up_holds_an_order_that_would_leave_too_few_of_the_role_up() {
    let d = desired("2.0", 3, 3);
    let lone = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("1.0")),
            ("f1", role("frontend"), beat("1.0")),
        ],
    );
    assert_eq!(why_whole(&d, &lone, "s1"), Decision::Gate(Gate::BelowMinUp));
    assert_eq!(why_whole(&d, &lone, "f1"), Decision::Gate(Gate::BelowMinUp));
    let sick = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("1.0")),
            ("s2", role("storage"), unhealthy(beat("1.0"))),
        ],
    );
    assert_eq!(ask(&d, &sick, "s1"), Reply::Nothing);
    let mut silent = fleet(NOW, &[("s1", role("storage"), beat("1.0"))]);
    silent.record_in(LEGACY, id("s2"), role("storage"), beat("1.0"), secs(900));
    assert_eq!(ask(&d, &silent, "s1"), Reply::Nothing);
    assert_eq!(
        (
            d.min_up.min_up_of("storage"),
            d.min_up.min_up_of("frontend")
        ),
        (3, 3)
    );
    // The same hosts under a minimum of one: the host that stays up allows the order.
    let one = desired("2.0", 1, 1);
    let pair = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("1.0")),
            ("s2", role("storage"), beat("1.0")),
        ],
    );
    assert_eq!(ask(&one, &pair, "s1"), update("2.0"));
    assert_eq!(ask(&one, &sick, "s1"), Reply::Nothing, "s2 is not up");
    assert_eq!(ask(&one, &silent, "s1"), Reply::Nothing, "s2 is silent");
    assert_eq!(ask(&one, &lone, "s1"), Reply::Nothing, "s1 is the only one");
}

/// Catches: an order given while the coordinator is still within 30 seconds of starting,
/// and one held back after them.
#[test]
fn no_order_is_given_within_thirty_seconds_of_starting() {
    let d = desired("2.0", 1, 1);
    let mut f = Fleet::new(secs(1_000), crate::fleet::QUIET_START);
    f.record_in(LEGACY, id("d1"), role("daemon"), beat("1.0"), secs(1_029));
    assert_eq!(decide(&d, &f, &id("d1"), secs(1_000)), Reply::Nothing);
    assert_eq!(decide(&d, &f, &id("d1"), secs(1_029)), Reply::Nothing);
    assert_eq!(decide(&d, &f, &id("d1"), secs(1_030)), update("2.0"));
}

/// Catches: a role waiting for another: a frontend (or any role) told nothing while a host of
/// another role is behind. With nothing ordered, the frontend is told to update whether a
/// storage, daemon or Mac host is behind, at the version, silent or not; it waits only while
/// that host's update fills the first stage.
#[test]
fn no_role_waits_for_another_role() {
    let d = desired("2.0", 0, 1);
    let with = |other: (&str, RoleName, hostkeeper_wire::wire::Heartbeat), seen| {
        let mut f = fleet(
            NOW,
            &[
                ("f1", role("frontend"), beat("1.0")),
                ("f2", role("frontend"), beat("1.0")),
            ],
        );
        f.record_in(LEGACY, id(other.0), other.1, other.2, seen);
        f
    };
    for role in [role("storage"), role("daemon"), role("mac-daemon")] {
        assert_eq!(
            ask(&d, &with(("x1", role.clone(), beat("1.0")), NOW), "f1"),
            update("2.0"),
            "{role} behind"
        );
        let busy = updating(beat("1.0"), "2.0");
        assert_eq!(
            ask(&d, &with(("x1", role.clone(), busy), NOW), "f1"),
            Reply::Nothing,
            "{role} updating"
        );
        assert_eq!(
            ask(&d, &with(("x1", role.clone(), beat("2.0")), NOW), "f1"),
            update("2.0"),
            "{role} at the version"
        );
        assert_eq!(
            ask(
                &d,
                &with(("x1", role.clone(), beat("1.0")), secs(900)),
                "f1"
            ),
            update("2.0"),
            "{role} down"
        );
        let f = with(("x1", role.clone(), beat("1.0")), NOW);
        assert_eq!(
            ask(&d, &f, "x1"),
            update("2.0"),
            "{role} goes while frontends wait for nothing"
        );
    }
}

/// Catches: a failed host giving its slot back before it heartbeats healthy (a bad version
/// would then roll on through the role), and keeping it after. The plan is of one stage and the
/// daemon limit is one, so the slot is the only thing that holds the second daemon.
#[test]
fn a_failed_host_keeps_its_slot_until_it_heartbeats_healthy() {
    let mut d = desired("2.0", 1, 1);
    d.max_updating.set("daemon", 1);
    let failed = last(beat("1.0"), "2.0", Outcome::Failed);
    let f = |d1: hostkeeper_wire::wire::Heartbeat| {
        fleet(
            NOW,
            &[
                ("d1", role("daemon"), d1),
                ("d2", role("daemon"), beat("1.0")),
            ],
        )
    };
    assert_eq!(
        why_whole(&d, &f(unhealthy(failed.clone())), "d2"),
        Decision::Gate(Gate::NoSlot)
    );
    assert_eq!(ask_whole(&d, &f(failed), "d2"), update("2.0"));
}

/// Catches: a host that failed the desired version being told it again as soon as it is
/// healthy (a retry loop on a bad version), and the block outliving a new desired version;
/// a host that failed another version being held back by its own old failure.
#[test]
fn a_host_that_failed_a_version_is_not_told_it_again() {
    let failed = last(beat("1.0"), "2.0", Outcome::Failed);
    let f = fleet(NOW, &[("d1", role("daemon"), failed.clone())]);
    assert_eq!(ask(&desired("2.0", 1, 1), &f, "d1"), Reply::Nothing);
    assert_eq!(ask(&desired("3.0", 1, 1), &f, "d1"), update("3.0"));
    let own = fleet(NOW, &[("d1", role("daemon"), unhealthy(failed))]);
    assert_eq!(
        ask(&desired("3.0", 1, 1), &own, "d1"),
        update("3.0"),
        "its own slot"
    );
    let succeeded = last(beat("1.0"), "2.0", Outcome::Succeeded);
    let ok = fleet(NOW, &[("d1", role("daemon"), succeeded)]);
    assert_eq!(ask(&desired("2.0", 1, 1), &ok, "d1"), update("2.0"));
}

/// Catches: a rollback (the desired version older than the one the hosts run) not ordered, or
/// ordered under other rules than an update: the first stage holds one host whatever its role,
/// the others wait for it, and the `min_up` of the role holds it as it holds an update.
#[test]
fn a_rollback_is_an_older_desired_version_under_the_same_rules() {
    let d = desired("1.0", 2, 1);
    let f = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("2.0")),
            ("s2", role("storage"), beat("2.0")),
            ("s3", role("storage"), beat("2.0")),
            ("f1", role("frontend"), beat("2.0")),
            ("f2", role("frontend"), beat("2.0")),
        ],
    );
    assert_eq!(ask(&d, &f, "s1"), update("1.0"));
    assert_eq!(ask(&d, &f, "f1"), update("1.0"), "no role goes first");
    let busy = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("2.0")),
            ("s2", role("storage"), updating(beat("2.0"), "1.0")),
            ("s3", role("storage"), beat("2.0")),
        ],
    );
    assert_eq!(
        why_whole(&d, &busy, "s1"),
        Decision::Gate(Gate::NoSlot),
        "the storage limit of one"
    );
    assert_eq!(
        decide_why(
            &kbf_services(),
            &whole_wants(&d),
            LEGACY,
            &busy,
            &id("s1"),
            NOW
        ),
        Decision::Gate(Gate::StageWait),
        "the first stage is full"
    );
    let thin = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("2.0")),
            ("s2", role("storage"), beat("2.0")),
        ],
    );
    assert_eq!(
        ask(&d, &thin, "s1"),
        Reply::Nothing,
        "two storage hosts do not leave two up"
    );
    assert_eq!(
        ask(&desired("3.0", 2, 1), &thin, "s1"),
        Reply::Nothing,
        "an update is held the same"
    );
}

/// Catches: a host whose role the service's definition does not name being ordered to
/// update: it is recorded but gets nothing, however far behind it is.
#[test]
fn a_role_the_code_does_not_name_is_ordered_nothing() {
    let cache: RoleName = "cache".parse().expect("well formed");
    let f = fleet(NOW, &[("c1", cache, beat("1.0"))]);
    assert_eq!(ask(&desired("2.0", 1, 1), &f, "c1"), Reply::Nothing);
}

/// Catches: a definition that names a role outside the format's four being refused an order:
/// a role the definition names is ordered like any role, and a host whose role it does not name
/// is ordered nothing.
#[test]
fn a_definition_that_names_an_unnamed_role_orders_it_by_the_default() {
    let cache: RoleName = "cache".parse().expect("well formed");
    let mut def = kbf_def();
    def.roles.insert(cache.name().to_owned());
    let services = kbf_services().with(def);
    let d = desired("2.0", 1, 1);
    let wants = BTreeMap::from([(LEGACY.to_owned(), d.clone())]);
    let f = fleet(NOW, &[("c1", cache, beat("1.0"))]);
    assert_eq!(
        decide_in(&services, &wants, LEGACY, &f, &id("c1"), NOW),
        update("2.0")
    );
    let other: RoleName = "queue".parse().expect("well formed");
    let f = fleet(NOW, &[("q1", other, beat("1.0"))]);
    assert_eq!(
        decide_in(&services, &wants, LEGACY, &f, &id("q1"), NOW),
        Reply::Nothing
    );
    let behind = fleet(NOW, &[("d1", role("daemon"), beat("1.0"))]);
    assert_eq!(
        decide_in(&services, &wants, LEGACY, &behind, &id("d1"), NOW),
        update("2.0")
    );
}

/// The service `pool`, which runs on one role, `builder`.
fn pool() -> Services {
    Services::from_defs(vec![ServiceDef::new("pool", &["builder"])])
}

/// The desired state of `pool` that `text` is.
fn pool_wants(text: &str) -> BTreeMap<String, Desired> {
    BTreeMap::from([(
        "pool".to_owned(),
        Desired::parse(text.as_bytes()).expect("parsed"),
    )])
}

const POOL_PLAIN: &str = r#"{"version":"2.0","min_up":{"storage":0,"frontend":0}}"#;

fn builders(hosts: &[&str]) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    for host in hosts {
        fleet.record_named_in("pool", id(host), "builder", beat("1.0"), NOW);
    }
    fleet
}

fn pool_ask(wants: &BTreeMap<String, Desired>, fleet: &Fleet, host: &str) -> Reply {
    decide_in(&pool(), wants, "pool", fleet, &id(host), NOW)
}

/// Catches: a role the definition names but the desired state does not mention getting no
/// order: host `b1` is ordered, and while its order stands `b2` is told nothing and `b1` is told
/// the same again.
#[test]
fn a_role_the_definition_names_with_no_overrides_gets_the_default_and_is_ordered() {
    let wants = pool_wants(POOL_PLAIN);
    let mut fleet = builders(&["b1", "b2"]);
    assert_eq!(pool_ask(&wants, &fleet, "b1"), update("2.0"));
    fleet.order("pool", id("b1"), version("2.0"), NOW);
    assert_eq!(pool_ask(&wants, &fleet, "b2"), Reply::Nothing);
    assert_eq!(pool_ask(&wants, &fleet, "b1"), update("2.0"));
}

/// Catches: a host whose role no definition names being ordered for that service: a
/// `stranger` host of `pool` (which runs on `builder`) and a storage host of a service that
/// runs on daemons are both told nothing, with every other rule satisfied.
#[test]
fn a_host_whose_role_no_definition_names_is_never_ordered() {
    let mut fleet = builders(&["b1"]);
    fleet.record_named_in("pool", id("x1"), "stranger", beat("1.0"), NOW);
    let wants = pool_wants(POOL_PLAIN);
    assert_eq!(pool_ask(&wants, &fleet, "x1"), Reply::Nothing);
    assert_eq!(pool_ask(&wants, &fleet, "b1"), update("2.0"));

    let other =
        Services::parse("[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n").expect("services");
    let mut fleet = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    fleet.record_in("other", id("s1"), role("storage"), beat("1.0"), NOW);
    fleet.record_in("other", id("d1"), role("daemon"), beat("1.0"), NOW);
    let all = BTreeMap::from([(
        "other".to_owned(),
        Desired::parse(POOL_PLAIN.as_bytes()).expect("parsed"),
    )]);
    let ask = |host: &str| decide_in(&other, &all, "other", &fleet, &id(host), NOW);
    assert_eq!(ask("s1"), Reply::Nothing);
    assert_eq!(ask("d1"), update("2.0"));
}

/// Catches: the desired state's limit or minimum for a role no format key names not read: a
/// limit of 0 for `builder` holds the role (`no_slot`), a minimum of 3 hosts up with three
/// builders holds every order (`below_min_up`), and a minimum of 2 lets the first builder go.
#[test]
fn a_limit_or_a_minimum_for_a_named_role_is_read() {
    let gate_of = |wants: &BTreeMap<String, Desired>, fleet: &Fleet, host: &str| {
        decide_why(&pool(), wants, "pool", fleet, &id(host), NOW)
    };
    let fleet = builders(&["b1", "b2", "b3"]);
    let limit = pool_wants(
        r#"{"version":"2.0","max_updating":{"builder":0},"min_up":{"storage":0,"frontend":0}}"#,
    );
    assert_eq!(gate_of(&limit, &fleet, "b1"), Decision::Gate(Gate::NoSlot));
    let minimum =
        pool_wants(r#"{"version":"2.0","min_up":{"storage":0,"frontend":0,"builder":3}}"#);
    assert_eq!(
        gate_of(&minimum, &fleet, "b1"),
        Decision::Gate(Gate::BelowMinUp)
    );
    let two = pool_wants(r#"{"version":"2.0","min_up":{"storage":0,"frontend":0,"builder":2}}"#);
    assert_eq!(pool_ask(&two, &fleet, "b1"), update("2.0"));
    let mut fleet = fleet;
    fleet.order("pool", id("b1"), version("2.0"), NOW);
    for host in ["b2", "b3"] {
        assert_eq!(pool_ask(&two, &fleet, host), Reply::Nothing, "{host}");
    }
}
