//! The SSH access a heartbeat over gRPC is answered with and the report it carries
//! ([`crate::ssh_heartbeat`]), through `crate::grpc::route`, and what `Hosts.Get` and `Hosts.List`
//! show of the report. No socket.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tonic::Code;

use super::heartbeat::{beat, json_beat, peer_of, proto_message, svc, updater};
use super::hosts::{operator_call, world_over};
use super::operator::World;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::ssh_heartbeat::FEATURE;
use crate::ssh_tunnel::tests::Rig;
use crate::testutil::id;

/// A heartbeat over gRPC of a daemon that announces `ssh-v1` and carries `report`.
fn heartbeat(report: Option<agent::SshReport>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("web", "1.0")]);
    message.features.push(FEATURE.to_owned());
    message.ssh = report;
    message
}

/// A report that says it applied `generation`.
fn applied(generation: u64) -> agent::SshReport {
    agent::SshReport {
        applied_generation: generation,
        state: "applied".to_owned(),
        host_key_fingerprints: vec![
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ],
        detail: "keyword".to_owned(),
    }
}

/// Proves: a heartbeat over gRPC that announces `ssh-v1` is answered the host's access (its
/// principal, the fleet's keys) until a later heartbeat reports its generation; after a
/// revocation of the host's certificate the next one is answered the access again, with the
/// serial; a heartbeat that does not announce the feature is answered none.
///
/// Catches: an access that never reaches the answer (no host would ever trust the CA), one that
/// is sent for ever, and a revocation no host is told.
#[tokio::test]
async fn a_grpc_heartbeat_is_answered_the_access_until_the_daemon_reports_it() {
    let rig = Rig::new("grpc-ssh-access", |_| {}).await;
    let who = updater("node-07", "web");
    let answered = |request: agent::HeartbeatRequest| {
        let coordinator = &rig.coordinator;
        let who = &who;
        async move {
            let answer: agent::HeartbeatResponse = beat(coordinator, who, &request).await.decode();
            answer.ssh
        }
    };
    let first = answered(heartbeat(None)).await.expect("the access");
    assert!(first.enabled);
    assert_eq!(first.principal, "hk-host-node-07");
    assert_eq!(first.ca_public_keys.len(), 2);
    assert_eq!(first.password_auth, "keep");
    assert_eq!(
        answered(heartbeat(Some(applied(1)))).await,
        Some(first.clone()),
        "an older generation does not hold it"
    );
    assert_eq!(
        answered(heartbeat(Some(applied(first.generation)))).await,
        None
    );

    let issued = crate::ssh_ledger::Ledger::issue(
        &*rig.ledger,
        &crate::ssh_ledger::Issue {
            operator: "operator",
            host_id: "node-07",
            fingerprint: "SHA256:x",
            ttl_secs: 3600,
            reason: "",
        },
    )
    .expect("signed");
    crate::ssh_ledger::Ledger::revoke(
        &*rig.ledger,
        "operator",
        crate::ssh_ledger::Revocation::Serial(issued.serial),
    )
    .expect("revoked");
    let second = answered(heartbeat(Some(applied(first.generation))))
        .await
        .expect("sent again");
    assert_eq!(second.revoked_serials, vec![issued.serial]);

    let mut plain = heartbeat(None);
    plain.features.retain(|name| name != FEATURE);
    assert_eq!(answered(plain).await, None);
}

/// What `Hosts.Get` and `Hosts.List` answer of `node`'s SSH report; both must say the same.
async fn shown(world: &World, node: &str) -> Option<agent::SshReport> {
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
        .ssh;
    let (code, _, listed) = operator_call::<proto::ListHostsResponse>(
        world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    let listed: Vec<Option<agent::SshReport>> = listed
        .expect("a reply")
        .hosts
        .into_iter()
        .filter(|host| host.node_id == node)
        .map(|host| host.heartbeat.expect("its heartbeat").ssh)
        .collect();
    assert_eq!(
        listed,
        std::slice::from_ref(&got),
        "List and Get say the same"
    );
    got
}

/// Proves: the report a gRPC heartbeat carries is what `Hosts.Get` and `Hosts.List` answer in the
/// host's heartbeat, field for field; a later heartbeat replaces it, one that breaks a rule
/// leaves the last shown, a heartbeat of a daemon that no longer announces the feature shows
/// none, and a host that announces it over the JSON route, which carries no report, shows none.
///
/// Catches: a field lost between the heartbeat handler and the operator's answer, a bad report
/// shown, and a report kept for a host after its daemon stopped announcing the feature.
#[tokio::test]
async fn hosts_answer_the_last_ssh_report_a_grpc_heartbeat_carried() {
    let enrollment = EnrollmentRig::new("grpc-ssh-report-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-ssh-report", &enrollment);
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
    send(heartbeat(Some(applied(7)))).await;
    assert_eq!(shown(&world, "s1").await, Some(applied(7)));

    let bad = agent::SshReport {
        state: "x".repeat(33),
        ..applied(8)
    };
    send(heartbeat(Some(bad))).await;
    assert_eq!(shown(&world, "s1").await, Some(applied(7)));

    send(heartbeat(Some(applied(9)))).await;
    assert_eq!(shown(&world, "s1").await, Some(applied(9)));

    let mut plain = heartbeat(Some(applied(9)));
    plain.features.retain(|name| name != FEATURE);
    send(plain).await;
    assert_eq!(shown(&world, "s1").await, None);

    let other = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-2", "s2");
    let json = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","agent_features":["heartbeat-v2","ssh-v1"]}"#;
    let (code, _) = json_beat(&world.coordinator, &peer_of(&other), json).await;
    assert_eq!(code, 200);
    let announced = world.coordinator.lock_fleet().announced(&id("s2"), FEATURE);
    assert!(announced, "the host's features are the ones it announced");
    assert_eq!(shown(&world, "s2").await, None);
}
