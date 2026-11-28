//! The host's hardware health (`host-health-v1`): counted toward the heartbeat's bound as
//! `health`, with a service's health reason, body and restarts beside its other facts; the two
//! groups [`fit`] drops first (the memory devices, source states and filesystems, then the services' health
//! bodies and restarts) and the records it gives up last; a service's reason, body and restarts
//! taken from the observer; and no `host-health-v1` announced and no `health` sent by a daemon
//! that reads no health, whatever the coordinator advertises (`host_health_source` tests one that
//! reads it). Its mapping onto the gRPC request is tested in `proto_map`.

use hostkeeper_link::beat::DROP_ORDER;
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, Feature, Fitted, Health, Heartbeat, Liveness, MAX_HEARTBEAT_BYTES,
    Observation, OptionalFact, Outcome, Oversize, Platform, Reply, ServiceFacts, fit,
};
use hostkeeper_proto::health::{
    MAX_CODE, MAX_EVENT_ID, MAX_HEALTH_BODY, MAX_HEALTH_BYTES, MAX_HEALTH_REASON, MAX_LAST_RESULT,
    MAX_MESSAGE, MAX_RAW, MAX_SOURCE, MAX_SUBJECT, check_health,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use prost::Message as _;
use serde_json::json;

use crate::{Rig, recorded, reply, service, update, verdict, version};

/// A service with a failed verdict whose detail is `detail_len` bytes, and no optional fact.
fn required(name: &str, detail_len: usize) -> ServiceFacts {
    let mut result = verdict(&update("o-1", 1, name, "2.0"), Outcome::Failed);
    result.detail = Some("d".repeat(detail_len));
    ServiceFacts {
        local_halt: None,
        name: service(name),
        version: version("1.0"),
        health: Health::Unhealthy,
        updating: None,
        last_update: Some(result),
        deferred: None,
        health_detail: None,
        observed: None,
        record_try: None,
        installer: None,
        drift: None,
        health_reason: None,
        health_body: None,
        restarts: None,
        deferring: None,
    }
}

/// The restarts of a crash loop.
fn restarts() -> pb::ServiceRestarts {
    pb::ServiceRestarts {
        count: 89,
        recent: 7,
        last_result: "exit-code".to_owned(),
        exit_status: Some(1),
    }
}

/// `facts` with a health reason, a health body and restarts.
fn with_health(mut facts: ServiceFacts) -> ServiceFacts {
    facts.health_reason = Some("health_url_status".to_owned());
    facts.health_body = Some("503 not placeable: memory short".to_owned());
    facts.restarts = Some(restarts());
    facts
}

fn beat(services: Vec<ServiceFacts>, health: Option<pb::HostHealth>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: version("0.9.0"),
        features: ANNOUNCED.to_vec(),
        services,
        sent_unix: 0,
        renewal: hostkeeper_link::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
        remote_login_probe: None,
        full_disk_access: None,
        operator_keys: None,
        ssh: None,
        prerequisites: None,
        bmc: None,
        serial_console: None,
        maintenance: None,
        health,
    }
}

/// Record `n`, critical, with every text at its bound.
fn record(n: usize) -> pb::HardwareEvent {
    let id = format!("sel:{n:04}@");
    pb::HardwareEvent {
        event_id: format!("{id}{}", "0".repeat(MAX_EVENT_ID - id.len())),
        time_unix: 1_795_662_012,
        of_previous_boot: true,
        component: pb::HardwareComponent::Memory.into(),
        subject: "s".repeat(MAX_SUBJECT),
        severity: pb::HardwareSeverity::Critical.into(),
        state: pb::HardwareEventState::Event.into(),
        source: "e".repeat(MAX_SOURCE),
        code: "c".repeat(MAX_CODE),
        message: "m".repeat(MAX_MESSAGE),
        raw: "r".repeat(MAX_RAW),
        about: String::new(),
    }
}

/// Health of `records` records, with a baseline of `devices` memory devices and `sources`
/// source states.
fn health(records: usize, devices: usize, sources: usize) -> pb::HostHealth {
    pb::HostHealth {
        events: (0..records).map(record).collect(),
        baseline: Some(pb::Baseline {
            mem_total_kib: 268_320_876,
            memory_devices: (0..devices)
                .map(|n| pb::MemoryDevice {
                    locator: format!("DIMM {n}"),
                    size_bytes: 16 << 30,
                })
                .collect(),
            cpus_present: 64,
            cpus_online: 64,
            smt: "on".to_owned(),
            disks: Vec::new(),
            nics: Vec::new(),
        }),
        boot_id: "b7c1".to_owned(),
        boot_unix: 1_795_662_060,
        clean_previous_shutdown: false,
        sources: (0..sources)
            .map(|n| pb::SourceState {
                source: format!("source{n}"),
                status: pb::SourceStatus::Unreadable.into(),
                reason: "database is locked".to_owned(),
            })
            .collect(),
        filesystems: Vec::new(),
    }
}

