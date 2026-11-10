//! The operator's calls by host group once the coordinator holds groups (`docs/design/hostgroups.md`,
//! sections 4 and 7.2): the plans a pause, a resume and an advance act on, the minimum version of
//! a group, a hold of a group, the refusal of an assignment, what `Services.List`, `Hosts.List`,
//! `Hosts.Get` and `Audit.List` answer, and the halt of each group.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::min_version::Scope;
use hostkeeper_proto::hostkeeper::operator::v1::{PolicyStage, StageAdvance};
use hostkeeper_wire::wire::ServiceReply;

use crate::group_decision_support::{Farm, policy};
use crate::host_groups::GroupName;
use crate::marks_support::{hold, operator, scope_host, scope_role, unhold, update};
use crate::testutil::version;

const NOTHING: ServiceReply = ServiceReply::Nothing;

fn group(name: &str) -> GroupName {
    name.parse().expect("a group name")
}

/// Alpha (a1, a2) and beta (b1) list `web`, and alpha lists `cfg` as well; gamma (c1) lists `db`.
/// `web` 1.0 is desired in alpha and beta, and every host runs it.
async fn farm(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.group("alpha", &["web", "cfg"], None);
    farm.group("beta", &["web"], None);
    farm.group("gamma", &["db"], None);
    for (host, group) in [
        ("a1", "alpha"),
        ("a2", "alpha"),
        ("b1", "beta"),
        ("c1", "gamma"),
    ] {
        farm.host(host, group);
    }
    farm.deploy("", "web", "1.0", 1).await;
    farm.deploy("alpha", "cfg", "1.0", 5).await;
    farm.deploy("gamma", "db", "1.0", 6).await;
    for host in ["a1", "a2"] {
        assert_eq!(farm.tell(host, &[("web", "1.0"), ("cfg", "1.0")]), NOTHING);
    }
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), NOTHING);
    assert_eq!(farm.tell("c1", &[("db", "1.0")]), NOTHING);
    farm
}

fn pause(
    farm: &Farm,
    service: &str,
    host_group: &str,
) -> Result<proto::PauseResponse, proto::RolloutsRefusal> {
    farm.world.coordinator().rollout_pause(
        &operator(),
        proto::PauseRequest {
            service: service.to_owned(),
            reason: "night".to_owned(),
            host_group: host_group.to_owned(),
        },
    )
}

fn resume(
    farm: &Farm,
    service: &str,
    host_group: &str,
) -> Result<proto::ResumeResponse, proto::RolloutsRefusal> {
    farm.world.coordinator().rollout_resume(
        &operator(),
        proto::ResumeRequest {
            service: service.to_owned(),
            reason: "morning".to_owned(),
            host_group: host_group.to_owned(),
        },
    )
}

fn advance(
    farm: &Farm,
    service: &str,
    host_group: &str,
) -> Result<proto::AdvanceResponse, proto::RolloutsRefusal> {
    farm.world.coordinator().rollout_advance(
        &operator(),
        proto::AdvanceRequest {
            service: service.to_owned(),
            reason: "looks fine".to_owned(),
            host_group: host_group.to_owned(),
        },
    )
}

fn paused(farm: &Farm, name: &str, package: &str) -> bool {
    farm.world
        .ops()
        .rollout_in(Some(&group(name)), package)
        .is_some_and(|rollout| rollout.paused.is_some())
}

fn entries(entries: &[crate::ops::AuditEntry]) -> Vec<(String, Option<String>)> {
    entries
        .iter()
        .map(|entry| (entry.action.clone(), entry.host_group.clone()))
        .collect()
}

