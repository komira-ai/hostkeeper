//! What "one repository, every platform" adds to the wire (docs/design/packages-repositories.md,
//! section 3.2): `Repository.descriptor` (10) and `base_uri` (11), `RegisteredRepository.builtin`
//! (5) and `platforms` (6), the `Repositories` calls `Discover` and `Add` with their messages,
//! and the `RepositoriesRefusal` values 12 to 15.
//!
//! Rule B8 in both directions, against a reader of the release before: it is this build's
//! descriptor set without the new fields, so it keeps each as a field it does not know and writes
//! it back byte for byte; and this build reads what that release wrote, which has none of them,
//! with each empty.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};
use prost_types::FileDescriptorSet;

use hostkeeper_proto::hostkeeper::operator::v1 as op;
use hostkeeper_proto::hostkeeper::release::v1 as release;

/// This build's descriptor set with the fields numbered at least `from` of the message `name`
/// taken out: what a reader of the release before knows. Answers how many were taken out.
fn before(name: &str, from: i32) -> (DescriptorPool, usize) {
    let mut set =
        FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let mut stripped = 0;
    for file in &mut set.file {
        for message in &mut file.message_type {
            if message.name() == name {
                let had = message.field.len();
                message.field.retain(|f| f.number() < from);
                stripped += had - message.field.len();
            }
        }
    }
    (
        DescriptorPool::from_file_descriptor_set(set).expect("the release before's set builds"),
        stripped,
    )
}

fn repository() -> release::Repository {
    release::Repository {
        name: "komira".to_owned(),
        kind: "apt".to_owned(),
        uri: "https://packages.example.com/apt".to_owned(),
        suites: vec!["testing".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec!["A".repeat(40)],
        keyring: vec![0x99, 1, 2],
        descriptor: br#"{"version":1}"#.to_vec(),
        base_uri: "https://packages.example.com".to_owned(),
        ..release::Repository::default()
    }
}

/// Catches: a reader of the release before that refuses a repository with a descriptor, drops its
/// two new fields, or writes them back changed (the registry it would put again), and this build
/// failing to read the repository that release wrote, which has neither.
#[test]
fn an_older_reader_keeps_a_repositorys_descriptor_and_base_uri() {
    let (older, stripped) = before("Repository", 10);
    assert_eq!(stripped, 2, "exactly fields 10 and 11 are new");
    let descriptor = older
        .get_message_by_name("hostkeeper.release.v1.Repository")
        .expect("Repository");
    let bytes = repository().encode_to_vec();
    let read = DynamicMessage::decode(descriptor.clone(), bytes.as_slice()).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, [10, 11]);
    assert_eq!(read.encode_to_vec(), bytes);

    let mut written = DynamicMessage::new(descriptor);
    written.set_field_by_name("name", prost_reflect::Value::String("komira".to_owned()));
    let read = release::Repository::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert!(read.descriptor.is_empty() && read.base_uri.is_empty());
}

/// Catches: a client of the release before that refuses a registered repository with the two new
/// fields, drops them, or reads the entry of that release as built in.
#[test]
fn an_older_reader_keeps_a_registered_repositorys_builtin_and_platforms() {
    let (older, stripped) = before("RegisteredRepository", 5);
    assert_eq!(stripped, 2);
    let descriptor = older
        .get_message_by_name("hostkeeper.operator.v1.RegisteredRepository")
        .expect("RegisteredRepository");
    let registered = op::RegisteredRepository {
        repository: Some(repository()),
        generation: 2,
        builtin: true,
        platforms: vec!["macos-arm64".to_owned()],
        ..op::RegisteredRepository::default()
    };
    let bytes = registered.encode_to_vec();
    let read = DynamicMessage::decode(descriptor, bytes.as_slice()).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    assert_eq!(kept, [5, 6]);
    assert_eq!(read.encode_to_vec(), bytes);
    let plain = op::RegisteredRepository::decode(
        op::RegisteredRepository::default()
            .encode_to_vec()
            .as_slice(),
    )
    .expect("decodes");
    assert!(!plain.builtin && plain.platforms.is_empty());
}

/// Catches: `Discover` and `Add` renumbered or reusing an existing call, the four refusals
/// numbered other than 12 to 15, or a refusal that shifts an earlier one's number.
#[test]
fn the_calls_and_the_refusals_are_appended() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let service = pool
        .get_service_by_name("hostkeeper.operator.v1.Repositories")
        .expect("service");
    let methods: Vec<String> = service.methods().map(|m| m.name().to_owned()).collect();
    assert_eq!(methods, ["List", "Get", "Put", "Remove", "Discover", "Add"]);
    assert_eq!(op::RepositoriesRefusal::CredentialUnusable as i32, 11);
    assert_eq!(op::RepositoriesRefusal::DiscoveryFailed as i32, 12);
    assert_eq!(op::RepositoriesRefusal::DescriptorInvalid as i32, 13);
    assert_eq!(op::RepositoriesRefusal::DescriptorChanged as i32, 14);
    assert_eq!(op::RepositoriesRefusal::Builtin as i32, 15);
}

/// Catches: the answer of `Discover` or the request of `Add` that does not read back as written.
#[test]
fn the_new_messages_round_trip() {
    let discovery = op::Discovery {
        name: "komira".to_owned(),
        description: "d".to_owned(),
        channels: vec!["testing".to_owned()],
        platforms: vec!["linux-x86_64".to_owned()],
        macos_packages: vec!["hello".to_owned()],
        fingerprints: vec!["A".repeat(40)],
        user_ids: vec!["komira-ai packages archive <packages@example.com>".to_owned()],
        descriptor_sha256: "ab".repeat(32),
        keyring_sha256: "cd".repeat(32),
        pinned: true,
    };
    let answer = op::DiscoverRepositoryResponse {
        discovery: Some(discovery),
    };
    assert_eq!(
        op::DiscoverRepositoryResponse::decode(answer.encode_to_vec().as_slice()).expect("d"),
        answer
    );
    let add = op::AddRepositoryRequest {
        name: "komira".to_owned(),
        base_uri: "https://packages.example.com".to_owned(),
        access: "hosts".to_owned(),
        credential_file: "/etc/cred".to_owned(),
        descriptor_sha256: "ab".repeat(32),
        fingerprints: vec!["A".repeat(40)],
        reason: "why".to_owned(),
    };
    assert_eq!(
        op::AddRepositoryRequest::decode(add.encode_to_vec().as_slice()).expect("d"),
        add
    );
    let discover = op::DiscoverRepositoryRequest {
        base_uri: "https://x.example.com".to_owned(),
        access: String::new(),
        credential_file: String::new(),
    };
    assert_eq!(
        op::DiscoverRepositoryRequest::decode(discover.encode_to_vec().as_slice()).expect("d"),
        discover
    );
    let added = op::AddRepositoryResponse::default();
    assert_eq!(
        op::AddRepositoryResponse::decode(added.encode_to_vec().as_slice()).expect("d"),
        added
    );
}