/// `health` with `n` filesystems, the first the root, each 97% used.
fn with_filesystems(mut health: pb::HostHealth, n: usize) -> pb::HostHealth {
    health.filesystems = (0..n)
        .map(|i| pb::FilesystemUse {
            mount: if i == 0 {
                "/".to_owned()
            } else {
                format!("/srv/data{i}")
            },
            size_bytes: 100 << 30,
            available_bytes: 3 << 30,
        })
        .collect();
    health
}

/// `health` without its memory devices, source states and filesystems.
fn without_detail(mut health: pb::HostHealth) -> pb::HostHealth {
    health.sources.clear();
    health.filesystems.clear();
    if let Some(baseline) = &mut health.baseline {
        baseline.memory_devices.clear();
    }
    health
}

/// The bytes of a heartbeat of one service with a verdict detail of `detail_len` bytes and
/// `health`, the service with its health facts when `body` is set.
fn len(detail_len: usize, body: bool, health: Option<pb::HostHealth>) -> usize {
    let facts = required("js-agent", detail_len);
    let facts = if body { with_health(facts) } else { facts };
    beat(vec![facts], health).encoded_len()
}

/// Proves the host's health counts toward the bound as `health`, with the gRPC message's field
/// names and its enums by their wire names, every field present; a heartbeat with none holds no
/// `health`.
#[test]
fn the_health_is_counted_as_json_with_the_names_of_the_message() {
    let mut health = with_filesystems(health(1, 1, 1), 1);
    health.events[0] = pb::HardwareEvent {
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
        raw: "01a4 | Uncorrectable ECC".to_owned(),
        about: String::new(),
    };
    if let Some(baseline) = &mut health.baseline {
        baseline.disks.push(pb::BaselineDisk {
            name: "nvme0n1".to_owned(),
            size_bytes: 1_000,
            serial_digest: 7,
        });
        baseline.nics.push(pb::BaselineNic {
            name: "eno1".to_owned(),
            speed_mbps: 25_000,
        });
    }
    assert_eq!(
        beat(Vec::new(), Some(health)).to_json()["health"],
        json!({
            "events": [{
                "event_id": "sel:01a4@1790000000",
                "time_unix": 1_795_662_012_u64,
                "of_previous_boot": true,
                "component": "HARDWARE_COMPONENT_MEMORY",
                "subject": "DIMM D1",
                "severity": "HARDWARE_SEVERITY_CRITICAL",
                "state": "HARDWARE_EVENT_STATE_EVENT",
                "source": "sel",
                "code": "memory_uncorrectable",
                "message": "Uncorrectable ECC error on DIMM D1",
                "raw": "01a4 | Uncorrectable ECC",
                "about": "",
            }],
            "baseline": {
                "mem_total_kib": 268_320_876_u64,
                "memory_devices": [{ "locator": "DIMM 0", "size_bytes": 17_179_869_184_u64 }],
                "cpus_present": 64,
                "cpus_online": 64,
                "smt": "on",
                "disks": [{ "name": "nvme0n1", "size_bytes": 1000, "serial_digest": 7 }],
                "nics": [{ "name": "eno1", "speed_mbps": 25_000 }],
            },
            "boot_id": "b7c1",
            "boot_unix": 1_795_662_060_u64,
            "clean_previous_shutdown": false,
            "sources": [{
                "source": "source0",
                "status": "SOURCE_STATUS_UNREADABLE",
                "reason": "database is locked",
            }],
            "filesystems": [{
                "mount": "/",
                "size_bytes": 107_374_182_400_u64,
                "available_bytes": 3_221_225_472_u64,
            }],
        })
    );
    assert_eq!(
        beat(Vec::new(), None).to_json().get("health"),
        None,
        "no health, no key"
    );
}

