//! What service identity adds to the wire (`docs/design/service-identity.md`), new since the
//! committed baseline (`proto/compat/baseline.binpb`, the release before): `Manifest.identity`
//! (10) and `Manifest.identity_account` (11), `HeartbeatRequest.service_identities` (17) with its
//! `ServiceIdentityReport`, the coordinator's `IssueServiceCertificate` and `ListDeny` RPCs and
//! the `HostIdentity` service of the operator API. A reader of the release before keeps each
//! field as one it does not know and writes it back byte for byte; this build reads what that
//! release wrote with each absent. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::release::v1 as release;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// This crate's descriptors as a reader of the release before would have them: the manifest
/// without its two identity fields (the committed baseline predates the manifest itself).
fn older_manifest_pool() -> DescriptorPool {
    let mut set = prost_types::FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("the set decodes");
    let file = set
        .file
        .iter_mut()
        .find(|f| f.package() == "hostkeeper.release.v1")
        .expect("the release file");
    let manifest = file
        .message_type
        .iter_mut()
        .find(|m| m.name() == "Manifest")
        .expect("Manifest");
    manifest
        .field
        .retain(|f| !["identity", "identity_account"].contains(&f.name()));
    // The synthetic oneofs of the optional fields that stay keep their order; drop those of the
    // two that left (the last two).
    let kept = manifest
        .field
        .iter()
        .filter(|f| f.proto3_optional())
        .count();
    manifest.oneof_decl.truncate(kept);
    DescriptorPool::from_file_descriptor_set(set).expect("the older set is valid")
}

/// The numbers of the fields a dynamic message keeps as unknown, once each, in order.
fn unknown_numbers(read: &DynamicMessage) -> Vec<u32> {
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    kept
}

/// Proves the release before (the baseline, and the manifest without its two fields) knows none of what service identity adds: not the manifest's two
/// fields, not the heartbeat's report, not the two RPCs, not the `HostIdentity` service.
/// Catches: a baseline that moved without this test, which would make the tests below vacuous.
#[test]
fn the_release_before_knows_nothing_of_service_identity() {
    let older = baseline_pool();
    let manifest = older_manifest_pool()
        .get_message_by_name("hostkeeper.release.v1.Manifest")
        .expect("Manifest");
    assert!(manifest.get_field_by_name("identity").is_none());
    assert!(manifest.get_field_by_name("identity_account").is_none());
    let beat = older
        .get_message_by_name("hostkeeper.agent.v1.HeartbeatRequest")
        .expect("HeartbeatRequest");
    assert!(beat.get_field_by_name("service_identities").is_none());
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.ServiceIdentityReport")
            .is_none()
    );
    let coordinator = older
        .get_service_by_name("hostkeeper.agent.v1.Coordinator")
        .expect("Coordinator");
    for method in ["IssueServiceCertificate", "ListDeny"] {
        assert!(
            coordinator.methods().all(|m| m.name() != method),
            "{method}"
        );
    }
    assert!(
        older
            .get_service_by_name("hostkeeper.operator.v1.HostIdentity")
            .is_none()
    );
}

