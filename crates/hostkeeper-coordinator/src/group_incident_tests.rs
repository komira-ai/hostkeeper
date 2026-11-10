//! The incident calls once the coordinator holds host groups (`docs/design/hostgroups.md`, section
//! 4): `Releases.Halt` and `ClearHalt` act on every group that lists the package, the alerts read
//! the marks of the groups, and the calls about a host and a package (abort, exclude, mark bad)
//! know a package only a group lists.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::ServiceReply;
use serde_json::json;

use crate::group_decision_support::Farm;
use crate::host_groups::GroupName;
use crate::marks_support::{hold, operator, scope_host, update};
use crate::testutil::{OPERATOR, version};

type Refusal = proto::ReleasesRefusal;

const NOTHING: ServiceReply = ServiceReply::Nothing;

fn group(name: &str) -> GroupName {
    name.parse().expect("a group name")
}

/// Alpha (a1, a2) and beta (b1) list `web`, alpha lists `cfg` as well, and `web` 1.0 is desired in
/// both and run by every host.
async fn farm(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.group("alpha", &["web", "cfg"], None);
    farm.group("beta", &["web"], None);
    for (host, group) in [("a1", "alpha"), ("a2", "alpha"), ("b1", "beta")] {
        farm.host(host, group);
    }
    farm.deploy("", "web", "1.0", 1).await;
    farm.deploy("alpha", "cfg", "1.0", 5).await;
    for host in ["a1", "a2"] {
        assert_eq!(farm.tell(host, &[("web", "1.0"), ("cfg", "1.0")]), NOTHING);
    }
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), NOTHING);
    farm
}

fn halt(farm: &Farm, service: &str) -> Result<proto::HaltResponse, Refusal> {
    farm.world.coordinator().halt(
        &operator(),
        proto::HaltRequest {
            service: service.to_owned(),
            reason: "a bad disk".to_owned(),
        },
    )
}

fn clear(
    farm: &Farm,
    service: &str,
) -> Result<crate::ops::AuditEntry, crate::operate::OperateError> {
    let body = json!({"action": "clear_halt", "service": service, "reason": "fixed"});
    farm.world
        .coordinator()
        .operate(Some(OPERATOR), body.to_string().as_bytes())
}

fn halted(farm: &Farm, name: &str, package: &str) -> bool {
    farm.world
        .ops()
        .group_marks(&group(name), package)
        .halt
        .is_some()
}

/// The alerts now, but the one of the certificate the tests' enrollment serves, which the tests'
/// fixed clock reads as expired.
fn alerts(farm: &Farm) -> Vec<proto::Alert> {
    farm.world
        .coordinator()
        .list_alerts(&operator(), proto::ListAlertsRequest {})
        .expect("listed")
        .alerts
        .into_iter()
        .filter(|alert| alert.kind() != proto::AlertKind::CertNearExpiry)
        .collect()
}

fn entries(entries: &[crate::ops::AuditEntry]) -> Vec<(String, Option<String>)> {
    entries
        .iter()
        .map(|entry| (entry.action.clone(), entry.host_group.clone()))
        .collect()
}

/// Proves: a halt halts the package in every group that lists it, each at its own desired
/// version, with one audit entry for each naming its group, the answer holding the first; a halt
/// finding some group halted halts the rest, one that finds all halted is `already_halted`; a
/// halted group's hosts are ordered nothing, and a clear lifts the halt of every group that lists
/// the package, one `clear_halt` entry for each, after which they are ordered again.
///
/// Catches: a halt of the package alone, which the group's decision does not read, or one that
/// halts only the first group.
#[tokio::test]
async fn a_halt_and_its_clear_act_on_every_group_that_lists_the_package() {
    let farm = farm("gi-halt").await;
    farm.deploy("beta", "web", "1.5", 3).await;
    farm.deploy("alpha", "web", "2.0", 2).await;
    let audited = farm.world.audit().len();

    let done = halt(&farm, "web").expect("halted");

    assert_eq!(
        entries(&farm.world.audit_after(audited)),
        [
            ("halt".to_owned(), Some("alpha".to_owned())),
            ("halt".to_owned(), Some("beta".to_owned()))
        ]
    );
    assert_eq!(
        done.entry.and_then(|entry| entry.host_group),
        Some("alpha".to_owned())
    );
    let ops = farm.world.ops();
    let target = |name: &str| {
        ops.group_marks(&group(name), "web")
            .halt
            .map(|halt| halt.target)
    };
    assert_eq!(target("alpha"), Some(version("2.0")));
    assert_eq!(target("beta"), Some(version("1.5")));
    assert!(
        ops.services
            .get("web")
            .is_none_or(|marks| marks.halt.is_none())
    );
    assert_eq!(halt(&farm, "web").err(), Some(Refusal::AlreadyHalted));
    assert_eq!(farm.tell("a1", &[("web", "1.0"), ("cfg", "1.0")]), NOTHING);
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), NOTHING);

    let audited = farm.world.audit().len();
    let cleared = clear(&farm, "web").expect("cleared");
    assert_eq!(cleared.host_group.as_deref(), Some("alpha"));
    assert_eq!(
        entries(&farm.world.audit_after(audited)),
        [
            ("clear_halt".to_owned(), Some("alpha".to_owned())),
            ("clear_halt".to_owned(), Some("beta".to_owned()))
        ]
    );
    assert!(!halted(&farm, "alpha", "web") && !halted(&farm, "beta", "web"));
    assert!(clear(&farm, "web").is_err());
    assert_eq!(
        farm.tell("a1", &[("web", "1.0"), ("cfg", "1.0")]),
        update("web", "2.0")
    );
    assert_eq!(farm.tell("b1", &[("web", "1.0")]), update("web", "1.5"));
}

