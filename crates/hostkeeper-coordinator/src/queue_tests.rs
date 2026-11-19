//! The defaults of a host group's rollout (stages 50 and 100, no cap on hosts updating within a
//! stage) and the one live rollout of the whole cluster with the queue of the services that wait
//! for it ([`crate::queue`]). The tests drive heartbeats of enrolled hosts placed in groups; the
//! clock is the test's own.

use std::time::Duration;

use hostkeeper_wire::wire::ServiceReply;

use crate::decide::{Gate, Why};
use crate::group_decision_support::{Farm, policy};
use crate::group_scope::GroupScope;
use crate::live::{Why as LiveWhy, live_elsewhere};
use crate::marks_support::update;
use crate::ops::{Halt, HaltOrigin};
use crate::queue::{Cluster, Entry};
use crate::services::ServiceDef;
use crate::testutil::{id, version};

const NOTHING: ServiceReply = ServiceReply::Nothing;

/// Takes the fleet's lock once, as any heartbeat does: the coordinator stamps the desired versions
/// it has not seen.
fn stamp(farm: &Farm) {
    drop(farm.world.coordinator().lock_fleet());
}

/// Moves the test's clock to `secs`.
fn at(farm: &Farm, secs: u64) {
    farm.world.rig.at(secs);
}

fn hosts(farm: &Farm, group: &str, names: &[&str]) {
    for name in names {
        farm.host(name, group);
    }
}