/// Proves: a pause that names no group pauses the plan of the package in every group that lists
/// it, in one change, with one audit entry for each naming its group, and not the plan of a group
/// that does not list it; a second pause is `already_paused` and one that finds only some groups
/// paused pauses the rest; a paused group's host is ordered nothing while the other group's host
/// goes on.
///
/// Catches: a pause that stops one group, the package alone, or every group including those
/// that do not run it.
#[tokio::test]
async fn a_pause_without_a_group_pauses_each_group_that_lists_the_package() {
    let farm = farm("gc-pause").await;
    let audited = farm.world.audit().len();

    let done = pause(&farm, "web", "").expect("paused");

    assert!(paused(&farm, "alpha", "web") && paused(&farm, "beta", "web"));
    assert!(!paused(&farm, "gamma", "web") && !paused(&farm, "alpha", "cfg"));
    assert_eq!(
        entries(&farm.world.audit_after(audited)),
        [
            ("rollout_pause".to_owned(), Some("alpha".to_owned())),
            ("rollout_pause".to_owned(), Some("beta".to_owned()))
        ]
    );
    assert_eq!(
        done.entry.map(|entry| entry.host_group),
        Some(Some("alpha".to_owned()))
    );
    assert_eq!(
        done.plan.map(|plan| plan.host_group),
        Some("alpha".to_owned())
    );
    assert_eq!(
        pause(&farm, "web", "").err(),
        Some(proto::RolloutsRefusal::AlreadyPaused)
    );

    resume(&farm, "web", "beta").expect("resumed");
    let audited = farm.world.audit().len();
    pause(&farm, "web", "").expect("paused");
    assert_eq!(
        entries(&farm.world.audit_after(audited)),
        [("rollout_pause".to_owned(), Some("beta".to_owned()))]
    );
}

/// Proves: a paused plan of one group orders its hosts nothing while the plan of another group
/// that lists the package goes on, and resuming the group's plan orders them again.
///
/// Catches: a pause that holds every group's plan, or none.
#[tokio::test]
async fn a_paused_group_orders_nothing_and_another_group_goes_on() {
    let farm = farm("gc-paused").await;
    farm.deploy("", "web", "2.0", 2).await;
    pause(&farm, "web", "alpha").expect("paused");

    assert_eq!(farm.tell("a1", &[("web", "1.0"), ("cfg", "1.0")]), NOTHING);
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));

    resume(&farm, "web", "alpha").expect("resumed");
    assert_eq!(
        farm.tell("a1", &[("web", "1.0"), ("cfg", "1.0")]),
        update("web", "2.0")
    );
}

/// Proves: a resume or an advance without a group acts on the one group that lists the package and
/// is `ambiguous_host_group` for a package more than one lists; a group that does not exist, does
/// not list the package or a package no group lists, are `not_in_host_group`; a name that is no
/// group name is `invalid_request`; resuming a plan that is not paused is `not_paused`.
///
/// Catches: a resume without a group that resumes every group, or the first.
#[tokio::test]
async fn a_resume_without_a_group_needs_exactly_one_group_to_list_the_package() {
    let farm = farm("gc-resume").await;
    pause(&farm, "web", "").expect("paused");
    pause(&farm, "cfg", "").expect("paused");
    let before = farm.world.state();

    assert_eq!(
        resume(&farm, "web", "").err(),
        Some(proto::RolloutsRefusal::AmbiguousHostGroup)
    );
    assert_eq!(
        advance(&farm, "web", "").err(),
        Some(proto::RolloutsRefusal::AmbiguousHostGroup)
    );
    for named in ["nothing", "gamma"] {
        assert_eq!(
            resume(&farm, "web", named).err(),
            Some(proto::RolloutsRefusal::NotInHostGroup)
        );
        assert_eq!(
            advance(&farm, "web", named).err(),
            Some(proto::RolloutsRefusal::NotInHostGroup)
        );
        assert_eq!(
            pause(&farm, "web", named).err(),
            Some(proto::RolloutsRefusal::NotInHostGroup)
        );
    }
    for call in [
        resume(&farm, "web", "Bad Name").err(),
        pause(&farm, "web", "Bad Name").err(),
        advance(&farm, "web", "Bad Name").err(),
    ] {
        assert_eq!(call, Some(proto::RolloutsRefusal::InvalidRequest));
    }
    assert_eq!(
        resume(&farm, "nothing", "").err(),
        Some(proto::RolloutsRefusal::UnknownService)
    );
    assert_eq!(farm.world.state(), before);

    let done = resume(&farm, "cfg", "").expect("resumed");
    assert_eq!(
        done.entry.and_then(|entry| entry.host_group),
        Some("alpha".to_owned())
    );
    assert!(!paused(&farm, "alpha", "cfg") && paused(&farm, "alpha", "web"));
    assert_eq!(
        resume(&farm, "cfg", "").err(),
        Some(proto::RolloutsRefusal::NotPaused)
    );
}

