//! The halt the host holds of a service, through the conversions of `proto_map`, the JSON facts
//! and the JSON route.

use hostkeeper_config::names::{ServiceName, Version};
use serde_json::json;

use super::heartbeat;
use crate::beat::{ANNOUNCED, Health, Heartbeat, LocalHalt, Platform, ServiceFacts};
use crate::names::Reason;
use crate::ports::HeldHalt;

fn facts(local_halt: Option<LocalHalt>) -> ServiceFacts {
    ServiceFacts {
        local_halt,
        name: ServiceName::new("web").expect("a service name"),
        version: Version::new("1.0").expect("a version"),
        health: Health::Healthy,
        updating: None,
        last_update: None,
        deferred: None,
        health_detail: None,
        observed: None,
        record_try: None,
        installer: None,
        drift: None,
    }
}

fn halt(reason: Option<&str>) -> LocalHalt {
    LocalHalt {
        reason: reason.map(|text| Reason::new(text).expect("a reason")),
        generation: 15,
        age_secs: Some(600),
    }
}

fn beat(services: Vec<ServiceFacts>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: Version::new("0.9.0").expect("a version"),
        features: ANNOUNCED.to_vec(),
        services,
        sent_unix: 0,
        renewal: crate::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
        remote_login_probe: None,
        operator_keys: None,
        ssh: None,
        prerequisites: None,
        bmc: None,
        maintenance: None,
    }
}

/// Proves the gRPC service entry carries the reason, generation and age of the halt the host
/// holds, a halt with no reason carries none, and facts with no halt carry none.
///
/// Catches: a hold dropped on the gRPC route, or a missing reason sent as an empty string.
#[test]
fn the_grpc_service_carries_the_local_halt() {
    let sent = heartbeat(&beat(vec![
        facts(Some(halt(Some("disk_tainted")))),
        facts(Some(halt(None))),
        facts(None),
    ]))
    .expect("a request");
    let carried: Vec<_> = sent.services.iter().map(|s| s.local_halt.clone()).collect();
    assert_eq!(
        carried[0],
        Some(hostkeeper_proto::hostkeeper::agent::v1::LocalHalt {
            reason: Some("disk_tainted".to_owned()),
            generation: 15,
            age_secs: Some(600),
        })
    );
    let bare = carried[1].as_ref().expect("a halt");
    assert_eq!((bare.reason.clone(), bare.generation), (None, 15));
    assert_eq!(carried[2], None);
}

/// Proves the facts as JSON name the halt only when the host holds one, with the wire's names.
///
/// Catches: a `local_halt` key (or a null) on facts that hold no halt.
#[test]
fn the_facts_as_json_name_the_local_halt_only_when_held() {
    let held = facts(Some(halt(Some("disk_tainted")))).to_json();
    assert_eq!(
        held["local_halt"],
        json!({ "reason": "disk_tainted", "generation": 15, "age_secs": 600 })
    );
    let bare = LocalHalt {
        reason: None,
        age_secs: None,
        ..halt(None)
    };
    assert_eq!(
        facts(Some(bare)).to_json()["local_halt"],
        json!({ "reason": null, "generation": 15, "age_secs": null })
    );
    assert_eq!(facts(None).to_json().get("local_halt"), None);
}

/// Proves the age of a halt is the seconds since the write, 0 for a write stamped after now, and
/// none for a write time that is unknown.
///
/// Catches: an age that underflows on a future stamp.
#[test]
fn the_age_of_a_halt_is_taken_from_its_write_time() {
    let held = |written_unix| HeldHalt {
        reason: None,
        generation: 4,
        written_unix,
    };
    assert_eq!(LocalHalt::at(held(Some(400)), 1000).age_secs, Some(600));
    assert_eq!(LocalHalt::at(held(Some(1500)), 1000).age_secs, Some(0));
    assert_eq!(LocalHalt::at(held(None), 1000).age_secs, None);
}

/// Proves the JSON route sends no `local_halt`, on the heartbeat or on a service: the body of a
/// heartbeat whose service holds a halt is the body of one that holds none.
///
/// Catches: the frozen JSON route given a field the release before would refuse or misread.
#[test]
fn the_json_route_sends_no_local_halt() {
    let with = crate::wire_map::heartbeat(&beat(vec![facts(Some(halt(Some("disk_tainted"))))]))
        .expect("a wire heartbeat");
    let without = crate::wire_map::heartbeat(&beat(vec![facts(None)])).expect("a wire heartbeat");
    let body = serde_json::to_string(&with).expect("JSON");
    assert_eq!(body, serde_json::to_string(&without).expect("JSON"));
    assert!(!body.contains("local_halt"), "{body}");
}
