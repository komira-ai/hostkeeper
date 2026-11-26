//! `host-health-v1` through the conversions of `proto_map`: the host's health out as `health`,
//! a service's health reason, body and restarts out as their fields, the feature read from a
//! reply's list, and the records a reply names written read into the reply's `health_written`.

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use super::{heartbeat, reply};
use crate::beat::{ANNOUNCED, Feature, Health, Heartbeat, Platform, ServiceFacts};

fn beat(health: Option<pb::HostHealth>, services: Vec<ServiceFacts>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: Version::new("0.9.0").expect("a version"),
        features: ANNOUNCED.to_vec(),
        services,
        sent_unix: 0,
        renewal: crate::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
        remote_login_probe: None,
        full_disk_access: None,
        operator_keys: None,
        prerequisites: None,
        ssh: None,
        bmc: None,
        serial_console: None,
        maintenance: None,
        health,
    }
}

/// A service with the health facts given.
fn service(
    health_reason: Option<&str>,
    health_body: Option<&str>,
    restarts: Option<pb::ServiceRestarts>,
) -> ServiceFacts {
    ServiceFacts {
        name: ServiceName::new("js-agent").expect("a name"),
        version: Version::new("1.0").expect("a version"),
        health: Health::Unhealthy,
        updating: None,
        last_update: None,
        deferred: None,
        health_detail: None,
        observed: None,
        record_try: None,
        installer: None,
        drift: None,
        local_halt: None,
        health_reason: health_reason.map(str::to_owned),
        health_body: health_body.map(str::to_owned),
        restarts,
    }
}

/// The incident's health: the SEL record of DIMM D1, a baseline and two sources.
fn incident() -> pb::HostHealth {
    pb::HostHealth {
        events: vec![pb::HardwareEvent {
            event_id: "sel:01a4@1790000000".to_owned(),
            time_unix: 1_795_662_012,
            of_previous_boot: true,
            component: pb::HardwareComponent::Memory.into(),
            subject: "DIMM D1".to_owned(),
            severity: pb::HardwareSeverity::Critical.into(),
            state: pb::HardwareEventState::Event.into(),
            source: "sel".to_owned(),
            code: "memory_uncorrectable".to_owned(),
            message: "Uncorrectable ECC error on DIMM D1".to_owned(),
            raw:
                "01a4 | 11/26/2026 | 03:00:12 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted"
                    .to_owned(),
            about: String::new(),
        }],
        baseline: Some(pb::Baseline {
            mem_total_kib: 251_543_660,
            memory_devices: vec![pb::MemoryDevice {
                locator: "DIMM D1".to_owned(),
                size_bytes: 0,
            }],
            cpus_present: 64,
            cpus_online: 64,
            smt: "on".to_owned(),
            disks: vec![pb::BaselineDisk {
                name: "nvme0n1".to_owned(),
                size_bytes: 1_920_383_410_176,
                serial_digest: 0x0123_4567_89ab_cdef,
            }],
            nics: vec![pb::BaselineNic {
                name: "eno1".to_owned(),
                speed_mbps: 25_000,
            }],
        }),
        boot_id: "b7c1".to_owned(),
        boot_unix: 1_795_662_060,
        clean_previous_shutdown: false,
        sources: vec![
            pb::SourceState {
                source: "sel".to_owned(),
                status: pb::SourceStatus::Ok.into(),
                reason: String::new(),
            },
            pb::SourceState {
                source: "rasdaemon".to_owned(),
                status: pb::SourceStatus::Unreadable.into(),
                reason: "database is locked".to_owned(),
            },
        ],
    }
}

/// Proves the health a heartbeat holds goes out as `health` unchanged, and a heartbeat with none
/// sends none.
#[test]
fn the_health_goes_out_as_its_field() {
    assert_eq!(
        heartbeat(&beat(Some(incident()), Vec::new()))
            .expect("a request")
            .health,
        Some(incident())
    );
    assert_eq!(
        heartbeat(&beat(None, Vec::new()))
            .expect("a request")
            .health,
        None
    );
}

/// Proves a service's health reason, body and restarts go out as `health_reason`, `health_body`
/// and `restarts`, each on its own, and a service with none of them sends none.
#[test]
fn a_services_health_facts_go_out_as_their_fields() {
    let restarts = pb::ServiceRestarts {
        count: 89,
        recent: 7,
        last_result: "exit-code".to_owned(),
        exit_status: Some(1),
    };
    let request = heartbeat(&beat(
        None,
        vec![
            service(
                Some("health_url_status"),
                Some("503 not placeable: memory short"),
                Some(restarts.clone()),
            ),
            service(None, None, None),
            service(Some("unit_failed"), None, None),
        ],
    ))
    .expect("a request");
    let facts: Vec<(Option<&str>, Option<&str>, Option<&pb::ServiceRestarts>)> = request
        .services
        .iter()
        .map(|s| {
            (
                s.health_reason.as_deref(),
                s.health_body.as_deref(),
                s.restarts.as_ref(),
            )
        })
        .collect();
    assert_eq!(
        facts,
        [
            (
                Some("health_url_status"),
                Some("503 not placeable: memory short"),
                Some(&restarts)
            ),
            (None, None, None),
            (Some("unit_failed"), None, None),
        ]
    );
}

/// Proves `host-health-v1` reads back as the feature of that name, in a response's list as by
/// `Feature::parse`, and that it is not one of the features every daemon announces.
#[test]
fn the_feature_reads_back_from_its_name() {
    assert_eq!(
        Feature::parse("host-health-v1"),
        Some(Feature::HostHealthV1)
    );
    assert_eq!(Feature::HostHealthV1.as_str(), "host-health-v1");
    assert_eq!(crate::beat::HOST_HEALTH, Feature::HostHealthV1);
    assert!(!ANNOUNCED.contains(&Feature::HostHealthV1));
    let mapped = reply(pb::HeartbeatResponse {
        features: vec!["host-health-v1".to_owned()],
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(mapped.reply.features, vec![Feature::HostHealthV1]);
}

/// Proves the records a response names written read into the reply's `health_written` in the
/// response's order, and a response of a coordinator that names none (one that predates the
/// field) reads as none.
#[test]
fn the_records_written_read_into_the_reply() {
    let mapped = reply(pb::HeartbeatResponse {
        features: vec!["host-health-v1".to_owned()],
        health_written: vec!["sel:01a4@1790000000".to_owned(), "panic:2b6f".to_owned()],
        ..pb::HeartbeatResponse::default()
    });
    assert_eq!(
        mapped.reply.health_written,
        ["sel:01a4@1790000000", "panic:2b6f"]
    );
    assert_eq!(mapped.dropped, None);
    let older = reply(pb::HeartbeatResponse::default());
    assert!(older.reply.health_written.is_empty());
}
