//! `Releases.Converge`, `WaitConverged`, `GetRelease`, `Releases.Status` and `HostGroups.Get` for
//! a coordinator that holds host groups ([`crate::group_converge`]): the convergence of a package
//! in one group, in every group that lists it, and what the status says of each pair of a group
//! and a package.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::WaitOutcome;
use serde_json::json;

use crate::converge::{Convergence, HaltNote, PickCounts, RoleCounts, State};
use crate::group_converge::aggregate;
use crate::group_decision_support::Farm;
use crate::marks_support::operator;
use crate::membership::HostCounts;
use crate::ops::{Halt, HaltOrigin};
use crate::releases_tests::targets;
use crate::service::{ConvergenceError, ci_calls::ReleaseError};
use crate::testutil::{ci_token, version};
use crate::wait::WaitRefused;

const WAIT: Duration = Duration::from_secs(5);

/// Two groups that list `web`, a host in each, and `web` set at 2.0 in both.
async fn two_groups(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    farm.host("a1", "alpha");
    farm.host("b1", "beta");
    farm.deploy("", "web", "2.0", 1).await;
    farm
}

fn roles(convergence: &Convergence) -> Vec<(&str, usize)> {
    convergence
        .roles
        .iter()
        .map(|counts| (counts.role.as_str(), counts.at_desired))
        .collect()
}

fn converge(farm: &Farm, group: &str) -> Convergence {
    farm.world
        .coordinator()
        .convergence_in(group, "web")
        .expect("a convergence")
}

/// Proves: with no group named, the convergence of a package is the state of every group that
/// lists it and has a desired version of it: in progress while one group's host is behind,
/// converged once every host runs the version, the roles of the answer being the groups' names.
/// The route CI reads (`GET /v1/convergence`) answers the same, in the shape it always had.
///
/// Catches: a convergence read from the rollout file's roles, or only from the first group.
#[tokio::test]
async fn a_convergence_with_no_group_is_every_groups_that_lists_the_package() {
    let farm = two_groups("gc-all").await;
    farm.tell("a1", &[("web", "2.0")]);
    farm.tell("b1", &[("web", "1.0")]);

    let behind = converge(&farm, "");
    assert_eq!(behind.state, State::InProgress);
    assert_eq!(behind.desired, Some(version("2.0")));
    assert_eq!(roles(&behind), [("alpha", 1)]);

    farm.tell("b1", &[("web", "2.0")]);
    let done = converge(&farm, "");
    assert_eq!(done.state, State::Converged);
    assert_eq!(roles(&done), [("alpha", 1), ("beta", 1)]);

    let route = farm
        .world
        .coordinator()
        .convergence(Some(&ci_token()), "web")
        .await
        .expect("read by CI");
    assert_eq!(
        serde_json::to_value(&route).expect("JSON"),
        json!({
            "desired": "2.0",
            "quiet": false,
            "state": "converged",
            "roles": [
                {"role": "alpha", "at_desired": 1, "updating": 0, "failed": 0, "down": 0},
                {"role": "beta", "at_desired": 1, "updating": 0, "failed": 0, "down": 0},
            ],
            "picks": {
                "picked": 2,
                "no_target": 0,
                "no_build_for_host": 0,
                "installer_dropped": 0,
                "dropped": [],
            },
            "commit": crate::BUILD_COMMIT,
        })
    );
}

/// Proves: a convergence that names a group is that group's alone: converged while another group
/// that lists the package is behind, and naming a group that does not list the package, or one
/// that does not exist, is `not_in_host_group`; a package nothing defines is `unknown_service`.
///
/// Catches: a named group answered with the state of every group, or an unknown name read as none.
#[tokio::test]
async fn a_convergence_that_names_a_group_is_that_groups_alone() {
    let farm = two_groups("gc-named").await;
    farm.group("gamma", &["db"], None);
    farm.tell("a1", &[("web", "2.0")]);
    farm.tell("b1", &[("web", "1.0")]);

    let alpha = converge(&farm, "alpha");
    assert_eq!(
        (alpha.state, roles(&alpha)),
        (State::Converged, [("alpha", 1)].into())
    );
    assert_eq!(converge(&farm, "beta").state, State::InProgress);
    farm.host("a2", "alpha");
    farm.tell("a2", &[("web", "1.0")]);
    let half = converge(&farm, "alpha");
    assert_eq!(
        (half.state, roles(&half)),
        (State::InProgress, [("alpha", 1)].into())
    );

    let coordinator = farm.world.coordinator();
    for named in ["gamma", "nowhere", "Not A Name"] {
        assert_eq!(
            coordinator.convergence_in(named, "web").err(),
            Some(ConvergenceError::NotInHostGroup),
            "{named}"
        );
    }
    assert_eq!(
        coordinator.convergence_in("", "nobody").err(),
        Some(ConvergenceError::UnknownService)
    );
    assert_eq!(
        coordinator.convergence_in("", "db").map(|c| c.desired),
        Ok(None)
    );
}

