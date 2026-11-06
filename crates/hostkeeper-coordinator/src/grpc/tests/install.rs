//! The rollout gate's `install` and `uninstall` as `Coordinator.Heartbeat` answers them through
//! the mounted route ([`crate::grpc::route`]): the verb, its typed arguments, the order id and
//! the try a daemon that announced `install-v1` is sent.

use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;

use super::heartbeat::{beat, farm, updater};
use crate::testutil::OPERATOR;

/// The features of a daemon that lists every service it runs and takes `install` and
/// `uninstall`.
const INSTALLER: [&str; 2] = ["heartbeat-v2", "install-v1"];

/// A heartbeat of a daemon that announced [`INSTALLER`] and runs `services`, each at its
/// version and healthy.
fn installer_beat(services: &[(&str, &str)]) -> proto::HeartbeatRequest {
    proto::HeartbeatRequest {
        platform: proto::Platform::LinuxArm64 as i32,
        agent: Some(proto::AgentInfo {
            name: "hostkeeper-daemon".to_owned(),
            version: "1.0".to_owned(),
        }),
        features: INSTALLER.iter().map(|name| (*name).to_owned()).collect(),
        services: services
            .iter()
            .map(|(name, version)| proto::ServiceBeat {
                name: (*name).to_owned(),
                version: (*version).to_owned(),
                health: proto::Health::Healthy as i32,
                ..proto::ServiceBeat::default()
            })
            .collect(),
    }
}

/// Catches an install or an uninstall the gate decides that the gRPC answer drops, sends as
/// another verb, or sends with the wrong service, version, digest, order id or try: a daemon of
/// the role `other` is assigned to, running only `kbf` at its desired version, is answered
/// `install` of `other` 3.0 with the package digest CI registered; once it runs it and an
/// operator unassigns the host with `uninstall`, it is answered `uninstall` of `other`.
#[tokio::test]
async fn the_grpc_heartbeat_answers_the_gates_install_and_uninstall() {
    let farm = farm("grpc-install").await;
    let who = updater("d1", "daemon");

    let reply = beat(&farm.coordinator, &who, &installer_beat(&[("kbf", "2.0")])).await;
    let response: proto::HeartbeatResponse = reply.decode();
    let order = response.order.expect("an install order");
    assert_eq!(
        (order.order_id.as_str(), order.r#try),
        (format!("install-{}", "1".repeat(24)).as_str(), 1)
    );
    assert_eq!(
        order.verb,
        Some(proto::order::Verb::Install(proto::Install {
            service: "other".to_owned(),
            version: "3.0".to_owned(),
            package_digest: "1".repeat(64),
            index_digest: String::new(),
        }))
    );

    let installed = installer_beat(&[("kbf", "2.0"), ("other", "3.0")]);
    let response: proto::HeartbeatResponse =
        beat(&farm.coordinator, &who, &installed).await.decode();
    assert_eq!(response.order, None, "up to date: nothing to order");

    let caller = farm.coordinator.caller(Some(OPERATOR)).await;
    farm.coordinator
        .unassign(
            &caller,
            operator::UnassignRequest {
                service: "other".to_owned(),
                node_id: "d1".to_owned(),
                uninstall: true,
                ..operator::UnassignRequest::default()
            },
        )
        .expect("unassigned with uninstall");
    let response: proto::HeartbeatResponse =
        beat(&farm.coordinator, &who, &installed).await.decode();
    let order = response.order.expect("an uninstall order");
    assert_eq!((order.order_id.as_str(), order.r#try), ("uninstall-1", 1));
    assert_eq!(
        order.verb,
        Some(proto::order::Verb::Uninstall(proto::Uninstall {
            service: "other".to_owned(),
        }))
    );
}
