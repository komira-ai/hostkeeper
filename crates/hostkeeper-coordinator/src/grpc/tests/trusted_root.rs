//! The trusted root a host's heartbeat says, end to end through the router as the listeners
//! serve it: the gRPC heartbeat on the host's certificate, then `Hosts.Get`, `Hosts.List` and
//! `Alerts.List` as an operator calls them.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tonic::Code;

use super::heartbeat::{beat, json_beat, json_text, peer_of, proto_message, svc};
use super::hosts::{operator_call, world_over};
use super::operator::World;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};

/// 91 days, in seconds: a root past the daemon's default limit of 90 days.
const STALE_AGE: u64 = 91 * 86_400;

/// What `Hosts.Get` answers of `node`'s trusted root: its age and its reason.
async fn root_of(world: &World, node: &str) -> (Option<u64>, agent::TrustedRootReason) {
    let get = proto::GetHostRequest {
        node_id: node.to_owned(),
    };
    let (code, _, got) =
        operator_call::<proto::GetHostResponse>(world, "/hostkeeper.operator.v1.Hosts/Get", get)
            .await;
    assert_eq!(code, Code::Ok);
    let host = got.and_then(|answer| answer.host).expect("the host");
    (host.trusted_root_age_secs, host.trusted_root_reason())
}

/// Every `trusted_root` alert of `Alerts.List`: its host, severity and message.
async fn root_alerts(world: &World) -> Vec<(String, proto::AlertSeverity, String)> {
    let (code, _, listed) = operator_call::<proto::ListAlertsResponse>(
        world,
        "/hostkeeper.operator.v1.Alerts/List",
        proto::ListAlertsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    listed
        .expect("a reply")
        .alerts
        .into_iter()
        .filter(|alert| alert.kind() == proto::AlertKind::TrustedRoot)
        .map(|alert| (alert.host_id.clone(), alert.severity(), alert.message))
        .collect()
}

/// The heartbeat of a daemon that runs kbf 1.0 and says `age` and `reason` of its trusted root.
fn said(age: Option<u64>, reason: i32) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("kbf", "1.0")]);
    message.trusted_root_age_secs = age;
    message.trusted_root_reason = reason;
    message
}

