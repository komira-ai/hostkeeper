//! `SetDesiredRequest.targets` and `Releases.GetRelease` (docs/design/packages.md section 3), new
//! since the committed baseline (`proto/compat/baseline.binpb`, the release before): the map of
//! targets is a new field of a shipped message, `GetRelease` a new method after `WaitConverged`
//! with two new messages, and three refusals (`target_invalid`, `release_exists`,
//! `release_unknown`) new values of `ReleasesRefusal`. A reader of the release before keeps what
//! it does not know and writes it back unchanged; this build reads what that release wrote with
//! the new field empty.

use std::collections::BTreeMap;

use prost::Message as _;
use prost_reflect::{Cardinality, DescriptorPool, DynamicMessage, Kind, Value};

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

/// A list of one `hostkeeper-archive` entry with every field set.
fn list(digest: &str) -> release::TargetList {
    release::TargetList {
        entries: vec![release::Target {
            installer: "hostkeeper-archive".to_owned(),
            locations: vec![release::Location {
                store: "https".to_owned(),
                locator: "example.invalid/a.tar".to_owned(),
            }],
            digest: digest.to_owned(),
            bytes: 1234,
            attestation: Some(release::Blob {
                digest: "b".repeat(64),
                bytes: 99,
                locations: Vec::new(),
            }),
            manifest: Some(release::Manifest::default()),
            ..release::Target::default()
        }],
    }
}

/// A set with targets for two platforms, and the fields a set of the release before has.
fn set_with_targets() -> op::SetDesiredRequest {
    op::SetDesiredRequest {
        service: "kbf".to_owned(),
        desired: Some(op::DesiredState {
            version: "2.0".to_owned(),
            ..op::DesiredState::default()
        }),
        if_generation: Some(7),
        write_id: Some("w-1".to_owned()),
        targets: BTreeMap::from([
            ("linux-x86_64".to_owned(), list(&"a".repeat(64))),
            ("macos-arm64".to_owned(), list(&"c".repeat(64))),
        ]),
        ..op::SetDesiredRequest::default()
    }
}

/// Asserts that `field` is a map from string to `hostkeeper.release.v1.TargetList`.
fn assert_map_of_target_lists(field: &prost_reflect::FieldDescriptor) {
    assert!(field.is_map());
    assert_eq!(field.cardinality(), Cardinality::Repeated);
    let Kind::Message(entry) = field.kind() else {
        panic!("a map entry message");
    };
    assert_eq!(entry.map_entry_key_field().kind(), Kind::String);
    match entry.map_entry_value_field().kind() {
        Kind::Message(value) => {
            assert_eq!(value.full_name(), "hostkeeper.release.v1.TargetList")
        }
        other => panic!("{other:?}"),
    }
}

/// Proves: `SetDesiredRequest.targets` is field 8, a map from string to
/// `hostkeeper.release.v1.TargetList`, and the release before has no field 8.
/// Catches: the map taking another number, another key or value type, or a number the release
/// before already gave a meaning.
#[test]
fn set_desired_targets_is_field_8_a_map_of_target_lists() {
    let pool = current_pool();
    let field = pool
        .get_message_by_name("hostkeeper.operator.v1.SetDesiredRequest")
        .expect("the message")
        .get_field_by_name("targets")
        .expect("targets");
    assert_eq!(field.number(), 8);
    assert_map_of_target_lists(&field);

    let older = baseline_pool()
        .get_message_by_name("hostkeeper.operator.v1.SetDesiredRequest")
        .expect("released");
    assert!(older.get_field(8).is_none());
    assert!(older.get_field_by_name("targets").is_none());
}

