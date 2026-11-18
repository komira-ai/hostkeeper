//! The host prerequisites a heartbeat over gRPC is answered with and reports
//! ([`crate::prerequisites`]), through `crate::grpc::route`, what `Hosts.Get` and `Hosts.List` show
//! of them, and the alert `Alerts.List` raises for a failed one. No socket.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::prerequisites::digest;
use tonic::Code;

use super::heartbeat::{beat, farm, json_beat, peer_of, proto_message, svc, updater};
use super::hosts::{operator_call, world_over};
use super::operator::World;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::prerequisites::tests::{file, group, names};
use crate::prerequisites::{FEATURE, HostPrerequisites, Reload};
use crate::testutil::{id, scratch};

/// The one prerequisite this release knows.
const XCODE: &str = "xcode-command-line-tools";

/// A heartbeat over gRPC of a daemon that announces `prerequisites-v1` and carries `report`.
fn heartbeat(report: Option<agent::PrerequisitesReport>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("kbf", "1.0")]);
    message.features.push(FEATURE.to_owned());
    message.prerequisites = report;
    message
}

/// A report that holds `digest`.
fn holding(digest: &str) -> agent::PrerequisitesReport {
    agent::PrerequisitesReport {
        digest: digest.to_owned(),
        ..agent::PrerequisitesReport::default()
    }
}

/// A report that holds `digest` and whose one state is `status`.
fn with_status(digest: &str, status: agent::PrerequisiteStatus) -> agent::PrerequisitesReport {
    agent::PrerequisitesReport {
        states: vec![agent::PrerequisiteState {
            name: XCODE.to_owned(),
            status: status as i32,
            version: "16.0.0.0.1".to_owned(),
            ..agent::PrerequisiteState::default()
        }],
        ..holding(digest)
    }
}

/// Proves: a heartbeat over gRPC that announces `prerequisites-v1` is answered its group's names
/// and their digest until a heartbeat reports that digest, and is sent them again when it reports
/// none (the daemon started again); after the file changes and the tick re-reads it, the next
/// heartbeat is answered the new set, and one that reports the new digest is answered none; a
/// host of a group the file does not list is answered the empty set; a heartbeat that does not
/// announce the feature is answered none, and so is the same daemon over the JSON route, which has
/// no field for it.
///
/// Catches: a set not in the answer, in every answer, left out after a change of the file, sent
/// to a daemon that did not ask or to the JSON route (which is frozen), and sent to a host of
/// another group.
#[tokio::test]
async fn a_grpc_heartbeat_is_answered_the_set_of_its_group_until_the_daemon_reports_it() {
    let dir = scratch("prerequisites-grpc");
    let path = file(dir.path(), &group("daemon", &[XCODE]));
    let held = HostPrerequisites::from_file(&path).expect("the file is read");
    let farm = farm("prerequisites-grpc-farm")
        .await
        .with_host_prerequisites(held.clone());
    let answered = |request: agent::HeartbeatRequest, name: &str, host_group: &str| {
        let coordinator = &farm.coordinator;
        let who = updater(name, host_group);
        async move {
            let answer: agent::HeartbeatResponse = beat(coordinator, &who, &request).await.decode();
            answer.prerequisites
        }
    };

    let said = answered(heartbeat(None), "d1", "daemon")
        .await
        .expect("the set");
    assert_eq!(said.digest, digest(&names(&[XCODE])));
    assert_eq!(said.names, [XCODE]);
    let in_force = heartbeat(Some(holding(&said.digest)));
    assert_eq!(answered(in_force, "d1", "daemon").await, None);
    assert_eq!(
        answered(heartbeat(Some(holding(""))), "d1", "daemon").await,
        Some(said.clone()),
        "a daemon that started again holds none"
    );

    file(dir.path(), &group("daemon", &[]));
    assert_eq!(held.reload(), Reload::Applied);
    let old = heartbeat(Some(holding(&said.digest)));
    let newer = answered(old, "d1", "daemon").await.expect("the new set");
    assert_eq!(
        (newer.names.clone(), newer.digest.clone()),
        (Vec::new(), digest(&[]))
    );
    let in_force = heartbeat(Some(holding(&newer.digest)));
    assert_eq!(answered(in_force, "d1", "daemon").await, None);

    let empty = answered(heartbeat(None), "d2", "web")
        .await
        .expect("the empty set");
    assert_eq!((empty.names, empty.digest), (Vec::new(), digest(&[])));

    let mut plain = heartbeat(None);
    plain.features.retain(|name| name != FEATURE);
    assert_eq!(answered(plain, "d3", "daemon").await, None);

    let json = r#"{"platform":"linux-x86_64","version":"2.0","health":"healthy","agent_features":["heartbeat-v2","prerequisites-v1"]}"#;
    let (code, body) = json_beat(&farm.coordinator, &updater("d4", "daemon"), json).await;
    assert_eq!(code, 200);
    assert!(body.get("prerequisites").is_none(), "{body}");
}

/// Proves: a coordinator started without a host prerequisites file answers a daemon that
/// announces the feature no set, whatever the host's group.
///
/// Catches: the empty set sent to every host of a fleet whose operator never gave a file.
#[tokio::test]
async fn a_coordinator_without_a_file_sends_no_set() {
    let farm = farm("prerequisites-grpc-no-file").await;
    let answer: agent::HeartbeatResponse = beat(
        &farm.coordinator,
        &updater("d1", "daemon"),
        &heartbeat(None),
    )
    .await
    .decode();
    assert_eq!(answer.prerequisites, None);
}

