//! What SSH through the coordinator adds to the wire (docs/design/ssh-through-coordinator.md,
//! section 9), new since the committed baseline (`proto/compat/baseline.binpb`, the release
//! before):
//!
//! - the `Ssh` service of the operator API (`operator/v1/ssh.proto`) with its messages and
//!   `SshRefusal`, whose `Tunnel` is a stream and so is not in the command table;
//! - `SshReport` and `SshAccess` of the host API, `HeartbeatRequest.ssh` (13),
//!   `HeartbeatResponse.ssh` (7) and `EnrollRequest.host_key_fingerprints` (6), announced by the
//!   feature `ssh-v1`.
//!
//! A reader of the release before keeps each new field as one it does not know and writes it back
//! byte for byte; this build reads what that release wrote with each absent. Those are the tests
//! of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes")
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

/// What the release before's `name` reads of `bytes`: the numbers of the fields it keeps as
/// unknown, and what it writes back.
fn older_reads(pool: &DescriptorPool, name: &str, bytes: &[u8]) -> (Vec<u32>, Vec<u8>) {
    let descriptor = pool.get_message_by_name(name).expect("the message");
    let read = DynamicMessage::decode(descriptor, bytes).expect("decodes");
    let mut kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    kept.dedup();
    (kept, read.encode_to_vec())
}

fn report() -> agent::SshReport {
    agent::SshReport {
        applied_generation: 7,
        state: "applied".to_owned(),
        host_key_fingerprints: vec![format!("SHA256:{}", "A".repeat(43))],
        detail: "AllowUsers".to_owned(),
    }
}

fn access() -> agent::SshAccess {
    agent::SshAccess {
        generation: 9,
        enabled: true,
        ca_public_keys: vec!["ssh-ed25519 AAAA".to_owned(), "ssh-ed25519 BBBB".to_owned()],
        login_user: "root".to_owned(),
        principal: "hk-host-h1".to_owned(),
        password_auth: "keep".to_owned(),
        revoked_serials: vec![3, 1_000_000_000_000],
    }
}

/// Catches a new message, service or RPC missing, renamed, or present in the release before (so
/// not an addition), and the tunnel losing its stream on either side.
#[test]
fn the_ssh_service_and_the_two_host_messages_are_new_since_the_release_before() {
    let older = baseline_pool();
    let current = current_pool();
    for name in [
        "hostkeeper.operator.v1.Ssh",
        "hostkeeper.agent.v1.SshReport",
        "hostkeeper.agent.v1.SshAccess",
        "hostkeeper.operator.v1.TunnelFrame",
        "hostkeeper.operator.v1.SshRefusal",
    ] {
        assert!(
            older.get_service_by_name(name).is_none()
                && older.get_message_by_name(name).is_none()
                && older.get_enum_by_name(name).is_none(),
            "{name} is new"
        );
    }
    let ssh = current
        .get_service_by_name("hostkeeper.operator.v1.Ssh")
        .expect("the Ssh service");
    let rpcs: Vec<(String, String, String, bool, bool)> = ssh
        .methods()
        .map(|m| {
            (
                m.name().to_owned(),
                m.input().name().to_owned(),
                m.output().name().to_owned(),
                m.is_client_streaming(),
                m.is_server_streaming(),
            )
        })
        .collect();
    let rpc = |n: &str, i: &str, o: &str, stream: bool| {
        (n.to_owned(), i.to_owned(), o.to_owned(), stream, stream)
    };
    assert_eq!(
        rpcs,
        [
            rpc(
                "SignUserCert",
                "SignUserCertRequest",
                "SignUserCertResponse",
                false
            ),
            rpc("HostKeys", "HostKeysRequest", "HostKeysResponse", false),
            rpc("Revoke", "RevokeRequest", "RevokeResponse", false),
            rpc("Tunnel", "TunnelFrame", "TunnelFrame", true),
        ]
    );
}