/// Proves: a group with no hosts is converged, whatever the quiet start says; a group whose
/// enrolled host has not been heard from is not, and a group with a host that is behind is not.
///
/// Catches: an empty group that holds `WaitConverged` for good, or an enrolled host nobody has
/// heard of read as no host.
#[tokio::test]
async fn a_group_with_no_hosts_is_converged() {
    let farm = Farm::new("gc-empty");
    farm.group("empty", &["web"], None);
    farm.group("silent", &["web"], None);
    farm.host("s1", "silent");
    farm.deploy("", "web", "2.0", 1).await;

    assert_eq!(converge(&farm, "empty").state, State::Converged);
    assert_eq!(converge(&farm, "silent").state, State::InProgress);
    assert_eq!(converge(&farm, "").state, State::InProgress);

    farm.tell("s1", &[("web", "2.0")]);
    assert_eq!(converge(&farm, "silent").state, State::Converged);
    assert_eq!(converge(&farm, "").state, State::Converged);
}

/// Proves: a pair with no desired version is left out of the answer: a package set in one group
/// only is judged by that group, a group that lists it with no desired version answers no desired
/// version and no convergence, and a coordinator that has a halt in one group answers `halted`.
///
/// Catches: a group with nothing set holding the other groups' convergence back.
#[tokio::test]
async fn a_pair_with_no_desired_version_is_left_out_of_the_verdict() {
    let farm = Farm::new("gc-left-out");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    farm.host("a1", "alpha");
    farm.host("b1", "beta");
    farm.deploy("alpha", "web", "2.0", 1).await;
    farm.tell("a1", &[("web", "2.0")]);
    farm.tell("b1", &[("web", "1.0")]);

    let all = converge(&farm, "");
    assert_eq!(
        (all.state, roles(&all)),
        (State::Converged, [("alpha", 1)].into())
    );
    let beta = converge(&farm, "beta");
    assert_eq!((beta.state, beta.desired), (State::InProgress, None));

    farm.world.seed_marks("alpha", "web", |marks| {
        marks.halt = Some(Halt {
            node_id: "a1".to_owned(),
            check: "artifact_changed".to_owned(),
            target: version("2.0"),
            report: None,
            origin: HaltOrigin::Updater,
        });
    });
    let halted = converge(&farm, "");
    assert_eq!(halted.state, State::Halted);
    assert_eq!(halted.halt.map(|halt| halt.node_id), Some("a1".to_owned()));
}

/// Proves: the record of the answer is the group's own: a named group answers its generation, no
/// group answers the record of the only group with a desired version and none when two have one.
///
/// Catches: the legacy service's record shown for a group.
#[tokio::test]
async fn the_write_record_is_the_groups_when_one_group_is_meant() {
    let farm = two_groups("gc-record").await;
    let coordinator = farm.world.coordinator();

    assert_eq!(
        coordinator
            .write_record_in("alpha", "web")
            .map(|r| r.generation),
        Some(1)
    );
    assert_eq!(coordinator.write_record_in("", "web"), None);
    farm.group("solo", &["web"], None);
    farm.deploy("solo", "web", "3.0", 2).await;
    farm.deploy("solo", "web", "4.0", 3).await;
    farm.world
        .put("alpha", &["hostkeeper"], Some(1))
        .expect("put");
    farm.world
        .put("beta", &["hostkeeper"], Some(1))
        .expect("put");
    assert_eq!(
        coordinator.write_record_in("", "web").map(|r| r.generation),
        Some(2)
    );
    assert_eq!(coordinator.write_record_in("nowhere", "web"), None);
}

