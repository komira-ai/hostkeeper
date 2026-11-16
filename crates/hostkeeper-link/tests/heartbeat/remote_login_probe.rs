//! The Remote Login probe of a Mac: read from its source by each heartbeat, and shaped as JSON for
//! the heartbeat's bound. Its mapping onto the gRPC request is tested in `proto_map`, and the
//! coordinator's keeping of it in the coordinator's crate.

use hostkeeper_link::testing::World;
use hostkeeper_link::{BeatOutcome, ProbeStep, RemoteLoginProbe};
use serde_json::json;

use crate::Rig;

fn probe() -> RemoteLoginProbe {
    RemoteLoginProbe {
        started_unix: 1_700_000_000,
        steps: vec![ProbeStep {
            name: "dscl_read".to_owned(),
            argv: vec!["/usr/bin/dscl".to_owned(), ".".to_owned()],
            exit_code: Some(0),
            stdout: "out".to_owned(),
            stderr: String::new(),
            error: String::new(),
            truncated: false,
        }],
    }
}

/// Proves a Mac's loop reads its probe source at each heartbeat: none is sent while the source
/// holds none, the first heartbeat after the source holds one carries exactly it, and every later
/// heartbeat carries it again.
#[test]
fn a_probing_world_sends_the_probe_from_the_beat_after_it_is_set_and_after() {
    let rig = Rig::with_world("probe-sent", World::default().probing());
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    rig.world.probe.set(Some(probe()));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let sent: Vec<Option<RemoteLoginProbe>> = rig
        .world
        .link
        .beats()
        .into_iter()
        .map(|beat| beat.remote_login_probe)
        .collect();
    assert_eq!(sent, vec![None, Some(probe()), Some(probe())]);
}

/// Catches a loop that reads a probe from a host that runs none: a world without `probing`
/// never sends one, even when the fake source holds one.
#[test]
fn a_world_that_does_not_probe_never_sends_a_probe() {
    let rig = Rig::new("probe-not-run");
    rig.world.probe.set(Some(probe()));
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let sent: Vec<Option<RemoteLoginProbe>> = rig
        .world
        .link
        .beats()
        .into_iter()
        .map(|beat| beat.remote_login_probe)
        .collect();
    assert_eq!(sent, vec![None, None]);
}

/// Proves a probe's JSON has the wire's names: `started_unix` and the steps in order, each with
/// `name`, `argv`, `exit_code` (null when none), `stdout`, `stderr`, `error` and `truncated`.
#[test]
fn a_probe_is_counted_as_json_with_the_names_of_the_wire() {
    assert_eq!(
        probe().to_json(),
        json!({
            "started_unix": 1_700_000_000_u64,
            "steps": [{
                "name": "dscl_read",
                "argv": ["/usr/bin/dscl", "."],
                "exit_code": 0,
                "stdout": "out",
                "stderr": "",
                "error": "",
                "truncated": false,
            }],
        })
    );
    let unrun = RemoteLoginProbe {
        started_unix: 5,
        steps: vec![ProbeStep {
            name: "sqlite_read".to_owned(),
            error: "timed out".to_owned(),
            truncated: true,
            ..ProbeStep::default()
        }],
    };
    assert_eq!(
        unrun.to_json(),
        json!({
            "started_unix": 5,
            "steps": [{
                "name": "sqlite_read",
                "argv": [],
                "exit_code": null,
                "stdout": "",
                "stderr": "",
                "error": "timed out",
                "truncated": true,
            }],
        })
    );
}