/// Catches a field renumbered, or a message that does not round trip: the numbers are the design's.
#[test]
fn the_ssh_messages_keep_their_numbers_and_round_trip() {
    let current = current_pool();
    let numbers = |name: &str| -> Vec<(String, u32)> {
        current
            .get_message_by_name(name)
            .expect("the message")
            .fields()
            .map(|f| (f.name().to_owned(), f.number()))
            .collect()
    };
    let n = |pairs: &[(&str, u32)]| -> Vec<(String, u32)> {
        pairs.iter().map(|(a, b)| ((*a).to_owned(), *b)).collect()
    };
    assert_eq!(
        numbers("hostkeeper.agent.v1.SshAccess"),
        n(&[
            ("generation", 1),
            ("enabled", 2),
            ("ca_public_keys", 3),
            ("login_user", 4),
            ("principal", 5),
            ("password_auth", 6),
            ("revoked_serials", 7),
        ])
    );
    assert_eq!(
        numbers("hostkeeper.agent.v1.SshReport"),
        n(&[
            ("applied_generation", 1),
            ("state", 2),
            ("host_key_fingerprints", 3),
            ("detail", 4),
        ])
    );
    assert_eq!(
        numbers("hostkeeper.operator.v1.SignUserCertRequest"),
        n(&[("public_key", 1), ("reason", 2), ("host_id", 3)])
    );
    assert_eq!(
        numbers("hostkeeper.operator.v1.SignUserCertResponse"),
        n(&[
            ("certificate", 1),
            ("login_user", 2),
            ("expires_unix", 3),
            ("key_id", 4),
        ])
    );
    assert_eq!(
        numbers("hostkeeper.operator.v1.TunnelFrame"),
        n(&[("open", 1), ("data", 2), ("close", 3), ("opened", 4)])
    );
    assert_eq!(
        numbers("hostkeeper.operator.v1.Open"),
        n(&[("host_id", 1), ("cert_serial", 2)])
    );
    assert_eq!(
        numbers("hostkeeper.operator.v1.HostKeysResponse"),
        n(&[("fingerprints", 1), ("state", 2)])
    );
    assert_eq!(
        numbers("hostkeeper.operator.v1.RevokeRequest"),
        n(&[("operator", 1), ("serial", 2)])
    );

    round_trip(&report());
    round_trip(&access());
    round_trip(&op::SignUserCertRequest {
        public_key: "ssh-ed25519 AAAA".to_owned(),
        reason: "incident".to_owned(),
        host_id: "h1".to_owned(),
    });
    round_trip(&op::SignUserCertResponse {
        certificate: "ssh-ed25519-cert-v01@openssh.com AAAA".to_owned(),
        login_user: "root".to_owned(),
        expires_unix: 1_800_000_000,
        key_id: "alice/12".to_owned(),
    });
    round_trip(&op::HostKeysRequest {
        host_id: "h1".to_owned(),
    });
    round_trip(&op::HostKeysResponse {
        fingerprints: vec!["SHA256:x".to_owned()],
        state: "pinned".to_owned(),
    });
    round_trip(&op::RevokeRequest {
        operator: String::new(),
        serial: 12,
    });
    round_trip(&op::RevokeResponse { revoked: 2 });
    for frame in [
        op::tunnel_frame::Frame::Open(op::Open {
            host_id: "h1".to_owned(),
            cert_serial: 12,
        }),
        op::tunnel_frame::Frame::Data(vec![0, 1, 255]),
        op::tunnel_frame::Frame::Close(op::Close {
            cause: "eof".to_owned(),
        }),
        op::tunnel_frame::Frame::Opened(op::Opened {
            login_user: "root".to_owned(),
        }),
    ] {
        round_trip(&op::TunnelFrame { frame: Some(frame) });
    }
}

/// Catches a refusal value renamed or renumbered: callers branch on the token, which is the
/// value's name without the prefix.
#[test]
fn the_ssh_refusal_tokens_are_pinned() {
    let current = current_pool();
    let refusal = current
        .get_enum_by_name("hostkeeper.operator.v1.SshRefusal")
        .expect("the enum");
    let tokens: Vec<String> = refusal
        .values()
        .skip(1)
        .map(|v| {
            v.name()
                .trim_start_matches("SSH_REFUSAL_")
                .to_ascii_lowercase()
        })
        .collect();
    assert_eq!(
        tokens,
        [
            "unauthorized",
            "forbidden",
            "not_permitted",
            "invalid_request",
            "bad_reason",
            "bad_key",
            "unknown_host",
            "not_configured",
            "host_not_live",
            "no_address",
            "bad_serial",
            "unknown_cert",
            "too_many_tunnels",
            "draining",
            "dial_failed",
            "store_failed",
            "audit_unavailable",
        ]
    );
    assert_eq!(
        refusal.values().map(|v| v.number()).collect::<Vec<_>>(),
        (0..=17).collect::<Vec<i32>>()
    );
}