/// Proves a service's health reason, body and restarts count toward the bound as
/// `health_reason`, `health_body` and `restarts`, and a service without them holds none of the
/// three keys, so a heartbeat of a release before them measures as it did.
#[test]
fn a_services_health_facts_are_counted_only_when_present() {
    let facts = with_health(required("js-agent", 0)).to_json();
    assert_eq!(facts["health_reason"], "health_url_status");
    assert_eq!(facts["health_body"], "503 not placeable: memory short");
    assert_eq!(
        facts["restarts"],
        json!({ "count": 89, "recent": 7, "last_result": "exit-code", "exit_status": 1 })
    );
    let bare = required("js-agent", 0).to_json();
    for key in ["health_reason", "health_body", "restarts"] {
        assert_eq!(bare.get(key), None, "{key}");
    }
}

/// Proves the two health groups lead [`DROP_ORDER`], the host's detail first.
#[test]
fn the_health_groups_are_dropped_first() {
    assert_eq!(
        DROP_ORDER[..2],
        [
            OptionalFact::HostHealthDetail,
            OptionalFact::ServiceHealthBody
        ]
    );
}

/// Catches the memory devices, source states and filesystems kept while the services' health
/// facts go, a group dropped when the one before it was enough, or a record given up while a group
/// was left: one byte over the bound with the host health's detail, that detail alone goes (the 8
/// filesystems with it, so the heartbeat says nothing of fullness rather than part of it); one
/// byte over without it, the services' health bodies and restarts go too, and nothing else.
#[test]
fn a_heartbeat_over_the_bound_loses_the_health_detail_then_the_health_bodies() {
    let whole = with_filesystems(health(1, 32, 12), 8);
    let detail_pad = MAX_HEARTBEAT_BYTES + 1 - len(0, true, Some(whole.clone()));
    let over = beat(
        vec![with_health(required("js-agent", detail_pad))],
        Some(whole.clone()),
    );
    assert_eq!(over.encoded_len(), MAX_HEARTBEAT_BYTES + 1);
    assert_eq!(
        fit(over),
        Ok(Fitted {
            beat: beat(
                vec![with_health(required("js-agent", detail_pad))],
                Some(without_detail(whole.clone())),
            ),
            dropped: vec![OptionalFact::HostHealthDetail],
        })
    );

    let body_pad = MAX_HEARTBEAT_BYTES + 1 - len(0, true, Some(without_detail(whole.clone())));
    let fitted = fit(beat(
        vec![with_health(required("js-agent", body_pad))],
        Some(whole.clone()),
    ))
    .expect("it fits");
    assert_eq!(
        fitted.dropped,
        [
            OptionalFact::HostHealthDetail,
            OptionalFact::ServiceHealthBody
        ]
    );
    // The health reason is no part of the group: it stays.
    let mut kept = required("js-agent", body_pad);
    kept.health_reason = Some("health_url_status".to_owned());
    assert_eq!(fitted.beat, beat(vec![kept], Some(without_detail(whole))));
}

/// Catches a health group listed when the heartbeat holds nothing of it: a heartbeat with health
/// but no memory devices or sources, and services with no health body or restarts, over the bound
/// by its health detail loses that, and lists neither health group.
#[test]
fn a_health_group_the_heartbeat_does_not_hold_is_passed_over() {
    let bare = health(1, 0, 0);
    let mut facts = required("js-agent", 0);
    facts.health_detail = Some("h".repeat(MAX_HEARTBEAT_BYTES));
    let fitted = fit(beat(vec![facts], Some(bare.clone()))).expect("it fits");
    assert_eq!(fitted.dropped, [OptionalFact::HealthDetail]);
    assert_eq!(fitted.beat.health, Some(bare));
}

