//! Whom a hook's halt holds in a host group ([`crate::halt::HaltScope`]): every host of the group
//! while the desired version is one the halt was reported at, and the hosts that reported it alone
//! once another version is desired (komira-ai/hostkeeper#418). Driven by the heartbeats of enrolled
//! hosts placed in a group of four, as the Macs of a farm are.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Phase, ServiceReply};
use serde_json::json;

use crate::decide::{Gate, Why};
use crate::group_decision_support::{Farm, failed, policy, stages};
use crate::host_groups::GroupName;
use crate::marks_support::{operator, runs, update};
use crate::testutil::{OPERATOR, version};

const NOTHING: ServiceReply = ServiceReply::Nothing;
const GROUP: &str = "mac";
const PACKAGE: &str = "js";
const HALTED: Option<Why> = Some(Why::Gate(Gate::Halted));

/// A group of four hosts, `m1` to `m4`, that wants `js` 2.0 in one stage with every host updating
/// at once, and `m1` reporting the hook's halt of its update to 2.0.
fn halted_farm(name: &str) -> Farm {
    let farm = Farm::new(name);
    farm.group(
        GROUP,
        &[PACKAGE],
        Some(policy(|p| {
            p.stages = stages(&[100]);
            p.max_updating = Some(4);
        })),
    );
    for host in ["m1", "m2", "m3", "m4"] {
        farm.host(host, GROUP);
    }
    farm.want(GROUP, PACKAGE, "2.0");
    assert_eq!(halt_report(&farm, "m1", "2.0"), NOTHING);
    farm
}

/// The heartbeat of `host` reporting that the hook halted its update of `js` to `target`, and its
/// reply.
fn halt_report(farm: &Farm, host: &str, target: &str) -> ServiceReply {
    farm.tell_with(
        host,
        vec![
            runs("hostkeeper", "1.0"),
            failed(PACKAGE, "1.0", target, Phase::Halt),
        ],
    )
}

/// The reply to a heartbeat of `host` running `js` 1.0.
fn behind(farm: &Farm, host: &str) -> ServiceReply {
    farm.tell(host, &[(PACKAGE, "1.0")])
}

/// The `halt` alerts, each as its host, version and message.
fn halt_alerts(farm: &Farm) -> Vec<(String, String, String)> {
    farm.world
        .coordinator()
        .list_alerts(&operator(), proto::ListAlertsRequest {})
        .expect("listed")
        .alerts
        .into_iter()
        .filter(|alert| alert.kind == i32::from(proto::AlertKind::Halt))
        .map(|alert| (alert.host_id, alert.version, alert.message))
        .collect()
}

fn group() -> GroupName {
    GROUP.parse().expect("a group name")
}

/// Proves: while the group wants the version `m1` halted at, no host of the group is ordered (each
/// is held `halted`); once it wants 3.0, `m2`, `m3` and `m4` are each ordered 3.0, `m1` is still
/// held `halted` and told nothing, the group's halt stands as it was (host, check, version), and
/// its alert still names `m1` and 2.0 and says whom the halt holds.
///
/// Catches: one host's halt holding the whole group across a new desired version, so that no
/// other host takes the release that would mend it; and a fix that frees the group by dropping
/// the halt, ordering the halted host as well or silencing its alert.
#[test]
fn a_new_version_is_ordered_to_the_group_and_the_halt_keeps_its_host() {
    let farm = halted_farm("gh-scope-new");
    for host in ["m2", "m3", "m4"] {
        assert_eq!(behind(&farm, host), NOTHING, "{host}");
        assert_eq!(farm.why(host, PACKAGE), HALTED, "{host}");
    }

    farm.want(GROUP, PACKAGE, "3.0");
    for host in ["m2", "m3", "m4"] {
        assert_eq!(behind(&farm, host), update(PACKAGE, "3.0"), "{host}");
    }
    assert_eq!(halt_report(&farm, "m1", "2.0"), NOTHING);
    assert_eq!(farm.why("m1", PACKAGE), HALTED);

    let marks = farm.world.ops().group_marks(&group(), PACKAGE);
    let halt = marks.halt.expect("the halt stands");
    assert_eq!(
        (halt.node_id.as_str(), halt.check.as_str(), halt.target),
        ("m1", "unspecified", version("2.0"))
    );
    assert_eq!(marks.halt_generation, 0);
    assert_eq!(
        halt_alerts(&farm),
        vec![(
            "m1".to_owned(),
            "2.0".to_owned(),
            "js is halted at 2.0 by m1, check unspecified in host group mac: the hosts that \
             reported it are ordered nothing of it there, and no host there is ordered a version \
             it was reported at, until an operator clears the halt"
                .to_owned()
        )]
    );
}

/// Proves: after the group wants 3.0, a second host that halts at 3.0 holds the group again: `m3`,
/// which was not ordered yet, is told nothing and held `halted`, and so are `m1` and `m2`; the
/// first halt is still the one that stands, and the second report is kept with it.
///
/// Catches: a halt narrowed to its hosts for good, so that a new version that halts host after
/// host is rolled to the whole group.
#[test]
fn a_second_halt_at_the_new_version_holds_the_group_again() {
    let farm = halted_farm("gh-scope-second");
    farm.want(GROUP, PACKAGE, "3.0");
    assert_eq!(behind(&farm, "m2"), update(PACKAGE, "3.0"));
    assert_eq!(halt_report(&farm, "m2", "3.0"), NOTHING);

    assert_eq!(behind(&farm, "m3"), NOTHING);
    for host in ["m1", "m2", "m3"] {
        assert_eq!(farm.why(host, PACKAGE), HALTED, "{host}");
    }
    let marks = farm.world.ops().group_marks(&group(), PACKAGE);
    assert_eq!(marks.halt.expect("the halt stands").node_id, "m1");
    assert_eq!(
        marks
            .halt_seen
            .iter()
            .map(|seen| (seen.node_id.as_str(), seen.target.clone()))
            .collect::<Vec<_>>(),
        vec![("m2", version("3.0"))]
    );
}