/// Proves: the calls of a plan refuse a request with a bad reason or for a package nothing defines
/// before they act, and an advance of a plan that does not wait is `not_waiting`.
///
/// Catches: a call that acts before it has checked its request.
#[tokio::test]
async fn the_calls_of_a_plan_check_their_request_first() {
    let farm = farm("gc-plan-checks").await;
    let before = farm.world.state();
    let coordinator = farm.world.coordinator();
    let bad_reason = |reason: &str| reason.to_owned();
    assert_eq!(
        coordinator
            .rollout_pause(
                &operator(),
                proto::PauseRequest {
                    service: "web".to_owned(),
                    reason: bad_reason(" "),
                    host_group: String::new(),
                },
            )
            .err(),
        Some(proto::RolloutsRefusal::InvalidRequest)
    );
    assert_eq!(
        coordinator
            .rollout_resume(
                &operator(),
                proto::ResumeRequest {
                    service: "web".to_owned(),
                    reason: bad_reason(" "),
                    host_group: String::new(),
                },
            )
            .err(),
        Some(proto::RolloutsRefusal::InvalidRequest)
    );
    assert_eq!(
        coordinator
            .rollout_advance(
                &operator(),
                proto::AdvanceRequest {
                    service: "web".to_owned(),
                    reason: bad_reason(" "),
                    host_group: String::new(),
                },
            )
            .err(),
        Some(proto::RolloutsRefusal::InvalidRequest)
    );
    assert_eq!(
        pause(&farm, "nothing", "").err(),
        Some(proto::RolloutsRefusal::UnknownService)
    );
    assert_eq!(
        advance(&farm, "nothing", "").err(),
        Some(proto::RolloutsRefusal::UnknownService)
    );
    assert_eq!(
        advance(&farm, "cfg", "").err(),
        Some(proto::RolloutsRefusal::NotWaiting)
    );
    assert_eq!(
        advance(&farm, "db", "").err(),
        Some(proto::RolloutsRefusal::NotWaiting)
    );
    assert_eq!(farm.world.state(), before);
}

