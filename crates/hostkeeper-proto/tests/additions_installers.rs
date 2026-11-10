//! What build item A3 of `docs/design/packages.md` adds to the wire (section 9.4), new since the
//! committed baseline (`proto/compat/baseline.binpb`, the release before):
//!
//! - `HeartbeatRequest.installers` (10) and `ServiceBeat.installer` (13): the installers a daemon
//!   lists and the one each service runs from;
//! - `target` on `Update`, `Install` and `ForceRollback` (5) and on `Force` (6): the entry of a
//!   release picked for the host, a `hostkeeper.release.v1.Target`;
//! - `Host.installers` (21), the `GateReason` values `NO_TARGET` (31), `NO_BUILD_FOR_HOST` (32)
//!   and `INSTALLER_DROPPED` (33), `HostCounts.picked`, `no_target`, `no_build_for_host` and
//!   `installer_dropped` (6 to 9), and `Convergence.installer_dropped` (10).
//!
//! - `ServiceBeat.drift` (14), a `DriftReport` (build item A7 of
//!   `docs/design/packages-repositories.md`): the version a native package was found at and the
//!   version recorded for it.
//!
//! The numbers 8 and 19 the design first named were taken by the trusted root's fields before
//! A3; these are the next free ones. A reader of the release before keeps each as a field it does
//! not know and writes it back byte for byte; this build reads what that release wrote with each
//! empty. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind, MessageDescriptor, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use hostkeeper_proto::hostkeeper::release::v1 as release;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set this build serves.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

/// The message `name` of `pool`.
fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// The entry of a release, with every field set.
fn entry() -> release::Target {
    release::Target {
        installer: "deb".to_owned(),
        locations: vec![release::Location {
            store: "github".to_owned(),
            locator: "example-org/web/releases/download/v2.0/web.deb".to_owned(),
        }],
        digest: "a".repeat(64),
        bytes: 48_812_032,
        attestation: Some(release::Blob {
            digest: "e".repeat(64),
            bytes: 7311,
            locations: vec![release::Location {
                store: "github".to_owned(),
                locator: "example-org/web/releases/download/v2.0/web.sigstore.json".to_owned(),
            }],
        }),
        manifest: Some(release::Manifest {
            package: Some("web".to_owned()),
            run: vec!["web.service".to_owned()],
            min_hostkeeper: "0.5.0".to_owned(),
            ..release::Manifest::default()
        }),

        ..release::Target::default()
    }
}

/// Decodes `bytes` with the release before's `descriptor`, checks it kept exactly the fields
/// numbered `unknown` as fields it does not know, and answers what it writes back.
fn older_reads(descriptor: &MessageDescriptor, bytes: &[u8], unknown: &[u32]) -> Vec<u8> {
    let read = DynamicMessage::decode(descriptor.clone(), bytes).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, unknown, "{}", descriptor.full_name());
    read.encode_to_vec()
}

/// Catches: the installers of a heartbeat that a coordinator of the release before refuses,
/// drops, or writes back changed; and a heartbeat of a daemon of the release before read with
/// installers. Proves the tags on the wire: field 10 (`0x52`) and, in a service, field 13
/// (`0x6a`).
#[test]
fn an_older_reader_keeps_a_heartbeats_installers() {
    let older = baseline_pool();
    let beat_d = message(&older, "hostkeeper.agent.v1.HeartbeatRequest");
    let service_d = message(&older, "hostkeeper.agent.v1.ServiceBeat");
    assert!(beat_d.get_field_by_name("installers").is_none());
    assert!(service_d.get_field_by_name("installer").is_none());
    let beat = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxX8664.into(),
        features: vec!["target-v1".to_owned()],
        installers: vec!["hostkeeper-archive".to_owned(), "deb".to_owned()],
        services: vec![agent::ServiceBeat {
            name: "web".to_owned(),
            version: "1.0".to_owned(),
            health: agent::Health::Healthy.into(),
            installer: Some("deb".to_owned()),
            ..agent::ServiceBeat::default()
        }],
        ..agent::HeartbeatRequest::default()
    };
    let bytes = beat.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        beat
    );
    let back = older_reads(&beat_d, &bytes, &[10]);
    assert_eq!(back, bytes);
    let service_bytes = beat.services[0].encode_to_vec();
    assert_eq!(
        older_reads(&service_d, &service_bytes, &[13]),
        service_bytes
    );
    assert!(bytes.windows(4).any(|w| w == [0x52, 3, b'd', b'e']));
    assert!(service_bytes.windows(3).any(|w| w == [0x6a, 3, b'd']));

    let mut written = DynamicMessage::new(beat_d);
    written.set_field_by_name(
        "platform",
        Value::EnumNumber(agent::Platform::LinuxX8664.into()),
    );
    let read =
        agent::HeartbeatRequest::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert!(read.installers.is_empty());
    let mut service = DynamicMessage::new(service_d);
    service.set_field_by_name("name", Value::String("web".to_owned()));
    let read = agent::ServiceBeat::decode(service.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.installer, None, "absent: an archive");
}

