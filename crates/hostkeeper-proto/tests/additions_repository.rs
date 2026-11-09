//! What build item A16 of `docs/design/packages-repositories.md` adds to the wire (sections 2.1
//! and 7): `Target.repository` (7) and `Target.package_version` (8), both `optional string`. An
//! entry with `repository` set is a repository entry; one without is an artifact entry, as every
//! entry was before.
//!
//! Rule B8 in both directions, against two readers of the release before:
//!
//! - the committed baseline (`proto/compat/baseline.binpb`), which predates `release.proto`: it
//!   reads an order and a set carrying a repository entry, keeps the entry as a field it does not
//!   know and writes it back byte for byte;
//! - the `Target` of the release before A16, which is this build's `Target` without fields 7 and
//!   8 (the baseline holds no `Target` to build it from): it keeps the two fields as fields it
//!   does not know and writes them back byte for byte, and its gateway refuses them by name in
//!   JSON, so a coordinator before A16 refuses such a set loudly (section 2.3).
//!
//! The other way, this build reads an entry the release before wrote, which has neither field, as
//! an artifact entry: both absent, not empty.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DeserializeOptions, DynamicMessage, MessageDescriptor, Value};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use hostkeeper_proto::hostkeeper::release::v1 as release;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes")
}

/// The full name of a message of the release package. Built, not written out: a name such as the
/// package's and `Target` reads to the hygiene lint as a host.
fn full(name: &str) -> String {
    format!("hostkeeper.release.v1.{name}")
}

/// The descriptor set of the release before A16: this build's, with fields 7 and 8 of `Target`
/// and their synthetic oneofs taken out, every other message as it is.
fn before_pool() -> DescriptorPool {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let mut stripped = 0;
    for file in &mut set.file {
        if file.name() != "hostkeeper/release/v1/release.proto" {
            continue;
        }
        for message in &mut file.message_type {
            if message.name() == "Target" {
                let before = message.field.len();
                message.field.retain(|f| f.number() < 7);
                stripped += before - message.field.len();
                // The two optional fields' synthetic oneofs are the message's only oneofs.
                message.oneof_decl.clear();
            }
        }
    }
    assert_eq!(
        stripped, 2,
        "this build's Target has exactly fields 7 and 8 past 6"
    );
    DescriptorPool::from_file_descriptor_set(set).expect("the release before's set builds")
}

/// The message `name` of `pool`.
fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// A repository entry as CI sends it (section 1): the installer, the repository, the package
/// version, the pin and a manifest naming the package; no artifact field.
fn repository_entry() -> release::Target {
    release::Target {
        installer: "apt".to_owned(),
        digest: "a".repeat(64),
        manifest: Some(release::Manifest {
            package: Some("tierstore".to_owned()),
            run: vec!["tierstore.service".to_owned()],
            min_hostkeeper: "0.5.0".to_owned(),
            ..release::Manifest::default()
        }),
        repository: Some("komira-testing".to_owned()),
        package_version: Some("1:1.6.0-1".to_owned()),
        ..release::Target::default()
    }
}

/// An artifact entry with every field of the release before set.
fn artifact_entry() -> release::Target {
    let location = |locator: &str| release::Location {
        store: "github".to_owned(),
        locator: locator.to_owned(),
    };
    release::Target {
        installer: "tarball".to_owned(),
        locations: vec![location("example-org/web/releases/download/v2.0/web.tar")],
        digest: "b".repeat(64),
        bytes: 4096,
        attestation: Some(release::Blob {
            digest: "c".repeat(64),
            bytes: 7311,
            locations: vec![location(
                "example-org/web/releases/download/v2.0/web.sigstore.json",
            )],
        }),
        manifest: Some(release::Manifest {
            run: vec!["web.service".to_owned()],
            min_hostkeeper: "0.5.0".to_owned(),
            ..release::Manifest::default()
        }),
        ..release::Target::default()
    }
}

/// Decodes `bytes` with `descriptor`, checks it kept exactly the fields numbered `unknown` as
/// fields it does not know, and answers what it writes back.
fn older_reads(descriptor: &MessageDescriptor, bytes: &[u8], unknown: &[u32]) -> Vec<u8> {
    let read = DynamicMessage::decode(descriptor.clone(), bytes).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, unknown, "{}", descriptor.full_name());
    read.encode_to_vec()
}

