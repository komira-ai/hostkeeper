//! Host health (`host-health-v1`) through `crate::grpc::route` as the heartbeat listener serves
//! it, with no socket: the feature advertised to no daemon, and a heartbeat that carries health
//! and a service's health facts answered as one without them.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use tonic::Code;

use super::heartbeat::{beat, peer_of, proto_message, svc};
use super::hosts::world_over;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::features::{FEATURES, GATED_FIELDS, OFFERED, offered};
use crate::host_health::FEATURE;

/// The incident host's health: its BMC's record of DIMM D1, its capacity and one source.
fn incident() -> agent::HostHealth {
    agent::HostHealth {
        events: vec![agent::HardwareEvent {
            event_id: "sel:01a4@1790000000".to_owned(),
            time_unix: 1_795_662_012,
            of_previous_boot: true,
            component: agent::HardwareComponent::Memory.into(),
            subject: "DIMM D1".to_owned(),
            severity: agent::HardwareSeverity::Critical.into(),
            state: agent::HardwareEventState::Event.into(),
            source: "sel".to_owned(),
            code: "memory_uncorrectable".to_owned(),
            message: "Uncorrectable ECC error on DIMM D1".to_owned(),
            raw: "01a4 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted".to_owned(),
            about: String::new(),
        }],
        baseline: Some(agent::Baseline {
            mem_total_kib: 251_543_660,
            cpus_present: 64,
            cpus_online: 64,
            ..agent::Baseline::default()
        }),
        boot_id: "b7c1".to_owned(),
        boot_unix: 1_795_662_060,
        clean_previous_shutdown: false,
        sources: vec![agent::SourceState {
            source: "sel".to_owned(),
            status: agent::SourceStatus::Ok.into(),
            reason: String::new(),
        }],
        filesystems: Vec::new(),
    }
}

/// Proves the feature is advertised to no daemon: it is not among the features this coordinator
/// advertises, nor offered to one that does not name it, with or without an SSH user CA, and no
/// answer field waits on it.
///
/// Catches: `host-health-v1` advertised before a coordinator writes the records a daemon sends,
/// which a daemon would then drop from its queue unwritten.
#[test]
fn the_feature_is_advertised_by_no_coordinator_yet() {
    assert_eq!(FEATURE, "host-health-v1");
    assert!(!FEATURES.contains(&FEATURE));
    assert!(!OFFERED.contains(&FEATURE));
    assert!(!offered(FEATURE, true));
    assert!(!offered(FEATURE, false));
    assert!(GATED_FIELDS.iter().all(|(_, feature)| *feature != FEATURE));
}

/// The answer to `message`, the first heartbeat of an enrolled host, from a coordinator of its
/// own named `name`: every coordinator of this file starts in the same state.
async fn first_answer(name: &str, message: &agent::HeartbeatRequest) -> agent::HeartbeatResponse {
    let enrollment = EnrollmentRig::new(&format!("{name}-enrollment"));
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over(name, &enrollment);
    let reply = beat(&world.coordinator, &peer_of(&certificate), message).await;
    assert_eq!(reply.token(), (Code::Ok, ""), "{name}");
    reply.decode()
}

/// Proves a gRPC heartbeat of a daemon that announces `host-health-v1` and carries the host's
/// health and a service's health reason, body and restarts is answered, not refused, exactly as
/// the same heartbeat without them is by a coordinator in the same state: the whole answer is the
/// same, `host-health-v1` is not among its features, and it names no record written.
///
/// Catches: a heartbeat of a later daemon refused, or answered differently in any field, for
/// fields this coordinator does not read; and records named written that no log holds.
#[tokio::test]
async fn a_heartbeat_with_health_is_answered_as_one_without() {
    let mut without = proto_message(&[svc("kbf", "1.0")]);
    without.features.push(FEATURE.to_owned());
    let mut with = without.clone();
    with.health = Some(incident());
    let service = &mut with.services[0];
    service.health_reason = Some("health_url_status".to_owned());
    service.health_body = Some("503 not placeable: memory short".to_owned());
    service.restarts = Some(agent::ServiceRestarts {
        count: 89,
        recent: 7,
        last_result: "exit-code".to_owned(),
        exit_status: Some(1),
    });

    let with_health = first_answer("grpc-host-health-with", &with).await;
    let without_health = first_answer("grpc-host-health-without", &without).await;
    assert_eq!(with_health, without_health);
    assert!(!with_health.features.iter().any(|name| name == FEATURE));
    assert!(with_health.health_written.is_empty());
}
