//! `Ssh` through the router. While the coordinator holds no SSH user CA every method, the stream
//! included, is refused `not_configured` once the caller is admitted as an operator, and a caller
//! with no credential or with CI's token is refused before the service is reached, as for every
//! other operator method. With the CA and the tunnels, `HostKeys`, `PinHostKeys` and `Tunnel`
//! answer over gRPC (and the unary two over the JSON gateway), a heartbeat tells the book where
//! the host was heard from, and a tunnel is answered a stream of frames.

use tonic::Code;

use super::support::{call, grpc_request, served};
use crate::http::Surface;
use crate::testutil::OPERATOR;

/// The path of each method of `Ssh`, `Tunnel` (which the command table leaves out) among them.
const PATHS: [&str; 5] = [
    "/hostkeeper.operator.v1.Ssh/SignUserCert",
    "/hostkeeper.operator.v1.Ssh/HostKeys",
    "/hostkeeper.operator.v1.Ssh/PinHostKeys",
    "/hostkeeper.operator.v1.Ssh/Revoke",
    "/hostkeeper.operator.v1.Ssh/Tunnel",
];

/// Proves: with the operator's token each method of `Ssh` answers `FAILED_PRECONDITION` with the
/// message `not_configured` and no message body, so nothing is signed or dialled before the CA and
/// the tunnel exist.
///
/// Catches: a method of `Ssh` that answers `UNIMPLEMENTED` (not routed) or succeeds with an empty
/// certificate.
#[tokio::test]
async fn every_method_is_refused_not_configured_for_an_operator() {
    let served = served("grpc-ssh-not-configured");
    for path in PATHS {
        let request = grpc_request(path, (), Some(OPERATOR));
        let answer = call(&served.coordinator, Surface::Control, &None, request).await;
        assert_eq!(
            answer,
            (Code::FailedPrecondition, "not_configured".to_owned(), None),
            "{path}"
        );
    }
}

/// Proves: with no token or a token that is no one's `Ssh` answers `UNAUTHENTICATED`
/// `unauthorized`, and on the heartbeat listener `UNIMPLEMENTED` with no message: the service is
/// the operator API's and is served on the private listener only. (CI's token is refused every
/// operator method but four, `credentials::every_operator_method_admits_exactly_its_credential`,
/// which lists `Ssh` among them.)
///
/// Catches: a method of `Ssh` that a caller with no credential could call, or one served on the
/// heartbeat listener.
#[tokio::test]
async fn ssh_takes_the_operators_credential_on_the_private_listener_only() {
    let served = served("grpc-ssh-credentials");
    for path in PATHS {
        for bearer in [None, Some("not-a-token")] {
            let request = grpc_request(path, (), bearer);
            let answer = call(&served.coordinator, Surface::Control, &None, request).await;
            let unauthorized = (Code::Unauthenticated, "unauthorized".to_owned(), None);
            assert_eq!(answer, unauthorized, "{path}");
        }
        let request = grpc_request(path, (), Some(OPERATOR));
        let answer = call(&served.coordinator, Surface::Heartbeat, &None, request).await;
        let unserved = (Code::Unimplemented, String::new(), None);
        assert_eq!(answer, unserved, "{path}");
    }
}

