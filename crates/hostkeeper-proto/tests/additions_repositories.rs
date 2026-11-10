//! What the coordinator's package repositories add to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before):
//!
//! - the message `hostkeeper.release.v1.Repository`, the messages `RepositoriesReport`,
//!   `RepositoryRefusal` and `RepositorySet` of the agent API, `HostRepositories`, and the
//!   `Repositories` service of the operator API with its messages and `RepositoriesRefusal`;
//! - `HeartbeatRequest.repositories` (11), `HeartbeatResponse.repositories` (5),
//!   `ServiceEntry.repository` (10), `Host.repositories` (24) and the `ReleasesRefusal` value
//!   `REPOSITORY_UNKNOWN` (28).
//!
//! A reader of the release before keeps each new field as one it does not know and writes it back
//! byte for byte; this build reads what that release wrote with each absent or empty. Those are
//! the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;
use hostkeeper_proto::hostkeeper::release::v1 as release;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The message `name` of `pool`.
fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
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

/// A repository with every field set.
fn repository() -> release::Repository {
    release::Repository {
        name: "komira".to_owned(),
        kind: "apt".to_owned(),
        uri: "https://packages.example.com/apt".to_owned(),
        suites: vec!["testing".to_owned(), "stable".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec!["A".repeat(40)],
        keyring: vec![0x99, 0x01, 0x02, 0xff],
    }
}

/// A registered repository with every field set.
fn registered() -> op::RegisteredRepository {
    op::RegisteredRepository {
        repository: Some(repository()),
        keyring_sha256: "ab".repeat(32),
        generation: 3,
        services: vec!["web".to_owned()],
    }
}

/// A report with every field set.
fn report() -> agent::RepositoriesReport {
    agent::RepositoriesReport {
        generation: 4,
        digest: "cd".repeat(32),
        refused: Some(agent::RepositoryRefusal {
            generation: 5,
            digest: "ef".repeat(32),
            reason: "the keyring lacks a fingerprint".to_owned(),
        }),
    }
}

/// A set with every field set.
fn set() -> agent::RepositorySet {
    agent::RepositorySet {
        generation: 5,
        digest: "ef".repeat(32),
        entries: vec![repository()],
    }
}

/// Every audit entry the operator API answers with is the one `entry.proto` defines.
fn audit() -> op::AuditEntry {
    op::AuditEntry::default()
}

/// One message of each type the repositories added, every field set, by full name.
fn new_messages() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "hostkeeper.release.v1.Repository",
            round_trip(&repository()),
        ),
        (
            "hostkeeper.agent.v1.RepositoriesReport",
            round_trip(&report()),
        ),
        (
            "hostkeeper.agent.v1.RepositoryRefusal",
            round_trip(&report().refused.expect("set")),
        ),
        ("hostkeeper.agent.v1.RepositorySet", round_trip(&set())),
        (
            "hostkeeper.operator.v1.HostRepositories",
            round_trip(&op::HostRepositories {
                named: vec!["komira".to_owned()],
                unregistered: vec!["other".to_owned()],
                digest: "ef".repeat(32),
                generation: 5,
                current: true,
                refused: "the keyring lacks a fingerprint".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.RegisteredRepository",
            round_trip(&registered()),
        ),
        (
            "hostkeeper.operator.v1.ListRepositoriesRequest",
            round_trip(&op::ListRepositoriesRequest {}),
        ),
        (
            "hostkeeper.operator.v1.ListRepositoriesResponse",
            round_trip(&op::ListRepositoriesResponse {
                repositories: vec![registered()],
                generation: 3,
                unregistered: vec!["other".to_owned()],
            }),
        ),
        (
            "hostkeeper.operator.v1.GetRepositoryRequest",
            round_trip(&op::GetRepositoryRequest {
                name: "komira".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.GetRepositoryResponse",
            round_trip(&op::GetRepositoryResponse {
                repository: Some(registered()),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutRepositoryRequest",
            round_trip(&op::PutRepositoryRequest {
                repository: Some(repository()),
                reason: "rotate the key".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.PutRepositoryResponse",
            round_trip(&op::PutRepositoryResponse {
                repository: Some(registered()),
                entry: Some(audit()),
            }),
        ),
        (
            "hostkeeper.operator.v1.RemoveRepositoryRequest",
            round_trip(&op::RemoveRepositoryRequest {
                name: "komira".to_owned(),
                reason: "retired".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.RemoveRepositoryResponse",
            round_trip(&op::RemoveRepositoryResponse {
                generation: 6,
                entry: Some(audit()),
            }),
        ),
    ]
}

/// Catches: a message, enum or service of the repositories that the release before already
/// knows, or that does not read back as written, or that a reader of that release drops: it
/// knows none of them, and a message it reads with no field keeps every byte of the new ones.
#[test]
fn the_repository_messages_are_new_and_an_older_reader_keeps_their_bytes() {
    let older = baseline_pool();
    let blind = message(&older, "hostkeeper.operator.v1.ListHostsRequest");
    assert_eq!(blind.fields().count(), 0);
    let messages = new_messages();
    assert_eq!(messages.len(), 14);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    assert!(
        older
            .get_enum_by_name("hostkeeper.operator.v1.RepositoriesRefusal")
            .is_none()
    );
    assert!(
        older
            .get_service_by_name("hostkeeper.operator.v1.Repositories")
            .is_none()
    );
}

/// Catches: a heartbeat's repositories report that a coordinator of the release before refuses,
/// drops, or writes back changed; and a heartbeat of the release before read with a report.
/// Proves the tag: field 11 (`0x5a`).
#[test]
fn an_older_reader_keeps_a_heartbeats_repositories_report() {
    let older = baseline_pool();
    let beat_d = message(&older, "hostkeeper.agent.v1.HeartbeatRequest");
    assert!(beat_d.get_field_by_name("repositories").is_none());
    let beat = agent::HeartbeatRequest {
        platform: agent::Platform::LinuxX8664.into(),
        features: vec!["repositories-v1".to_owned()],
        repositories: Some(report()),
        ..agent::HeartbeatRequest::default()
    };
    let bytes = beat.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        beat
    );
    assert_eq!(older_reads(&beat_d, &bytes, &[11]), bytes);
    assert!(bytes.contains(&0x5a));

    let mut written = DynamicMessage::new(beat_d);
    written.set_field_by_name(
        "platform",
        Value::EnumNumber(agent::Platform::LinuxX8664.into()),
    );
    let read =
        agent::HeartbeatRequest::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.repositories, None, "absent: nothing reported");
}

/// Catches: a report that tells a daemon holding no set from one that has not reported: a
/// report with nothing refused reads back with `refused` absent, and a refusal of generation 0
/// is written and read back as present.
#[test]
fn a_report_tells_a_refusal_from_none() {
    let none = agent::RepositoriesReport {
        generation: 0,
        digest: String::new(),
        refused: None,
    };
    let read = agent::RepositoriesReport::decode(round_trip(&none).as_slice()).expect("decodes");
    assert_eq!(read.refused, None);
    let zero = agent::RepositoriesReport {
        refused: Some(agent::RepositoryRefusal::default()),
        ..none.clone()
    };
    let bytes = round_trip(&zero);
    assert!(bytes.len() > round_trip(&none).len());
    let read = agent::RepositoriesReport::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(read.refused, Some(agent::RepositoryRefusal::default()));
}

/// Catches: a heartbeat answer's repository set that a daemon of the release before refuses,
/// drops, or writes back changed; and an answer of the release before read with a set. Proves
/// the tag: field 5 (`0x2a`).
#[test]
fn an_older_reader_keeps_an_answers_repository_set() {
    let older = baseline_pool();
    let answer_d = message(&older, "hostkeeper.agent.v1.HeartbeatResponse");
    assert!(answer_d.get_field_by_name("repositories").is_none());
    let answer = agent::HeartbeatResponse {
        repositories: Some(set()),
        ..agent::HeartbeatResponse::default()
    };
    let bytes = answer.encode_to_vec();
    assert_eq!(
        agent::HeartbeatResponse::decode(bytes.as_slice()).expect("decodes"),
        answer
    );
    assert_eq!(older_reads(&answer_d, &bytes, &[5]), bytes);
    assert_eq!(bytes[0], 0x2a);

    let empty = DynamicMessage::new(answer_d);
    let read = agent::HeartbeatResponse::decode(empty.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.repositories, None, "absent: keep what it has");
}

/// Catches: a service entry's repository that a client of the release before refuses, drops or
/// writes back changed, and an entry of that release read with a repository. Proves the tag:
/// field 10 (`0x52`).
#[test]
fn an_older_reader_keeps_a_service_entrys_repository() {
    let older = baseline_pool();
    let entry_d = message(&older, "hostkeeper.operator.v1.ServiceEntry");
    assert!(entry_d.get_field_by_name("repository").is_none());
    let entry = op::ServiceEntry {
        service: "web".to_owned(),
        repository: "komira".to_owned(),
        ..op::ServiceEntry::default()
    };
    let bytes = entry.encode_to_vec();
    assert_eq!(
        op::ServiceEntry::decode(bytes.as_slice()).expect("decodes"),
        entry
    );
    assert_eq!(older_reads(&entry_d, &bytes, &[10]), bytes);
    assert!(bytes.windows(2).any(|w| w == [0x52, 6]));

    let mut written = DynamicMessage::new(entry_d);
    written.set_field_by_name("service", Value::String("web".to_owned()));
    let read = op::ServiceEntry::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.repository, "", "absent: the rollout file's default");
}

/// Catches: a host's repositories that a client of the release before refuses, drops or writes
/// back changed, and a host of that release read with repositories. Proves the tag: field 24
/// (`0xc2 0x01`).
#[test]
fn an_older_reader_keeps_a_hosts_repositories() {
    let older = baseline_pool();
    let host_d = message(&older, "hostkeeper.operator.v1.Host");
    assert!(host_d.get_field_by_name("repositories").is_none());
    let host = op::Host {
        repositories: Some(op::HostRepositories {
            named: vec!["komira".to_owned()],
            ..op::HostRepositories::default()
        }),
        ..op::Host::default()
    };
    let bytes = host.encode_to_vec();
    assert_eq!(op::Host::decode(bytes.as_slice()).expect("decodes"), host);
    assert_eq!(older_reads(&host_d, &bytes, &[24]), bytes);
    assert_eq!(&bytes[..2], [0xc2, 0x01]);

    let empty = DynamicMessage::new(host_d);
    let read = op::Host::decode(empty.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.repositories, None, "absent: not announced");
}

/// Catches: the refusal `REPOSITORY_UNKNOWN` of `SetDesired` renumbered or already known to the
/// release before, whose clients would then read it as a number they have no token for.
#[test]
fn the_releases_refusal_repository_unknown_is_new() {
    let older = baseline_pool();
    let was = older
        .get_enum_by_name("hostkeeper.operator.v1.ReleasesRefusal")
        .expect("released");
    assert!(was.get_value(28).is_none());
    assert!(
        was.values()
            .all(|v| v.name() != "RELEASES_REFUSAL_REPOSITORY_UNKNOWN")
    );
    assert_eq!(op::ReleasesRefusal::RepositoryUnknown as i32, 28);
}
