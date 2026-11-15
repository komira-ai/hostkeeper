//! `Ssh` while the coordinator holds no SSH user CA: every method, the stream included, is
//! refused `not_configured` once the caller is admitted as an operator, and a caller with no
//! credential or with CI's token is refused before the service is reached, as for every other
//! operator method.

use tonic::Code;

use super::support::{call, grpc_request, served};
use crate::http::Surface;
use crate::testutil::{OPERATOR, ci_token};

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

/// Proves: with no token `Ssh` answers `UNAUTHENTICATED` `unauthorized`, with CI's token
/// `PERMISSION_DENIED` `forbidden`, and on the heartbeat listener `UNIMPLEMENTED` with no message:
/// the service is the operator API's and is served on the private listener only.
///
/// Catches: a method of `Ssh` that a host or CI could call, or one served on the heartbeat
/// listener.
#[tokio::test]
async fn ssh_takes_the_operators_credential_on_the_private_listener_only() {
    let served = served("grpc-ssh-credentials");
    let ci = ci_token();
    for path in PATHS {
        for (bearer, code, message) in [
            (None, Code::Unauthenticated, "unauthorized"),
            (Some(ci.as_str()), Code::PermissionDenied, "forbidden"),
        ] {
            let request = grpc_request(path, (), bearer);
            let answer = call(&served.coordinator, Surface::Control, &None, request).await;
            assert_eq!(answer, (code, message.to_owned(), None), "{path}");
        }
        let request = grpc_request(path, (), Some(OPERATOR));
        let answer = call(&served.coordinator, Surface::Heartbeat, &None, request).await;
        assert_eq!(answer, (Code::Unimplemented, String::new(), None), "{path}");
    }
}