/// Catches: a service's drift that a coordinator of the release before refuses, drops, or writes
/// back changed, and a beat of the release before read with a drift. Proves the tag: field 14
/// (`0x72`).
#[test]
fn an_older_reader_keeps_a_services_drift() {
    let older = baseline_pool();
    let service_d = message(&older, "hostkeeper.agent.v1.ServiceBeat");
    assert!(service_d.get_field_by_name("drift").is_none());
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.DriftReport")
            .is_none()
    );
    let service = agent::ServiceBeat {
        name: "web".to_owned(),
        version: "1.0".to_owned(),
        health: agent::Health::Healthy.into(),
        drift: Some(agent::DriftReport {
            found: Some("2:1.0~rc1".to_owned()),
            recorded: None,
        }),
        ..agent::ServiceBeat::default()
    };
    let bytes = service.encode_to_vec();
    assert_eq!(
        agent::ServiceBeat::decode(bytes.as_slice()).expect("decodes"),
        service
    );
    assert_eq!(older_reads(&service_d, &bytes, &[14]), bytes);
    assert!(bytes.contains(&0x72));

    let mut written = DynamicMessage::new(service_d);
    written.set_field_by_name("name", Value::String("web".to_owned()));
    let read = agent::ServiceBeat::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.drift, None, "absent: no drift found");
}

/// Catches: `found` or `recorded` of a drift report given another number or no presence, so that
/// a package that is not installed reads as an empty version.
#[test]
fn a_drift_report_tells_absent_from_empty() {
    let now = current_pool();
    let report = message(&now, "hostkeeper.agent.v1.DriftReport");
    for (field, number) in [("found", 1), ("recorded", 2)] {
        let found = report.get_field_by_name(field).expect("the field");
        assert_eq!(
            (found.number(), found.kind(), found.supports_presence()),
            (number, Kind::String, true),
            "{field}"
        );
    }
}

