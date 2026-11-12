//! `Hosts.Retry` and `Hosts.ClearRecord` of a package that only a host group lists (the rollout
//! file does not define it): both know the package as `Releases.ClearHalt`, `Hosts.Exclude` and
//! `Hosts.Abort` do, and still refuse a package that neither the file nor a group names.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, Phase, ServiceReply};
use serde_json::json;

use crate::clear_record::FEATURE;
use crate::decide::{Gate, Skip, Why};
use crate::group_decision_support::{Farm, failed};
use crate::host_groups::GroupName;
use crate::marks_support::{NEW_DAEMON, ask, beat_of, operator, runs, update};
use crate::testutil::OPERATOR;

const GROUP: &str = "farm-linux";
const PACKAGE: &str = "tierstore";
const NOTHING: ServiceReply = ServiceReply::Nothing;

/// The heartbeat of a daemon that announces `features` and has failed the update of the package
/// from 1.0 to 2.0 with a halt.
fn failed_beat(features: &[&str]) -> Heartbeat {
    beat_of(
        features,
        vec![
            runs("hostkeeper", "1.0"),
            failed(PACKAGE, "1.0", "2.0", Phase::Halt),
        ],
    )
}

/// The reply to `host`'s heartbeat with the failed report of the package, as a daemon of today.
fn stale(farm: &Farm, host: &str) -> ServiceReply {
    ask(&farm.world.rig, host, "storage", failed_beat(NEW_DAEMON))
}

/// The operator's `Hosts.Retry` of `package` on `node`.
fn retry(
    farm: &Farm,
    node: &str,
    package: &str,
) -> Result<proto::RetryResponse, proto::HostsRefusal> {
    farm.world.coordinator().retry(
        &operator(),
        &proto::RetryRequest {
            node_id: node.to_owned(),
            service: package.to_owned(),
        },
    )
}

/// The operator's `Hosts.ClearRecord` of `package` on `node`.
fn clear_record(
    farm: &Farm,
    node: &str,
    package: &str,
) -> Result<proto::ClearRecordResponse, proto::HostsRefusal> {
    farm.world.coordinator().clear_record(
        &operator(),
        &proto::ClearRecordRequest {
            node_id: node.to_owned(),
            service: package.to_owned(),
        },
    )
}

/// A farm whose group `farm-linux` lists `tierstore`, which the rollout file does not define,
/// with the hosts n1 and n2 in it; `tierstore` 1.0 ran on both, 2.0 is desired, n2 failed its
/// update to 2.0 with a halt, which halted the package in the group, and the operator cleared the
/// halt.
fn failed_and_cleared(name: &str) -> Farm {
    let farm = Farm::new(name);
    assert!(
        farm.world.coordinator().services.get(PACKAGE).is_none(),
        "the rollout file does not define the package"
    );
    farm.group(GROUP, &[PACKAGE], None);
    farm.host("n1", GROUP);
    farm.host("n2", GROUP);
    farm.want(GROUP, PACKAGE, "1.0");
    for host in ["n1", "n2"] {
        assert_eq!(farm.tell(host, &[(PACKAGE, "1.0")]), NOTHING);
    }
    farm.want(GROUP, PACKAGE, "2.0");
    assert_eq!(farm.tell("n2", &[(PACKAGE, "1.0")]), update(PACKAGE, "2.0"));
    assert_eq!(stale(&farm, "n2"), NOTHING);
    let group: GroupName = GROUP.parse().expect("a group name");
    assert!(farm.world.ops().group_marks(&group, PACKAGE).halt.is_some());
    assert_eq!(farm.why("n1", PACKAGE), Some(Why::Gate(Gate::Halted)));

    let body = json!({"action": "clear_halt", "service": PACKAGE, "reason": "lock fixed"});
    farm.world
        .coordinator()
        .operate(Some(OPERATOR), body.to_string().as_bytes())
        .expect("the halt cleared");
    assert!(farm.world.ops().group_marks(&group, PACKAGE).halt.is_none());
    farm
}

/// Proves: after a cleared halt, the host of a package only a group lists that failed its update
/// is held back (`failed_here`) at its heartbeat with the failed report; `Hosts.Retry` of it is
/// accepted, answers the failed report (target 2.0), writes one `retry` audit line naming the
/// package and the host, and the host's next heartbeat, with the same report, is ordered the
/// update to 2.0 again. A package that neither the rollout file nor a group names is still
/// `unknown_service`, and nothing more is audited.
///
/// Catches: a retry that looks the package up in the rollout file alone and refuses a package a
/// group lists as `unknown_service`, leaving exclusion as the operator's only way out.
#[test]
fn retry_after_a_clear_halt_orders_a_package_only_a_group_lists_again() {
    let farm = failed_and_cleared("gr-retry");
    assert_eq!(stale(&farm, "n2"), NOTHING);
    assert_eq!(farm.why("n2", PACKAGE), Some(Why::Skip(Skip::FailedHere)));
    let audited = farm.world.audit().len();

    let answer = retry(&farm, "n2", PACKAGE).expect("set aside");
    let set_aside = answer.set_aside.expect("the failed report");
    assert_eq!(set_aside.target, "2.0");
    let entry = answer.entry.expect("an audit entry");
    assert_eq!(
        (
            entry.action.as_str(),
            entry.service.as_deref(),
            entry.node_id.as_deref()
        ),
        ("retry", Some(PACKAGE), Some("n2"))
    );
    assert_eq!(farm.world.audit_after(audited).len(), 1);
    assert_eq!(farm.why("n2", PACKAGE), None);
    assert_eq!(stale(&farm, "n2"), update(PACKAGE, "2.0"));

    assert_eq!(
        retry(&farm, "n2", "nothing").err(),
        Some(proto::HostsRefusal::UnknownService)
    );
    assert_eq!(farm.world.audit_after(audited).len(), 1);
}

/// Proves: `Hosts.ClearRecord` of a package only a group lists is accepted for a host whose
/// daemon announced `clear-record-v1`, with one `record_cleared` audit line naming the package
/// and the host; a package that neither the rollout file nor a group names is still
/// `unknown_service`, and nothing more is audited.
///
/// Catches: a clear-record that looks the package up in the rollout file alone and refuses a
/// package a group lists as `unknown_service`.
#[test]
fn clear_record_knows_a_package_only_a_group_lists() {
    let farm = failed_and_cleared("gr-clear-record");
    let mut features = NEW_DAEMON.to_vec();
    features.push(FEATURE);
    assert_eq!(
        ask(&farm.world.rig, "n2", "storage", failed_beat(&features)),
        NOTHING
    );
    let audited = farm.world.audit().len();

    let answer = clear_record(&farm, "n2", PACKAGE).expect("ordered");
    assert!(!answer.order_id.is_empty());
    let entries = farm.world.audit_after(audited);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (
                entry.action.as_str(),
                entry.service.as_deref(),
                entry.node_id.as_deref()
            ))
            .collect::<Vec<_>>(),
        [("record_cleared", Some(PACKAGE), Some("n2"))]
    );

    assert_eq!(
        clear_record(&farm, "n2", "nothing").err(),
        Some(proto::HostsRefusal::UnknownService)
    );
    assert_eq!(farm.world.audit_after(audited).len(), 1);
}
