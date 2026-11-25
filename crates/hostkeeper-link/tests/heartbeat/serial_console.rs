//! The host's serial console baseline report: read from its source by each heartbeat, and shaped
//! as JSON for the heartbeat's bound. Its mapping onto the gRPC request is tested in `proto_map`,
//! and the daemon's keeping of the baseline in the daemon's crate.

use hostkeeper_link::{BeatOutcome, SerialConsoleReport};
use serde_json::json;

use crate::Rig;

fn report(state: &str, detail: &str) -> SerialConsoleReport {
    SerialConsoleReport {
        port: "ttyS1".to_owned(),
        state: state.to_owned(),
        detail: detail.to_owned(),
    }
}

/// Proves the loop reads its serial console source at each heartbeat: none is sent while the
/// source holds none, the first heartbeat after the source holds one carries exactly it, and the
/// first after it changes carries the new one.
#[test]
fn each_heartbeat_carries_the_serial_console_report_its_source_holds_then() {
    let rig = Rig::new("serial-console-sent");
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    rig.world
        .serial_console
        .set(Some(report("reboot_pending", "")));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    rig.world
        .serial_console
        .set(Some(report("failed", "update_grub")));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let sent: Vec<Option<SerialConsoleReport>> = rig
        .world
        .link
        .beats()
        .into_iter()
        .map(|beat| beat.serial_console)
        .collect();
    assert_eq!(
        sent,
        vec![
            None,
            Some(report("reboot_pending", "")),
            Some(report("reboot_pending", "")),
            Some(report("failed", "update_grub")),
        ]
    );
}

/// Proves a report's JSON has the wire's names, and that a heartbeat counts it toward its bound:
/// the heartbeat's JSON holds it as `serial_console`, and one without it holds none, shorter by
/// exactly the report's key and value.
#[test]
fn a_serial_console_report_is_counted_as_json_with_the_names_of_the_wire() {
    assert_eq!(
        report("applied", "").to_json(),
        json!({ "port": "ttyS1", "state": "applied", "detail": "" })
    );
    let rig = Rig::new("serial-console-json");
    rig.world.serial_console.set(Some(report("applied", "")));
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let mut beat = rig.world.link.beats().remove(0);
    assert_eq!(
        beat.to_json()["serial_console"],
        report("applied", "").to_json()
    );
    let with = beat.encoded_len();
    beat.serial_console = None;
    assert_eq!(beat.to_json().get("serial_console"), None);
    assert_eq!(
        with - beat.encoded_len(),
        r#","serial_console":{"detail":"","port":"ttyS1","state":"applied"}"#.len()
    );
}