/// Proves: with the CA configured, `SignUserCert` and `Revoke` answer over gRPC and over the JSON
/// gateway alike: a listed operator's key is signed (the key id `operator/<serial>`, a certificate
/// line of the key's certificate type), a refusal is the status and token of its value, a
/// revocation says how many certificates it revoked, and `HostKeys`, `PinHostKeys` and `Tunnel`
/// still say `not_configured` when the coordinator holds no tunnels.
///
/// Catches: a method that answers on one wire only, and a refusal that changes its token or its
/// code on the way out.
#[tokio::test]
async fn signing_and_revoking_answer_over_both_wires_and_refuse_by_token() {
    use hostkeeper_proto::hostkeeper::operator::v1 as proto;

    use super::rollouts::{WIRES, rpc};
    use crate::ssh_cert::tests::ED25519_KEY;
    use crate::ssh_sign::tests::{Tune, world};

    const SIGN: &str = "/hostkeeper.operator.v1.Ssh/SignUserCert";
    const REVOKE: &str = "/hostkeeper.operator.v1.Ssh/Revoke";
    let world = world(
        "grpc-ssh-signing",
        Some("[[operator]]\nname = \"operator\"\nclasses = [\"fleet\"]\n"),
        Tune::default(),
    );
    let coordinator = std::sync::Arc::new(world.coordinator);
    let ask = |host: &str, key: &str| proto::SignUserCertRequest {
        public_key: key.to_owned(),
        reason: String::new(),
        host_id: host.to_owned(),
    };
    for (n, wire) in WIRES.into_iter().enumerate() {
        let signed: proto::SignUserCertResponse = rpc(
            &coordinator,
            wire,
            SIGN,
            Some(OPERATOR),
            &ask("node-07", ED25519_KEY),
        )
        .await
        .expect("signed");
        assert_eq!(signed.key_id, format!("operator/{}", n + 1), "{wire:?}");
        assert!(
            signed.certificate.starts_with(concat!(
                "ssh-ed25519-cert-v01",
                "\u{40}",
                "openssh.com",
                " "
            )),
            "{wire:?}"
        );
        let bad: Result<proto::SignUserCertResponse, _> = rpc(
            &coordinator,
            wire,
            SIGN,
            Some(OPERATOR),
            &ask("node-07", "nope"),
        )
        .await;
        assert_eq!(
            bad.err(),
            Some((Code::InvalidArgument, "bad_key".to_owned())),
            "{wire:?}"
        );
        let unknown: Result<proto::SignUserCertResponse, _> = rpc(
            &coordinator,
            wire,
            SIGN,
            Some(OPERATOR),
            &ask("node-99", ED25519_KEY),
        )
        .await;
        assert_eq!(
            unknown.err(),
            Some((Code::NotFound, "unknown_host".to_owned())),
            "{wire:?}"
        );
    }
    let by_name = proto::RevokeRequest {
        operator: "operator".to_owned(),
        serial: 0,
    };
    for (wire, revoked) in WIRES.into_iter().zip([2, 0]) {
        let answer: proto::RevokeResponse =
            rpc(&coordinator, wire, REVOKE, Some(OPERATOR), &by_name)
                .await
                .expect("revoked");
        assert_eq!(answer.revoked, revoked, "{wire:?}");
    }
    let none = proto::RevokeRequest {
        operator: "nobody".to_owned(),
        serial: 0,
    };
    let refused: Result<proto::RevokeResponse, _> =
        rpc(&coordinator, WIRES[0], REVOKE, Some(OPERATOR), &none).await;
    assert_eq!(
        refused.err(),
        Some((Code::NotFound, "unknown_cert".to_owned()))
    );
    for path in [PATHS[1], PATHS[2], PATHS[4]] {
        let request = grpc_request(path, (), Some(OPERATOR));
        let answer = call(&coordinator, Surface::Control, &None, request).await;
        assert_eq!(
            answer,
            (Code::FailedPrecondition, "not_configured".to_owned(), None),
            "{path}"
        );
    }
}

/// Proves: with the CA and the tunnels, `HostKeys` and `PinHostKeys` answer over gRPC and over the
/// JSON gateway alike: no pin at first, the host's reported set pinned (the state `pinned`, the
/// set in the answer), a set the host did not report refused `FAILED_PRECONDITION` `not_reported`,
/// and a host that is none `NOT_FOUND` `unknown_host`.
///
/// Catches: a method that answers on one wire only, and a refusal that changes its token or its
/// code on the way out.
#[tokio::test]
async fn host_keys_and_pin_host_keys_answer_over_both_wires_with_tunnels() {
    use hostkeeper_proto::hostkeeper::operator::v1 as proto;

    use super::rollouts::{WIRES, rpc};
    use crate::ssh_book::Book;
    use crate::ssh_tunnel::tests::Rig;

    const HOST_KEYS: &str = "/hostkeeper.operator.v1.Ssh/HostKeys";
    const PIN: &str = "/hostkeeper.operator.v1.Ssh/PinHostKeys";
    const FP_A: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const FP_B: &str = "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    let rig = Rig::new("grpc-ssh-keys", |_| {}).await;
    let ask = |host: &str| proto::HostKeysRequest {
        host_id: host.to_owned(),
    };
    let pin = |fingerprints: &[&str]| proto::PinHostKeysRequest {
        host_id: "node-07".to_owned(),
        fingerprints: fingerprints.iter().map(|f| (*f).to_owned()).collect(),
    };
    for (n, wire) in WIRES.into_iter().enumerate() {
        let host = "node-07";
        let none: proto::HostKeysResponse = rpc(
            &rig.coordinator,
            wire,
            HOST_KEYS,
            Some(OPERATOR),
            &ask(host),
        )
        .await
        .expect("answered");
        let expected = if n == 0 { "no_pin" } else { "pinned" };
        assert_eq!(none.state, expected, "{wire:?}");
        if n == 0 {
            assert!(none.fingerprints.is_empty(), "{wire:?}");
            rig.book
                .inner
                .heard(host, "127.0.0.1", &[FP_A.to_owned()])
                .expect("heard");
            let refused: Result<proto::HostKeysResponse, _> =
                rpc(&rig.coordinator, wire, PIN, Some(OPERATOR), &pin(&[FP_B])).await;
            assert_eq!(
                refused.err(),
                Some((Code::FailedPrecondition, "not_reported".to_owned())),
                "{wire:?}"
            );
        }
        let pinned: proto::HostKeysResponse =
            rpc(&rig.coordinator, wire, PIN, Some(OPERATOR), &pin(&[FP_A]))
                .await
                .expect("pinned");
        assert_eq!(
            (pinned.state.as_str(), pinned.fingerprints.as_slice()),
            ("pinned", [FP_A.to_owned()].as_slice()),
            "{wire:?}"
        );
        let unknown: Result<proto::HostKeysResponse, _> = rpc(
            &rig.coordinator,
            wire,
            HOST_KEYS,
            Some(OPERATOR),
            &ask("node-99"),
        )
        .await;
        assert_eq!(
            unknown.err(),
            Some((Code::NotFound, "unknown_host".to_owned())),
            "{wire:?}"
        );
    }
}