/// Catches a heartbeat that carries the new report but loses another field on the way, or a
/// reader of the release before that drops or alters the report: it keeps field 13 and writes the
/// message back byte for byte.
#[test]
fn an_older_reader_keeps_the_ssh_report_in_a_heartbeat() {
    let beat = agent::HeartbeatRequest {
        features: vec!["ssh-v1".to_owned()],
        facts_generation: Some(4),
        ssh: Some(report()),
        ..Default::default()
    };
    let bytes = round_trip(&beat);
    let (kept, written) = older_reads(
        &baseline_pool(),
        "hostkeeper.agent.v1.HeartbeatRequest",
        &bytes,
    );
    assert_eq!(kept, [13]);
    assert_eq!(written, bytes);
}

/// Catches an answer whose new field is dropped by a reader of the release before: it keeps field
/// 7 and writes the message back byte for byte.
#[test]
fn an_older_reader_keeps_the_ssh_access_in_an_answer() {
    let answer = agent::HeartbeatResponse {
        features: vec!["ssh-v1".to_owned()],
        ssh: Some(access()),
        ..Default::default()
    };
    let bytes = round_trip(&answer);
    let (kept, written) = older_reads(
        &baseline_pool(),
        "hostkeeper.agent.v1.HeartbeatResponse",
        &bytes,
    );
    assert_eq!(kept, [7]);
    assert_eq!(written, bytes);
}

/// Catches an enrollment request whose fingerprints a reader of the release before drops: it
/// keeps field 6 and writes the message back byte for byte.
#[test]
fn an_older_reader_keeps_the_host_key_fingerprints_of_an_enrollment() {
    let request = agent::EnrollRequest {
        csr_der: vec![1, 2, 3],
        host_key_fingerprints: vec![
            format!("SHA256:{}", "A".repeat(43)),
            format!("SHA256:{}", "B".repeat(43)),
        ],
        ..Default::default()
    };
    let bytes = round_trip(&request);
    let (kept, written) = older_reads(
        &baseline_pool(),
        "hostkeeper.agent.v1.EnrollRequest",
        &bytes,
    );
    assert_eq!(kept, [6]);
    assert_eq!(written, bytes);
}

/// Catches a new reader that takes what the release before wrote for a new field: a heartbeat, an
/// answer and an enrollment request written with every field of that release and none of the new
/// ones read back with the new ones absent.
#[test]
fn this_build_reads_what_the_release_before_wrote_with_the_ssh_fields_absent() {
    let older = baseline_pool();
    let write = |name: &str, fields: &[(&str, Value)]| -> Vec<u8> {
        let descriptor = older.get_message_by_name(name).expect("the message");
        let mut message = DynamicMessage::new(descriptor);
        for (field, value) in fields {
            message.set_field_by_name(field, value.clone());
        }
        message.encode_to_vec()
    };
    let text = |s: &str| Value::String(s.to_owned());
    let list = |s: &str| Value::List(vec![text(s)]);

    let beat = write(
        "hostkeeper.agent.v1.HeartbeatRequest",
        &[("features", list("facts-v1")), ("facts_generation", Value::U64(3))],
    );
    let beat = agent::HeartbeatRequest::decode(beat.as_slice()).expect("decodes");
    assert_eq!(beat.features, ["facts-v1"]);
    assert_eq!(beat.facts_generation, Some(3));
    assert_eq!(beat.ssh, None);

    let answer = write(
        "hostkeeper.agent.v1.HeartbeatResponse",
        &[("features", list("facts-v1"))],
    );
    let answer = agent::HeartbeatResponse::decode(answer.as_slice()).expect("decodes");
    assert_eq!(answer.features, ["facts-v1"]);
    assert_eq!(answer.ssh, None);

    let enroll = write(
        "hostkeeper.agent.v1.EnrollRequest",
        &[("renewal_of", text("h1"))],
    );
    let enroll = agent::EnrollRequest::decode(enroll.as_slice()).expect("decodes");
    assert_eq!(enroll.renewal_of, "h1");
    assert!(enroll.host_key_fingerprints.is_empty());
}
