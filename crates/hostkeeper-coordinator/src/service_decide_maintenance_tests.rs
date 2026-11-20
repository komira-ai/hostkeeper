//! The maintenance report of a beat another coordinator heard and kept in `fleet_hosts`, taken
//! in by this one ([`Coordinator::take_in_host`]).

use super::*;
use crate::maintenance::tests::report;
use crate::maintenance::{FEATURE, text_of};
use crate::testutil::{Rig, beat, id};

/// The row of host `d1` that holds `heard`, as the coordinator that heard it stored it.
fn row_of(heard: &Heartbeat) -> HostRow {
    HostRow {
        host_id: "d1".to_owned(),
        role: "storage".to_owned(),
        beat: serde_json::to_vec(heard).expect("JSON"),
        seen_unix: 100,
        sent_unix: 100,
        instance: "other".to_owned(),
    }
}

/// Proves: the node a coordinator makes of a beat another coordinator kept holds the beat's
/// maintenance report when its agent announces `maintenance-v1`, and holds none when it does not.
///
/// Catches: a report that is shown by the coordinator that heard it and lost by every other
/// coordinator of a shared database, which would answer `Hosts.Get` without it.
#[test]
fn a_beat_another_coordinator_kept_brings_its_maintenance_report_in() {
    let rig = Rig::new("take-in-maintenance");
    let ops = rig.coordinator.ops().state();
    let now = Duration::from_secs(1_000);
    let mut heard = beat("1.0");
    heard.agent_features = vec![FEATURE.to_owned()];
    heard.maintenance = Some(text_of(&report(2, 1_800_000_000)));

    let mut fleet = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    rig.coordinator
        .take_in_host(&mut fleet, &ops, &row_of(&heard), 100, now);
    let kept = fleet
        .node(&id("d1"))
        .and_then(|node| node.maintenance.clone());
    assert_eq!(
        kept.map(|kept| kept.report().clone()),
        Some(report(2, 1_800_000_000))
    );

    heard.agent_features = Vec::new();
    let mut fleet = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    rig.coordinator
        .take_in_host(&mut fleet, &ops, &row_of(&heard), 100, now);
    let node = fleet.node(&id("d1")).expect("the node");
    assert_eq!(node.maintenance, None);
}