/// Proves: the request and the response of `GetRelease` have the fields and numbers the design
/// gives (`service` 1, `version` 2; `targets` 1 a map of target lists, `release_index_digest` 2
/// an optional string), and the release before knows neither message.
/// Catches: a renumbered field, `release_index_digest` losing its presence, a message the release
/// before already has being re-meant.
#[test]
fn get_release_messages_have_the_fields_the_design_gives() {
    let pool = current_pool();
    let request = pool
        .get_message_by_name("hostkeeper.operator.v1.GetReleaseRequest")
        .expect("request");
    let fields: Vec<_> = request
        .fields()
        .map(|f| (f.name().to_owned(), f.number(), f.kind()))
        .collect();
    assert_eq!(
        fields,
        vec![
            ("service".to_owned(), 1, Kind::String),
            ("version".to_owned(), 2, Kind::String),
        ]
    );

    let response = pool
        .get_message_by_name("hostkeeper.operator.v1.GetReleaseResponse")
        .expect("response");
    assert_eq!(response.fields().count(), 2);
    let targets = response.get_field(1).expect("field 1");
    assert_eq!(targets.name(), "targets");
    assert_map_of_target_lists(&targets);
    let digest = response.get_field(2).expect("field 2");
    assert_eq!(digest.name(), "release_index_digest");
    assert_eq!(digest.kind(), Kind::String);
    assert!(digest.supports_presence());

    let older = baseline_pool();
    for name in ["GetReleaseRequest", "GetReleaseResponse"] {
        assert!(
            older
                .get_message_by_name(&format!("hostkeeper.operator.v1.{name}"))
                .is_none(),
            "{name}"
        );
    }
}

/// Proves: `GetRelease` is the last method of `Releases`, after every method the release before
/// has, with the request and response above; the release before has no such method.
/// Catches: the method inserted before a shipped one or retyped.
#[test]
fn get_release_is_the_last_method_of_releases() {
    let service = "hostkeeper.operator.v1.Releases";
    let names = |pool: &DescriptorPool| -> Vec<String> {
        pool.get_service_by_name(service)
            .expect("service")
            .methods()
            .map(|m| m.name().to_owned())
            .collect()
    };
    let before = names(&baseline_pool());
    let now = names(&current_pool());
    assert_eq!(now.last().map(String::as_str), Some("GetRelease"));
    assert!(!before.iter().any(|n| n == "GetRelease"));
    assert_eq!(now[..before.len()], before[..]);

    let pool = current_pool();
    let method = pool
        .get_service_by_name(service)
        .expect("service")
        .methods()
        .last()
        .expect("a method");
    assert_eq!(
        method.input().full_name(),
        "hostkeeper.operator.v1.GetReleaseRequest"
    );
    assert_eq!(
        method.output().full_name(),
        "hostkeeper.operator.v1.GetReleaseResponse"
    );
    assert!(!method.is_client_streaming() && !method.is_server_streaming());
}

/// Proves: the refusals of targets are `ReleasesRefusal` 25, 26 and 27 with their names and are
/// the last values; a reader of the release before knows none of the three numbers (it reads them
/// as unknown values) and keeps every value it has under its name and number.
/// Catches: a number the release before already gave a meaning, or a renumbered shipped value.
#[test]
fn the_release_refusals_are_new_values_unknown_to_the_release_before() {
    let older = baseline_pool();
    let refusal = older
        .get_enum_by_name("hostkeeper.operator.v1.ReleasesRefusal")
        .expect("released");
    for (added, number, token) in [
        (
            op::ReleasesRefusal::TargetInvalid,
            25,
            "RELEASES_REFUSAL_TARGET_INVALID",
        ),
        (
            op::ReleasesRefusal::ReleaseExists,
            26,
            "RELEASES_REFUSAL_RELEASE_EXISTS",
        ),
        (
            op::ReleasesRefusal::ReleaseUnknown,
            27,
            "RELEASES_REFUSAL_RELEASE_UNKNOWN",
        ),
    ] {
        assert_eq!(added as i32, number, "{added:?}");
        assert_eq!(added.as_str_name(), token);
        assert!(refusal.get_value(number).is_none(), "{added:?}");
        assert!(refusal.get_value_by_name(token).is_none(), "{added:?}");
    }
    for value in refusal.values() {
        assert_eq!(
            op::ReleasesRefusal::try_from(value.number())
                .expect("a value of the release before")
                .as_str_name(),
            value.name()
        );
    }
    let now = current_pool()
        .get_enum_by_name("hostkeeper.operator.v1.ReleasesRefusal")
        .expect("enum");
    assert_eq!(now.values().map(|v| v.number()).max(), Some(27));
}

