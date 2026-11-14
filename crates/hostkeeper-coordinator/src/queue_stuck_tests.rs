//! What takes a place in the queue of services that wait for the one live rollout
//! ([`crate::queue`]) and where a service stands in it: a plan that orders nobody now holds no
//! place, a service listed in two groups queues by the group that wants a version of it, and a
//! place survives a moment in which the package wants no version.

use std::time::Duration;

use hostkeeper_wire::wire::{Phase, ServiceReply};

use crate::decide::{Gate, Why};
use crate::desired::Desired;
use crate::group_decision_support::{Farm, failed, policy};
use crate::live::live_elsewhere;
use crate::marks_support::{runs, update};
use crate::ops::{AuditEntry, Write};
use crate::queue::{Cluster, Entry};
use crate::rollouts::Pause;
use crate::services::ServiceDef;
use crate::testutil::{id, version};

const NOTHING: ServiceReply = ServiceReply::Nothing;

fn at(farm: &Farm, secs: u64) {
    farm.world.rig.at(secs);
}

fn stamp(farm: &Farm) {
    drop(farm.world.coordinator().lock_fleet());
}

/// Two groups, `one` with `web` and `two` with `db`, each with one host that runs `1.0`, `web`
/// set to `2.0` before `db`. `edit` changes the group `one`'s policy.
fn web_then_db(
    name: &str,
    edit: impl FnOnce(&mut hostkeeper_proto::hostkeeper::operator::v1::RolloutPolicy),
) -> Farm {
    let farm = Farm::new(name);
    farm.group("one", &["web"], Some(policy(edit)));
    farm.group("two", &["db"], None);
    farm.host("o1", "one");
    farm.host("t1", "two");
    assert_eq!(farm.tell("o1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    at(&farm, 2_005);
    farm.want("one", "web", "2.0");
    stamp(&farm);
    at(&farm, 2_010);
    farm.want("two", "db", "2.0");
    stamp(&farm);
    farm
}

/// Proves: a service set first whose plan waits for an operator's Advance (its first stage is
/// manual) takes no place: `db`, set after it, is ordered at once.
///
/// Catches: a head that nothing can order now and that holds every service behind it for good.
#[test]
fn a_plan_that_waits_for_an_advance_does_not_hold_the_queue() {
    let farm = web_then_db("q-manual", |p| {
        p.stages = vec![hostkeeper_proto::hostkeeper::operator::v1::PolicyStage {
            percent: 100,
            advance: hostkeeper_proto::hostkeeper::operator::v1::StageAdvance::Manual.into(),
        }];
    });

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a service set first whose only host cannot go down (`min_up` 1 with that one host)
/// takes no place: `db` is ordered at once.
///
/// Catches: a head that is below `min_up`, which orders nobody, holding the queue.
#[test]
fn a_plan_below_min_up_does_not_hold_the_queue() {
    let farm = web_then_db("q-min-up", |p| p.min_up = 1);

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a service set first whose plan an operator paused takes no place: `db` is ordered.
///
/// Catches: a paused head that holds the queue.
#[test]
fn a_paused_plan_does_not_hold_the_queue() {
    let farm = web_then_db("q-paused", |_| {});
    farm.world.seed_marks("one", "web", |marks| {
        marks.rollout.version = Some(version("2.0"));
        marks.rollout.paused = Some(Pause {
            by: "ops".to_owned(),
            reason: "look".to_owned(),
        });
    });

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a service whose desired version is marked bad takes no place: `db` is ordered.
///
/// Catches: a service at a bad version that holds the queue.
#[test]
fn a_version_marked_bad_does_not_hold_the_queue() {
    let farm = web_then_db("q-bad", |_| {});
    farm.world
        .coordinator()
        .ops()
        .change(
            Write::BeforeMemory,
            AuditEntry::new("test", "seed"),
            |state| {
                state
                    .services
                    .entry("web".to_owned())
                    .or_default()
                    .bad
                    .push(version("2.0"));
            },
        )
        .expect("seeded");

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a service one of whose hosts failed the update takes no place: `db` is ordered.
///
/// Catches: a failed head that holds the queue.
#[test]
fn a_service_with_a_failed_host_does_not_hold_the_queue() {
    let farm = web_then_db("q-failed", |_| {});
    farm.tell_with(
        "o1",
        vec![
            runs("hostkeeper", "1.0"),
            failed("web", "1.0", "2.0", Phase::AfterCommit),
        ],
    );

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a service every host of which already runs the desired version takes no place, and
/// a service nothing wants a version of takes none: `db` is ordered at once even though `web`
/// was set before it.
///
/// Catches: a finished service, or one with no desired version, counted in the queue.
#[test]
fn a_finished_service_does_not_hold_the_queue() {
    let farm = web_then_db("q-finished", |_| {});
    assert_eq!(farm.tell("o1", &[("web", "2.0")]), NOTHING);

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a package listed in two groups queues by the group that wants a version of it now: a
/// `shared` that finished long ago in `a` and is set in `b` after `web` waits for `web`.
///
/// Catches: the place of the finished entry of the other group, which lets `shared` jump the
/// queue.
#[test]
fn a_package_of_two_groups_queues_by_the_group_that_wants_it_now() {
    let farm = Farm::new("q-two-groups");
    farm.group("a", &["shared"], None);
    farm.group("b", &["shared"], None);
    farm.group("c", &["web"], None);
    farm.host("a1", "a");
    farm.host("b1", "b");
    farm.host("c1", "c");
    assert_eq!(farm.tell("a1", &[("shared", "2.0")]), NOTHING);
    assert_eq!(farm.tell("b1", &[("shared", "1.0")]), NOTHING);
    assert_eq!(farm.tell("c1", &[("web", "1.0")]), NOTHING);
    at(&farm, 2_005);
    farm.want("a", "shared", "2.0");
    stamp(&farm);
    at(&farm, 2_010);
    farm.want("c", "web", "2.0");
    stamp(&farm);
    at(&farm, 2_020);
    farm.want("b", "shared", "2.0");
    stamp(&farm);

    assert_eq!(farm.tell("b1", &[("shared", "1.0")]), NOTHING);
    assert_eq!(
        farm.why("b1", "shared"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );
    assert_eq!(farm.tell("c1", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: with two live places, one taken, a service with one queued ahead of it is first in line
/// for the place that is left: its status says position 1 and names the live service and the
/// queued one.
///
/// Catches: a position that counts the places the live services leave as queued ahead.
#[test]
fn the_position_counts_only_those_ahead_that_get_no_place() {
    let farm = Farm::new("q-position");
    for (group, package) in [("g1", "web"), ("g2", "db"), ("g3", "cache")] {
        farm.group(group, &[package], None);
    }
    farm.flag(2);
    farm.host("a1", "g1");
    farm.host("b1", "g2");
    farm.host("c1", "g3");
    for (host, package) in [("a1", "web"), ("b1", "db"), ("c1", "cache")] {
        assert_eq!(farm.tell(host, &[(package, "1.0")]), NOTHING);
    }
    for (secs, group, package) in [
        (2_005, "g1", "web"),
        (2_010, "g2", "db"),
        (2_015, "g3", "cache"),
    ] {
        at(&farm, secs);
        farm.want(group, package, "2.0");
        stamp(&farm);
    }
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));

    let coordinator = farm.world.coordinator();
    let basis = coordinator.basis_of(&id("c1"));
    let all = coordinator.desired_for(&basis.services);
    let fleet = coordinator.lock_fleet();
    let now: Duration = (coordinator.clock)();
    let held = live_elsewhere(&basis.services, &all, "cache", &fleet, now).expect("held");
    assert_eq!(held.len(), 2);
    let def = basis.services.get("cache").expect("a def");
    let plan = crate::rollouts::plan_proto(
        &basis.services,
        &all,
        def,
        &all["cache"],
        false,
        &fleet,
        now,
    );
    assert_eq!(plan.queue_position, Some(1));
}

fn entry(_group: &str, want: Option<&str>) -> Entry {
    Entry {
        def: ServiceDef::new("web", &[]),
        want: want.map(|v| {
            serde_json::from_value::<Desired>(serde_json::json!({"version": v, "min_up": {}}))
                .expect("a desired state")
        }),
        at_unix: 0,
    }
}

/// Proves: the first-seen stamp of a desired version is kept across a take in which its package
/// wants no version, and a version no entry wants is forgotten once its package wants one again.
///
/// Catches: a service that loses its place for a moment's absence of its desired state.
#[test]
fn a_place_survives_a_moment_with_no_desired_version() {
    let first = Cluster::default().refreshed(vec![entry("g", Some("2.0"))], Duration::from_secs(5));
    let absent = first.refreshed(vec![entry("g", None)], Duration::from_secs(9));
    let back = absent.refreshed(vec![entry("g", Some("2.0"))], Duration::from_secs(12));
    let ready = &entry("g", Some("2.0"));
    assert_eq!(first.stamp_of(ready), Duration::from_secs(5));
    assert_eq!(back.stamp_of(ready), Duration::from_secs(5));

    let newer = back.refreshed(vec![entry("g", Some("3.0"))], Duration::from_secs(20));
    assert_eq!(
        newer.stamp_of(&entry("g", Some("3.0"))),
        Duration::from_secs(20)
    );
    assert_eq!(newer.stamp_of(ready), Duration::ZERO);
}