/// Catches: either field missing, renumbered, retyped, not `optional` (so an absent repository
/// could not be told from an empty one), named otherwise on the wire or in JSON, or present in
/// the release before; or another field added beside them.
#[test]
fn repository_and_package_version_are_optional_strings_7_and_8_new_since_the_release_before() {
    let current = current_pool();
    let target = message(&current, &full("Target"));
    let added: Vec<(u32, String, String, bool)> = target
        .fields()
        .filter(|f| f.number() > 6)
        .map(|f| {
            assert!(
                matches!(f.kind(), prost_reflect::Kind::String),
                "{}",
                f.name()
            );
            (
                f.number(),
                f.name().to_owned(),
                f.json_name().to_owned(),
                f.field_descriptor_proto().proto3_optional(),
            )
        })
        .collect();
    assert_eq!(
        added,
        [
            (7, "repository".to_owned(), "repository".to_owned(), true),
            (
                8,
                "package_version".to_owned(),
                "packageVersion".to_owned(),
                true
            ),
        ]
    );
    let before = message(&before_pool(), &full("Target"));
    assert_eq!(before.fields().count(), 6);
    assert!(before.get_field_by_name("repository").is_none());
    assert!(
        baseline_pool()
            .get_file_by_name("hostkeeper/release/v1/release.proto")
            .is_none(),
        "the baseline holds no release.proto, so no Target with either field"
    );
}

/// Catches: the two fields' numbers drifting, read off the wire rather than the descriptor:
/// each field alone encodes to its tag, `0x3a` for 7 and `0x42` for 8, and a present empty
/// string is still written.
#[test]
fn the_tags_of_repository_and_package_version_are_pinned_on_the_wire() {
    let repository = release::Target {
        repository: Some("r".to_owned()),
        ..release::Target::default()
    };
    assert_eq!(repository.encode_to_vec(), [0x3a, 1, b'r']);
    let version = release::Target {
        package_version: Some("1".to_owned()),
        ..release::Target::default()
    };
    assert_eq!(version.encode_to_vec(), [0x42, 1, b'1']);
    let empty = release::Target {
        repository: Some(String::new()),
        package_version: Some(String::new()),
        ..release::Target::default()
    };
    assert_eq!(empty.encode_to_vec(), [0x3a, 0, 0x42, 0]);
    assert_eq!(
        release::Target::decode(empty.encode_to_vec().as_slice()).expect("decodes"),
        empty
    );
}

