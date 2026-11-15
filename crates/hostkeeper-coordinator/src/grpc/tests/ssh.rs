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
