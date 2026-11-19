//! The host's BMC: read from its source by each heartbeat, and shaped as JSON for the heartbeat's
//! bound. Its mapping onto the gRPC request is tested in `proto_map`, and the coordinator's keeping
//! of it in the coordinator's crate.

use hostkeeper_link::{BeatOutcome, BmcReport};
use serde_json::json;

use crate::Rig;

fn report(last: u8) -> BmcReport {
    BmcReport {
        ip: format!("192.0.2.{last}"),
        mac: "0c:c4:7a:00:00:01".to_owned(),
    }
}

/// Proves the loop reads its BMC source at each heartbeat: none is sent while the source holds
/// none, the first heartbeat after the source holds one carries exactly it, and the first after it
/// changes carries the new one.
#[test]
fn each_heartbeat_carries_the_bmc_its_source_holds_then() {
    let rig = Rig::new("bmc-sent");
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    rig.world.bmc.set(Some(report(12)));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    rig.world.bmc.set(Some(report(40)));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let sent: Vec<Option<BmcReport>> = rig
        .world
        .link
        .beats()
        .into_iter()
        .map(|beat| beat.bmc)
        .collect();
    assert_eq!(
        sent,
        vec![None, Some(report(12)), Some(report(12)), Some(report(40))]
    );
}

/// Proves a report's JSON has the wire's names, the address dotted, and that a heartbeat counts
/// it toward its bound: the heartbeat's JSON holds it as `bmc`, and one without it holds none.
#[test]
fn a_report_is_counted_as_json_with_the_names_of_the_wire() {
    assert_eq!(
        report(12).to_json(),
        json!({ "ip": "192.0.2.12", "mac": "0c:c4:7a:00:00:01" })
    );
    let rig = Rig::new("bmc-json");
    rig.world.bmc.set(Some(report(12)));
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let mut beat = rig.world.link.beats().remove(0);
    assert_eq!(beat.to_json()["bmc"], report(12).to_json());
    let with = beat.encoded_len();
    beat.bmc = None;
    assert_eq!(beat.to_json().get("bmc"), None);
    assert_eq!(
        with - beat.encoded_len(),
        r#","bmc":{"ip":"192.0.2.12","mac":"0c:c4:7a:00:00:01"}"#.len()
    );
}