/// Proves: `GetRelease` reads the package's release for a group that lists the package and for
/// no group, refuses a group that does not list it or does not exist as `not_in_host_group`, and
/// answers `unknown_service` and `release_unknown` as it did.
///
/// Catches: a release looked up per group, or a package a group does not list read through it.
#[tokio::test]
async fn a_release_is_read_for_a_group_that_lists_the_package() {
    let farm = Farm::new("gc-release");
    farm.group("alpha", &["db"], None);
    farm.group("beta", &["web"], None);
    farm.set(
        "",
        &json!({"service": "db", "version": "2.0"}),
        Some(targets("db")),
    )
    .await;
    let coordinator = farm.world.coordinator();

    for named in ["", "alpha"] {
        assert!(
            coordinator.release_verified_in(named, "db", "2.0").is_ok(),
            "{named}"
        );
        assert_eq!(
            coordinator.release_verified_in(named, "db", "3.0").err(),
            Some(ReleaseError::Unknown),
            "{named}"
        );
    }
    for named in ["beta", "nowhere"] {
        assert_eq!(
            coordinator.release_verified_in(named, "db", "2.0").err(),
            Some(ReleaseError::NotInHostGroup),
            "{named}"
        );
    }
    assert_eq!(
        coordinator.release_verified_in("", "nobody", "2.0").err(),
        Some(ReleaseError::UnknownService)
    );
}

/// Proves: a wait with no group ends `no_desired` at once when no group has a desired version of
/// the package, `converged` when every group is, `superseded` when a group wants another version
/// than the caller's, `halted` when one is halted, and at its deadline when one is behind; a wait
/// that names a group reads that group alone, and one for a group that does not list the package
/// is refused.
///
/// Catches: a wait on a package nothing sets that is held to its deadline, or one that ends
/// before every group has the version.
#[tokio::test(start_paused = true)]
async fn a_wait_ends_by_the_groups_it_reaches() {
    let farm = Farm::new("gc-wait");
    farm.group("alpha", &["web"], None);
    farm.group("beta", &["web"], None);
    farm.host("a1", "alpha");
    farm.host("b1", "beta");
    let coordinator = farm.world.coordinator();
    let wait = |named: &'static str, at: &'static str| async move {
        let held = coordinator.hold_wait("web", named).expect("held");
        coordinator.wait_converged(&held, at, WAIT).await
    };

    let (outcome, convergence) = wait("", "2.0").await;
    assert_eq!(
        (outcome, convergence.desired),
        (WaitOutcome::NoDesired, None)
    );
    assert_eq!(wait("alpha", "2.0").await.0, WaitOutcome::NoDesired);

    farm.deploy("", "web", "2.0", 1).await;
    farm.tell("a1", &[("web", "2.0")]);
    farm.tell("b1", &[("web", "1.0")]);
    assert_eq!(wait("", "2.0").await.0, WaitOutcome::Deadline);
    assert_eq!(wait("alpha", "2.0").await.0, WaitOutcome::Converged);
    assert_eq!(wait("beta", "2.0").await.0, WaitOutcome::Deadline);

    farm.tell("b1", &[("web", "2.0")]);
    assert_eq!(wait("", "2.0").await.0, WaitOutcome::Converged);

    farm.deploy("beta", "web", "3.0", 2).await;
    let (outcome, convergence) = wait("", "2.0").await;
    assert_eq!(outcome, WaitOutcome::Superseded);
    assert_eq!(convergence.desired, Some(version("3.0")));
    assert_eq!(wait("alpha", "2.0").await.0, WaitOutcome::Converged);

    farm.world.seed_marks("alpha", "web", |marks| {
        marks.halt = Some(Halt {
            node_id: "a1".to_owned(),
            check: "artifact_changed".to_owned(),
            target: version("2.0"),
            report: None,
            origin: HaltOrigin::Updater,
        });
    });
    assert_eq!(wait("alpha", "2.0").await.0, WaitOutcome::Halted);

    farm.group("gamma", &["db"], None);
    assert_eq!(
        coordinator.hold_wait("web", "gamma").err(),
        Some(WaitRefused::NotInHostGroup)
    );
    assert_eq!(
        coordinator.hold_wait("nobody", "").err(),
        Some(WaitRefused::UnknownService)
    );
}

fn counts(role: &str, at_desired: usize) -> RoleCounts {
    RoleCounts {
        role: role.to_owned(),
        at_desired,
        updating: 0,
        failed: 0,
        down: 0,
    }
}