/// Proves: an advance of a group's plan that waits for an operator enters its stage, audited
/// `rollout_advance` naming the group, answers the plan of that group, and orders the group's
/// hosts at their next heartbeat; a group whose plan does not wait refuses with `not_waiting`, as
/// does one with no desired version.
///
/// Catches: an advance that enters the stage of the package alone, so that no group moves.
#[tokio::test]
async fn an_advance_enters_the_stage_the_groups_plan_waits_at() {
    let farm = Farm::new("gc-advance");
    farm.group(
        "alpha",
        &["web"],
        Some(policy(|p| {
            p.max_updating = Some(5);
            p.stages = vec![PolicyStage {
                percent: 100,
                advance: StageAdvance::Manual.into(),
            }];
        })),
    );
    farm.group("beta", &["db"], None);
    farm.host("a1", "alpha");
    farm.deploy("alpha", "web", "1.0", 1).await;
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), NOTHING);
    farm.deploy("alpha", "web", "2.0", 2).await;
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), NOTHING);
    assert_eq!(
        advance(&farm, "db", "beta").err(),
        Some(proto::RolloutsRefusal::NoDesired)
    );

    farm.world.rig.at(10);
    assert_eq!(
        advance(&farm, "web", "").err(),
        Some(proto::RolloutsRefusal::NotWaiting),
        "within the quiet start"
    );
    farm.world.rig.at(2_000);
    let coordinator = farm.world.coordinator();
    coordinator
        .mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                reason: "broken".to_owned(),
            },
        )
        .expect("marked");
    assert_eq!(
        advance(&farm, "web", "").err(),
        Some(proto::RolloutsRefusal::NotWaiting),
        "a version marked bad"
    );
    coordinator
        .unmark_bad(
            &operator(),
            proto::UnmarkBadRequest {
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                reason: "fine".to_owned(),
            },
        )
        .expect("lifted");
    coordinator
        .halt(
            &operator(),
            proto::HaltRequest {
                service: "web".to_owned(),
                reason: "a bad disk".to_owned(),
            },
        )
        .expect("halted");
    assert_eq!(
        advance(&farm, "web", "").err(),
        Some(proto::RolloutsRefusal::NotWaiting),
        "a halted group"
    );
    farm.world
        .seed_marks("alpha", "web", |marks| marks.halt = None);
    let before = farm.world.state();
    farm.world.fake.fail();
    assert_eq!(
        advance(&farm, "web", "").err(),
        Some(proto::RolloutsRefusal::StoreFailed)
    );
    farm.world.fake.heal();
    assert_eq!(farm.world.state(), before);

    let done = advance(&farm, "web", "").expect("advanced");

    let entry = done.entry.expect("an entry");
    assert_eq!(
        (entry.action.as_str(), entry.host_group.as_deref()),
        ("rollout_advance", Some("alpha"))
    );
    let plan = done.plan.expect("a plan");
    assert_eq!(plan.host_group, "alpha");
    assert_eq!(farm.tell("a1", &[("web", "1.0")]), update("web", "2.0"));
    assert_eq!(
        advance(&farm, "web", "alpha").err(),
        Some(proto::RolloutsRefusal::NotWaiting)
    );
}

fn min_version(
    farm: &Farm,
    service: &str,
    scope: Scope,
    version: &str,
) -> Result<proto::SetMinVersionResponse, proto::ServicesRefusal> {
    farm.world.coordinator().set_min_version(
        &operator(),
        proto::SetMinVersionRequest {
            service: service.to_owned(),
            min_version: Some(proto::MinVersion {
                scope: Some(scope),
                version: version.to_owned(),
            }),
        },
    )
}

/// Proves: a minimum version set for a role scope is the minimum of the package in the group of
/// that name: it is written to the group's marks and to no other group's, audited
/// `set_min_version` naming the group, leaves the group's generation, and an empty version removes
/// it; a host below it is ordered no install of an older version.
///
/// Catches: a minimum version that is written to the assignments of a role no group has, or that
/// raises the generation of the group.
#[tokio::test]
async fn a_minimum_version_of_a_group_is_written_to_the_group_and_audited() {
    let farm = farm("gc-min").await;
    let generation = farm.world.coordinator().desired.host_group(&group("alpha"));
    let audited = farm.world.audit().len();

    let done = min_version(&farm, "web", Scope::Role("alpha".to_owned()), "1.0").expect("set");

    let entry = done.entry.expect("an entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.host_group.as_deref(),
            entry.service.as_deref()
        ),
        ("set_min_version", Some("alpha"), Some("web"))
    );
    assert_eq!(
        farm.world
            .ops()
            .group_marks(&group("alpha"), "web")
            .min_version,
        Some(version("1.0"))
    );
    assert_eq!(
        farm.world
            .ops()
            .group_marks(&group("beta"), "web")
            .min_version,
        None
    );
    assert!(
        farm.world
            .ops()
            .assignments
            .get("web")
            .is_none_or(|entries| entries.host_min_versions.is_empty())
    );
    assert_eq!(farm.world.audit_after(audited).len(), 1);
    assert_eq!(
        farm.world.coordinator().desired.host_group(&group("alpha")),
        generation
    );

    min_version(&farm, "web", Scope::Role("alpha".to_owned()), "").expect("removed");
    assert_eq!(
        farm.world
            .ops()
            .group_marks(&group("alpha"), "web")
            .min_version,
        None
    );
}