/// Catches: an order with a target that a daemon of the release before refuses or cannot read
/// past, or reads with its package digest or index digest changed; and an order of the release
/// before read with a target. Proves the tags: field 5 (`0x2a`) on `Update`, `Install` and
/// `ForceRollback`, field 6 (`0x32`) on `Force`.
#[test]
fn an_older_reader_keeps_an_orders_target() {
    let older = baseline_pool();
    let target = Some(entry());
    let (service, version) = ("web".to_owned(), "2.0".to_owned());
    let (package_digest, index_digest) = ("a".repeat(64), String::new());
    let verbs: [(&str, Vec<u8>, u32, u8); 4] = [
        (
            "Update",
            agent::Update {
                service: service.clone(),
                version: version.clone(),
                package_digest: package_digest.clone(),
                index_digest: index_digest.clone(),
                target: target.clone(),
            }
            .encode_to_vec(),
            5,
            0x2a,
        ),
        (
            "Install",
            agent::Install {
                service: service.clone(),
                version: version.clone(),
                package_digest: package_digest.clone(),
                index_digest: index_digest.clone(),
                target: target.clone(),
            }
            .encode_to_vec(),
            5,
            0x2a,
        ),
        (
            "ForceRollback",
            agent::ForceRollback {
                service: service.clone(),
                version: version.clone(),
                package_digest: package_digest.clone(),
                index_digest: index_digest.clone(),
                target: target.clone(),
            }
            .encode_to_vec(),
            5,
            0x2a,
        ),
        (
            "Force",
            agent::Force {
                service: service.clone(),
                version: version.clone(),
                package_digest: package_digest.clone(),
                halt_generation: 3,
                index_digest,
                target,
            }
            .encode_to_vec(),
            6,
            0x32,
        ),
    ];
    for (name, bytes, number, tag) in verbs {
        let descriptor = message(&older, &format!("hostkeeper.agent.v1.{name}"));
        assert!(descriptor.get_field_by_name("target").is_none(), "{name}");
        assert_eq!(older_reads(&descriptor, &bytes, &[number]), bytes, "{name}");
        let read = DynamicMessage::decode(descriptor.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(
            read.get_field_by_name("package_digest").as_deref(),
            Some(&Value::String(package_digest.clone())),
            "{name}"
        );
        assert!(bytes.contains(&tag), "{name}");

        let mut written = DynamicMessage::new(descriptor);
        written.set_field_by_name("service", Value::String(service.clone()));
        let old = written.encode_to_vec();
        let target = match name {
            "Update" => {
                agent::Update::decode(old.as_slice())
                    .expect("decodes")
                    .target
            }
            "Install" => {
                agent::Install::decode(old.as_slice())
                    .expect("decodes")
                    .target
            }
            "ForceRollback" => {
                agent::ForceRollback::decode(old.as_slice())
                    .expect("decodes")
                    .target
            }
            _ => {
                agent::Force::decode(old.as_slice())
                    .expect("decodes")
                    .target
            }
        };
        assert_eq!(target, None, "{name}");
    }
}

/// Catches: a host's installers, a new gate reason, the counts of a version set with targets or
/// the hosts whose installer was dropped that an operator client of the release before drops or
/// writes back changed, and what that release wrote read with any of them set.
#[test]
fn an_older_reader_keeps_what_operators_are_shown() {
    let older = baseline_pool();
    let host = op::Host {
        node_id: "d1".to_owned(),
        installers: vec!["hostkeeper-archive".to_owned(), "tarball".to_owned()],
        ..op::Host::default()
    };
    let bytes = host.encode_to_vec();
    let host_d = message(&older, "hostkeeper.operator.v1.Host");
    assert!(host_d.get_field_by_name("installers").is_none());
    assert_eq!(older_reads(&host_d, &bytes, &[21]), bytes);
    assert!(bytes.windows(2).any(|w| w == [0xaa, 0x01]), "field 21");

    let reasons = older
        .get_enum_by_name("hostkeeper.operator.v1.GateReason")
        .expect("released");
    let view = message(&older, "hostkeeper.operator.v1.HostService");
    let new_reasons = [
        (op::GateReason::NoTarget, 31),
        (op::GateReason::NoBuildForHost, 32),
        (op::GateReason::InstallerDropped, 33),
    ];
    for (reason, number) in new_reasons {
        assert_eq!(reason as i32, number, "{reason:?}");
        assert!(reasons.get_value(number).is_none(), "{reason:?} is new");
        let shown = op::HostService {
            service: "web".to_owned(),
            gate_reason: reason.into(),
            ..op::HostService::default()
        };
        let bytes = shown.encode_to_vec();
        let read = DynamicMessage::decode(view.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(
            read.get_field_by_name("gate_reason").as_deref(),
            Some(&Value::EnumNumber(number)),
            "{reason:?}"
        );
        assert_eq!(read.encode_to_vec(), bytes, "{reason:?}");
    }
    let now = current_pool();
    let now_reasons = now
        .get_enum_by_name("hostkeeper.operator.v1.GateReason")
        .expect("the reasons");
    // 34 is `INSTALL_ORDER`, which the host groups add (tests/additions_host_groups.rs).
    assert_eq!(now_reasons.values().map(|v| v.number()).max(), Some(34));

    let convergence = op::Convergence {
        desired: Some("2.0".to_owned()),
        hosts: Some(op::HostCounts {
            missing: 1,
            picked: 2,
            no_target: 3,
            no_build_for_host: 4,
            installer_dropped: 1,
            ..op::HostCounts::default()
        }),
        installer_dropped: vec!["d1".to_owned()],
        ..op::Convergence::default()
    };
    let bytes = convergence.encode_to_vec();
    let counts_d = message(&older, "hostkeeper.operator.v1.HostCounts");
    let counts_bytes = convergence.hosts.as_ref().expect("counts").encode_to_vec();
    assert_eq!(
        older_reads(&counts_d, &counts_bytes, &[6, 7, 8, 9]),
        counts_bytes
    );
    let convergence_d = message(&older, "hostkeeper.operator.v1.Convergence");
    assert_eq!(older_reads(&convergence_d, &bytes, &[10]), bytes);

    let mut written = DynamicMessage::new(convergence_d);
    written.set_field_by_name("desired", Value::String("2.0".to_owned()));
    let read = op::Convergence::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert!(read.installer_dropped.is_empty());
    let mut written = DynamicMessage::new(counts_d);
    written.set_field_by_name("missing", Value::U32(1));
    let read = op::HostCounts::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        (read.missing, read.picked, read.no_target),
        (1, 0, 0),
        "a count the release before did not write is 0"
    );
    let mut written = DynamicMessage::new(host_d);
    written.set_field_by_name("node_id", Value::String("d1".to_owned()));
    let read = op::Host::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert!(read.installers.is_empty());
}

/// Catches: a new field given another number, label or type than the design's table and this
/// file's header say, which a peer built apart would read as something else.
#[test]
fn the_new_fields_have_their_numbers_and_types() {
    let now = current_pool();
    let target = Kind::Message(message(&now, "hostkeeper.release.v1.Target"));
    let report = Kind::Message(message(&now, "hostkeeper.agent.v1.DriftReport"));
    let fields: [(&str, &str, u32, Kind, bool); 13] = [
        (
            "hostkeeper.agent.v1.ServiceBeat",
            "drift",
            14,
            report,
            false,
        ),
        (
            "hostkeeper.agent.v1.HeartbeatRequest",
            "installers",
            10,
            Kind::String,
            true,
        ),
        (
            "hostkeeper.agent.v1.ServiceBeat",
            "installer",
            13,
            Kind::String,
            false,
        ),
        (
            "hostkeeper.agent.v1.Update",
            "target",
            5,
            target.clone(),
            false,
        ),
        (
            "hostkeeper.agent.v1.Install",
            "target",
            5,
            target.clone(),
            false,
        ),
        (
            "hostkeeper.agent.v1.ForceRollback",
            "target",
            5,
            target.clone(),
            false,
        ),
        ("hostkeeper.agent.v1.Force", "target", 6, target, false),
        (
            "hostkeeper.operator.v1.Host",
            "installers",
            21,
            Kind::String,
            true,
        ),
        (
            "hostkeeper.operator.v1.HostCounts",
            "picked",
            6,
            Kind::Uint32,
            false,
        ),
        (
            "hostkeeper.operator.v1.HostCounts",
            "no_target",
            7,
            Kind::Uint32,
            false,
        ),
        (
            "hostkeeper.operator.v1.HostCounts",
            "no_build_for_host",
            8,
            Kind::Uint32,
            false,
        ),
        (
            "hostkeeper.operator.v1.HostCounts",
            "installer_dropped",
            9,
            Kind::Uint32,
            false,
        ),
        (
            "hostkeeper.operator.v1.Convergence",
            "installer_dropped",
            10,
            Kind::String,
            true,
        ),
    ];
    for (name, field, number, kind, repeated) in fields {
        let found = message(&now, name)
            .get_field_by_name(field)
            .unwrap_or_else(|| panic!("{name}.{field}"));
        assert_eq!(
            (found.number(), found.kind(), found.is_list()),
            (number, kind, repeated),
            "{name}.{field}"
        );
    }
    let installer = message(&now, "hostkeeper.agent.v1.ServiceBeat")
        .get_field_by_name("installer")
        .expect("the field");
    assert!(installer.supports_presence(), "absent means an archive");
}