/// Proves: a group that names no stages and no `max_updating` rolls six hosts in two stages of
/// three and six, and every host a stage enters updates at once: three hosts are ordered together,
/// the fourth waits for the stage, and once the three are done the other three are ordered
/// together.
///
/// Catches: the old defaults (stages of 1, 3 and 6 hosts, one host updating at a time).
#[test]
fn a_group_with_no_policy_rolls_half_its_hosts_at_once_and_then_the_rest() {
    let farm = Farm::new("q-default");
    farm.group("g", &["web"], None);
    let names = ["h1", "h2", "h3", "h4", "h5", "h6"];
    hosts(&farm, "g", &names);
    for name in names {
        assert_eq!(farm.tell(name, &[("web", "1.0")]), NOTHING);
    }
    farm.want("g", "web", "2.0");

    for name in &names[..3] {
        assert_eq!(farm.tell(name, &[("web", "1.0")]), update("web", "2.0"));
    }
    assert_eq!(farm.tell("h4", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("h4", "web"), Some(Why::Gate(Gate::StageWait)));

    for name in &names[..3] {
        assert_eq!(farm.tell(name, &[("web", "2.0")]), NOTHING);
    }
    for name in &names[3..] {
        assert_eq!(farm.tell(name, &[("web", "1.0")]), update("web", "2.0"));
    }
}

/// Proves: an explicit `max_updating` still caps the hosts of a stage updating at once, and 0
/// holds the group: with 2, two of three hosts of the first stage are ordered and the third is
/// told `no_slot`; with 0 none is ordered.
///
/// Catches: a cap that is ignored once the default is the whole stage, or a 0 read as no cap.
#[test]
fn an_explicit_max_updating_still_caps_the_hosts_updating_at_once() {
    let farm = Farm::new("q-cap");
    farm.group(
        "capped",
        &["web"],
        Some(policy(|p| p.max_updating = Some(2))),
    );
    farm.group("held", &["web"], Some(policy(|p| p.max_updating = Some(0))));
    farm.flag(2);
    let names = ["c1", "c2", "c3", "c4", "c5", "c6"];
    hosts(&farm, "capped", &names);
    hosts(&farm, "held", &["z1", "z2"]);
    for name in names.iter().chain(&["z1", "z2"]) {
        assert_eq!(farm.tell(name, &[("web", "1.0")]), NOTHING);
    }
    farm.want("capped", "web", "2.0");
    farm.want("held", "web", "2.0");

    assert_eq!(farm.tell("c1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("c2", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("c3", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("c3", "web"), Some(Why::Gate(Gate::NoSlot)));
    assert_eq!(farm.tell("z1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.why("z1", "web"), Some(Why::Gate(Gate::NoSlot)));
}

/// Proves: one service rolls at a time in the whole cluster: while `web` of one group is live,
/// `db` of another group is told `rollout_elsewhere`, and hostkeeper counts as a service like any
/// other (a group's list always has it): its rollout holds `web`'s.
///
/// Catches: a count per group, which lets every group roll a package of its own at once, or
/// hostkeeper exempt from the rule.
#[test]
fn only_one_service_rolls_in_the_cluster_and_hostkeeper_is_one() {
    let farm = Farm::new("q-one");
    farm.group("one", &["web"], None);
    farm.group("two", &["db"], None);
    hosts(&farm, "one", &["o1"]);
    hosts(&farm, "two", &["t1"]);
    farm.want("one", "web", "2.0");
    farm.want("two", "db", "2.0");

    assert_eq!(farm.tell("o1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    assert_eq!(
        farm.why("t1", "db"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );

    assert_eq!(farm.tell("o1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
    farm.want("one", "hostkeeper", "2.0");
    assert_eq!(farm.tell("o1", &[("web", "2.0")]), NOTHING);
    assert_eq!(
        farm.why("o1", "hostkeeper"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );
    assert_eq!(farm.tell("t1", &[("db", "2.0")]), NOTHING);
    assert_eq!(
        farm.tell("o1", &[("web", "2.0")]),
        update("hostkeeper", "2.0")
    );
}

/// Proves: the services that wait take their turns in the order their versions were set: `db`,
/// set after `web`, is held while `web` waits for the live place even though `db`'s host asks
/// first; `web` is then ordered, and `db` only after it is done.
///
/// Catches: first-to-ask admission, which lets a later-set service jump the queue.
#[test]
fn waiting_services_are_ordered_by_when_their_versions_were_set() {
    let farm = Farm::new("q-fifo");
    farm.group("one", &["web"], None);
    farm.group("two", &["db"], None);
    hosts(&farm, "one", &["o1"]);
    hosts(&farm, "two", &["t1"]);
    assert_eq!(farm.tell("o1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    at(&farm, 2_005);
    farm.want("one", "web", "2.0");
    stamp(&farm);
    at(&farm, 2_010);
    farm.want("two", "db", "2.0");
    stamp(&farm);

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    assert_eq!(
        farm.why("t1", "db"),
        Some(Why::Gate(Gate::RolloutElsewhere))
    );
    assert_eq!(farm.tell("o1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    assert_eq!(farm.tell("o1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a service that nothing can order takes no place in the queue: `web`, set first, is
/// halted, so `db` set after it is ordered at once.
///
/// Catches: a halted head that holds every service behind it for good.
#[test]
fn a_halted_service_ahead_in_the_queue_does_not_hold_the_others() {
    let farm = Farm::new("q-halted-head");
    farm.group("one", &["web"], None);
    farm.group("two", &["db"], None);
    hosts(&farm, "one", &["o1"]);
    hosts(&farm, "two", &["t1"]);
    assert_eq!(farm.tell("o1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    at(&farm, 2_005);
    farm.want("one", "web", "2.0");
    stamp(&farm);
    at(&farm, 2_010);
    farm.want("two", "db", "2.0");
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    farm.world.seed_marks("one", "web", |marks| {
        marks.halt = Some(Halt {
            node_id: "o1".to_owned(),
            check: "artifact_changed".to_owned(),
            target: version("2.0"),
            report: None,
            origin: HaltOrigin::Updater,
        });
    });

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a service whose every host that has not started declined the update takes no place in
/// the queue: `web`, set first, has its only host deferring, so `db` set after it is ordered.
///
/// Catches: a stalled head, hosts that decline for a while, that holds the queue behind it.
#[test]
fn a_service_whose_hosts_all_defer_does_not_hold_the_queue() {
    let farm = Farm::new("q-deferring-head");
    farm.group("one", &["web"], None);
    farm.group("two", &["db"], None);
    hosts(&farm, "one", &["o1"]);
    hosts(&farm, "two", &["t1"]);
    assert_eq!(farm.tell("o1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("t1", &[("db", "1.0")]), NOTHING);
    at(&farm, 2_005);
    farm.want("one", "web", "2.0");
    stamp(&farm);
    at(&farm, 2_010);
    farm.want("two", "db", "2.0");
    stamp(&farm);
    let mut declined = crate::marks_support::runs("web", "1.0");
    declined.deferred = serde_json::from_value(serde_json::json!({
        "target": "2.0", "reason": "drain_bound", "retry_after_secs": 3600}))
    .expect("a deferral");
    assert_eq!(
        farm.tell_with(
            "o1",
            vec![crate::marks_support::runs("hostkeeper", "1.0"), declined]
        ),
        NOTHING
    );

    assert_eq!(farm.tell("t1", &[("db", "1.0")]), update("db", "2.0"));
}

/// Proves: a group's `live_rollouts` can only lower the coordinator's: with `--live-rollouts` 2
/// and a group at 1, a second service of that group waits for the first, and the status of the
/// waiting plan names the service it waits on as live, with no queue position.
///
/// Catches: a group limit that raises the coordinator's, or a status that names no holder.
#[test]
fn a_group_limit_only_lowers_the_coordinators_and_the_status_names_the_holder() {
    let farm = Farm::new("q-lower");
    farm.group(
        "one",
        &["web", "db"],
        Some(policy(|p| p.live_rollouts = Some(1))),
    );
    farm.flag(2);
    hosts(&farm, "one", &["o1", "o2"]);
    let both = [("web", "1.0"), ("db", "1.0")];
    for name in ["o1", "o2"] {
        assert_eq!(farm.tell(name, &both), NOTHING);
    }
    farm.want("one", "web", "2.0");
    farm.want("one", "db", "2.0");

    assert_eq!(farm.tell("o1", &both), update("web", "2.0"));
    assert_eq!(farm.tell("o2", &[("web", "1.0"), ("db", "1.0")]), NOTHING);

    let coordinator = farm.world.coordinator();
    let basis = coordinator.basis_of(&id("o2"));
    let all = coordinator.desired_for(&basis.services);
    let fleet = coordinator.lock_fleet();
    let now = (coordinator.clock)();
    let holding = live_elsewhere(&basis.services, &all, "db", &fleet, now).expect("held");
    assert_eq!(
        holding
            .iter()
            .map(|(n, l)| (n.as_str(), l.why))
            .collect::<Vec<_>>(),
        vec![("web", LiveWhy::Ordered)]
    );
    let def = basis.services.get("db").expect("a def");
    let plan =
        crate::rollouts::plan_proto(&basis.services, &all, def, &all["db"], false, &fleet, now);
    assert_eq!(plan.waiting_on, vec!["web".to_owned()]);
    assert_eq!(plan.queue_position, None);
}

/// Proves: a service set after two others that wait, with one place free, is second in line for a
/// place: the status of its plan names the two ahead as queued, in order, and gives position 2
/// (the first of them gets the place), and the first of the three, with nothing ahead, is not held.
///
/// Catches: a queue the status cannot show, or a head that is held by those behind it.
#[test]
fn the_status_of_a_queued_plan_names_those_ahead_and_its_position() {
    let farm = Farm::new("q-status");
    for (group, package) in [("g1", "web"), ("g2", "db"), ("g3", "cache")] {
        farm.group(group, &[package], None);
    }
    hosts(&farm, "g1", &["a1"]);
    hosts(&farm, "g2", &["b1"]);
    hosts(&farm, "g3", &["c1"]);
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

    let coordinator = farm.world.coordinator();
    let basis = coordinator.basis_of(&id("c1"));
    let all = coordinator.desired_for(&basis.services);
    let fleet = coordinator.lock_fleet();
    let now: Duration = (coordinator.clock)();
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
    assert_eq!(plan.waiting_on, vec!["web".to_owned(), "db".to_owned()]);
    assert_eq!(plan.queue_position, Some(2));
    assert!(
        plan.waiting_on_why.iter().all(|w| w.reason
            == i32::from(hostkeeper_proto::hostkeeper::operator::v1::LiveReason::Queued))
    );
    let first = basis_of_first(coordinator, &fleet, now);
    assert_eq!(first, None);
}

/// `web` asks first of the three and is not held: nothing is ahead of it.
fn basis_of_first(
    coordinator: &crate::service::Coordinator,
    fleet: &crate::fleet::Fleet,
    now: Duration,
) -> Option<Vec<String>> {
    let basis = coordinator.basis_of(&id("a1"));
    let all = coordinator.desired_for(&basis.services);
    live_elsewhere(&basis.services, &all, "web", fleet, now)
        .map(|held| held.into_iter().map(|(name, _)| name).collect())
}

fn queued(group: &str, package: &str, at_unix: u64, set_seq: Option<i64>) -> Entry {
    let mut def = ServiceDef::new(package, &[]);
    def.scope = Some(GroupScope {
        group: group.parse().expect("a group name"),
        live_rollouts: None,
        max_updating: 1,
    });
    Entry {
        def,
        want: None,
        at_unix,
        set_seq,
    }
}

/// The entries as `group/package`, first in line of `cluster` first.
fn line(cluster: &Cluster, entries: &[Entry]) -> Vec<String> {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by_key(|entry| cluster.order(entry));
    sorted
        .iter()
        .map(|entry| format!("{}/{}", entry.def.group().expect("a group"), entry.def.name))
        .collect()
}

/// Proves: the number a desired version was set under is the first key of the queue: an entry with
/// a lower number is ahead of one with a higher number whatever the group's last change, the
/// group's name or the first-seen stamp say, an entry with no number is ahead of every numbered
/// one, and entries with the same number fall to the group's last change, then the group's name.
///
/// Catches: a queue that orders by what each coordinator saw first, so that two coordinators on
/// one database disagree, or one that puts a row no release numbered behind the numbered ones.
#[test]
fn the_number_a_version_was_set_under_leads_the_queue() {
    let entries = [
        queued("a", "late", 100, Some(9)),
        queued("z", "early", 900, Some(3)),
        queued("m", "none", 500, None),
        queued("c", "same", 50, Some(9)),
        queued("b", "same", 100, Some(9)),
    ];
    let seen = Cluster::default().refreshed(entries.to_vec(), Duration::from_secs(1));
    assert_eq!(
        line(&seen, &entries),
        ["m/none", "z/early", "c/same", "a/late", "b/same"]
    );
    // Seen first, the entry with the highest number is still behind the lower ones.
    let first = Cluster::default().refreshed(entries[..1].to_vec(), Duration::from_secs(1));
    let seen = first.refreshed(entries.to_vec(), Duration::from_secs(2));
    assert_eq!(
        line(&seen, &entries),
        ["m/none", "z/early", "a/late", "c/same", "b/same"],
        "equal numbers fall to the stamp, the lower numbers stay ahead of it"
    );
}

/// Proves: over the files, where no entry has a number, the queue is what it always was: the
/// first-seen stamp, then the group's last change, the group's name, the package's place in its
/// group's list and its name.
///
/// Catches: a first key that reorders a coordinator no database numbers for.
#[test]
fn without_numbers_the_queue_is_the_one_it_was() {
    let mut listed_later = queued("b", "web", 100, None);
    listed_later.def.priority = 1;
    let mut listed_earlier = queued("b", "zed", 100, None);
    listed_earlier.def.priority = 2;
    let entries = [
        queued("a", "web", 300, None),
        listed_later,
        listed_earlier,
        queued("c", "web", 200, None),
    ];
    let seen = Cluster::default().refreshed(entries[..1].to_vec(), Duration::from_secs(1));
    let seen = seen.refreshed(entries.to_vec(), Duration::from_secs(2));
    assert_eq!(
        line(&seen, &entries),
        ["a/web", "b/zed", "b/web", "c/web"],
        "the entry seen first, then the group's last change, then the earlier listed"
    );
}