/// Proves: a minimum version of a role scope is refused `unknown_host_group` when no group has the
/// name, `not_in_host_group` when the group does not list the package, `below_min_version` when it
/// is above the group's desired version, and `invalid_request` for a version that does not parse;
/// a host scope is as it is without groups, its ceiling the desired version of the host's group
/// (none for a host of no group); nothing changes.
///
/// Catches: a role that is read as a role of the rollout file where groups are held.
#[tokio::test]
async fn a_minimum_version_of_a_role_that_is_no_group_is_refused() {
    let farm = farm("gc-min-refused").await;
    let before = farm.world.state();
    assert_eq!(
        min_version(&farm, "web", Scope::Role("nothing".to_owned()), "1.0").err(),
        Some(proto::ServicesRefusal::UnknownHostGroup)
    );
    assert_eq!(
        min_version(&farm, "web", Scope::Role("Bad Name".to_owned()), "1.0").err(),
        Some(proto::ServicesRefusal::InvalidRequest)
    );
    assert_eq!(
        min_version(&farm, "web", Scope::Role("gamma".to_owned()), "1.0").err(),
        Some(proto::ServicesRefusal::NotInHostGroup)
    );
    assert_eq!(
        min_version(&farm, "web", Scope::Role("alpha".to_owned()), "3.0").err(),
        Some(proto::ServicesRefusal::BelowMinVersion)
    );
    assert_eq!(
        min_version(&farm, "web", Scope::Role("alpha".to_owned()), "x.y").err(),
        Some(proto::ServicesRefusal::InvalidRequest)
    );
    assert_eq!(
        min_version(&farm, "web", Scope::Host("a1".to_owned()), "3.0").err(),
        Some(proto::ServicesRefusal::BelowMinVersion)
    );
    assert_eq!(farm.world.state(), before);

    farm.lone("lone");
    assert_eq!(farm.tell_with("lone", vec![]), NOTHING);
    min_version(&farm, "web", Scope::Host("lone".to_owned()), "9.0")
        .expect("a host of no group has no ceiling");
    min_version(&farm, "web", Scope::Host("a1".to_owned()), "1.0").expect("a host's minimum");
    assert_eq!(
        farm.world
            .ops()
            .assignments
            .get("web")
            .and_then(|entries| entries.host_min_versions.get("a1").cloned()),
        Some(version("1.0"))
    );
}

/// Proves: a minimum version of a group above the version a host runs is the floor of what that
/// host's group is ordered: a package the group lists that the rollout file does not define takes
/// a minimum version of its group as well.
///
/// Catches: a minimum version for a package only a group lists that is refused as unknown.
#[tokio::test]
async fn a_package_only_a_group_lists_takes_a_minimum_version() {
    let farm = farm("gc-min-cfg").await;
    min_version(&farm, "cfg", Scope::Role("alpha".to_owned()), "1.0").expect("set");
    assert_eq!(
        farm.world
            .ops()
            .group_marks(&group("alpha"), "cfg")
            .min_version,
        Some(version("1.0"))
    );
    assert_eq!(
        min_version(&farm, "cfg", Scope::Role("beta".to_owned()), "1.0").err(),
        Some(proto::ServicesRefusal::NotInHostGroup)
    );
}