/// The frames of the gRPC messages in `bytes`.
fn frames_of(bytes: &[u8]) -> Vec<hostkeeper_proto::hostkeeper::operator::v1::TunnelFrame> {
    use prost::Message;

    let mut frames = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let length = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
        frames.push(Message::decode(&rest[5..5 + length]).expect("a frame"));
        rest = &rest[5 + length..];
    }
    frames
}

/// Proves: through the router a tunnel is refused before any frame is answered (a first frame that
/// is no `open`: `INVALID_ARGUMENT`; a host that is none: `NOT_FOUND`; an instance that is
/// draining: `UNAVAILABLE`), and one that is opened answers a stream of frames: `opened` with the
/// login user, and, once the client's stream ends, `close` with the cause `eof`, with the status
/// `OK` in the trailers; the connection's hold on the stream is released at the end.
///
/// Catches: a refusal that is answered after an `opened`, a stream that never ends or ends without
/// its `close`, and a stream that holds its connection's hold for ever.
#[tokio::test]
async fn a_tunnel_is_refused_with_a_status_or_answered_a_stream_of_frames() {
    use bytes::Bytes;
    use hostkeeper_proto::hostkeeper::operator::v1 as proto;
    use http_body_util::BodyExt;
    use prost::Message;
    use proto::tunnel_frame::Frame;

    use super::support::{PATIENCE, framed, grpc_request_raw};
    use crate::grpc::route;
    use crate::ssh_tunnel::tests::{Rig, open};
    use crate::wait::ConnectionWaits;

    let rig = Rig::new("grpc-ssh-tunnel", |_| {}).await;
    let serial = rig.cert("operator", "node-07");
    let send = |frames: &[proto::TunnelFrame]| {
        let body: Vec<u8> = frames
            .iter()
            .flat_map(|frame| framed(&frame.encode_to_vec()))
            .collect();
        grpc_request_raw(PATHS[4], Bytes::from(body), Some(OPERATOR))
    };
    let none = send(&[proto::TunnelFrame { frame: None }]);
    let answer = call(&rig.coordinator, Surface::Control, &None, none).await;
    assert_eq!(
        answer,
        (Code::InvalidArgument, "invalid_request".to_owned(), None)
    );
    let unknown = send(&[open("node-99", serial)]);
    let answer = call(&rig.coordinator, Surface::Control, &None, unknown).await;
    assert_eq!(answer, (Code::NotFound, "unknown_host".to_owned(), None));

    let waits = ConnectionWaits::default();
    let mut request = send(&[open("node-07", serial)]);
    request.extensions_mut().insert(waits.clone());
    let response = route(&rig.coordinator, Surface::Control, &None, request, PATIENCE).await;
    let collected = response.into_body().collect().await.expect("a body");
    let status = collected
        .trailers()
        .and_then(tonic::Status::from_header_map)
        .expect("a status in the trailers");
    assert_eq!(status.code(), Code::Ok);
    let frames = frames_of(&collected.to_bytes());
    assert_eq!(
        frames
            .into_iter()
            .map(|frame| frame.frame.expect("a frame"))
            .collect::<Vec<_>>(),
        vec![
            Frame::Opened(proto::Opened {
                login_user: "root".to_owned()
            }),
            Frame::Close(proto::Close {
                cause: "eof".to_owned()
            }),
        ]
    );
    // The relay is over: the connection holds nothing, and is quiet at once.
    waits.quiet_for(std::time::Duration::ZERO).await;

    rig.tunnels.drain().await;
    let late = send(&[open("node-07", serial)]);
    let answer = call(&rig.coordinator, Surface::Control, &None, late).await;
    assert_eq!(answer, (Code::Unavailable, "draining".to_owned(), None));
}