/// Proves a release entry's manifest with the identity fields reads in the release before with
/// the package and the run list as written, keeps both new fields unknown, and writes them back
/// byte for byte.
/// Catches: a manifest a reader of the release before refuses, reads other fields of
/// differently, or loses the identity request from when it passes the entry on.
#[test]
fn an_older_reader_keeps_a_manifests_identity_fields() {
    let manifest_d = older_manifest_pool()
        .get_message_by_name("hostkeeper.release.v1.Manifest")
        .expect("Manifest");
    let manifest = release::Manifest {
        package: Some("js-agent".to_owned()),
        run: vec!["js-agent.service".to_owned()],
        identity: Some(true),
        identity_account: Some("jsagent".to_owned()),
        ..release::Manifest::default()
    };
    let bytes = manifest.encode_to_vec();
    assert_eq!(
        release::Manifest::decode(bytes.as_slice()).expect("decodes"),
        manifest
    );
    let read = DynamicMessage::decode(manifest_d, bytes.as_slice()).expect("decodes");
    assert_eq!(unknown_numbers(&read), [10, 11]);
    assert_eq!(
        read.get_field_by_name("package").as_deref(),
        Some(&Value::String("js-agent".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("run").as_deref(),
        Some(&Value::List(vec![Value::String(
            "js-agent.service".to_owned()
        )]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Proves a heartbeat with identity reports reads in the release before with the platform and
/// the features as written, keeps field 17 unknown, and writes it back byte for byte.
/// Catches: a heartbeat a coordinator of the release before refuses, reads other fields of
/// differently, or writes back changed.
#[test]
fn an_older_coordinator_keeps_a_heartbeats_identity_reports() {
    let older = baseline_pool();
    let beat_d = older
        .get_message_by_name("hostkeeper.agent.v1.HeartbeatRequest")
        .expect("HeartbeatRequest");
    let beat = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxX8664.into(),
        features: vec!["prerequisites-v1".to_owned()],
        service_identities: vec![
            agent::ServiceIdentityReport {
                service: "js-agent".to_owned(),
                not_after_unix: Some(1_900_000_000),
                last_error: None,
            },
            agent::ServiceIdentityReport {
                service: "other".to_owned(),
                not_after_unix: None,
                last_error: Some("identity_not_approved".to_owned()),
            },
        ],
        ..agent::HeartbeatRequest::default()
    };
    let bytes = beat.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        beat
    );
    let read = DynamicMessage::decode(beat_d, bytes.as_slice()).expect("decodes");
    assert_eq!(unknown_numbers(&read), [17]);
    assert_eq!(
        read.get_field_by_name("platform").as_deref(),
        Some(&Value::EnumNumber(agent::Platform::LinuxX8664.into()))
    );
    assert_eq!(
        read.get_field_by_name("features").as_deref(),
        Some(&Value::List(vec![Value::String(
            "prerequisites-v1".to_owned()
        )]))
    );
    assert_eq!(read.encode_to_vec(), bytes, "written back byte for byte");
}

/// Proves a manifest and a heartbeat the release before wrote read here with the new fields
/// absent: no identity asked for, no account, no reports.
/// Catches: a manifest read as asking for an identity, or a heartbeat read with reports no daemon
/// sent.
#[test]
fn the_release_befores_messages_read_without_identity_fields() {
    let older = baseline_pool();
    let mut manifest = DynamicMessage::new(
        older_manifest_pool()
            .get_message_by_name("hostkeeper.release.v1.Manifest")
            .expect("Manifest"),
    );
    manifest.set_field_by_name("package", Value::String("web".to_owned()));
    let read = release::Manifest::decode(manifest.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.identity, None, "absent: no identity");
    assert_eq!(read.identity_account, None, "absent: no account");
    assert_eq!(read.package.as_deref(), Some("web"));

    let mut beat = DynamicMessage::new(
        older
            .get_message_by_name("hostkeeper.agent.v1.HeartbeatRequest")
            .expect("HeartbeatRequest"),
    );
    beat.set_field_by_name(
        "features",
        Value::List(vec![Value::String("facts-v1".to_owned())]),
    );
    let read = agent::HeartbeatRequest::decode(beat.encode_to_vec().as_slice()).expect("decodes");
    assert!(read.service_identities.is_empty(), "absent: no reports");
    assert_eq!(read.features, ["facts-v1"]);
}

/// Proves `identity = false` is told from an absent `identity`, and that an empty account is told
/// from an absent one.
/// Catches: an explicit refusal of identity read as no statement, which a release check could not
/// tell apart.
#[test]
fn a_manifest_tells_identity_false_from_absent() {
    let off = release::Manifest {
        identity: Some(false),
        identity_account: Some(String::new()),
        ..release::Manifest::default()
    };
    assert_eq!(off.encode_to_vec(), [0x50, 0, 0x5a, 0]);
    let read = release::Manifest::decode([0x50u8, 0, 0x5a, 0].as_slice()).expect("decodes");
    assert_eq!(read, off);
    let none = release::Manifest::decode(&[][..]).expect("decodes");
    assert_eq!((none.identity, none.identity_account), (None, None));
}