/// Proves: a halt that finds one group halted already halts the others only, and a clear lifts
/// the halt of the groups that have one, leaving the others as they are.
///
/// Catches: a halt that is refused because one group stands halted, leaving the rest running.
#[tokio::test]
async fn a_halt_leaves_a_group_that_is_halted_already_as_it_is() {
    let farm = farm("gi-halt-some").await;
    halt(&farm, "cfg").expect("halted");
    assert!(halted(&farm, "alpha", "cfg") && !halted(&farm, "beta", "web"));
    farm.world.seed_marks("beta", "web", |marks| {
        marks.halt = Some(crate::ops::Halt {
            node_id: "b1".to_owned(),
            check: "disk".to_owned(),
            target: version("1.0"),
            report: None,
            origin: crate::ops::HaltOrigin::Hook,
        });
    });
    let audited = farm.world.audit().len();

    halt(&farm, "web").expect("halted");

    assert_eq!(
        entries(&farm.world.audit_after(audited)),
        [("halt".to_owned(), Some("alpha".to_owned()))]
    );
    let cleared = clear(&farm, "web").expect("cleared");
    assert_eq!(cleared.host_group.as_deref(), Some("alpha"));
    assert!(!halted(&farm, "beta", "web") && !halted(&farm, "alpha", "web"));
    assert!(halted(&farm, "alpha", "cfg"));
}

/// Proves: a halt is refused `unknown_service` for a package nothing defines, `invalid_request`
/// for a bad reason and for a package no group has a desired version of, and `not_in_host_group`
/// for a package of the rollout file no group lists; a clear of a package nothing manages is
/// invalid; nothing changes.
///
/// Catches: a halt of a package no group runs, which would stand for ever.
#[tokio::test]
async fn a_halt_is_refused_where_no_group_can_be_halted() {
    let farm = Farm::new("gi-halt-refused");
    farm.group("alpha", &["web"], None);
    farm.host("a1", "alpha");
    let before = farm.world.state();
    assert_eq!(halt(&farm, "nothing").err(), Some(Refusal::UnknownService));
    assert_eq!(halt(&farm, "db").err(), Some(Refusal::NotInHostGroup));
    assert_eq!(halt(&farm, "web").err(), Some(Refusal::InvalidRequest));
    assert_eq!(
        farm.world
            .coordinator()
            .halt(
                &operator(),
                proto::HaltRequest {
                    service: "web".to_owned(),
                    reason: " ".to_owned(),
                },
            )
            .err(),
        Some(Refusal::InvalidRequest)
    );
    assert!(clear(&farm, "nothing").is_err());
    assert_eq!(farm.world.state(), before);
}

/// Proves: halts of packages in groups are alerts: an operator's halt of a package in two groups
/// at the same version is one alert naming both groups, halts at two versions are two alerts, a
/// halt of a package only a group lists is an alert, and clearing the halts ends them.
///
/// Catches: group halts missing from the alerts, because only the package's own marks were read.
#[tokio::test]
async fn the_halts_of_groups_are_alerts() {
    let farm = farm("gi-alerts-halt").await;
    assert_eq!(alerts(&farm), []);

    halt(&farm, "web").expect("halted");
    halt(&farm, "cfg").expect("halted");

    let listed = alerts(&farm);
    let halts: Vec<_> = listed
        .iter()
        .map(|alert| {
            (
                alert.service.as_str(),
                alert.version.as_str(),
                alert.message.as_str(),
            )
        })
        .collect();
    assert_eq!(
        halts,
        [
            (
                "cfg",
                "1.0",
                "cfg is halted at 1.0 by an operator in host group alpha: nothing is ordered \
                 for it there until an operator clears the halt"
            ),
            (
                "web",
                "1.0",
                "web is halted at 1.0 by an operator in host group alpha, beta: nothing is \
                 ordered for it there until an operator clears the halt"
            )
        ]
    );
    assert!(
        listed
            .iter()
            .all(|alert| alert.kind() == proto::AlertKind::Halt)
    );

    clear(&farm, "web").expect("cleared");
    farm.deploy("beta", "web", "2.0", 2).await;
    halt(&farm, "web").expect("halted");
    let listed: Vec<_> = alerts(&farm)
        .into_iter()
        .filter(|alert| alert.service == "web")
        .map(|alert| alert.version)
        .collect();
    assert_eq!(listed, ["1.0", "2.0"]);

    clear(&farm, "web").expect("cleared");
    clear(&farm, "cfg").expect("cleared");
    assert_eq!(alerts(&farm), []);
}