/// What `Hosts.Get` and `Hosts.List` answer of `node`'s prerequisites report; both must say the
/// same.
async fn shown(world: &World, node: &str) -> Option<agent::PrerequisitesReport> {
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
        .prerequisites;
    let (code, _, listed) = operator_call::<proto::ListHostsResponse>(
        world,
        "/hostkeeper.operator.v1.Hosts/List",
        proto::ListHostsRequest::default(),
    )
    .await;
    assert_eq!(code, Code::Ok);
    let listed: Vec<Option<agent::PrerequisitesReport>> = listed
        .expect("a reply")
        .hosts
        .into_iter()
        .filter(|host| host.node_id == node)
        .map(|host| host.heartbeat.expect("its heartbeat").prerequisites)
        .collect();
    assert_eq!(
        listed,
        std::slice::from_ref(&got),
        "List and Get say the same"
    );
    got
}

/// Every `prerequisite_failed` alert of `Alerts.List`: its host, severity and message.
async fn failed_alerts(world: &World) -> Vec<(String, proto::AlertSeverity, String)> {
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
        .filter(|alert| alert.kind() == proto::AlertKind::PrerequisiteFailed)
        .map(|alert| (alert.host_id.clone(), alert.severity(), alert.message))
        .collect()
}

/// Proves: the report a gRPC heartbeat carries is what `Hosts.Get` and `Hosts.List` answer in the
/// host's heartbeat, field for field (the digest, the refusal, each state); a later heartbeat
/// replaces it, one that carries none shows the empty report of a daemon that announced the
/// feature and holds nothing, one that breaks a rule leaves the last shown, a heartbeat of a
/// daemon that no longer announces the feature shows none, and a host that announces it over the
/// JSON route, which carries no report, shows none.
///
/// Catches: a field lost between the heartbeat handler and the operator's answer, a bad report
/// shown, and a report kept for a host after its daemon stopped announcing the feature.
#[tokio::test]
async fn hosts_answer_the_last_prerequisites_report_a_grpc_heartbeat_carried() {
    let enrollment = EnrollmentRig::new("grpc-prerequisites-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-prerequisites", &enrollment);
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
    let refused = agent::PrerequisitesReport {
        refused: Some(agent::PrerequisitesRefusal {
            digest: "b".repeat(64),
            reason: "prerequisite 2 is prerequisite 1 again".to_owned(),
        }),
        ..with_status(&"a".repeat(64), agent::PrerequisiteStatus::Present)
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
        Some(agent::PrerequisitesReport::default())
    );

    let mut plain = heartbeat(Some(holding(&"c".repeat(64))));
    plain.features.retain(|name| name != FEATURE);
    send(plain).await;
    assert_eq!(shown(&world, "s1").await, None);

    // A daemon that announces the feature over the JSON route carries no report there.
    let other = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-2", "s2");
    let json = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy","agent_features":["heartbeat-v2","prerequisites-v1"]}"#;
    let (code, _) = json_beat(&world.coordinator, &peer_of(&other), json).await;
    assert_eq!(code, 200);
    let announced = world.coordinator.lock_fleet().announced(&id("s2"), FEATURE);
    assert!(announced, "the host's features are the ones it announced");
    assert_eq!(shown(&world, "s2").await, None);
}

/// Proves: `Alerts.List` holds one warning `prerequisite_failed` alert of a host exactly while
/// the host's last gRPC heartbeat reports a failed state, naming the host, the prerequisite, the
/// failures in a row, the next try and the reason; a report with the state installing or present
/// raises none, and a later report without the failure takes the alert away.
///
/// Catches: an alert for a prerequisite that is present or being installed, an alert that names
/// nothing of the failure, and one that stands after the daemon reported success.
#[tokio::test]
async fn alerts_hold_a_failed_prerequisite_while_the_last_report_says_failed() {
    let enrollment = EnrollmentRig::new("grpc-prerequisite-alert-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-prerequisite-alert", &enrollment);
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
    let digest = "a".repeat(64);

    send(heartbeat(Some(with_status(
        &digest,
        agent::PrerequisiteStatus::Installing,
    ))))
    .await;
    assert_eq!(failed_alerts(&world).await, []);

    let mut failed = with_status(&digest, agent::PrerequisiteStatus::Failed);
    failed.states[0].failures = 2;
    failed.states[0].next_try_unix = 1_800_003_600;
    failed.states[0].reason = "softwareupdate --install exited 1".to_owned();
    send(heartbeat(Some(failed))).await;
    assert_eq!(
        failed_alerts(&world).await,
        [(
            "s1".to_owned(),
            proto::AlertSeverity::Warning,
            "s1 cannot make sure of its host prerequisites (xcode-command-line-tools failed 2 in \
             a row, next try at unix 1800003600: softwareupdate --install exited 1); its daemon \
             tries again by itself, and until it succeeds the host lacks what its host group \
             needs"
                .to_owned(),
        )]
    );

    send(heartbeat(Some(with_status(
        &digest,
        agent::PrerequisiteStatus::Present,
    ))))
    .await;
    assert_eq!(failed_alerts(&world).await, []);
}