fn part(desired: Option<&str>, state: State) -> Convergence {
    Convergence {
        desired: desired.map(version),
        quiet: false,
        state,
        roles: Vec::new(),
        halt: None,
        hosts: None,
        picks: None,
        commit: "unknown",
    }
}

/// Proves: several convergences are one: halted wins over blocked over in progress over
/// converged, none at all is in progress with no version, the roles follow one another, the halt
/// is the first, the counts of hosts not counted and of picks are summed, and the version named is
/// the first group's, or the first that differs from the version a wait is for.
///
/// Catches: a verdict that calls a farm converged while one group is not, or that loses the halt,
/// the roles or the counts of one group.
#[test]
fn several_convergences_are_one_by_the_worst_state_with_everything_kept() {
    let state = |states: &[State]| {
        let parts = states.iter().map(|s| part(Some("2.0"), *s)).collect();
        aggregate(parts, false, None).state
    };
    assert_eq!(
        state(&[State::Converged, State::Converged]),
        State::Converged
    );
    assert_eq!(
        state(&[State::Converged, State::InProgress]),
        State::InProgress
    );
    assert_eq!(state(&[State::InProgress, State::Blocked]), State::Blocked);
    assert_eq!(
        state(&[State::Blocked, State::Halted, State::Converged]),
        State::Halted
    );
    let none = aggregate(Vec::new(), true, None);
    assert_eq!(
        (none.state, none.desired, none.quiet),
        (State::InProgress, None, true)
    );

    let mut first = part(Some("2.0"), State::Converged);
    first.roles = vec![counts("alpha", 2)];
    first.hosts = Some(HostCounts {
        missing: 1,
        deferred: 2,
        ..HostCounts::default()
    });
    first.picks = Some(PickCounts {
        picked: 3,
        no_target: 1,
        dropped: vec!["a1".to_owned()],
        ..PickCounts::default()
    });
    let mut second = part(Some("3.0"), State::Halted);
    second.roles = vec![counts("beta", 1)];
    second.hosts = Some(HostCounts {
        missing: 4,
        absent: 1,
        excluded: 2,
        unmanaged: 3,
        deferred: 1,
    });
    second.picks = Some(PickCounts {
        picked: 1,
        no_build_for_host: 2,
        installer_dropped: 1,
        dropped: vec!["b1".to_owned()],
        ..PickCounts::default()
    });
    second.halt = Some(HaltNote {
        node_id: "b1".to_owned(),
        check: "c".to_owned(),
        target: version("3.0"),
    });
    let sum = aggregate(vec![first.clone(), second.clone()], false, None);
    assert_eq!(sum.desired, Some(version("2.0")));
    assert_eq!(roles(&sum), [("alpha", 2), ("beta", 1)]);
    assert_eq!(sum.halt.map(|halt| halt.node_id), Some("b1".to_owned()));
    assert_eq!(
        sum.hosts,
        Some(HostCounts {
            missing: 5,
            absent: 1,
            excluded: 2,
            unmanaged: 3,
            deferred: 3,
        })
    );
    assert_eq!(
        sum.picks,
        Some(PickCounts {
            picked: 4,
            no_target: 1,
            no_build_for_host: 2,
            installer_dropped: 1,
            dropped: vec!["a1".to_owned(), "b1".to_owned()],
        })
    );
    let waiting = aggregate(vec![first, second], false, Some("2.0"));
    assert_eq!(waiting.desired, Some(version("3.0")));
}