/// Proves: a failed canary, a held host running a version marked bad, and an abandoned host are
/// alerts for a package only a group lists, as they are for a package of the rollout file.
///
/// Catches: alerts that read the rollout file's services alone, so that a package placed by a
/// group is never reported.
#[tokio::test]
async fn the_alerts_of_a_package_only_a_group_lists_are_derived() {
    let farm = farm("gi-alerts-cfg").await;
    let coordinator = farm.world.coordinator();
    coordinator
        .mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: "cfg".to_owned(),
                version: "1.0".to_owned(),
                reason: "broken".to_owned(),
            },
        )
        .expect("marked");
    coordinator
        .lock_fleet()
        .note_failed_canary("cfg", &version("1.0"), "a1");
    hold(&farm.world.rig, "cfg", scope_host("a2")).expect("held");
    coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: "cfg".to_owned(),
                node_id: "a1".to_owned(),
                reason: "gone".to_owned(),
                abandon: true,
            },
        )
        .expect("excluded");

    let kinds: Vec<_> = alerts(&farm)
        .into_iter()
        .map(|alert| (alert.kind(), alert.service, alert.host_id))
        .collect();

    assert_eq!(
        kinds,
        [
            (
                proto::AlertKind::FailedCanary,
                "cfg".to_owned(),
                "a1".to_owned()
            ),
            (
                proto::AlertKind::HeldOnBadVersion,
                "cfg".to_owned(),
                "a2".to_owned()
            ),
            (
                proto::AlertKind::AbandonedWhileUpdating,
                "cfg".to_owned(),
                "a1".to_owned()
            ),
        ]
    );
}

/// Proves: an abort, an exclusion, an inclusion, a mark bad and its lifting are made for a package
/// only a group lists.
///
/// Catches: a call that finds a package only in the rollout file, and refuses a package that a
/// group places as unknown.
#[tokio::test]
async fn the_calls_about_a_host_and_a_package_know_a_package_only_a_group_lists() {
    let farm = farm("gi-calls").await;
    let coordinator = farm.world.coordinator();
    let aborted = coordinator
        .abort(
            &operator(),
            proto::AbortRequest {
                node_id: "a1".to_owned(),
                service: "cfg".to_owned(),
                reason: "stuck".to_owned(),
            },
        )
        .expect("queued");
    assert!(aborted.queued);
    coordinator
        .exclude(
            &operator(),
            proto::ExcludeRequest {
                service: "cfg".to_owned(),
                node_id: "a2".to_owned(),
                reason: "broken".to_owned(),
                abandon: false,
            },
        )
        .expect("excluded");
    assert!(
        farm.world
            .ops()
            .excluded
            .iter()
            .any(|e| e.service == "cfg" && e.node_id == "a2")
    );
    let body = json!({"action": "include", "service": "cfg", "node_id": "a2"});
    coordinator
        .operate(Some(OPERATOR), body.to_string().as_bytes())
        .expect("included");
    assert!(farm.world.ops().excluded.is_empty());
    let body = json!({"action": "include", "service": "nothing", "node_id": "a2"});
    assert!(
        coordinator
            .operate(Some(OPERATOR), body.to_string().as_bytes())
            .is_err()
    );

    for (version_text, expected) in [("2.0", true), ("2.0", false)] {
        let marked = coordinator.mark_bad(
            &operator(),
            proto::MarkBadRequest {
                service: "cfg".to_owned(),
                version: version_text.to_owned(),
                reason: "broken".to_owned(),
            },
        );
        assert_eq!(marked.is_ok(), expected);
    }
    coordinator
        .unmark_bad(
            &operator(),
            proto::UnmarkBadRequest {
                service: "cfg".to_owned(),
                version: "2.0".to_owned(),
                reason: "fine".to_owned(),
            },
        )
        .expect("lifted");
    assert!(
        farm.world
            .ops()
            .services
            .get("cfg")
            .is_none_or(|marks| marks.bad.is_empty())
    );
    assert_eq!(
        coordinator
            .unmark_bad(
                &operator(),
                proto::UnmarkBadRequest {
                    service: "nothing".to_owned(),
                    version: "2.0".to_owned(),
                    reason: "fine".to_owned(),
                },
            )
            .err(),
        Some(Refusal::UnknownService)
    );
}