/// Catches a group whose test of what the heartbeat holds reads only one of its parts, and a group
/// that clears only some services: each part alone (memory devices without sources, sources
/// without memory devices or with no baseline at all, filesystems alone, a health body without restarts, restarts without a health body) makes
/// its group dropped and listed, and the services' group clears every service's body and restarts.
#[test]
fn each_part_of_a_health_group_alone_makes_it_dropped() {
    let over = |services: Vec<ServiceFacts>, health: pb::HostHealth| {
        let mut services = services;
        services[0].health_detail = Some("h".repeat(MAX_HEARTBEAT_BYTES));
        fit(beat(services, Some(health))).expect("it fits")
    };
    let groups = |fitted: &Fitted| fitted.dropped[..fitted.dropped.len() - 1].to_vec();

    let mut no_baseline = health(1, 0, 2);
    no_baseline.baseline = None;
    let filesystems_only = with_filesystems(health(1, 0, 0), 1);
    for health in [
        health(1, 2, 0),
        health(1, 0, 2),
        no_baseline,
        filesystems_only,
    ] {
        let fitted = over(vec![required("js-agent", 0)], health.clone());
        assert_eq!(groups(&fitted), [OptionalFact::HostHealthDetail]);
        assert_eq!(fitted.beat.health, Some(without_detail(health)));
    }

    let mut body_only = with_health(required("a", 0));
    body_only.restarts = None;
    let mut restarts_only = with_health(required("b", 0));
    restarts_only.health_body = None;
    for service in [body_only, restarts_only] {
        let fitted = over(vec![service], health(1, 0, 0));
        assert_eq!(groups(&fitted), [OptionalFact::ServiceHealthBody]);
        let kept = &fitted.beat.services[0];
        assert_eq!((&kept.health_body, &kept.restarts), (&None, &None));
    }

    let fitted = over(
        vec![
            with_health(required("a", 0)),
            with_health(required("b", 0)),
            with_health(required("c", 0)),
        ],
        health(1, 0, 0),
    );
    assert_eq!(groups(&fitted), [OptionalFact::ServiceHealthBody]);
    for kept in &fitted.beat.services {
        assert_eq!(
            (&kept.health_body, &kept.restarts),
            (&None, &None),
            "{:?}",
            kept.name
        );
        assert_eq!(kept.health_reason.as_deref(), Some("health_url_status"));
    }
}

/// Catches a record given up before the optional facts, the most severe or oldest record given
/// up first, a record given up when the heartbeat fit without it, and the give-up listed more than
/// once: over the bound by one record, the last record alone goes; over it by two, the last two
/// go, listed once, after every group.
#[test]
fn records_are_given_up_last_from_the_end() {
    let six = health(6, 0, 0);
    let mut five = six.clone();
    five.events.truncate(5);
    let mut four = six.clone();
    four.events.truncate(4);
    let every_group = [
        OptionalFact::HealthDetail,
        OptionalFact::DeferralDetail,
        OptionalFact::ObserverDetail,
        OptionalFact::HealthRecords,
    ];

    let pad = MAX_HEARTBEAT_BYTES - len(0, false, Some(five.clone()));
    let fitted = fit(beat(vec![required("js-agent", pad)], Some(six.clone()))).expect("it fits");
    assert_eq!(fitted.dropped, every_group);
    assert_eq!(
        fitted.beat,
        beat(vec![required("js-agent", pad)], Some(five.clone()))
    );
    assert_eq!(fitted.beat.encoded_len(), MAX_HEARTBEAT_BYTES);

    let pad = MAX_HEARTBEAT_BYTES - len(0, false, Some(four.clone()));
    let fitted = fit(beat(vec![required("js-agent", pad)], Some(six))).expect("it fits");
    assert_eq!(fitted.dropped, every_group);
    assert_eq!(fitted.beat.health, Some(four));
}

/// Catches a heartbeat cut short or sent over the bound when giving up every record is not enough:
/// it is refused with its length once every record is gone, and a heartbeat with no health is
/// refused as before, listing no records.
#[test]
fn a_heartbeat_over_the_bound_with_every_record_given_up_is_refused() {
    let none = health(0, 0, 0);
    let pad = MAX_HEARTBEAT_BYTES + 1 - len(0, false, Some(none));
    assert_eq!(
        fit(beat(vec![required("js-agent", pad)], Some(health(6, 0, 0)))),
        Err(Oversize::Bytes {
            len: MAX_HEARTBEAT_BYTES + 1
        })
    );
    let pad = MAX_HEARTBEAT_BYTES + 1 - len(0, false, None);
    assert_eq!(
        fit(beat(vec![required("js-agent", pad)], None)),
        Err(Oversize::Bytes {
            len: MAX_HEARTBEAT_BYTES + 1
        })
    );
}

/// Proves a heartbeat of 16 services, each with its health reason, body and restarts at their
/// bounds, and a host health of 6 records at every bound (at most its 4 KiB encoded) fits whole,
/// with nothing dropped.
#[test]
fn a_heartbeat_with_every_health_fact_at_its_bound_fits_whole() {
    let full = health(6, 0, 0);
    check_health(&full).expect("within its bounds");
    assert!(full.encoded_len() <= MAX_HEALTH_BYTES);
    let services: Vec<ServiceFacts> = (0..16)
        .map(|n| {
            let mut facts = required(&format!("service-{n:02}"), 0);
            facts.last_update = None;
            facts.health_reason = Some("r".repeat(MAX_HEALTH_REASON));
            facts.health_body = Some("b".repeat(MAX_HEALTH_BODY));
            facts.restarts = Some(pb::ServiceRestarts {
                count: u32::MAX,
                recent: u32::MAX,
                last_result: "l".repeat(MAX_LAST_RESULT),
                exit_status: Some(i32::MIN),
            });
            facts
        })
        .collect();
    let whole = beat(services, Some(full));
    assert!(whole.encoded_len() <= MAX_HEARTBEAT_BYTES);
    assert_eq!(
        fit(whole.clone()),
        Ok(Fitted {
            beat: whole,
            dropped: Vec::new()
        })
    );
}