/// Proves: a hold with a role scope that names a group holds that group's hosts of the package and
/// no other group's: with 2.0 desired in both groups, the held group's hosts are told nothing and
/// the other group's host is ordered the update; a release of the hold orders them; a role that is
/// no group is `unknown_host_group`, and a hold of a package only a group lists is made.
///
/// Catches: a hold of a role that holds no one, or the hosts of every group.
#[tokio::test]
async fn a_hold_of_a_group_stops_that_group_only() {
    let farm = farm("gc-hold").await;
    farm.deploy("", "web", "2.0", 2).await;
    let rig = &farm.world.rig;
    let before = farm.world.state();
    assert_eq!(
        hold(rig, "web", scope_role("nothing")).err(),
        Some(proto::ServicesRefusal::UnknownHostGroup)
    );
    assert_eq!(farm.world.state(), before);

    hold(rig, "web", scope_role("alpha")).expect("held");

    assert_eq!(farm.tell("a1", &[("web", "1.0"), ("cfg", "1.0")]), NOTHING);
    assert_eq!(farm.tell("a2", &[("web", "1.0"), ("cfg", "1.0")]), NOTHING);
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "2.0"));
    unhold(rig, "web", scope_role("alpha")).expect("released");
    assert_eq!(
        farm.tell("a1", &[("web", "1.0"), ("cfg", "1.0")]),
        update("web", "2.0")
    );
    hold(rig, "cfg", scope_host("a2")).expect("a host of a package only a group lists");
}

/// Proves: `Assign` and `Unassign` are refused `placed_by_host_group` once a group is held, for a
/// host and for a role, and change nothing; nothing is read of the request first.
///
/// Catches: an assignment written that no decision reads, which would look like a placement.
#[tokio::test]
async fn an_assignment_is_refused_where_groups_place_the_packages() {
    let farm = farm("gc-assign").await;
    let before = farm.world.state();
    let coordinator = farm.world.coordinator();
    for (node_id, role) in [("a1", ""), ("", "alpha")] {
        assert_eq!(
            coordinator
                .assign(
                    &operator(),
                    proto::AssignRequest {
                        node_id: node_id.to_owned(),
                        service: "web".to_owned(),
                        role: role.to_owned(),
                    },
                )
                .err(),
            Some(proto::ServicesRefusal::PlacedByHostGroup)
        );
        assert_eq!(
            coordinator
                .unassign(
                    &operator(),
                    proto::UnassignRequest {
                        node_id: node_id.to_owned(),
                        service: "web".to_owned(),
                        uninstall: true,
                        role: role.to_owned(),
                    },
                )
                .err(),
            Some(proto::ServicesRefusal::PlacedByHostGroup)
        );
    }
    assert_eq!(farm.world.state(), before);
}

/// Proves: `Services.List` answers each package the rollout file defines or a group lists, with
/// `host_groups` the groups that list it, the hosts of those groups as `hosts` and `assigned`, the
/// minimum version of each group as a role scope named by the group, and the desired version when
/// every group agrees (none when they differ); `roles` and `assignments` are empty.
///
/// Catches: a list that shows the rollout file's roles for packages groups place, or leaves out a
/// package only a group lists.
#[tokio::test]
async fn services_list_names_the_groups_that_list_each_package() {
    let farm = farm("gc-list").await;
    min_version(&farm, "web", Scope::Role("beta".to_owned()), "1.0").expect("set");
    min_version(&farm, "web", Scope::Host("a2".to_owned()), "1.0").expect("set");
    hold(&farm.world.rig, "web", scope_host("a1")).expect("held");

    let listed = farm
        .world
        .coordinator()
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("listed");

    let names: Vec<_> = listed
        .services
        .iter()
        .map(|entry| entry.service.as_str())
        .collect();
    assert_eq!(names, ["cfg", "db", "hostkeeper", "web"]);
    let web = &listed.services[3];
    assert_eq!(web.host_groups, ["alpha", "beta"]);
    assert_eq!((web.roles.len(), web.assignments.len()), (0, 0));
    let mut hosts = web.hosts.clone();
    hosts.sort();
    assert_eq!(hosts, ["a1", "a2", "b1"]);
    assert_eq!(web.desired_version.as_deref(), Some("1.0"));
    assert_eq!(web.holds.len(), 1);
    let mins: Vec<_> = web
        .min_versions
        .iter()
        .map(|min| (min.scope.clone(), min.version.as_str()))
        .collect();
    assert_eq!(
        mins,
        [
            (Some(Scope::Role("beta".to_owned())), "1.0"),
            (Some(Scope::Host("a2".to_owned())), "1.0")
        ]
    );
    let held: Vec<_> = web
        .assigned
        .iter()
        .map(|host| (host.node_id.as_str(), host.role.as_str(), host.holds.len()))
        .collect();
    assert_eq!(
        held,
        [("a1", "alpha", 1), ("a2", "alpha", 0), ("b1", "beta", 0)]
    );
    assert_eq!(listed.services[0].host_groups, ["alpha"]);
    assert_eq!(listed.services[1].host_groups, ["gamma"]);
    assert_eq!(listed.services[2].host_groups, ["alpha", "beta", "gamma"]);

    farm.deploy("alpha", "web", "2.0", 2).await;
    let again = farm
        .world
        .coordinator()
        .list_services(&operator(), proto::ListServicesRequest {})
        .expect("listed");
    assert_eq!(again.services[3].desired_version, None);
}

