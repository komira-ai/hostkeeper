//! The decision by host group ([`crate::group_scope`]): what a host is ordered when the
//! coordinator holds a group, and by whose policy. The tests drive heartbeats of enrolled hosts
//! placed in groups by `HostGroups.Move`, against the desired versions of the groups.

use hostkeeper_wire::wire::{Phase, ServiceReply};

use crate::group_decision_support::{Farm, failed, policy, stages};
use crate::host_groups::GroupName;
use crate::marks_support::{runs, update};
use crate::testutil::id;

const NOTHING: ServiceReply = ServiceReply::Nothing;

/// Proves: a host is ordered the desired version its own group has for a package: two groups that
/// list `web` at different versions order their hosts different ones, and changing one group's
/// version changes nothing the other's host is told.
///
/// Catches: a desired version read from one place for every group, which would roll the second
/// group to the first group's version.
#[test]
fn a_host_is_ordered_the_version_its_group_wants() {
    let farm = Farm::new("gd-own-version");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    farm.host("a1", "alpha");
    farm.host("b1", "beta");
    farm.want("alpha", "web", "2.0");
    farm.want("beta", "web", "3.0");

    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "3.0"));

    farm.want("alpha", "web", "4.0");
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "3.0"));
}

/// Proves: a host runs exactly the packages its group lists: a package it runs that the group
/// does not list (installed by hand) is never ordered, though the group holds a desired version
/// for it and another group lists it, and a package the group lists is.
///
/// Catches: the rollout file or the desired version of another group placing a package on the
/// host.
#[test]
fn a_package_the_group_does_not_list_is_never_ordered() {
    let farm = Farm::new("gd-unlisted");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web", "db"], None);
    farm.host("a1", "alpha");
    farm.want("alpha", "web", "2.0");
    farm.want("alpha", "db", "2.0");
    farm.want("beta", "db", "2.0");

    assert_eq!(
        farm.tell("a1", &[("web", "2.0"), ("db", "1.0")]),
        ServiceReply::Nothing
    );
    assert_eq!(
        farm.tell("a1", &[("web", "1.0"), ("db", "1.0")]),
        update("web", "2.0")
    );
}

/// Proves: a host in no group is ordered nothing though groups hold desired versions of what it
/// runs, and the fleet records no role for it.
///
/// Catches: a host with no group falling back to the rollout file's roles.
#[test]
fn a_host_in_no_group_is_ordered_nothing() {
    let farm = Farm::new("gd-no-group");
    farm.group("alpha", &["web"], None);
    farm.lone("l1");
    farm.want("alpha", "web", "2.0");

    assert_eq!(farm.tell("l1", &[("web", "1.0")]), NOTHING);

    let fleet = farm.world.coordinator().lock_fleet();
    assert_eq!(fleet.node(&id("l1")).expect("a node").role, "");
}

/// Proves: a host never enrolled, and a coordinator with no enrollment at all, place nothing: the
/// heartbeat is answered and the host is in no group.
///
/// Catches: a heartbeat of a host the enrollment store does not hold falling to the rollout file.
#[test]
fn a_host_the_enrollment_does_not_hold_is_in_no_group() {
    let farm = Farm::new("gd-unenrolled");
    farm.group("alpha", &["web"], None);
    farm.want("alpha", "web", "2.0");

    assert_eq!(farm.tell("never", &[("web", "1.0")]), NOTHING);
}