/// Proves: `Releases.Status` of a coordinator that holds groups answers one service for each
/// package of each group, by group and then package, `host_group` naming the group, its hosts
/// those of the group alone, its role counts keyed by the group's name, its desired version the
/// group's, its marks and force those of the group's, and its plan naming the group; a request for
/// one package answers its pairs.
///
/// Catches: a status of the rollout file's services, hosts of another group listed under a
/// package, or the marks of a package shown for a group.
#[tokio::test]
async fn the_status_has_a_service_for_each_pair_of_a_group_and_a_package() {
    let farm = two_groups("gc-status").await;
    farm.group("gamma", &["db"], None);
    farm.tell("a1", &[("web", "2.0")]);
    farm.tell("b1", &[("web", "1.0")]);
    farm.deploy("alpha", "web", "3.0", 4).await;
    farm.world.seed_marks("beta", "web", |marks| {
        marks.last_good = Some(version("1.0"));
    });
    let coordinator = farm.world.coordinator();
    let now = (coordinator.clock)();
    let status = {
        let fleet = coordinator.lock_fleet();
        coordinator.status_in(&fleet, now)
    };

    let answer = crate::grpc::report::status(&status, "");
    let keys: Vec<(&str, &str)> = answer
        .services
        .iter()
        .map(|one| (one.host_group.as_str(), one.service.as_str()))
        .collect();
    assert_eq!(
        keys,
        [
            ("alpha", "hostkeeper"),
            ("alpha", "web"),
            ("beta", "hostkeeper"),
            ("beta", "web"),
            ("gamma", "db"),
            ("gamma", "hostkeeper"),
        ]
    );
    let of = |group: &str, package: &str| {
        answer
            .services
            .iter()
            .find(|one| one.host_group == group && one.service == package)
            .expect("a service")
    };
    let alpha = of("alpha", "web");
    assert_eq!(
        alpha.desired.as_ref().map(|d| d.version.as_str()),
        Some("3.0")
    );
    assert_eq!(
        alpha
            .hosts
            .iter()
            .map(|h| h.node_id.as_str())
            .collect::<Vec<_>>(),
        ["a1"]
    );
    assert_eq!(alpha.hosts[0].role, "alpha");
    assert_eq!(
        alpha.plan.as_ref().map(|plan| plan.host_group.as_str()),
        Some("alpha")
    );
    let beta = of("beta", "web");
    assert_eq!(
        beta.desired.as_ref().map(|d| d.version.as_str()),
        Some("2.0")
    );
    assert_eq!(
        beta.marks
            .as_ref()
            .and_then(|marks| marks.last_good.clone()),
        Some("1.0".to_owned())
    );
    assert_eq!(
        alpha
            .marks
            .as_ref()
            .and_then(|marks| marks.last_good.clone()),
        None
    );
    assert_eq!(
        beta.convergence.as_ref().map(|c| c.roles.is_empty()),
        Some(true)
    );
    assert_eq!(of("gamma", "db").desired, None);
    assert_eq!(of("gamma", "db").hosts, []);

    let only = crate::grpc::report::status(&status, "web");
    assert_eq!(only.services.len(), 2);
    assert!(only.services.iter().all(|one| one.service == "web"));
    assert_eq!(crate::grpc::report::status(&status, "nobody").services, []);
}

/// Proves: `HostGroups.Get` answers each package's convergence as `Converge` does for the group:
/// the desired version and the state with the group's name as the role, none for a package with
/// no desired version, and the empty group's converged.
///
/// Catches: the empty convergence a group's package was answered before it was filled in.
#[tokio::test]
async fn get_answers_each_packages_convergence_in_the_group() {
    let farm = Farm::new("gc-get");
    farm.group("alpha", &["web", "db"], None);
    farm.group("empty", &["web"], None);
    farm.host("a1", "alpha");
    farm.deploy("", "web", "2.0", 1).await;
    farm.tell("a1", &[("web", "2.0")]);
    let coordinator = farm.world.coordinator();
    let get = |group: &str| {
        coordinator
            .host_groups_get(
                &operator(),
                &proto::GetHostGroupRequest {
                    host_group: group.to_owned(),
                    limit: 0,
                    page_token: String::new(),
                },
            )
            .expect("got")
            .host_group
            .expect("the group")
            .packages
    };

    let alpha = get("alpha");
    let web = alpha
        .iter()
        .find(|package| package.package == "web")
        .and_then(|package| package.convergence.clone())
        .expect("a convergence");
    assert_eq!(web.desired.as_deref(), Some("2.0"));
    assert_eq!(web.state, proto::ConvergenceState::Converged as i32);
    assert_eq!(
        web.roles
            .iter()
            .map(|r| (r.role.as_str(), r.at_desired))
            .collect::<Vec<_>>(),
        [("alpha", 1)]
    );
    let db = alpha
        .iter()
        .find(|package| package.package == "db")
        .and_then(|package| package.convergence.clone())
        .expect("a convergence");
    assert_eq!(
        (db.desired, db.state),
        (None, proto::ConvergenceState::InProgress as i32)
    );
    let empty = get("empty");
    assert_eq!(
        empty[1].convergence.as_ref().map(|c| c.state),
        Some(proto::ConvergenceState::Converged as i32)
    );
}
