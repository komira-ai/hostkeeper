//! The facts of `facts-v1` through the conversions of `proto_map`: the generation out, the host's
//! facts in.

use hostkeeper_config::names::Version;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::{heartbeat, reply};
use crate::beat::{ANNOUNCED, Heartbeat, Platform};

fn beat(platform: Platform) -> Heartbeat {
    Heartbeat {
        platform,
        agent_version: Version::new("0.9.0").expect("a version"),
        features: ANNOUNCED.to_vec(),
        services: Vec::new(),
        sent_unix: 0,
        renewal: crate::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
    }
}

fn sent(beat: &Heartbeat) -> pb::HeartbeatRequest {
    heartbeat(beat).expect("a request")
}

/// Proves the generation the facts file holds goes out as `facts_generation`, 0 included, and a
/// heartbeat with none leaves the field out.
#[test]
fn the_facts_generation_goes_out_as_its_field() {
    let mut with = beat(Platform::LinuxX86_64);
    with.facts_generation = Some(12);
    assert_eq!(sent(&with).facts_generation, Some(12));
    with.facts_generation = Some(0);
    assert_eq!(sent(&with).facts_generation, Some(0));
    with.facts_generation = None;
    assert_eq!(sent(&with).facts_generation, None);
}

/// Proves the facts of a response come in whole, as decoded, their labels by key, and a response
/// with none has none.
#[test]
fn a_response_keeps_its_facts_as_decoded() {
    use crate::host_facts::PushedFacts;
    let mapped = reply(pb::HeartbeatResponse {
        facts: Some(pb::HostFacts {
            generation: 6,
            host_id: "web-04".to_owned(),
            host_group: "web".to_owned(),
            labels: [("rack", "r12"), ("az", "b")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }),
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(
        mapped.reply.facts,
        Some(PushedFacts {
            generation: 6,
            host_id: "web-04".to_owned(),
            host_group: "web".to_owned(),
            labels: [("az", "b"), ("rack", "r12")]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        })
    );
    assert_eq!(mapped.dropped, None);
    assert_eq!(reply(pb::HeartbeatResponse::default()).reply.facts, None);
}
