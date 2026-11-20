//! The maintenance report a host's daemon sends ([`crate::maintenance`]), through
//! `crate::grpc::route` as the listeners serve it, and what `Hosts.Get` and `Hosts.List` show of
//! it. No socket.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use prost::Message;
use serde_json::json;
use tonic::Code;

use super::heartbeat::{HEARTBEAT, beat, farm, grpc_call, json_beat, peer_of, proto_message, svc};
use super::hosts::{operator_call, world_over};
use super::operator::{AUDIT_UNIX, World};
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::http::Surface;
use crate::maintenance::{FEATURE, STALE_AFTER_SECS, text_of};
use crate::testutil::{OPERATOR, id};

/// A heartbeat over gRPC of a daemon that announces `maintenance-v1` and carries `report`.
fn heartbeat(report: Option<agent::MaintenanceReport>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("kbf", "1.0")]);
    message.features.push(FEATURE.to_owned());
    message.maintenance = report;
    message
}

/// A report that names every kind of fact a report holds: the host, an Xcode, a Mac's update
/// facts, due items of two kinds, a look, unparsed lines, the run held and a failed look's reason.
fn full(due: usize, looked: u64) -> agent::MaintenanceReport {
    agent::MaintenanceReport {
        host: Some(agent::MaintenanceHost {
            os_name: "macOS".to_owned(),
            os_version: "26.1".to_owned(),
            os_build: "25B78".to_owned(),
            hardware_serial: "SERIAL0001".to_owned(),
            running_kernel: "25.1.0".to_owned(),
            newest_kernel: String::new(),
            boot_id: "boot-7".to_owned(),
            boot_unix: AUDIT_UNIX - 86_400,
        }),
        xcodes: vec![agent::InstalledXcode {
            version: "27.2".to_owned(),
            build: "27B52".to_owned(),
            path: "/Applications/Xcode-27.2.app".to_owned(),
            selected: true,
        }],
        mac: Some(agent::MacUpdateFacts {
            filevault: agent::FileVaultState::On as i32,
            authrestart_supported: true,
            data_free_bytes: Some(40_000_000_000),
            update_account: Some(agent::UpdateAccountState {
                present: true,
                admin: true,
                secure_token: true,
            }),
            command_line_tools: Some(agent::CommandLineToolsState {
                receipt: true,
                files: true,
                version: "26.1.0".to_owned(),
            }),
            mdm_conflicts: vec!["Deferrals".to_owned()],
        }),
        due: (0..due)
            .map(|n| agent::DueItem {
                kind: agent::MaintenanceKind::Macos as i32,
                from: "26.1".to_owned(),
                to: "26.2".to_owned(),
                needs_reboot: n % 2 == 0,
                label: format!("update-{n}"),
                security: false,
            })
            .collect(),
        due_left_out: 3,
        looked_unix: looked,
        unparsed_lines: 2,
        run: Some(agent::MaintenanceRunState {
            run_id: "run-1".to_owned(),
            order_id: "order-1".to_owned(),
            ..agent::MaintenanceRunState::default()
        }),
        look_failed: "apt: apt-get update did not finish within its deadline".to_owned(),
    }
}

/// What `Hosts.Get` and `Hosts.List` answer of `node`'s maintenance report, its age and whether it
/// is stale; both must say the same.
async fn shown(world: &World, node: &str) -> (Option<agent::MaintenanceReport>, Option<u64>, bool) {
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
    let listed: Vec<proto::Host> = listed
        .expect("a reply")
        .hosts
        .into_iter()
        .filter(|host| host.node_id == node)
        .collect();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (
            &listed[0].maintenance,
            listed[0].maintenance_look_age_secs,
            listed[0].maintenance_stale
        ),
        (
            &got.maintenance,
            got.maintenance_look_age_secs,
            got.maintenance_stale
        ),
        "List and Get say the same"
    );
    (
        got.maintenance,
        got.maintenance_look_age_secs,
        got.maintenance_stale,
    )
}

/// What the status counts of dropped facts, under `maintenance`.
fn dropped(world: &World) -> Option<u64> {
    let status = world.coordinator.status(Some(OPERATOR)).expect("status");
    status.extras_dropped.get("maintenance").copied()
}

