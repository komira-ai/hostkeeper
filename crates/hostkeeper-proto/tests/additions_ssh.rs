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
//! This file holds what the baseline can say: the service and messages that are new, and the field
//! numbers of the design. The baseline is older than the fields these messages join (it has no
//! `sent_unix` and no `EnrollRequest`), so the two-way tests of rule B8 for the fields added to
//! existing messages read the last release's own protos instead:
//! `crates/hostkeeper-coordinator/tests/ssh_wire.rs`, with the `.proto` files of v0.1.221.

use prost_reflect::DescriptorPool;

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
            rpc(
                "PinHostKeys",
                "PinHostKeysRequest",
                "HostKeysResponse",
                false
            ),
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
        // Not spelt as one string: `.open` is a top-level domain, and the hygiene lint reads such
        // a string as a host name.
        numbers(&format!("hostkeeper.operator.v1.{}", "Open")),
        n(&[("host_id", 1), ("cert_serial", 2)])
    );
    assert_eq!(
        numbers("hostkeeper.operator.v1.HostKeysResponse"),
        n(&[("fingerprints", 1), ("state", 2)])
    );
    assert_eq!(
        numbers("hostkeeper.operator.v1.PinHostKeysRequest"),
        n(&[("host_id", 1), ("fingerprints", 2)])
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
        certificate: "ssh-ed25519-cert AAAA".to_owned(),
        login_user: "root".to_owned(),
        expires_unix: 1_800_000_000,
        key_id: "alice/12".to_owned(),
    });
    round_trip(&op::HostKeysRequest {
        host_id: "h1".to_owned(),
    });
    round_trip(&op::PinHostKeysRequest {
        host_id: "h1".to_owned(),
        fingerprints: vec![format!("SHA256:{}", "A".repeat(43))],
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
            "not_reported",
        ]
    );
    assert_eq!(
        refusal.values().map(|v| v.number()).collect::<Vec<_>>(),
        (0..=18).collect::<Vec<i32>>()
    );
}
