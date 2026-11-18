//! The fields SSH through the coordinator adds to existing messages of the host API
//! (docs/design/ssh-through-coordinator.md, section 9) against the release before, v0.1.221, read
//! with that release's own `.proto` files (`tests/fixtures/v0.1.221/proto`, copied unchanged from
//! the tag) and not this crate's: `HeartbeatRequest.ssh` (13), `HeartbeatResponse.ssh` (9) and
//! `EnrollRequest.host_key_fingerprints` (6). Rule B8 asks for both directions: the release before
//! keeps what it does not know and writes it back byte for byte, and this build reads what that
//! release wrote with each new field absent.

use std::path::Path;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use serde_json::{Value, json};

/// The messages of v0.1.221's `hostkeeper.agent.v1` and `hostkeeper.release.v1`.
fn old_pool() -> DescriptorPool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/v0.1.221/proto");
    let mut compiler = protox::Compiler::new([root]).expect("the include root");
    compiler.include_imports(true);
    compiler
        .open_file("hostkeeper/agent/v1/agent.proto")
        .expect("v0.1.221's agent.proto compiles");
    DescriptorPool::decode(compiler.encode_file_descriptor_set().as_slice()).expect("a pool")
}

fn message(pool: &DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name).expect("the message")
}

/// The numbers of the fields v0.1.221's `name` keeps as unknown when it reads `bytes`, and what it
/// writes back.
fn old_reads(name: &str, bytes: &[u8]) -> (Vec<u32>, Vec<u8>) {
    let read = DynamicMessage::decode(message(&old_pool(), name), bytes).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|field| field.number()).collect();
    kept.dedup();
    (kept, read.encode_to_vec())
}

/// What v0.1.221's `name` writes of the JSON `text`.
fn old_writes(name: &str, text: Value) -> Vec<u8> {
    DynamicMessage::deserialize(message(&old_pool(), name), text)
        .expect("v0.1.221's message")
        .encode_to_vec()
}

fn fingerprint(letter: &str) -> String {
    format!("SHA256:{}", letter.repeat(43))
}

/// Proves: a heartbeat with `ssh` (13) is read by v0.1.221 with the report as its one unknown field
/// and written back byte for byte; the report and the fields v0.1.221 knows round trip in this
/// build.
///
/// Catches: a new heartbeat field that a daemon or coordinator of v0.1.221 drops or rejects, which
/// would break a rolling update that mixes the two releases.
#[test]
fn v0_1_221_keeps_the_ssh_report_of_a_heartbeat() {
    let beat = agent::HeartbeatRequest {
        features: vec!["heartbeat-v2".to_owned(), "ssh-v1".to_owned()],
        sent_unix: Some(4),
        ssh: Some(agent::SshReport {
            applied_generation: 7,
            state: "applied".to_owned(),
            host_key_fingerprints: vec![fingerprint("A")],
            detail: "AllowUsers".to_owned(),
        }),
        ..Default::default()
    };
    let bytes = beat.encode_to_vec();
    assert_eq!(
        agent::HeartbeatRequest::decode(bytes.as_slice()).expect("decodes"),
        beat
    );
    let (kept, written) = old_reads("hostkeeper.agent.v1.HeartbeatRequest", &bytes);
    assert_eq!(kept, [13]);
    assert_eq!(written, bytes);
}

/// Proves: an answer with `ssh` (9) is read by v0.1.221 with the access as its one unknown field
/// and written back byte for byte.
///
/// Catches: a new answer field that an agent of v0.1.221 drops or rejects.
#[test]
fn v0_1_221_keeps_the_ssh_access_of_an_answer() {
    let answer = agent::HeartbeatResponse {
        features: vec!["ssh-v1".to_owned()],
        ssh: Some(agent::SshAccess {
            generation: 9,
            enabled: true,
            ca_public_keys: vec!["ssh-ed25519 AAAA".to_owned(), "ssh-ed25519 BBBB".to_owned()],
            login_user: "root".to_owned(),
            principal: "hk-host-h1".to_owned(),
            password_auth: "keep".to_owned(),
            revoked_serials: vec![3, 1_000_000_000_000],
        }),
        ..Default::default()
    };
    let bytes = answer.encode_to_vec();
    assert_eq!(
        agent::HeartbeatResponse::decode(bytes.as_slice()).expect("decodes"),
        answer
    );
    let (kept, written) = old_reads("hostkeeper.agent.v1.HeartbeatResponse", &bytes);
    assert_eq!(kept, [9]);
    assert_eq!(written, bytes);
}

/// Proves: an enrollment request with `host_key_fingerprints` (6) is read by v0.1.221 with them as
/// its one unknown field and written back byte for byte.
///
/// Catches: a new enrollment field that a coordinator of v0.1.221 refuses, which would stop a new
/// host from enrolling against it.
#[test]
fn v0_1_221_keeps_the_host_key_fingerprints_of_an_enrollment() {
    let request = agent::EnrollRequest {
        csr_der: vec![1, 2, 3],
        image: "boot-1".to_owned(),
        host_key_fingerprints: vec![fingerprint("A"), fingerprint("B")],
        ..Default::default()
    };
    let bytes = request.encode_to_vec();
    assert_eq!(
        agent::EnrollRequest::decode(bytes.as_slice()).expect("decodes"),
        request
    );
    let (kept, written) = old_reads("hostkeeper.agent.v1.EnrollRequest", &bytes);
    assert_eq!(kept, [6]);
    assert_eq!(written, bytes);
}

/// Proves: what v0.1.221 writes of a heartbeat, an answer and an enrollment request is read by
/// this build with the new fields absent and every field v0.1.221 sets intact.
///
/// Catches: a new reader that fails on, or invents a value for, a field an older writer leaves
/// out.
#[test]
fn this_build_reads_what_v0_1_221_wrote_with_the_ssh_fields_absent() {
    let beat = old_writes(
        "hostkeeper.agent.v1.HeartbeatRequest",
        json!({"features": ["heartbeat-v2"], "sentUnix": 3, "installers": ["apt"]}),
    );
    let beat = agent::HeartbeatRequest::decode(beat.as_slice()).expect("decodes");
    assert_eq!(beat.features, ["heartbeat-v2"]);
    assert_eq!(beat.sent_unix, Some(3));
    assert_eq!(beat.installers, ["apt"]);
    assert_eq!(beat.ssh, None);

    let answer = old_writes(
        "hostkeeper.agent.v1.HeartbeatResponse",
        json!({"features": ["facts-v1"]}),
    );
    let answer = agent::HeartbeatResponse::decode(answer.as_slice()).expect("decodes");
    assert_eq!(answer.features, ["facts-v1"]);
    assert_eq!(answer.ssh, None);

    let enroll = old_writes(
        "hostkeeper.agent.v1.EnrollRequest",
        json!({"image": "boot-1", "renewalOf": "h1"}),
    );
    let enroll = agent::EnrollRequest::decode(enroll.as_slice()).expect("decodes");
    assert_eq!(
        (enroll.image.as_str(), enroll.renewal_of.as_str()),
        ("boot-1", "h1")
    );
    assert!(enroll.host_key_fingerprints.is_empty());
}