/// Proves: the report a gRPC heartbeat carries is what `Hosts.Get` and `Hosts.List` answer in
/// `Host.maintenance`, byte for byte once encoded (the host, the Xcode, the Mac's facts, the due
/// items, the look, the unparsed lines and the run), and a later report replaces it.
///
/// Catches: a field lost between the heartbeat handler, the fleet and the operator's answer, and a
/// report that Get shows and List does not.
#[tokio::test]
async fn hosts_answer_the_report_a_grpc_heartbeat_carried_byte_for_byte() {
    let enrollment = EnrollmentRig::new("grpc-maintenance-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-maintenance", &enrollment);
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
    assert_eq!(shown(&world, "s1").await.0, None);

    let sent = full(2, AUDIT_UNIX - 60);
    send(heartbeat(Some(sent.clone()))).await;
    let (report, _, _) = shown(&world, "s1").await;
    let report = report.expect("the report");
    assert_eq!(report.encode_to_vec(), sent.encode_to_vec());
    assert_eq!(report, sent);

    let newer = full(0, AUDIT_UNIX - 30);
    send(heartbeat(Some(newer.clone()))).await;
    let (report, _, _) = shown(&world, "s1").await;
    assert_eq!(report, Some(newer));
    assert_eq!(dropped(&world), None);
}

/// Proves: a daemon that announces `maintenance-v1` is answered it in `features`, and a daemon
/// that does not announce it, even when its heartbeat carries a report, is answered without it and
/// has no `Host.maintenance`.
///
/// Catches: the feature not advertised, and a report kept for a daemon that did not announce it.
#[tokio::test]
async fn only_a_daemon_that_announces_the_feature_is_agreed_it_and_shown() {
    let enrollment = EnrollmentRig::new("grpc-maintenance-feature-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-maintenance-feature", &enrollment);
    let peer = peer_of(&certificate);

    let mut plain = heartbeat(Some(full(1, AUDIT_UNIX - 60)));
    plain.features.retain(|name| name != FEATURE);
    let answer: agent::HeartbeatResponse = beat(&world.coordinator, &peer, &plain).await.decode();
    assert!(!answer.features.iter().any(|name| name == FEATURE));
    assert_eq!(shown(&world, "s1").await, (None, None, false));

    let announcing = heartbeat(Some(full(1, AUDIT_UNIX - 60)));
    let answer: agent::HeartbeatResponse =
        beat(&world.coordinator, &peer, &announcing).await.decode();
    assert!(answer.features.iter().any(|name| name == FEATURE));
    assert_eq!(shown(&world, "s1").await.0, Some(full(1, AUDIT_UNIX - 60)));

    let answer: agent::HeartbeatResponse = beat(&world.coordinator, &peer, &plain).await.decode();
    assert!(!answer.features.iter().any(|name| name == FEATURE));
    assert_eq!(shown(&world, "s1").await, (None, None, false));
}

/// Proves: a report that breaks a bound (13 due items) is answered like any heartbeat, counted as
/// the dropped fact `maintenance`, and leaves the report shown before; a following heartbeat with
/// no report keeps that report too.
///
/// Catches: a broken report shown, one that refuses the heartbeat, a bad report that goes
/// uncounted, and a heartbeat without a report that clears the last one.
#[tokio::test]
async fn a_report_over_a_bound_is_dropped_and_the_last_one_stays() {
    let enrollment = EnrollmentRig::new("grpc-maintenance-bound-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-maintenance-bound", &enrollment);
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
    let good = full(2, AUDIT_UNIX - 60);

    send(heartbeat(Some(good.clone()))).await;
    assert_eq!(dropped(&world), None);
    send(heartbeat(Some(full(13, AUDIT_UNIX - 30)))).await;
    assert_eq!(shown(&world, "s1").await.0, Some(good.clone()));
    assert_eq!(dropped(&world), Some(1));

    send(heartbeat(None)).await;
    assert_eq!(shown(&world, "s1").await.0, Some(good));
    assert_eq!(dropped(&world), Some(1));
}

