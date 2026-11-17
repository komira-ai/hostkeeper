//! The operator keys a heartbeat over gRPC is answered with and reports ([`crate::operator_keys`]),
//! through `crate::grpc::route`, and what `Hosts.Get` and `Hosts.List` show of them. No socket.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::operator_keys::digest;
use tonic::Code;

use super::heartbeat::{beat, farm, json_beat, peer_of, proto_message, svc, updater};
use super::hosts::{operator_call, world_over};
use super::operator::World;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::operator_keys::tests::{file, group, key};
use crate::operator_keys::{FEATURE, OperatorKeys, Reload};
use crate::testutil::{id, scratch};

/// A heartbeat over gRPC of a daemon that announces `operator-keys-v1` and carries `report`.
fn heartbeat(report: Option<agent::OperatorKeysReport>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("kbf", "1.0")]);
    message.features.push(FEATURE.to_owned());
    message.operator_keys = report;
    message
}

/// A report that holds `digest` in force.
fn holding(digest: &str) -> agent::OperatorKeysReport {
    agent::OperatorKeysReport {
        digest: digest.to_owned(),
        ..agent::OperatorKeysReport::default()
    }
}

/// Proves: a heartbeat over gRPC that announces `operator-keys-v1` is answered its group's keys
/// and their digest until a heartbeat reports that digest in force, and is sent them again when
/// it reports none (the daemon started again); after the file changes and the tick re-reads it,
/// the next heartbeat is answered the new keys, and one that reports the new digest is answered
/// none; a host of a group the file does not list is answered the empty set; a heartbeat that does
/// not announce the feature is answered none, and so is the same daemon over the JSON route, which
/// has no key for it.
///
/// Catches: keys not in the answer, in every answer, left out after a change of the file, sent
/// to a daemon that did not ask or to the JSON route (which is frozen), and sent to a host of
/// another group.
#[tokio::test]
async fn a_grpc_heartbeat_is_answered_the_keys_of_its_group_until_the_daemon_reports_them() {
    let dir = scratch("operator-keys-grpc");
    let first = vec![key(1, "ops laptop"), key(2, "ci")];
    let path = file(dir.path(), &group("daemon", &first));
    let keys = OperatorKeys::from_file(&path).expect("the file is read");
    let farm = farm("operator-keys-grpc-farm")
        .await
        .with_operator_keys(keys.clone());
    let answered = |request: agent::HeartbeatRequest, name: &str, host_group: &str| {
        let coordinator = &farm.coordinator;
        let who = updater(name, host_group);
        async move {
            let answer: agent::HeartbeatResponse = beat(coordinator, &who, &request).await.decode();
            answer.operator_keys
        }
    };

    let said = answered(heartbeat(None), "d1", "daemon")
        .await
        .expect("the keys");
    assert_eq!(said.digest, digest(&first));
    assert_eq!(said.keys, first);
    let in_force = heartbeat(Some(holding(&said.digest)));
    assert_eq!(answered(in_force, "d1", "daemon").await, None);
    assert_eq!(
        answered(heartbeat(Some(holding(""))), "d1", "daemon").await,
        Some(said.clone()),
        "a daemon that started again holds none"
    );

    let second = vec![key(3, "ops laptop")];
    file(dir.path(), &group("daemon", &second));
    assert_eq!(keys.reload(), Reload::Applied);
    let old = heartbeat(Some(holding(&said.digest)));
    let newer = answered(old, "d1", "daemon").await.expect("the new keys");
    assert_eq!(newer.keys, second);
    let in_force = heartbeat(Some(holding(&newer.digest)));
    assert_eq!(answered(in_force, "d1", "daemon").await, None);

    let empty = answered(heartbeat(None), "d2", "web")
        .await
        .expect("the empty set");
    assert_eq!((empty.keys, empty.digest), (Vec::new(), digest(&[])));

    let mut plain = heartbeat(None);
    plain.features.retain(|name| name != FEATURE);
    assert_eq!(answered(plain, "d3", "daemon").await, None);

    let json = r#"{"platform":"linux-x86_64","version":"2.0","health":"healthy","agent_features":["heartbeat-v2","operator-keys-v1"]}"#;
    let (code, body) = json_beat(&farm.coordinator, &updater("d4", "daemon"), json).await;
    assert_eq!(code, 200);
    assert!(body.get("operator_keys").is_none(), "{body}");
}