/// Proves: `Hosts.Get` and `Hosts.List` name the group of a host in `host_group`, and its name in
/// `role` as well, for a host heard, for one enrolled that has not been heard, and leave both as
/// they were for a host in no group.
///
/// Catches: a host shown with the role of its certificate, which no longer places it.
#[tokio::test]
async fn hosts_name_their_group() {
    let farm = farm("gc-hosts").await;
    farm.host("quiet", "beta");
    farm.lone("lone");
    let coordinator = farm.world.coordinator();

    let listed = coordinator
        .list_hosts(&operator(), proto::ListHostsRequest::default())
        .expect("listed");

    let of = |id: &str| {
        let host = listed
            .hosts
            .iter()
            .find(|host| host.node_id == id)
            .unwrap_or_else(|| panic!("{id} is listed"));
        (host.role.clone(), host.host_group.clone())
    };
    assert_eq!(of("a1"), ("alpha".to_owned(), "alpha".to_owned()));
    assert_eq!(of("b1"), ("beta".to_owned(), "beta".to_owned()));
    assert_eq!(of("quiet"), ("beta".to_owned(), "beta".to_owned()));
    assert_eq!(of("lone").1, "");
    let got = coordinator
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: "c1".to_owned(),
            },
        )
        .expect("a host")
        .host
        .expect("a host");
    assert_eq!(
        (got.role.as_str(), got.host_group.as_str()),
        ("gamma", "gamma")
    );
}

fn audit_of(
    farm: &Farm,
    host_group: Option<&str>,
) -> Result<Vec<(String, Option<String>)>, proto::AuditRefusal> {
    farm.world
        .coordinator()
        .audit_list(
            &operator(),
            proto::ListAuditRequest {
                host_group: host_group.map(str::to_owned),
                ..proto::ListAuditRequest::default()
            },
        )
        .map(|listed| {
            listed
                .entries
                .into_iter()
                .map(|entry| (entry.action, entry.host_group))
                .collect()
        })
}

/// Proves: `Audit.List` with the filter `host_group` lists the entries that name the group and no
/// other, and none for a group no entry names; a filter that is no group name is
/// `invalid_request`.
///
/// Catches: a filter that is ignored, which would list every entry, or one that lists none.
#[tokio::test]
async fn the_audit_list_filters_by_group() {
    let farm = farm("gc-audit").await;
    pause(&farm, "web", "").expect("paused");
    min_version(&farm, "cfg", Scope::Role("alpha".to_owned()), "1.0").expect("set");

    let alpha = audit_of(&farm, Some("alpha")).expect("listed");
    assert!(
        alpha
            .iter()
            .all(|(_, group)| group.as_deref() == Some("alpha"))
    );
    assert!(alpha.iter().any(|(action, _)| action == "rollout_pause"));
    assert!(alpha.iter().any(|(action, _)| action == "set_min_version"));
    let beta = audit_of(&farm, Some("beta")).expect("listed");
    assert!(
        beta.iter()
            .all(|(_, group)| group.as_deref() == Some("beta"))
    );
    assert!(beta.iter().any(|(action, _)| action == "rollout_pause"));
    assert!(beta.iter().all(|(action, _)| action != "set_min_version"));
    assert_eq!(audit_of(&farm, Some("nothing")).expect("listed"), []);
    assert!(audit_of(&farm, None).expect("listed").len() > alpha.len());
    assert_eq!(
        audit_of(&farm, Some("Bad Name")).err(),
        Some(proto::AuditRefusal::InvalidRequest)
    );
}

