//! One test per rule of the decision, each with the hosts that make the rule matter.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::RoleName;
use hostkeeper_wire::wire::{Outcome, Reply};

use crate::decide::decide_in;
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::services::{ServiceDef, Services};
use crate::testutil::{
    LEGACY, beat, decide, desired, fleet, id, kbf_def, kbf_services, last, role, secs, unhealthy,
    updating, version,
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
/// one is updating, though the desired state's daemon limit is two (which no decision reads);
/// and the slot not freeing when the update ends.
#[test]
fn a_host_updating_holds_the_one_slot_until_it_is_done() {
    let with = |d1: hostkeeper_wire::wire::Heartbeat| {
        fleet(
            NOW,
            &[
                ("d1", role("daemon"), d1),
                ("d2", role("daemon"), beat("1.0")),
            ],
        )
    };
    let d = desired("2.0", 1, 1);
    assert_eq!(
        ask(&d, &with(updating(beat("1.0"), "2.0")), "d2"),
        Reply::Nothing,
        "one update at a time, though the daemon limit is two"
    );
    assert_eq!(ask(&d, &with(beat("2.0")), "d2"), update("2.0"));
}

/// Catches: the slot counted per role (a storage host updating leaving a daemon free to
/// start), a limit above one letting a second host go, and a limit of zero holding its role (it
/// no longer does).
#[test]
fn no_role_limit_widens_a_roll_or_holds_a_role() {
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
    let mut d = desired("2.0", 1, 1);
    assert_eq!(
        ask(&d, &busy, "d1"),
        Reply::Nothing,
        "a storage host updating holds the daemons' slot too"
    );
    d.max_updating.set("storage", 2);
    d.max_updating.set("daemon", 3);
    d.max_updating.set("mac-daemon", 3);
    for host in ["s2", "d1", "m1"] {
        assert_eq!(ask(&d, &busy, host), Reply::Nothing, "{host}");
    }
    let idle = fleet(
        NOW,
        &[
            ("d1", role("daemon"), beat("2.0")),
            ("m1", role("mac-daemon"), beat("1.0")),
            ("m2", role("mac-daemon"), beat("1.0")),
        ],
    );
    assert_eq!(ask(&d, &idle, "m2"), update("2.0"));
    d.max_updating.set("mac-daemon", 0);
    assert_eq!(
        ask(&d, &idle, "m2"),
        update("2.0"),
        "a limit of zero holds nothing: no decision reads it"
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

/// Catches: a minimum of hosts up read again: the desired state names more storage and
/// frontend hosts up than the fleet has, with hosts silent, unhealthy or alone, and the host
/// asked is told to update all the same; the numbers are kept in the state.
#[test]
fn min_up_is_kept_and_holds_no_order() {
    let d = desired("2.0", 3, 3);
    let lone = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("1.0")),
            ("f1", role("frontend"), beat("1.0")),
        ],
    );
    assert_eq!(ask(&d, &lone, "s1"), update("2.0"));
    assert_eq!(ask(&d, &lone, "f1"), update("2.0"));
    let sick = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("1.0")),
            ("s2", role("storage"), unhealthy(beat("1.0"))),
        ],
    );
    assert_eq!(ask(&d, &sick, "s1"), update("2.0"));
    let mut silent = fleet(NOW, &[("s1", role("storage"), beat("1.0"))]);
    silent.record_in(LEGACY, id("s2"), role("storage"), beat("1.0"), secs(900));
    assert_eq!(ask(&d, &silent, "s1"), update("2.0"));
    assert_eq!(
        (
            d.min_up.min_up_of("storage"),
            d.min_up.min_up_of("frontend")
        ),
        (3, 3)
    );
}

/// Catches: an order given while the coordinator is still within 30 seconds of starting,
/// and one held back after them.
#[test]
fn no_order_is_given_within_thirty_seconds_of_starting() {
    let d = desired("2.0", 1, 1);
    let mut f = Fleet::new(secs(1_000));
    f.record_in(LEGACY, id("d1"), role("daemon"), beat("1.0"), secs(1_029));
    assert_eq!(decide(&d, &f, &id("d1"), secs(1_000)), Reply::Nothing);
    assert_eq!(decide(&d, &f, &id("d1"), secs(1_029)), Reply::Nothing);
    assert_eq!(decide(&d, &f, &id("d1"), secs(1_030)), update("2.0"));
}

/// Catches: a role waiting for another: a frontend (or any role) told nothing while a host of
/// another role is behind. With nothing ordered, the frontend is told to update whether a
/// storage, daemon or Mac host is behind, at the version, silent or not; it waits only while
/// that host holds the slot (updating).
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
/// would then roll on through the role), and keeping it after.
#[test]
fn a_failed_host_keeps_its_slot_until_it_heartbeats_healthy() {
    let d = desired("2.0", 1, 1);
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
    assert_eq!(ask(&d, &f(unhealthy(failed.clone())), "d2"), Reply::Nothing);
    assert_eq!(ask(&d, &f(failed), "d2"), update("2.0"));
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
/// ordered under other rules than an update: one host at a time, and nothing else. Storage and
/// frontends are alike, and too few storage hosts for the minimum do not hold it.
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
    assert_eq!(ask(&d, &busy, "s1"), Reply::Nothing, "one host at a time");
    let thin = fleet(
        NOW,
        &[
            ("s1", role("storage"), beat("2.0")),
            ("s2", role("storage"), beat("2.0")),
        ],
    );
    assert_eq!(ask(&d, &thin, "s1"), update("1.0"), "no minimum holds it");
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
    let mut fleet = Fleet::new(Duration::ZERO);
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
    let mut fleet = Fleet::new(Duration::ZERO);
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

/// Catches: the desired state's limit or minimum for a role no format key names read again: a
/// limit of 0 and a minimum of 3 hosts up for `builder` hold nothing; the first builder is
/// ordered and the others wait for its slot, not for a number.
#[test]
fn a_limit_or_a_minimum_for_a_named_role_decides_nothing() {
    let wants = pool_wants(
        r#"{"version":"2.0","max_updating":{"builder":0},"min_up":{"storage":0,"frontend":0,"builder":3}}"#,
    );
    let mut fleet = builders(&["b1", "b2", "b3"]);
    assert_eq!(pool_ask(&wants, &fleet, "b1"), update("2.0"));
    fleet.order("pool", id("b1"), version("2.0"), NOW);
    for host in ["b2", "b3"] {
        assert_eq!(pool_ask(&wants, &fleet, host), Reply::Nothing, "{host}");
    }
}
