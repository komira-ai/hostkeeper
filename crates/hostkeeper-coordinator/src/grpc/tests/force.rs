//! A standing force as `Coordinator.Heartbeat` answers it through the mounted route
//! ([`crate::grpc::route`]): the features the response advertises, and the typed `force` a
//! daemon that announced `forced-v1` is sent.

use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::hostkeeper::operator::v1::force_request::Targets;

use super::heartbeat::{beat, farm, updater};
use crate::service::FEATURES;
use crate::testutil::OPERATOR;

/// A heartbeat of a daemon that announced `features` and runs `kbf` at 2.0 and `other` at 1.0,
/// both healthy.
fn daemon_beat(features: &[&str]) -> proto::HeartbeatRequest {
    proto::HeartbeatRequest {
        platform: proto::Platform::LinuxArm64 as i32,
        agent: Some(proto::AgentInfo {
            name: "hostkeeper-daemon".to_owned(),
            version: "1.0".to_owned(),
        }),
        features: features.iter().map(|name| (*name).to_owned()).collect(),
        services: [("kbf", "2.0"), ("other", "1.0")]
            .iter()
            .map(|(name, version)| proto::ServiceBeat {
                name: (*name).to_owned(),
                version: (*version).to_owned(),
                health: proto::Health::Healthy as i32,
                ..proto::ServiceBeat::default()
            })
            .collect(),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
    }
}

/// Catches the gRPC response advertising other features than the JSON answer, `forced-v1`
/// among them: a daemon that announced every feature the coordinator advertises is answered
/// with all of them, in the coordinator's order, and the one `forced-v1` daemon of a force of
/// `other` to 3.0 is sent the typed `force` with `forced-v1` in the same response.
#[tokio::test]
async fn the_grpc_response_advertises_forced_v1_with_the_force() {
    let farm = farm("grpc-force").await;
    let who = updater("d1", "daemon");
    let every: proto::HeartbeatResponse = beat(&farm.coordinator, &who, &daemon_beat(FEATURES))
        .await
        .decode();
    assert_eq!(every.features, FEATURES);

    let forced = ["heartbeat-v2", "forced-v1"];
    let response: proto::HeartbeatResponse = beat(&farm.coordinator, &who, &daemon_beat(&forced))
        .await
        .decode();
    assert_eq!(response.features, forced);
    let caller = farm.coordinator.caller(Some(OPERATOR)).await;
    let made = farm
        .coordinator
        .force(
            &caller,
            operator::ForceRequest {
                service: "other".to_owned(),
                version: "3.0".to_owned(),
                reason: "fix the farm".to_owned(),
                targets: Some(Targets::Hosts(operator::HostList {
                    node_ids: vec!["d1".to_owned()],
                })),
                include_held: false,
            },
        )
        .expect("the force is made");
    assert_eq!(made.targeted, 1, "{made:?}");

    let response: proto::HeartbeatResponse = beat(&farm.coordinator, &who, &daemon_beat(&forced))
        .await
        .decode();
    assert_eq!(response.features, forced);
    let order = response.order.expect("the force");
    assert_eq!((order.order_id.as_str(), order.r#try), ("force-1", 1));
    assert_eq!(
        order.verb,
        Some(proto::order::Verb::Force(proto::Force {
            service: "other".to_owned(),
            version: "3.0".to_owned(),
            package_digest: "1".repeat(64),
            index_digest: String::new(),
            halt_generation: 1,
        }))
    );
}