/// Proves: a coordinator that holds no host group lists no audit entry for a `host_group` filter,
/// whatever its log keeps of the groups it held, answers `next_seq` as the request's `since_seq`
/// and does not read the filter as a group name; the same entries are listed while a group is
/// held.
///
/// Catches: a filter read against the log of a coordinator that holds no group, which would list
/// the entries of the groups removed.
#[test]
fn a_coordinator_that_holds_no_group_lists_no_entry_for_a_group() {
    let farm = Farm::new("gc-audit-none");
    farm.group("alpha", &["web"], None);
    assert!(
        audit_of(&farm, Some("alpha"))
            .expect("listed")
            .iter()
            .any(|(action, _)| action == "host_group_put")
    );
    farm.world
        .coordinator()
        .host_groups_remove(&operator(), crate::host_groups_support::remove_of("alpha"))
        .expect("removed");

    assert_eq!(audit_of(&farm, Some("alpha")).expect("listed"), []);
    assert_eq!(audit_of(&farm, Some("Bad Name")).expect("listed"), []);
    let since = farm
        .world
        .coordinator()
        .audit_list(
            &operator(),
            proto::ListAuditRequest {
                since_seq: 1,
                host_group: Some("alpha".to_owned()),
                ..proto::ListAuditRequest::default()
            },
        )
        .expect("listed");
    assert_eq!((since.entries.len(), since.next_seq), (0, 1));
    assert!(!audit_of(&farm, None).expect("listed").is_empty());
}

/// Proves: a change that cannot be written is refused `store_failed` and changes nothing: a
/// resume of a group's plan, a minimum version of a group, a force, and a clear of the halts of
/// the groups.
///
/// Catches: a change made in memory that storage does not hold, which a restart would lose.
#[tokio::test]
async fn a_change_that_cannot_be_written_is_refused_and_changes_nothing() {
    let farm = farm("gc-unwritable").await;
    pause(&farm, "web", "alpha").expect("paused");
    halt_web(&farm);
    let before = farm.world.state();
    farm.world.fake.fail();

    assert_eq!(
        resume(&farm, "web", "alpha").err(),
        Some(proto::RolloutsRefusal::StoreFailed)
    );
    assert_eq!(
        min_version(&farm, "web", Scope::Role("alpha".to_owned()), "1.0").err(),
        Some(proto::ServicesRefusal::StoreFailed)
    );
    assert!(matches!(
        clear_web(&farm),
        Err(crate::operate::OperateError::Store(_))
    ));
    farm.world.fake.heal();

    assert_eq!(farm.world.state(), before);
    resume(&farm, "web", "alpha").expect("resumed");
    clear_web(&farm).expect("cleared");
}

fn halt_web(farm: &Farm) {
    farm.world
        .coordinator()
        .halt(
            &operator(),
            proto::HaltRequest {
                service: "web".to_owned(),
                reason: "a bad disk".to_owned(),
            },
        )
        .expect("halted");
}

fn clear_web(farm: &Farm) -> Result<crate::ops::AuditEntry, crate::operate::OperateError> {
    let body = serde_json::json!({"action": "clear_halt", "service": "web", "reason": "fixed"});
    farm.world
        .coordinator()
        .operate(Some(crate::testutil::OPERATOR), body.to_string().as_bytes())
}