/// Proves: a heartbeat over 16 KiB that carries a padded report is refused `OUT_OF_RANGE` with
/// the limit in the message, as every other oversized heartbeat is, and the host is not heard.
///
/// Catches: a report that lifts the heartbeat's size limit.
#[tokio::test]
async fn a_heartbeat_over_16_kib_with_a_padded_report_is_refused() {
    let farm = farm("grpc-maintenance-size").await;
    let message = heartbeat(Some(agent::MaintenanceReport {
        host: Some(agent::MaintenanceHost {
            os_name: "a".repeat(17_000),
            ..agent::MaintenanceHost::default()
        }),
        ..agent::MaintenanceReport::default()
    }));
    let bytes = message.encode_to_vec();
    assert!(bytes.len() > 16 * 1024);
    let reply = grpc_call(
        &farm.coordinator,
        Surface::Heartbeat,
        &super::heartbeat::updater("d1", "daemon"),
        HEARTBEAT,
        None,
        &bytes,
    )
    .await;
    assert_eq!(reply.status.code(), Code::OutOfRange);
    assert!(
        reply.status.message().contains("the limit is: 16384 bytes"),
        "{}",
        reply.status.message()
    );
    assert!(farm.coordinator.lock_fleet().node(&id("d1")).is_none());
}

/// Proves: the JSON route `POST /v1/heartbeat` reads the key `maintenance` (standard base64 of
/// the report) of a daemon that announces `maintenance-v1`, answers the feature, and `Hosts.Get`
/// shows the report; text that is not base64 is counted as the dropped fact `maintenance` and the
/// report shown before stays.
///
/// Catches: a report the JSON route ignores, and a bad text that replaces a good report.
#[tokio::test]
async fn the_json_route_keeps_the_report_of_its_base64_key() {
    let enrollment = EnrollmentRig::new("grpc-maintenance-json-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-maintenance-json", &enrollment);
    let peer = peer_of(&certificate);
    let json = |maintenance: &str| {
        json!({
            "platform": "linux-x86_64",
            "version": "1.0",
            "health": "healthy",
            "agent_features": ["heartbeat-v2", "maintenance-v1"],
            "maintenance": maintenance,
        })
        .to_string()
    };
    let sent = full(2, AUDIT_UNIX - 60);

    let (code, body) = json_beat(&world.coordinator, &peer, &json(&text_of(&sent))).await;
    assert_eq!(code, 200);
    assert_eq!(body["features"], json!(["heartbeat-v2", "maintenance-v1"]));
    assert_eq!(
        shown(&world, "s1").await,
        (Some(sent.clone()), Some(60), false)
    );
    assert_eq!(dropped(&world), None);

    let (code, _) = json_beat(&world.coordinator, &peer, &json("not base64!")).await;
    assert_eq!(code, 200);
    assert_eq!(shown(&world, "s1").await, (Some(sent), Some(60), false));
    assert_eq!(dropped(&world), Some(1));
}

/// Proves: a report looked at 7201 s before the coordinator's Unix time is shown stale with its
/// age, one looked at 60 s before is not stale, one looked at exactly 7200 s before is not stale,
/// and one whose look is 0 is stale with no age.
///
/// Catches: an empty `due` of an old report taken for a host with nothing due, a stale limit off
/// by one, and an age made of a look that was never made.
#[tokio::test]
async fn a_report_is_shown_stale_when_its_look_is_old_or_never() {
    let enrollment = EnrollmentRig::new("grpc-maintenance-stale-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-maintenance-stale", &enrollment);
    let peer = peer_of(&certificate);
    assert_eq!(STALE_AFTER_SECS, 7200);
    for (looked, age, stale) in [
        (AUDIT_UNIX - 7201, Some(7201), true),
        (AUDIT_UNIX - 60, Some(60), false),
        (AUDIT_UNIX - 7200, Some(7200), false),
        (0, None, true),
    ] {
        let sent = full(0, looked);
        let message = heartbeat(Some(sent.clone()));
        assert_eq!(
            beat(&world.coordinator, &peer, &message).await.token(),
            (Code::Ok, "")
        );
        assert_eq!(
            shown(&world, "s1").await,
            (Some(sent), age, stale),
            "look {looked}"
        );
    }
}