/// Proves: a deployment to one group orders that group's hosts alone, and by the group's slots:
/// the other group's hosts, whose group has no desired version, are told nothing; of the deployed
/// group one host is ordered and the next waits for the slot, until the first runs the version.
///
/// Catches: a group's deployment reaching the hosts of a group that lists the same package.
#[test]
fn a_deployment_to_one_group_orders_its_hosts_alone() {
    let farm = Farm::new("gd-one-group");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    for host in ["a1", "a2"] {
        farm.host(host, "alpha");
    }
    for host in ["b1", "b2"] {
        farm.host(host, "beta");
    }
    farm.want("alpha", "web", "2.0");

    assert_eq!(farm.tell("b1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("b2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("a1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: a group's `max_updating` of 0 holds the group, while another group's hosts are
/// ordered; the default of 1 lets one host of a group update at a time.
///
/// Catches: the limit read from the desired state's per-role map, or from the rollout file.
#[test]
fn max_updating_zero_holds_the_group() {
    let farm = Farm::new("gd-max-updating");
    farm.group("held", &["web"], Some(policy(|p| p.max_updating = Some(0))));
    farm.group("free", &["web"], None);
    farm.host("h1", "held");
    farm.host("f1", "free");
    farm.want("held", "web", "2.0");
    farm.want("free", "web", "2.0");

    assert_eq!(farm.tell("h1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("f1", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: `min_up` counts the group's live hosts: a group of two hosts that must keep two up
/// orders neither, though the coordinator has many more live hosts in other groups, and a group
/// that must keep one up orders one of two.
///
/// Catches: the minimum counted over the fleet, which would order the host of the small group
/// because other groups' hosts are up.
#[test]
fn min_up_counts_the_groups_live_hosts() {
    let farm = Farm::new("gd-min-up");
    farm.group(
        "tight",
        &["web"],
        Some(policy(|p| {
            p.min_up = 2;
            p.max_updating = Some(2);
        })),
    );
    farm.group(
        "loose",
        &["web"],
        Some(policy(|p| {
            p.min_up = 1;
            p.max_updating = Some(2);
        })),
    );
    farm.group("big", &["web"], None);
    for (host, group) in [
        ("t1", "tight"),
        ("t2", "tight"),
        ("l1", "loose"),
        ("l2", "loose"),
        ("g1", "big"),
        ("g2", "big"),
        ("g3", "big"),
        ("g4", "big"),
        ("g5", "big"),
    ] {
        farm.host(host, group);
        assert_eq!(farm.tell(host, &[("web", "1.0")]), NOTHING);
    }
    for group in ["tight", "loose"] {
        farm.want(group, "web", "2.0");
    }

    assert_eq!(farm.tell("t1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("t2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("l1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("l2", &[("web", "1.0")]), NOTHING);
}

/// Proves: the group's stages bound how many hosts its plan reaches: with a first stage of half
/// the hosts, two of four hosts are ordered, and the other two wait for the first two to run
/// the version; then they are ordered.
///
/// Catches: a plan read from the rollout file's defaults, which would go to one host at a time
/// by its first stage.
#[test]
fn the_groups_stages_bound_the_hosts_ordered() {
    let farm = Farm::new("gd-stages");
    farm.group(
        "alpha",
        &["web"],
        Some(policy(|p| {
            p.stages = stages(&[50, 100]);
            p.max_updating = Some(4);
        })),
    );
    let hosts = ["a1", "a2", "a3", "a4"];
    for host in hosts {
        farm.host(host, "alpha");
        assert_eq!(farm.tell(host, &[("web", "1.0")]), NOTHING);
    }
    farm.want("alpha", "web", "2.0");

    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a3", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("a4", &[("web", "1.0")]), NOTHING);
    for host in ["a1", "a2"] {
        assert_eq!(farm.tell(host, &[("web", "2.0")]), NOTHING);
    }
    assert_eq!(farm.tell("a3", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a4", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: a group's canary goes first and its `bake_beats` hold the rest: with a canary of one
/// host and two bake beats, the first host is ordered alone, the others wait while it runs the
/// version and while it has beaten healthy once, and are ordered after its second healthy beat.
///
/// Catches: a canary or a bake read from the rollout file, or counted once for the fleet.
#[test]
fn the_groups_canary_and_bake_hold_the_rest() {
    let farm = Farm::new("gd-canary-bake");
    farm.group(
        "alpha",
        &["web"],
        Some(policy(|p| {
            p.stages = stages(&[100]);
            p.canary = Some(hostkeeper_proto::hostkeeper::operator::v1::Canary { count: 1 });
            p.bake_beats = 2;
            p.max_updating = Some(3);
        })),
    );
    for host in ["a1", "a2", "a3"] {
        farm.host(host, "alpha");
        assert_eq!(farm.tell(host, &[("web", "1.0")]), NOTHING);
    }
    farm.want("alpha", "web", "2.0");

    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("a1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("a1", &[("web", "2.0")]), NOTHING);
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("a3", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: `live_rollouts` counts within the group: with the default of one, a second package of
/// a group waits while the first rolls (`rollout_elsewhere`) and a package of another group does
/// not; with a policy of two, the second package rolls beside the first.
///
/// Catches: the live rollouts counted over every group's packages, which would hold the second
/// group's package while the first group's rolls.
#[test]
fn live_rollouts_count_within_the_group_and_not_across_groups() {
    let farm = Farm::new("gd-live");
    farm.group("one", &["web", "db"], None);
    farm.group(
        "two",
        &["web", "db"],
        Some(policy(|p| p.live_rollouts = Some(2))),
    );
    farm.group("other", &["web"], None);
    for host in ["o1", "o2"] {
        farm.host(host, "one");
    }
    for host in ["t1", "t2"] {
        farm.host(host, "two");
    }
    farm.host("x1", "other");
    for group in ["one", "two"] {
        farm.want(group, "web", "2.0");
        farm.want(group, "db", "2.0");
    }
    farm.want("other", "web", "2.0");
    let both = [("web", "1.0"), ("db", "1.0")];

    assert_eq!(farm.tell("o1", &both), update("web", "2.0"));
    assert_eq!(farm.tell("o2", &both), NOTHING);
    assert_eq!(farm.tell("x1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(farm.tell("t1", &both), update("web", "2.0"));
    assert_eq!(farm.tell("t2", &both), update("db", "2.0"));
}

/// Proves: a halt a host reports halts the plan of its group's package and no other group's: the
/// halt is kept as the group's mark and not the package's, the group's other host is ordered
/// nothing of it, another group's host is ordered the version, and the halted package holds no
/// live rollout of the group's other package.
///
/// Catches: a halt written to the package, which would stop every group that lists it, or a halted
/// package that keeps another of its group from rolling.
#[test]
fn a_halt_stops_the_plan_of_its_group_alone() {
    let farm = Farm::new("gd-halt");
    farm.group(
        "alpha",
        &["web", "db"],
        Some(policy(|p| p.max_updating = Some(2))),
    );
    farm.group("beta", &["web"], None);
    farm.host("a1", "alpha");
    farm.host("a2", "alpha");
    farm.host("b1", "beta");
    farm.want("alpha", "web", "2.0");
    farm.want("alpha", "db", "2.0");
    farm.want("beta", "web", "2.0");

    assert_eq!(
        farm.tell_with(
            "a1",
            vec![
                runs("hostkeeper", "1.0"),
                failed("web", "1.0", "2.0", Phase::Halt)
            ]
        ),
        NOTHING
    );

    let ops = farm.world.ops();
    let alpha: GroupName = "alpha".parse().expect("a group");
    let beta: GroupName = "beta".parse().expect("a group");
    assert!(ops.group_marks(&alpha, "web").halt.is_some());
    assert!(ops.group_marks(&beta, "web").halt.is_none());
    assert!(
        ops.services
            .get("web")
            .is_none_or(|marks| marks.halt.is_none())
    );
    assert_eq!(farm.tell("a2", &[("web", "1.0")]), NOTHING);
    assert_eq!(
        farm.tell("a2", &[("web", "1.0"), ("db", "1.0")]),
        update("db", "2.0")
    );
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
}

/// Proves: the next heartbeat of a moved host keeps its new group: the node the move placed is
/// not rewritten from the host's old record, and the host is ordered the new group's version.
///
/// Catches: a heartbeat that records the role of the host's certificate over the move's.
#[test]
fn a_moved_hosts_next_heartbeat_keeps_the_new_group() {
    let farm = Farm::new("gd-moved");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    farm.host("m1", "alpha");
    farm.want("alpha", "web", "2.0");
    farm.want("beta", "web", "3.0");
    assert_eq!(farm.tell("m1", &[("web", "1.0")]), update("web", "2.0"));

    farm.world.move_to("m1", "beta").expect("moved");
    assert_eq!(
        farm.world
            .coordinator()
            .lock_fleet()
            .node(&id("m1"))
            .expect("a node")
            .role,
        "beta"
    );
    assert_eq!(farm.tell("m1", &[("web", "1.0")]), update("web", "3.0"));
    assert_eq!(
        farm.world
            .coordinator()
            .lock_fleet()
            .node(&id("m1"))
            .expect("a node")
            .role,
        "beta"
    );
}

/// Proves: a heartbeat reads its host's group while it holds the fleet's lock, which a
/// `HostGroups.Move` of the host also runs under: a heartbeat stopped where it reads the clock to
/// decide the host finds the fleet locked, so a move can neither land between the read of the
/// host's group and the lock, nor be undone by a heartbeat that read the group it left.
///
/// Catches: a heartbeat that reads the host's group, and takes the fleet's lock after, and so
/// records the host under the group it was moved out of and ends the move the call began.
#[test]
fn a_heartbeat_reads_the_hosts_group_under_the_fleets_lock() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, TryLockError, mpsc};

    let mut farm = Farm::new("gd-group-under-lock");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    farm.host("m1", "alpha");
    farm.want("alpha", "web", "2.0");
    farm.want("beta", "web", "3.0");

    let (inside_tx, inside_rx) = mpsc::channel();
    let done_tx = inside_tx.clone();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let stopped = AtomicBool::new(false);
    let clock = Arc::clone(&farm.world.coordinator().clock);
    farm.world.rig.coordinator.clock = Arc::new(move || {
        if !stopped.swap(true, Ordering::SeqCst) {
            inside_tx.send(true).expect("the test listens");
            go_rx
                .lock()
                .expect("the receiver")
                .recv()
                .expect("the test lets it go on");
        }
        clock()
    });

    let (locked, reply) = std::thread::scope(|scope| {
        let farm = &farm;
        let heartbeat = scope.spawn(move || {
            let reply = farm.tell("m1", &[("web", "1.0")]);
            // Wakes the test even when the heartbeat never read the clock, so it never waits
            // without end.
            let _ = done_tx.send(false);
            reply
        });
        let inside = inside_rx
            .recv()
            .expect("the heartbeat reads the clock or ends");
        assert!(inside, "the heartbeat ended without reading the clock");
        let locked = matches!(
            farm.world.coordinator().fleet.try_lock(),
            Err(TryLockError::WouldBlock)
        );
        go_tx.send(()).expect("the heartbeat waits");
        (locked, heartbeat.join().expect("the heartbeat ends"))
    });
    assert!(locked, "the host's group was read outside the fleet's lock");
    assert_eq!(reply, update("web", "2.0"));
}

/// What `Hosts.Get` says of the first serving heartbeat of `host`.
fn first_serving(farm: &Farm, host: &str) -> Option<u64> {
    farm.world
        .coordinator()
        .get_host(
            &crate::marks_support::operator(),
            &hostkeeper_proto::hostkeeper::operator::v1::GetHostRequest {
                node_id: host.to_owned(),
            },
        )
        .expect("answered")
        .host
        .expect("a host")
        .first_serving_unix
}

/// Proves: the first serving heartbeat of a host is read against the packages of its group: a
/// healthy package its group lists is serving, one only another group lists is not.
///
/// Catches: serving read against the rollout file's services, which place no host by role in group
/// mode, so that no host is ever noted serving.
#[test]
fn a_host_serves_the_packages_its_group_lists() {
    let farm = Farm::new("gd-serving");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["db"], None);
    farm.host("a1", "alpha");
    farm.host("b1", "beta");

    farm.tell_with("b1", vec![runs("web", "1.0")]);
    assert_eq!(first_serving(&farm, "b1"), None);
    farm.tell_with("a1", vec![runs("web", "1.0")]);
    assert!(first_serving(&farm, "a1").is_some());
}
