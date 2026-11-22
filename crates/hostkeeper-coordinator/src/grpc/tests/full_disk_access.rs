//! The Full Disk Access state a host's heartbeat carries, end to end through the router as the
//! listeners serve it: the gRPC heartbeat on the host's certificate, then `Hosts.Get` and
//! `Hosts.List` as an operator calls them.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tonic::Code;

use super::heartbeat::{beat, peer_of, proto_message, svc};
use super::hosts::{operator_call, world_over};
use super::operator::World;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};

/// The heartbeat of a daemon that runs kbf 1.0 and carries the state number `state`.
fn carrying(state: i32) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("kbf", "1.0")]);
    message.full_disk_access = state;
    message
}

/// What `Hosts.Get` and `Hosts.List` answer of `node`'s state; both must say the same.
async fn shown(world: &World, node: &str) -> proto::Host {
    let get = proto::GetHostRequest {
        node_id: node.to_owned(),
    };
    let (code, _, got) =
        operator_call::<proto::GetHostResponse>(world, "/hostkeeper.operator.v1.Hosts/Get", get)
            .await;
    assert_eq!(code, Code::Ok);
    let got = got.and_then(|answer| answer.host).expect("the host");
    let (code, _, listed) = operator_call::<proto::ListHostsResponse>(
        world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    let listed: Vec<i32> = listed
        .expect("a reply")
        .hosts
        .into_iter()
        .filter(|host| host.node_id == node)
        .map(|host| host.full_disk_access)
        .collect();
    assert_eq!(listed, [got.full_disk_access], "List and Get say the same");
    got
}

/// Proves: the state a gRPC heartbeat carries is what `Hosts.Get` and `Hosts.List` answer in
/// `Host.full_disk_access`, for each of the states; the next heartbeat replaces it, and one that
/// carries none (a daemon before the field) makes the host unknown again.
///
/// Catches: a state lost between the heartbeat handler and the operator's answer, a newer state
/// that does not replace the older one, and a host left as granted after a heartbeat that says
/// nothing.
#[tokio::test]
async fn hosts_answer_the_state_the_last_grpc_heartbeat_carried() {
    let enrollment = EnrollmentRig::new("grpc-fda-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-fda", &enrollment);
    let peer = peer_of(&certificate);
    for state in [
        agent::FullDiskAccess::Granted,
        agent::FullDiskAccess::NotGranted,
        agent::FullDiskAccess::Undetermined,
        agent::FullDiskAccess::NotApplicable,
        agent::FullDiskAccess::Unspecified,
    ] {
        let reply = beat(&world.coordinator, &peer, &carrying(state.into())).await;
        assert_eq!(reply.token(), (Code::Ok, ""));
        assert_eq!(shown(&world, "s1").await.full_disk_access, i32::from(state));
    }
}

/// Proves: a state number this coordinator does not know is answered as UNSPECIFIED, and the
/// heartbeat is answered OK.
///
/// Catches: a state a later daemon sends shown as granted, or a heartbeat refused for it.
#[tokio::test]
async fn an_unknown_state_number_is_shown_as_unspecified() {
    let enrollment = EnrollmentRig::new("grpc-fda-unknown-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-fda-unknown", &enrollment);
    let peer = peer_of(&certificate);
    let reply = beat(&world.coordinator, &peer, &carrying(99)).await;
    assert_eq!(reply.token(), (Code::Ok, ""));
    assert_eq!(
        shown(&world, "s1").await.full_disk_access,
        i32::from(agent::FullDiskAccess::Unspecified)
    );
}