/// Proves: a group set back to the version the halt was reported at is held whole again: after
/// 3.0, which `m2` is ordered, then 2.0 once more, `m2` is told nothing and held `halted`.
///
/// Catches: a halt whose hold of the group ends for good once the desired version first moves.
#[test]
fn the_halted_version_desired_again_holds_the_group_again() {
    let farm = halted_farm("gh-scope-back");
    farm.want(GROUP, PACKAGE, "3.0");
    assert_eq!(behind(&farm, "m2"), update(PACKAGE, "3.0"));
    farm.want(GROUP, PACKAGE, "2.0");
    assert_eq!(behind(&farm, "m2"), NOTHING);
    assert_eq!(farm.why("m2", PACKAGE), HALTED);
}

/// Proves: an operator's halt (`Releases.Halt`, which names no host) holds every host of the group
/// whatever version is desired: after the group wants 3.0, `m2` is still told nothing and held
/// `halted`.
///
/// Catches: the narrowing of a host's halt applied to an operator's, which has no host to hold.
#[test]
fn an_operators_halt_holds_the_group_across_a_new_version() {
    let farm = Farm::new("gh-scope-operator");
    farm.group(
        GROUP,
        &[PACKAGE],
        Some(policy(|p| p.stages = stages(&[100]))),
    );
    farm.host("m2", GROUP);
    farm.want(GROUP, PACKAGE, "2.0");
    farm.world
        .coordinator()
        .halt(
            &operator(),
            proto::HaltRequest {
                service: PACKAGE.to_owned(),
                reason: "hold the macs".to_owned(),
            },
        )
        .expect("halted");

    farm.want(GROUP, PACKAGE, "3.0");
    assert_eq!(behind(&farm, "m2"), NOTHING);
    assert_eq!(farm.why("m2", PACKAGE), HALTED);
}

/// Proves: the operator's clear of the halt, once the group wants 3.0, lifts the halted host
/// too: `m1`, still reporting its halt of the update to 2.0, is ordered 3.0.
///
/// Catches: a hold of the halted host that outlives the clear.
#[test]
fn the_clear_orders_the_halted_host_the_new_version() {
    let farm = halted_farm("gh-scope-clear");
    farm.want(GROUP, PACKAGE, "3.0");
    let body = json!({"action": "clear_halt", "service": PACKAGE, "reason": "taint cleared"});
    farm.world
        .coordinator()
        .operate(Some(OPERATOR), body.to_string().as_bytes())
        .expect("cleared");

    assert_eq!(halt_report(&farm, "m1", "2.0"), update(PACKAGE, "3.0"));
    assert_eq!(halt_alerts(&farm), Vec::new());
}

/// Proves whom a scope holds, case by case: no halt has no scope; a host's halt at 2.0, with a
/// report of `m2` at 3.0 kept with it, holds every host while 2.0, 3.0 or no version is desired,
/// and `m1` and `m2` alone while 4.0 is; an operator's halt, which names no host, holds every host
/// at 4.0 too, and no host of its own.
///
/// Catches: a scope read from the halt alone, which forgets the reports kept with it, and an
/// operator's empty host counted as a host.
#[test]
fn a_scope_holds_every_host_at_a_halted_version_and_its_hosts_after() {
    use crate::halt::HaltScope;
    use crate::ops::{Halt, HaltAck, HaltOrigin};

    assert_eq!(HaltScope::of(None, &[]), None);
    let halt = |node: &str, origin| Halt {
        node_id: node.to_owned(),
        check: "tainted".to_owned(),
        target: version("2.0"),
        report: None,
        origin,
    };
    let seen = [HaltAck {
        node_id: "m2".to_owned(),
        target: version("3.0"),
        report: None,
    }];
    let hosts = HaltScope::of(Some(&halt("m1", HaltOrigin::Hook)), &seen).expect("a scope");
    for every in [Some(version("2.0")), Some(version("3.0")), None] {
        assert!(hosts.holds_every_host(every.as_ref()), "{every:?}");
        assert!(hosts.holds(every.as_ref(), "m3"), "{every:?}");
    }
    let next = version("4.0");
    assert!(!hosts.holds_every_host(Some(&next)));
    assert!(hosts.holds(Some(&next), "m1") && hosts.holds(Some(&next), "m2"));
    assert!(!hosts.holds(Some(&next), "m3"));

    let operators = HaltScope::of(Some(&halt("", HaltOrigin::Operator)), &[]).expect("a scope");
    assert!(operators.holds_every_host(Some(&next)));
    assert!(operators.holds(Some(&next), "m3"));
    let marks = crate::ops::ServiceOps {
        halt: Some(halt("m1", HaltOrigin::Hook)),
        halt_seen: seen.to_vec(),
        ..crate::ops::ServiceOps::default()
    };
    assert_eq!(HaltScope::of_marks(&marks), Some(hosts));
    assert!(crate::halt::stops(&marks, &version("3.0")));
    assert!(!crate::halt::stops(&marks, &next));
    assert!(!crate::halt::stops(
        &crate::ops::ServiceOps::default(),
        &next
    ));
}
