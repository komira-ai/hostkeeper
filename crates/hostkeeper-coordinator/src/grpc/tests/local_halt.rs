//! A host's own halt over gRPC: `ServiceBeat.local_halt` reaches the coordinator's reader as the
//! service's `local_halt`, and `Hosts.Get`, `Hosts.List` and `Releases.Status` show it back in
//! the host's beat.

use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_wire::wire::{LocalHalt, Token, read_heartbeat};
use serde_json::json;

use super::agent::{agent, request, service_json};
use super::report::plain_host;
use crate::grpc::beat_map;
use crate::testutil::beat;

/// A service entry of `svc` 1.0 that holds a halt under generation 15.
fn holding(reason: Option<&str>, age_secs: Option<u64>) -> proto::ServiceBeat {
    proto::ServiceBeat {
        name: "svc".to_owned(),
        version: "1.0".to_owned(),
        health: proto::Health::Healthy as i32,
        local_halt: Some(proto::LocalHalt {
            reason: reason.map(str::to_owned),
            generation: 15,
            age_secs,
        }),
        ..proto::ServiceBeat::default()
    }
}

/// Proves: an entry's `local_halt` is the JSON entry's `local_halt`, its reason, generation and
/// age as they were sent, an absent reason or age `null`; the JSON reader reads it back into the
/// service's beat, and an entry without one has no key.
///
/// Catches: a halt the daemon reports that the coordinator drops before any rule reads it, so
/// the host is shown `failed_here`.
#[test]
fn a_services_local_halt_reaches_the_reader() {
    assert_eq!(
        service_json(holding(Some("disk_tainted"), Some(604_800)))["local_halt"],
        json!({"reason": "disk_tainted", "generation": 15, "age_secs": 604_800})
    );
    assert_eq!(
        service_json(holding(None, None))["local_halt"],
        json!({"reason": null, "generation": 15, "age_secs": null})
    );
    let bare = proto::ServiceBeat {
        local_halt: None,
        ..holding(None, None)
    };
    assert!(service_json(bare).get("local_halt").is_none());

    let message = request(
        agent("hostkeeper-daemon", "1.0"),
        &["order-v1"],
        vec![holding(Some("disk_tainted"), Some(9))],
    );
    let read = read_heartbeat(&beat_map::json(&message, None), None).expect("a heartbeat");
    let listed = read.beat.services.expect("the services");
    assert_eq!(
        listed[0].local_halt,
        Some(LocalHalt {
            reason: Token::parse("disk_tainted"),
            generation: 15,
            age_secs: Some(9),
        })
    );
}

/// Proves: the host's beat, as `Hosts.Get` and the status show it, carries the halt the host
/// holds: its reason, generation and age; a beat with none shows none.
///
/// Catches: a halt shown only as a gate reason, with no cause or generation for the operator.
#[test]
fn the_hosts_beat_shows_the_halt_it_holds() {
    let mut held = beat("2.0");
    held.local_halt = Some(LocalHalt {
        reason: Token::parse("disk_tainted"),
        generation: 15,
        age_secs: Some(604_800),
    });
    assert_eq!(
        plain_host(held).beat.expect("the beat").local_halt,
        Some(proto::LocalHalt {
            reason: Some("disk_tainted".to_owned()),
            generation: 15,
            age_secs: Some(604_800),
        })
    );
    let mut bare = beat("2.0");
    bare.local_halt = Some(LocalHalt {
        reason: None,
        generation: 0,
        age_secs: None,
    });
    assert_eq!(
        plain_host(bare).beat.expect("the beat").local_halt,
        Some(proto::LocalHalt::default())
    );
    assert_eq!(
        plain_host(beat("2.0")).beat.expect("the beat").local_halt,
        None
    );
}
