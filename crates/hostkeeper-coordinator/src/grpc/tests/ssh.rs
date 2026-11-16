//! `Ssh` while the coordinator holds no SSH user CA: every method, the stream included, is
//! refused `not_configured` once the caller is admitted as an operator, and a caller with no
//! credential or with CI's token is refused before the service is reached, as for every other
//! operator method.

use tonic::Code;

use super::support::{call, grpc_request, served};
use crate::http::Surface;
use crate::testutil::OPERATOR;

/// The path of each method of `Ssh`, `Tunnel` (which the command table leaves out) among them.
const PATHS: [&str; 4] = [
    "/hostkeeper.operator.v1.Ssh/SignUserCert",
    "/hostkeeper.operator.v1.Ssh/HostKeys",
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
/// revocation says how many certificates it revoked, and `HostKeys` and `Tunnel`, which are a
/// later change, still say `not_configured`.
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
            signed
                .certificate
                .starts_with(concat!("ssh-ed25519-cert-v01", "\u{40}", "openssh.com", " ")),
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
    for path in [PATHS[1], PATHS[3]] {
        let request = grpc_request(path, (), Some(OPERATOR));
        let answer = call(&coordinator, Surface::Control, &None, request).await;
        assert_eq!(
            answer,
            (Code::FailedPrecondition, "not_configured".to_owned(), None),
            "{path}"
        );
    }
}