/// Proves: a set with targets reads back as written with the generated types, and a
/// `GetReleaseResponse` keeps its targets in the list order set, an absent digest absent and a
/// present one present.
#[test]
fn a_set_with_targets_and_a_get_release_answer_round_trip() {
    let set = set_with_targets();
    let bytes = set.encode_to_vec();
    assert_eq!(
        op::SetDesiredRequest::decode(bytes.as_slice()).expect("decodes"),
        set
    );

    let mut two = list(&"a".repeat(64));
    let mut second = two.entries[0].clone();
    second.installer = "tarball".to_owned();
    two.entries.push(second);
    let answer = op::GetReleaseResponse {
        targets: BTreeMap::from([("linux-arm64".to_owned(), two)]),
        release_index_digest: None,
    };
    let read = op::GetReleaseResponse::decode(answer.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read, answer);
    assert_eq!(read.release_index_digest, None);
    let installers: Vec<_> = read.targets["linux-arm64"]
        .entries
        .iter()
        .map(|e| e.installer.as_str())
        .collect();
    assert_eq!(installers, ["hostkeeper-archive", "tarball"]);

    let indexed = op::GetReleaseResponse {
        release_index_digest: Some("d".repeat(64)),
        ..answer
    };
    let read = op::GetReleaseResponse::decode(indexed.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(read.release_index_digest, Some("d".repeat(64)));
}

/// Proves: a reader built from the release before decodes a set with targets, reads the fields it
/// knows, keeps `targets` as unknown fields, and writes the request back byte for byte.
/// Catches: the old reader losing or reordering the new field, so a proxy built from it would
/// strip the release from a set.
#[test]
fn a_reader_of_the_release_before_keeps_targets_and_writes_them_back_unchanged() {
    let older = baseline_pool();
    let descriptor = older
        .get_message_by_name("hostkeeper.operator.v1.SetDesiredRequest")
        .expect("released");
    let bytes = set_with_targets().encode_to_vec();
    let read = DynamicMessage::decode(descriptor, bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("service").as_deref(),
        Some(&Value::String("kbf".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("if_generation").as_deref(),
        Some(&Value::U64(7))
    );
    assert!(read.unknown_fields().count() > 0, "targets kept unknown");
    assert!(read.unknown_fields().all(|f| f.number() == 8));
    assert_eq!(read.encode_to_vec(), bytes);
}

/// Proves: this build reads a request the release before wrote (no targets) with every field it
/// had unchanged and an empty targets map.
/// Catches: the new field changing how an old request reads, so an old CI's set would be taken
/// for one with a release.
#[test]
fn this_build_reads_a_set_of_the_release_before_with_no_targets() {
    let older = baseline_pool();
    let descriptor = older
        .get_message_by_name("hostkeeper.operator.v1.SetDesiredRequest")
        .expect("released");
    let mut written = DynamicMessage::new(descriptor);
    written.set_field_by_name("service", Value::String("kbf".to_owned()));
    written.set_field_by_name("package_digest", Value::String("e".repeat(64)));
    written.set_field_by_name("if_generation", Value::U64(3));
    let bytes = written.encode_to_vec();

    let read = op::SetDesiredRequest::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(
        read,
        op::SetDesiredRequest {
            service: "kbf".to_owned(),
            package_digest: Some("e".repeat(64)),
            if_generation: Some(3),
            ..op::SetDesiredRequest::default()
        }
    );
    assert!(read.targets.is_empty());
}