/// Proves: what each gRPC heartbeat says of the host's trusted root is what `Hosts.Get` and
/// `Hosts.List` answer until the next one, and `Alerts.List` holds one warning `trusted_root`
/// alert of the host, naming it with its reason and age, exactly while the last heartbeat says
/// stale, missing or unparseable: a stale root of 91 days, then one that needs no attention (no
/// alert), a missing one with no age, an unparseable one, and last a heartbeat of a daemon that
/// predates the fields, which is unknown with no age and no alert, not a root that needs no
/// attention.
///
/// Catches: a reason or an age dropped between the heartbeat handler and the operator's answer,
/// a reason kept from an earlier heartbeat, an alert for a root that needs no attention or that
/// is unknown, an alert that stands after the host said its root is fine, and a heartbeat that
/// says nothing read as `NONE`.
#[tokio::test]
async fn hosts_and_alerts_answer_the_trusted_root_the_last_grpc_heartbeat_said() {
    let enrollment = EnrollmentRig::new("grpc-trusted-root-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-trusted-root", &enrollment);
    let send = |message: agent::HeartbeatRequest| {
        let peer = peer_of(&certificate);
        let coordinator = &world.coordinator;
        async move {
            assert_eq!(
                beat(coordinator, &peer, &message).await.token(),
                (Code::Ok, "")
            );
        }
    };
    let warning = |message: &str| {
        vec![(
            "s1".to_owned(),
            proto::AlertSeverity::Warning,
            message.to_owned(),
        )]
    };

    send(said(
        Some(STALE_AGE),
        agent::TrustedRootReason::Stale as i32,
    ))
    .await;
    assert_eq!(
        root_of(&world, "s1").await,
        (Some(STALE_AGE), agent::TrustedRootReason::Stale)
    );
    let (code, _, listed) = operator_call::<proto::ListHostsResponse>(
        &world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    let listed: Vec<(Option<u64>, agent::TrustedRootReason)> = listed
        .expect("a reply")
        .hosts
        .iter()
        .map(|host| (host.trusted_root_age_secs, host.trusted_root_reason()))
        .collect();
    assert_eq!(listed, [(Some(STALE_AGE), agent::TrustedRootReason::Stale)]);
    assert_eq!(
        root_alerts(&world).await,
        warning(
            "s1 reports its Sigstore trusted root stale (7862400 s old): its package checks go \
             on, and refuse every package signed after a Sigstore key rotation the root does not \
             hold; update hostkeeper on it, or copy a current root to it"
        )
    );

    send(said(Some(60), agent::TrustedRootReason::None as i32)).await;
    assert_eq!(
        root_of(&world, "s1").await,
        (Some(60), agent::TrustedRootReason::None)
    );
    assert_eq!(root_alerts(&world).await, []);

    send(said(None, agent::TrustedRootReason::Missing as i32)).await;
    assert_eq!(
        root_of(&world, "s1").await,
        (None, agent::TrustedRootReason::Missing)
    );
    assert_eq!(
        root_alerts(&world).await,
        warning(
            "s1 reports its Sigstore trusted root missing (no age): its package checks fail; \
             update hostkeeper on it, or copy a current root to it"
        )
    );

    send(said(Some(5), agent::TrustedRootReason::Unparseable as i32)).await;
    assert_eq!(
        root_of(&world, "s1").await,
        (Some(5), agent::TrustedRootReason::Unparseable)
    );
    assert_eq!(
        root_alerts(&world).await,
        warning(
            "s1 reports its Sigstore trusted root unparseable (5 s old): its package checks \
             fail; update hostkeeper on it, or copy a current root to it"
        )
    );

    // A daemon that predates the fields: nothing said is unknown, and raises nothing.
    send(proto_message(&[svc("kbf", "1.0")])).await;
    assert_eq!(
        root_of(&world, "s1").await,
        (None, agent::TrustedRootReason::Unspecified)
    );
    assert_eq!(root_alerts(&world).await, []);
}

/// Proves: a heartbeat on the JSON route, which carries no trusted root, leaves the host's root
/// unknown, though its gRPC heartbeat before said stale, and so ends the host's alert; and a
/// reason by a value this coordinator does not know is unknown, with the age it came with, and
/// raises no alert.
///
/// Catches: the JSON route reading as a root that needs no attention, a stale root kept from a
/// heartbeat that is no longer the last, and an unknown reason read as one that needs attention
/// or as one that needs none.
#[tokio::test]
async fn the_json_route_and_an_unknown_reason_leave_the_trusted_root_unknown() {
    let enrollment = EnrollmentRig::new("grpc-trusted-root-json-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-trusted-root-json", &enrollment);
    let peer = peer_of(&certificate);
    let stale = said(Some(STALE_AGE), agent::TrustedRootReason::Stale as i32);
    assert_eq!(
        beat(&world.coordinator, &peer, &stale).await.token(),
        (Code::Ok, "")
    );
    assert_eq!(root_alerts(&world).await.len(), 1);

    let (status, _) = json_beat(&world.coordinator, &peer, &json_text(&[svc("kbf", "1.0")])).await;
    assert_eq!(status, 200);
    assert_eq!(
        root_of(&world, "s1").await,
        (None, agent::TrustedRootReason::Unspecified)
    );
    assert_eq!(root_alerts(&world).await, []);

    let later = said(Some(9), 99);
    assert_eq!(
        beat(&world.coordinator, &peer, &later).await.token(),
        (Code::Ok, "")
    );
    assert_eq!(
        root_of(&world, "s1").await,
        (Some(9), agent::TrustedRootReason::Unspecified)
    );
    assert_eq!(root_alerts(&world).await, []);
}