/// Catches: a repository entry that the release before's `Target` reader refuses, drops a field
/// of, or writes back changed: it keeps exactly fields 7 and 8 as fields it does not know, reads
/// every other field as written, and writes the entry back byte for byte.
#[test]
fn the_target_reader_of_the_release_before_keeps_fields_7_and_8_and_writes_them_back() {
    let before = message(&before_pool(), &full("Target"));
    let entry = repository_entry();
    let bytes = entry.encode_to_vec();
    assert_eq!(older_reads(&before, &bytes, &[7, 8]), bytes);

    let read = DynamicMessage::decode(before.clone(), bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("installer").as_deref(),
        Some(&Value::String("apt".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("digest").as_deref(),
        Some(&Value::String("a".repeat(64)))
    );
    // Its own copy holds no location: the coordinator before A16 refuses the set for that
    // (`target_invalid`, section 2.3).
    assert!(!read.has_field_by_name("locations"));

    // A list of such entries, as a set's and a release's targets hold them.
    let list = message(&before_pool(), &full("TargetList"));
    let both = release::TargetList {
        entries: vec![entry, artifact_entry()],
    }
    .encode_to_vec();
    let read = DynamicMessage::decode(list, both.as_slice()).expect("decodes");
    assert_eq!(read.encode_to_vec(), both);
}

/// Catches: a reader of the committed baseline that cannot read past, or changes, an order or a
/// set carrying a repository entry: the entry is inside a field it does not know (`target`, 5,
/// of `Update`; `targets`, 8, of `SetDesiredRequest`), kept and written back byte for byte.
#[test]
fn a_reader_built_from_the_baseline_keeps_a_repository_entry_and_writes_it_back() {
    let older = baseline_pool();
    let update = agent::Update {
        service: "tierstore".to_owned(),
        version: "1.6.0".to_owned(),
        package_digest: "a".repeat(64),
        index_digest: String::new(),
        target: Some(repository_entry()),
    }
    .encode_to_vec();
    let descriptor = message(&older, "hostkeeper.agent.v1.Update");
    assert_eq!(older_reads(&descriptor, &update, &[5]), update);

    let set = op::SetDesiredRequest {
        service: "tierstore".to_owned(),
        desired: Some(op::DesiredState {
            version: "1.6.0".to_owned(),
            ..op::DesiredState::default()
        }),
        targets: [(
            "linux-x86_64".to_owned(),
            release::TargetList {
                entries: vec![repository_entry(), artifact_entry()],
            },
        )]
        .into(),
        ..op::SetDesiredRequest::default()
    }
    .encode_to_vec();
    let descriptor = message(&older, "hostkeeper.operator.v1.SetDesiredRequest");
    assert_eq!(older_reads(&descriptor, &set, &[8]), set);
    // The bytes of fields 7 and 8 are inside what it kept.
    let seven = [&[0x3a, 14][..], b"komira-testing"].concat();
    assert!(set.windows(seven.len()).any(|w| w == seven.as_slice()));
}

/// Catches: the release before's gateway taking a repository entry in JSON and dropping its two
/// keys, which would set an entry with no repository; it refuses each key by name, as a
/// coordinator before A16 answers `invalid_request` (section 2.3). This build's reads both.
#[test]
fn the_json_of_a_repository_entry_is_refused_by_the_release_before_and_read_by_this_build() {
    let entry = serde_json::json!({
        "installer": "apt",
        "digest": "a".repeat(64),
        "repository": "komira-testing",
        "packageVersion": "1.6.0-1",
        "manifest": { "package": "tierstore", "minHostkeeper": "0.5.0" },
    });
    let before = message(&before_pool(), &full("Target"));
    for key in ["repository", "packageVersion"] {
        let mut one = entry.clone();
        let other = if key == "repository" {
            "packageVersion"
        } else {
            "repository"
        };
        one.as_object_mut().expect("an object").remove(other);
        let refused = DynamicMessage::deserialize_with_options(
            before.clone(),
            one,
            &DeserializeOptions::new(),
        )
        .expect_err("the release before refuses an unknown key");
        assert!(refused.to_string().contains(key), "{refused}");
    }

    let current = message(&current_pool(), &full("Target"));
    let read = DynamicMessage::deserialize(current, entry)
        .expect("this build reads it")
        .transcode_to::<release::Target>()
        .expect("a Target");
    assert_eq!(read.repository.as_deref(), Some("komira-testing"));
    assert_eq!(read.package_version.as_deref(), Some("1.6.0-1"));
}

/// Catches: an entry the release before wrote read by this build with an empty repository or
/// package version rather than none, which would make every artifact entry a repository entry:
/// both are absent, every other field is as written, and the entry encodes back to the same
/// bytes. The same in JSON, as the releases file and the `releases` table hold it.
#[test]
fn this_build_reads_an_entry_of_the_release_before_as_an_artifact_entry() {
    let before = message(&before_pool(), &full("Target"));
    let artifact = artifact_entry();
    let mut written = DynamicMessage::new(before.clone());
    written
        .transcode_from(&artifact)
        .expect("the release before writes every field it knows");
    let bytes = written.encode_to_vec();
    let read = release::Target::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(read.repository, None);
    assert_eq!(read.package_version, None);
    assert_eq!(read, artifact);
    assert_eq!(read.encode_to_vec(), bytes);

    let json = serde_json::to_value(&written).expect("the release before writes JSON");
    assert!(json.get("repository").is_none());
    let current = message(&current_pool(), &full("Target"));
    let read = DynamicMessage::deserialize(current, json)
        .expect("this build reads it")
        .transcode_to::<release::Target>()
        .expect("a Target");
    assert_eq!(
        (read.repository.clone(), read.package_version.clone()),
        (None, None)
    );
    assert_eq!(read, artifact);
}
