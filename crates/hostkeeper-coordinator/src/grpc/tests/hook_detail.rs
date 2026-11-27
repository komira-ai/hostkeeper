//! The hook's `detail` of a halted update, end to end through the router as the listeners serve
//! it: the gRPC heartbeat on the host's certificate, then `Hosts.Get` and `Alerts.List` as an
//! operator calls them.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tonic::Code;

use super::heartbeat::{beat, peer_of, proto_message, svc};
use super::hosts::{operator_call, world_over};
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};

/// What js-agent's hook said when its state directory had no parent on a first install.
const SAID: &str = "/Library/Application Support/js-agent: No such file or directory (os error 2)";

/// The heartbeat of a daemon whose update of kbf to 2.0 halted with `state_invalid` and `detail`.
fn halted(detail: Option<&str>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("kbf", "1.0")]);
    message.services[0].last_update = Some(agent::UpdateResult {
        target: "2.0".to_owned(),
        outcome: agent::Outcome::Failed as i32,
        phase: agent::FailurePhase::Halt as i32,
        reason: "state_invalid".to_owned(),
        detail: detail.map(str::to_owned),
        ..agent::UpdateResult::default()
    });
    message
}

/// Proves: the detail a gRPC heartbeat's halted verdict carries is what `Hosts.Get` answers, in
/// the host's heartbeat and in kbf's `last_update` of the host, with the reason beside it, and
/// what the halt alert of `Alerts.List` quotes after the check; a detail longer than the
/// coordinator keeps is answered cut to 256 bytes at a character boundary.
///
/// Catches: the hook's text dropped anywhere between the heartbeat handler and the operator's
/// answers, the gap that left a halt on a Mac with nothing but `state_invalid`.
#[tokio::test]
async fn a_halts_detail_reaches_hosts_get_and_the_halt_alert() {
    let enrollment = EnrollmentRig::new("grpc-detail-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-detail", &enrollment);
    let peer = peer_of(&certificate);
    let shown = || async {
        let get = proto::GetHostRequest {
            node_id: "s1".to_owned(),
        };
        let (code, said, got) = operator_call::<proto::GetHostResponse>(
            &world,
            "/hostkeeper.operator.v1.Hosts/Get",
            get,
        )
        .await;
        assert_eq!((code, said.as_str()), (Code::Ok, ""));
        let host = got.and_then(|answer| answer.host).expect("the host");
        let beat_detail = host.heartbeat.expect("its heartbeat").services[0]
            .last_update
            .clone()
            .expect("the beat's last update")
            .detail;
        let kbf = host
            .services
            .iter()
            .find(|one| one.service == "kbf")
            .expect("kbf of the host");
        let last = kbf.last_update.clone().expect("kbf's last update");
        assert_eq!(last.reason, "state_invalid");
        assert_eq!(
            beat_detail, last.detail,
            "the beat and kbf's view say the same"
        );
        last.detail
    };

    let reply = beat(&world.coordinator, &peer, &halted(Some(SAID))).await;
    assert_eq!(reply.token(), (Code::Ok, ""));
    assert_eq!(shown().await.as_deref(), Some(SAID));

    let (code, said, listed) = operator_call::<proto::ListAlertsResponse>(
        &world,
        "/hostkeeper.operator.v1.Alerts/List",
        proto::ListAlertsRequest {},
    )
    .await;
    assert_eq!((code, said.as_str()), (Code::Ok, ""));
    let halts: Vec<String> = listed
        .expect("a reply")
        .alerts
        .into_iter()
        .filter(|alert| alert.kind == proto::AlertKind::Halt as i32)
        .map(|alert| alert.message)
        .collect();
    assert_eq!(
        halts,
        [format!(
            "kbf is halted at 2.0 by s1, check state_invalid ({SAID:?}): the hosts that \
             reported it are ordered nothing of it, and no host is ordered a version it was \
             reported at, until an operator clears the halt"
        )]
    );

    let long = format!("{}é{}", "x".repeat(255), "y".repeat(255));
    let reply = beat(&world.coordinator, &peer, &halted(Some(&long))).await;
    assert_eq!(reply.token(), (Code::Ok, ""));
    assert_eq!(shown().await, Some("x".repeat(255)));
}
