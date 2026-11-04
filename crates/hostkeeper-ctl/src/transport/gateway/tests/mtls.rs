//! The gateway against a loopback server that requires a client certificate signed by the test
//! CA: the call with the right pair succeeds and carries the bearer exactly as sent; one without
//! a certificate, or with a certificate of another CA, never reaches the server's handler.

use bytes::Bytes;
use prost::Message as _;
use tonic::metadata::{MetadataMap, MetadataValue};
use tonic::{Code, Status};

use super::{authorization, response, serve_with, server_config};
use crate::TransportKind;
use crate::testutil::{file, loopback_target, pki, scratch};
use crate::transport::{Connect as _, Network};

/// A call of Hosts.List with the bearer `Bearer t0k` to the loopback gateway at `port`, trusting
/// `ca` and presenting `client`, a certificate file and a key file, when given.
fn list(port: u16, ca: &str, client: Option<(&str, &str)>) -> Result<Bytes, Status> {
    let transport = Network
        .connect(&loopback_target(port, TransportKind::Gateway, ca, client))
        .expect("a transport");
    let mut metadata = MetadataMap::new();
    metadata.insert("authorization", MetadataValue::from_static("Bearer t0k"));
    transport.call(
        "/hostkeeper.operator.v1.Hosts/List",
        Bytes::new(),
        &metadata,
    )
}

/// Catches a client certificate not presented to a server that requires one, or a bearer that
/// reaches the server changed: the pair signed by the server's CA is accepted, the reply is
/// read, and the request carries exactly one Authorization header, `Bearer t0k`.
#[test]
fn a_client_certificate_of_the_servers_ca_is_accepted() {
    let pki = pki();
    let dir = scratch("gateway-mtls-accepted");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let cert = file(&dir, "client.pem", pki.client_cert.as_bytes());
    let key = file(&dir, "client.key", pki.client_key.as_bytes());
    let (port, received) = serve_with(
        server_config(&pki, Some(&pki.ca)),
        response("200 OK", r#"{"hosts": [{"nodeId": "h1"}]}"#),
    );
    let reply = list(port, &ca, Some((&cert, &key))).expect("a reply");
    let hosts = hostkeeper_proto::hostkeeper::operator::v1::ListHostsResponse::decode(reply)
        .expect("a ListHostsResponse");
    assert_eq!(hosts.hosts.len(), 1);
    assert_eq!(hosts.hosts[0].node_id, "h1");
    let request = String::from_utf8(received.recv().expect("the request")).expect("UTF-8");
    assert_eq!(authorization(&request), ["Authorization: Bearer t0k"]);
}

/// Catches a server that requires a client certificate being called without one: the call fails
/// as UNAVAILABLE and the server never sees the request.
#[test]
fn a_missing_client_certificate_is_refused() {
    let pki = pki();
    let dir = scratch("gateway-mtls-missing");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let (port, received) = serve_with(
        server_config(&pki, Some(&pki.ca)),
        response("200 OK", r#"{"hosts": []}"#),
    );
    let status = list(port, &ca, None).expect_err("refused");
    assert_eq!(status.code(), Code::Unavailable, "{status:?}");
    // The wording after the prefix is the TLS library's.
    assert!(
        status
            .message()
            .starts_with("the gateway connection failed: "),
        "{status:?}"
    );
    // The server's thread ends without having sent a request.
    assert!(received.recv().is_err());
}

/// Catches a client certificate of another CA accepted: the call fails as UNAVAILABLE and the
/// server never sees the request.
#[test]
fn a_client_certificate_of_another_ca_is_refused() {
    let pki = pki();
    let other = crate::testutil::pki();
    let dir = scratch("gateway-mtls-other");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let cert = file(&dir, "client.pem", other.client_cert.as_bytes());
    let key = file(&dir, "client.key", other.client_key.as_bytes());
    let (port, received) = serve_with(
        server_config(&pki, Some(&pki.ca)),
        response("200 OK", r#"{"hosts": []}"#),
    );
    let status = list(port, &ca, Some((&cert, &key))).expect_err("refused");
    assert_eq!(status.code(), Code::Unavailable, "{status:?}");
    assert!(
        status
            .message()
            .starts_with("the gateway connection failed: "),
        "{status:?}"
    );
    assert!(received.recv().is_err());
}
