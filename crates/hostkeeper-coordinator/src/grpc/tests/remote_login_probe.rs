//! The Remote Login probe a host's heartbeat carries, end to end through the router as the
//! listeners serve it: the gRPC heartbeat on the host's certificate, then `Hosts.Get` and
//! `Hosts.List` as an operator calls them.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tonic::Code;

use super::heartbeat::{beat, peer_of, proto_message, svc};
use super::hosts::{operator_call, world_over};
use super::operator::World;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::testutil::OPERATOR;

/// A probe of two steps: one that ran a program and one that read a file.
fn probe(started: u64) -> agent::RemoteLoginProbe {
    agent::RemoteLoginProbe {
        started_unix: started,
        steps: vec![
            agent::ProbeStep {
                name: "systemsetup_getremotelogin".to_owned(),
                argv: vec![
                    "/usr/sbin/systemsetup".to_owned(),
                    "-getremotelogin".to_owned(),
                ],
                exit_code: Some(1),
                stdout: "Remote Login: Off".to_owned(),
                stderr: "needs Full Disk Access".to_owned(),
                error: String::new(),
                truncated: true,
            },
            agent::ProbeStep {
                name: "read_sshd_config".to_owned(),
                argv: vec!["read /etc/ssh/sshd_config".to_owned()],
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                error: "no such file".to_owned(),
                truncated: false,
            },
        ],
    }
}

/// The heartbeat of a daemon that runs kbf 1.0 and carries `probe`.
fn carrying(probe: Option<agent::RemoteLoginProbe>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("kbf", "1.0")]);
    message.remote_login_probe = probe;
    message
}

/// What `Hosts.Get` and `Hosts.List` answer of `node`'s probe; both must say the same.
async fn shown(world: &World, node: &str) -> Option<agent::RemoteLoginProbe> {
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
        .remote_login_probe;
    let (code, _, listed) = operator_call::<proto::ListHostsResponse>(
        world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    let listed: Vec<Option<agent::RemoteLoginProbe>> = listed
        .expect("a reply")
        .hosts
        .into_iter()
        .filter(|host| host.node_id == node)
        .map(|host| host.heartbeat.expect("its heartbeat").remote_login_probe)
        .collect();
    assert_eq!(
        listed,
        std::slice::from_ref(&got),
        "List and Get say the same"
    );
    got
}

/// What the status counts of dropped facts, under `remote_login_probe`.
fn dropped(world: &World) -> Option<u64> {
    let status = world.coordinator.status(Some(OPERATOR)).expect("status");
    status.extras_dropped.get("remote_login_probe").copied()
}

/// Proves: the probe a gRPC heartbeat carries is what `Hosts.Get` and `Hosts.List` answer in the
/// host's heartbeat, field for field (the exit code and its absence, the texts, the truncated
/// flag); a later heartbeat with none leaves it shown; a later one with another valid probe
/// replaces it. Nothing is counted as dropped.
///
/// Catches: a field lost between the heartbeat handler and the operator's answer, a probe
/// cleared by a heartbeat that carries none (a daemon sends none until its probe is done), and
/// a newer probe that does not replace the older one.
#[tokio::test]
async fn hosts_answer_the_last_probe_a_grpc_heartbeat_carried() {
    let enrollment = EnrollmentRig::new("grpc-probe-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-probe", &enrollment);
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

    send(carrying(Some(probe(100)))).await;
    assert_eq!(shown(&world, "s1").await, Some(probe(100)));

    send(carrying(None)).await;
    assert_eq!(shown(&world, "s1").await, Some(probe(100)));

    send(carrying(Some(probe(200)))).await;
    assert_eq!(shown(&world, "s1").await, Some(probe(200)));
    assert_eq!(dropped(&world), None);
}

/// Proves: a host that never sent a probe shows none in its heartbeat, with no drop counted.
///
/// Catches: an empty probe made up for a host that sent none.
#[tokio::test]
async fn a_host_that_never_sent_a_probe_shows_none() {
    let enrollment = EnrollmentRig::new("grpc-probe-none-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-probe-none", &enrollment);
    let peer = peer_of(&certificate);
    assert_eq!(
        beat(&world.coordinator, &peer, &carrying(None))
            .await
            .token(),
        (Code::Ok, "")
    );
    assert_eq!(shown(&world, "s1").await, None);
    assert_eq!(dropped(&world), None);
}

/// Proves: a probe of 25 steps is answered OK like any heartbeat, is counted once in the
/// status under `remote_login_probe`, and is not shown; the probe kept before it is still shown,
/// and a valid probe after it is shown again.
///
/// Catches: a heartbeat refused for a bad probe, a bad probe shown or replacing the good one,
/// a bad probe not counted, and a host that shows none after one bad probe.
#[tokio::test]
async fn a_probe_that_breaks_a_rule_is_counted_and_not_shown() {
    let enrollment = EnrollmentRig::new("grpc-probe-bad-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-probe-bad", &enrollment);
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
    let mut too_many = probe(300);
    too_many.steps = vec![probe(300).steps.remove(1); 25];

    send(carrying(Some(too_many.clone()))).await;
    assert_eq!(shown(&world, "s1").await, None);
    assert_eq!(dropped(&world), Some(1));

    send(carrying(Some(probe(100)))).await;
    assert_eq!(shown(&world, "s1").await, Some(probe(100)));

    send(carrying(Some(too_many))).await;
    assert_eq!(shown(&world, "s1").await, Some(probe(100)));
    assert_eq!(dropped(&world), Some(2));
}