/// Proves a daemon with no health source announces no `host-health-v1` and sends no health,
/// before any reply and after one that advertises it and names records written, and keeps
/// beating; a service the observer gives no reason, body or restarts of has none in its facts.
///
/// Catches: the name announced, or health sent, by a daemon that does not read its host's health.
#[test]
fn nothing_is_announced_or_sent_whatever_the_coordinator_advertises() {
    let rig = Rig::new("health-none");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(Reply {
        health_written: vec!["sel:01a4@1790000000".to_owned()],
        ..reply(None, &[Feature::HostHealthV1])
    }));
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    let beats = rig.world.link.beats();
    assert_eq!(beats.len(), 3);
    for beat in beats {
        assert_eq!(beat.health, None);
        assert!(!beat.features.contains(&Feature::HostHealthV1));
        for facts in &beat.services {
            assert_eq!(
                (&facts.health_reason, &facts.health_body, &facts.restarts),
                (&None, &None, &None)
            );
        }
    }
}

/// Proves a service's facts carry what the observer saw of it: its detail as `health_reason`
/// when the detail is a reason token, its health body and its restarts; a detail that is not a
/// token (white space in it) is no reason; and a service the observer gave none of them has
/// none. Each beat takes them from that beat's observation, with no feature advertised.
///
/// Catches: the observer's reason, body or restarts left out of the heartbeat (a restart loop
/// the coordinator is never told of), carried to another service, or a detail outside the
/// token grammar sent as a reason the coordinator would refuse.
#[test]
fn the_observers_reason_body_and_restarts_are_the_services_facts() {
    let rig = Rig::new("health-service-facts");
    rig.install(vec![
        recorded("looping", "1.0"),
        recorded("odd", "1.0"),
        recorded("plain", "1.0"),
    ]);
    let looping = Observation {
        liveness: Liveness::Running,
        version: Some(version("1.0")),
        health: Health::Unhealthy,
        health_detail: Some("status_non_2xx".to_owned()),
        drift: None,
        health_body: Some("503 not placeable: memory short".to_owned()),
        restarts: Some(restarts()),
    };
    rig.world.observer.set(&service("looping"), looping.clone());
    rig.world.observer.set(
        &service("odd"),
        Observation {
            health_detail: Some("probe refused".to_owned()),
            health_body: None,
            restarts: None,
            ..looping.clone()
        },
    );
    let mut heartbeater = rig.heartbeater();
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let first = rig.world.link.beats().remove(0);
    let [looping_facts, odd, plain] = &first.services[..] else {
        panic!("three services: {first:?}");
    };
    assert_eq!(
        (looping_facts.name.as_str(), health_of(looping_facts)),
        (
            "looping",
            (
                Some("status_non_2xx"),
                Some("503 not placeable: memory short"),
                Some(&restarts())
            )
        )
    );
    assert_eq!(
        (odd.name.as_str(), health_of(odd)),
        ("odd", (None, None, None))
    );
    assert_eq!(
        (plain.name.as_str(), health_of(plain)),
        ("plain", (None, None, None))
    );

    // The loop recovered: the next beat says so, with no body and the restarts as they are now.
    let recovered = pb::ServiceRestarts {
        recent: 0,
        last_result: "success".to_owned(),
        exit_status: None,
        ..restarts()
    };
    rig.world.observer.set(
        &service("looping"),
        Observation {
            health: Health::Healthy,
            health_detail: None,
            health_body: None,
            restarts: Some(recovered.clone()),
            ..looping
        },
    );
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let second = rig.world.link.beats().remove(1);
    assert_eq!(second.services[0].name.as_str(), "looping");
    assert_eq!(
        health_of(&second.services[0]),
        (None, None, Some(&recovered))
    );
}

/// A service's reason, body and restarts.
fn health_of(facts: &ServiceFacts) -> (Option<&str>, Option<&str>, Option<&pb::ServiceRestarts>) {
    (
        facts.health_reason.as_deref(),
        facts.health_body.as_deref(),
        facts.restarts.as_ref(),
    )
}