/// Proves: the decode limit of `Ssh.Tunnel` is 16 KiB and 64 bytes a frame and every other method
/// of `Ssh` keeps 4 KiB.
///
/// Catches: a stream whose full frames are refused as too large, and one that takes any size.
#[test]
fn the_tunnel_reads_frames_of_16_kib_and_64_bytes_and_the_rest_of_ssh_4_kib() {
    use crate::grpc::operator_limit;
    use crate::http::{MAX_OPERATOR_BYTES, MAX_TUNNEL_FRAME_BYTES};

    assert_eq!(MAX_TUNNEL_FRAME_BYTES, 16 * 1024 + 64);
    assert_eq!(operator_limit(PATHS[4]), 16_448);
    for path in &PATHS[..4] {
        assert_eq!(operator_limit(path), MAX_OPERATOR_BYTES, "{path}");
    }
}

/// Proves: a heartbeat of an admitted host over gRPC tells the book the address of its connection
/// and the host keys it reports; a heartbeat of a retiring host and one that has no address tell it
/// nothing, and a book that cannot be written does not fail the heartbeat.
///
/// Catches: a heartbeat that fails because the book is down, and a book that learns the address
/// of a host that is being denied.
#[tokio::test]
async fn a_heartbeat_tells_the_book_where_the_host_was_heard_from() {
    use hostkeeper_proto::hostkeeper::agent::v1 as agent;

    use super::heartbeat::updater;
    use crate::http::PeerAddr;
    use crate::ssh_book::Book;
    use crate::ssh_tunnel::tests::Rig;

    const FP: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let rig = Rig::new("grpc-ssh-heartbeat", |_| {}).await;
    let beat = |reported: &[&str]| agent::HeartbeatRequest {
        platform: agent::Platform::LinuxArm64 as i32,
        agent: Some(agent::AgentInfo {
            name: "hostkeeper-daemon".to_owned(),
            version: "1.0".to_owned(),
        }),
        features: vec!["heartbeat-v2".to_owned(), "ssh-v1".to_owned()],
        services: vec![agent::ServiceBeat {
            name: "web".to_owned(),
            version: "1.0".to_owned(),
            health: agent::Health::Healthy as i32,
            ..agent::ServiceBeat::default()
        }],
        ssh: Some(agent::SshReport {
            host_key_fingerprints: reported.iter().map(|f| (*f).to_owned()).collect(),
            ..agent::SshReport::default()
        }),
        ..agent::HeartbeatRequest::default()
    };
    let send = |host: &str, from: Option<&str>, reported: &[&str]| {
        let mut request = grpc_request(super::heartbeat::HEARTBEAT, beat(reported), None);
        if let Some(from) = from {
            request
                .extensions_mut()
                .insert(PeerAddr(from.parse().expect("a socket address")));
        }
        let rig = &rig;
        let peer = updater(host, "web");
        async move { call(&rig.coordinator, Surface::Heartbeat, &peer, request).await }
    };
    let (code, said, _) = send("node-07", Some("192.0.2.7:40000"), &[FP]).await;
    assert_eq!((code, said.as_str()), (Code::Ok, ""));
    assert_eq!(
        rig.book.inner.address("node-07").as_deref(),
        Some("192.0.2.7")
    );
    assert_eq!(
        rig.book.inner.host_keys("node-07").expect("keys").reported,
        vec![FP.to_owned()]
    );

    let (code, _, _) = send("node-07", None, &[]).await;
    assert_eq!(code, Code::Ok);
    assert_eq!(
        rig.book.inner.address("node-07").as_deref(),
        Some("192.0.2.7")
    );

    rig.book.fail("heard");
    let (code, _, _) = send("node-07", Some("192.0.2.8:40000"), &[]).await;
    assert_eq!(code, Code::Ok);
    rig.book.mend("heard");
    assert_eq!(
        rig.book.inner.address("node-07").as_deref(),
        Some("192.0.2.7")
    );

    let host = hostkeeper_enrollment::names::HostId::parse("node-08").expect("an id");
    rig.coordinator
        .enrollment()
        .expect("enrollment")
        .store()
        .deny(&host)
        .expect("denied");
    let (code, _, _) = send("node-08", Some("192.0.2.9:40000"), &[]).await;
    assert_eq!(code, Code::PermissionDenied);
    assert_eq!(
        rig.book.inner.address("node-08").as_deref(),
        Some("127.0.0.1")
    );
}
