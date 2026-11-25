//! The serial console report a heartbeat over gRPC carries ([`crate::serial_console_report`]),
//! through `crate::grpc::route`, and what `Hosts.Get` and `Hosts.List` show of it. No socket.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tonic::Code;

use super::heartbeat::{beat, peer_of, proto_message, svc};
use super::hosts::{operator_call, world_over};
use super::operator::World;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::testutil::OPERATOR;

/// A heartbeat over gRPC of a daemon that carries `report`; it announces no feature.
fn heartbeat(report: Option<agent::SerialConsoleReport>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("web", "1.0")]);
    message.serial_console = report;
    message
}

/// A report of `ttyS1` in `state`.
fn report(state: &str, detail: &str) -> agent::SerialConsoleReport {
    agent::SerialConsoleReport {
        port: "ttyS1".to_owned(),
        state: state.to_owned(),
        detail: detail.to_owned(),
    }
}

/// What `Hosts.Get` and `Hosts.List` answer of `node`'s serial console report; both must say the
/// same.
async fn shown(world: &World, node: &str) -> Option<agent::SerialConsoleReport> {
    let get = proto::GetHostRequest {
        node_id: node.to_owned(),
    };
    let (code, _, got) =
        operator_call::<proto::GetHostResponse>(world, "/hostkeeper.operator.v1.Hosts/Get", get)
            .await;
    assert_eq!(code, Code::Ok);
    let got = got
        .and_then(|answer| answer.host)
        .expect("the host")
        .heartbeat
        .expect("its heartbeat")
        .serial_console;
    let (code, _, listed) = operator_call::<proto::ListHostsResponse>(
        world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    let listed: Vec<Option<agent::SerialConsoleReport>> = listed
        .expect("a reply")
        .hosts
        .into_iter()
        .filter(|host| host.node_id == node)
        .map(|host| host.heartbeat.expect("its heartbeat").serial_console)
        .collect();
    assert_eq!(
        listed,
        std::slice::from_ref(&got),
        "List and Get say the same"
    );
    got
}

/// Proves: the report a gRPC heartbeat carries is what `Hosts.Get` and `Hosts.List` answer in the
/// host's heartbeat, field for field (state `reboot_pending`); a heartbeat without one leaves it
/// shown; a report that breaks a rule is answered OK, counted under `serial_console`, and leaves
/// the one shown; a later valid report replaces it; an unknown state is shown as `other`.
#[tokio::test]
async fn hosts_answer_the_last_serial_console_report_a_grpc_heartbeat_carried() {
    let enrollment = EnrollmentRig::new("grpc-serial-console-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-serial-console", &enrollment);
    let peer = peer_of(&certificate);
    let send = |message: agent::HeartbeatRequest| {
        let coordinator = &world.coordinator;
        let peer = &peer;
        async move {
            assert_eq!(
                beat(coordinator, peer, &message).await.token(),
                (Code::Ok, "")
            );
        }
    };
    send(heartbeat(None)).await;
    assert_eq!(shown(&world, "s1").await, None);

    send(heartbeat(Some(report("reboot_pending", "")))).await;
    assert_eq!(
        shown(&world, "s1").await,
        Some(report("reboot_pending", ""))
    );

    send(heartbeat(None)).await;
    assert_eq!(
        shown(&world, "s1").await,
        Some(report("reboot_pending", ""))
    );

    let bad = agent::SerialConsoleReport {
        port: "ttyUSB0".to_owned(),
        ..report("applied", "")
    };
    send(heartbeat(Some(bad))).await;
    assert_eq!(
        shown(&world, "s1").await,
        Some(report("reboot_pending", ""))
    );
    let status = world.coordinator.status(Some(OPERATOR)).expect("status");
    assert_eq!(
        status.extras_dropped.get("serial_console").copied(),
        Some(1)
    );

    send(heartbeat(Some(report("failed", "update_grub")))).await;
    assert_eq!(
        shown(&world, "s1").await,
        Some(report("failed", "update_grub"))
    );

    send(heartbeat(Some(report("rebooting", "")))).await;
    assert_eq!(shown(&world, "s1").await, Some(report("other", "")));
}