/// Proves: a coordinator started without an operator keys file answers a daemon that announces
/// the feature no keys, whatever the host's group.
///
/// Catches: the empty set sent to every host of a fleet whose operator never gave a file, which
/// would take away the keys the host's owner put in the managed block by hand.
#[tokio::test]
async fn a_coordinator_without_a_file_sends_no_keys() {
    let farm = farm("operator-keys-grpc-no-file").await;
    let answer: agent::HeartbeatResponse = beat(
        &farm.coordinator,
        &updater("d1", "daemon"),
        &heartbeat(None),
    )
    .await
    .decode();
    assert_eq!(answer.operator_keys, None);
}

/// What `Hosts.Get` and `Hosts.List` answer of `node`'s operator keys report; both must say the
/// same.
async fn shown(world: &World, node: &str) -> Option<agent::OperatorKeysReport> {
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
        .operator_keys;
    let (code, _, listed) = operator_call::<proto::ListHostsResponse>(
        world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    let listed: Vec<Option<agent::OperatorKeysReport>> = listed
        .expect("a reply")
        .hosts
        .into_iter()
        .filter(|host| host.node_id == node)
        .map(|host| host.heartbeat.expect("its heartbeat").operator_keys)
        .collect();
    assert_eq!(
        listed,
        std::slice::from_ref(&got),
        "List and Get say the same"
    );
    got
}

/// Proves: the report a gRPC heartbeat carries is what `Hosts.Get` and `Hosts.List` answer in the
/// host's heartbeat, field for field (the digest, the refusal, the failure); a later heartbeat
/// replaces it, one that carries none shows the empty report of a daemon that announced the
/// feature and holds nothing, one that breaks a rule leaves the last shown, a heartbeat of a
/// daemon that no longer announces the feature shows none, and a host that announces it over the
/// JSON route, which carries no report, shows none.
///
/// Catches: a field lost between the heartbeat handler and the operator's answer, a bad report
/// shown, and a report kept for a host after its daemon stopped announcing the feature.
#[tokio::test]
async fn hosts_answer_the_last_operator_keys_report_a_grpc_heartbeat_carried() {
    let enrollment = EnrollmentRig::new("grpc-operator-keys-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-operator-keys", &enrollment);
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
    let refused = agent::OperatorKeysReport {
        digest: "a".repeat(64),
        refused: Some(agent::OperatorKeysRefusal {
            digest: "b".repeat(64),
            reason: "no single local administrator account".to_owned(),
        }),
        failed: "Remote Login did not start".to_owned(),
    };

    send(heartbeat(Some(refused.clone()))).await;
    assert_eq!(shown(&world, "s1").await, Some(refused.clone()));

    send(heartbeat(Some(holding("zz")))).await;
    assert_eq!(shown(&world, "s1").await, Some(refused));

    send(heartbeat(Some(holding(&"c".repeat(64))))).await;
    assert_eq!(shown(&world, "s1").await, Some(holding(&"c".repeat(64))));

    send(heartbeat(None)).await;
    assert_eq!(
        shown(&world, "s1").await,
        Some(agent::OperatorKeysReport::default())
    );

    let mut plain = heartbeat(Some(holding(&"c".repeat(64))));
    plain.features.retain(|name| name != FEATURE);
    send(plain).await;
    assert_eq!(shown(&world, "s1").await, None);

    // A daemon that announces the feature over the JSON route carries no report there.
    let other = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-2", "s2");
    let json = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","agent_features":["heartbeat-v2","operator-keys-v1"]}"#;
    let (code, _) = json_beat(&world.coordinator, &peer_of(&other), json).await;
    assert_eq!(code, 200);
    let announced = world.coordinator.lock_fleet().announced(&id("s2"), FEATURE);
    assert!(announced, "the host's features are the ones it announced");
    assert_eq!(shown(&world, "s2").await, None);
}
