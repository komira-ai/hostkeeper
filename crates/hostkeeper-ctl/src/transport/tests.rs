//! `--endpoint`, and the TLS material read before a transport is made.

use std::sync::Arc;
use std::thread::JoinHandle;

use bytes::Bytes;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tonic::Code;
use tonic::metadata::MetadataMap;

use super::tls::Material;
use super::{Connect as _, Endpoint, Gateway, Network, TransportKind};
use crate::testutil::{file, loopback_target, pki, scratch};

/// Catches an endpoint that is not plain `https://host[:port]` accepted (plain HTTP, a path, a
/// query, user information), the default port wrong, an IPv6 host kept in brackets, or an
/// explicit port that is empty, not a number, zero or above 65535 read as no port.
#[test]
fn an_endpoint_is_https_host_and_port_only() {
    let ok = Endpoint::parse("https://coordinator.example").expect("plain");
    assert_eq!((ok.host(), ok.port()), ("coordinator.example", 443));
    assert_eq!(ok.uri().to_string(), "https://coordinator.example/");
    let slash = Endpoint::parse("https://coordinator.example/").expect("a slash");
    assert_eq!(slash.port(), 443);
    let v6 = Endpoint::parse("https://[::1]:8443").expect("IPv6, parsed only");
    assert_eq!((v6.host(), v6.port()), ("::1", 8443));
    let v6_default = Endpoint::parse("https://[::1]").expect("IPv6, no port");
    assert_eq!((v6_default.host(), v6_default.port()), ("::1", 443));
    let last = Endpoint::parse("https://coordinator.example:65535").expect("the last port");
    assert_eq!(last.port(), 65535);
    for (given, why) in [
        ("coordinator.example", "the scheme must be https"),
        ("http://coordinator.example", "the scheme must be https"),
        ("https://coordinator.example/v1", "it has a path or a query"),
        (
            "https://coordinator.example/?a=b",
            "it has a path or a query",
        ),
        (
            "https://user@coordinator.example",
            "it carries user information",
        ),
        ("https://", "not a URL"),
        (
            "https://coordinator.example:",
            "the port must be a number from 1 to 65535",
        ),
        (
            "https://coordinator.example:abc",
            "the port must be a number from 1 to 65535",
        ),
        (
            "https://coordinator.example:65536",
            "the port must be a number from 1 to 65535",
        ),
        (
            "https://coordinator.example:0",
            "the port must be a number from 1 to 65535",
        ),
        (
            "https://coordinator.example:+80",
            "the port must be a number from 1 to 65535",
        ),
        (
            "https://[::1]:",
            "the port must be a number from 1 to 65535",
        ),
        (
            "https://[::1]:99999",
            "the port must be a number from 1 to 65535",
        ),
        ("", "not a URL"),
    ] {
        let refused = Endpoint::parse(given).expect_err(given);
        assert_eq!(
            refused.to_string(),
            format!("--endpoint {given:?} is not https://<host>[:<port>]: {why}")
        );
    }
}

/// A CA file, a client pair, and the start of the error they give.
type Case<'a> = (&'a str, Option<(&'a str, &'a str)>, String);

/// Catches a CA or client file that is missing, holds no certificate, holds a broken one, or a
/// key that is no key, getting as far as a connection; and a client key of another
/// certificate accepted by the gateway's TLS.
#[test]
fn unusable_tls_files_are_refused_before_connecting() {
    let pki = pki();
    let other = crate::testutil::pki();
    let dir = scratch("material");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let empty = file(&dir, "empty.pem", b"no certificate here\n");
    let broken = file(
        &dir,
        "broken.pem",
        b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    );
    let unterminated = file(
        &dir,
        "unterminated.pem",
        b"-----BEGIN CERTIFICATE-----\nAAAA\n",
    );
    let cert = file(&dir, "client.pem", pki.client_cert.as_bytes());
    let key = file(&dir, "client.key", pki.client_key.as_bytes());
    let not_a_key = file(&dir, "not.key", pki.client_cert.as_bytes());
    let other_key = file(&dir, "other.key", other.client_key.as_bytes());
    let gone = dir.path().join("gone.pem");
    let gone = gone.to_str().expect("UTF-8");

    let cases: Vec<Case<'_>> = vec![
        (gone, None, format!("cannot read the CA from {gone}: ")),
        (
            empty.as_str(),
            None,
            format!("the CA in {empty} is not usable: no PEM certificate"),
        ),
        (
            broken.as_str(),
            None,
            format!("the CA in {broken} is not usable: "),
        ),
        (
            unterminated.as_str(),
            None,
            format!("the CA in {unterminated} is not usable: "),
        ),
        (
            ca.as_str(),
            Some((gone, key.as_str())),
            format!("cannot read the client certificate from {gone}: "),
        ),
        (
            ca.as_str(),
            Some((cert.as_str(), gone)),
            format!("cannot read the client key from {gone}: "),
        ),
        (
            ca.as_str(),
            Some((cert.as_str(), not_a_key.as_str())),
            format!("the client key in {not_a_key} is not usable: "),
        ),
    ];
    for kind in [TransportKind::Grpc, TransportKind::Gateway] {
        for (ca_file, client, error) in &cases {
            let refused = Network
                .connect(&loopback_target(0, kind, ca_file, *client))
                .err()
                .expect("refused");
            assert!(
                refused.to_string().starts_with(error.as_str()),
                "{refused} / {error}"
            );
        }
    }
    let mismatched = Network
        .connect(&loopback_target(
            0,
            TransportKind::Gateway,
            &ca,
            Some((&cert, &other_key)),
        ))
        .err()
        .expect("a key of another certificate");
    assert!(
        mismatched
            .to_string()
            .starts_with(&format!("the client certificate in {cert} is not usable: ")),
        "{mismatched}"
    );
    // The good pair is accepted by both.
    for kind in [TransportKind::Grpc, TransportKind::Gateway] {
        assert!(
            Network
                .connect(&loopback_target(0, kind, &ca, Some((&cert, &key))))
                .is_ok()
        );
    }
}

/// Catches a gateway made for a host that is no TLS server name, or with descriptors that do
/// not decode.
#[test]
fn a_gateway_needs_a_server_name_and_descriptors() {
    let pki = pki();
    let dir = scratch("gateway-new");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let target = loopback_target(0, TransportKind::Gateway, &ca, None);
    let material = Material::read(&target).expect("material");
    let refused = Gateway::new(&target, &material, b"\xff").expect_err("no descriptors");
    assert!(
        refused
            .to_string()
            .starts_with("the descriptor set does not decode: "),
        "{refused}"
    );
    let mut bad_name = target.clone();
    bad_name.endpoint = Endpoint::parse("https://a..b.example").expect("a URL");
    let refused = Gateway::new(&bad_name, &material, hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect_err("not a server name");
    assert!(
        refused
            .to_string()
            .starts_with("the endpoint https://a..b.example/ is not a TLS server name: "),
        "{refused}"
    );
}

/// A TLS server on 127.0.0.1, port 0, that serves one connection with `cert` and `key` (PEM) and
/// asks for no client certificate: it does the handshake, and its result is what the thread
/// returns. The port is open until the thread has accepted.
fn handshake_once(cert: &str, key: &str) -> (u16, JoinHandle<std::io::Result<()>>) {
    let chain = CertificateDer::pem_slice_iter(cert.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("a certificate");
    let key = PrivateKeyDer::from_pem_slice(key.as_bytes()).expect("a key");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("a certificate and its key");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let handle = std::thread::spawn(move || {
        let (mut tcp, _) = listener.accept()?;
        let mut connection =
            rustls::ServerConnection::new(Arc::new(config)).map_err(std::io::Error::other)?;
        connection.complete_io(&mut tcp).map(|_| ())
    });
    (port, handle)
}

/// Catches the gateway transport trusting a server certificate the CA signed but made for another
/// name, or already expired: the call must end UNAVAILABLE, and the server's handshake must
/// not complete. The server whose certificate is good completes its handshake, so a refusal is
/// the client's doing.
#[test]
fn the_gateway_refuses_a_certificate_for_another_name_or_out_of_date() {
    let pki = pki();
    let dir = scratch("gateway-bad-certificates");
    let ca = file(&dir, "ca.pem", pki.ca.as_bytes());
    let path = "/hostkeeper.operator.v1.Hosts/List";
    let mut metadata = MetadataMap::new();
    metadata.insert(
        "authorization",
        "Bearer token-value-3".parse().expect("ASCII"),
    );
    for (what, cert, key, handshake_ends) in [
        ("good", &pki.server_cert, &pki.server_key, true),
        (
            "another name",
            &pki.other_name_cert,
            &pki.other_name_key,
            false,
        ),
        ("expired", &pki.expired_cert, &pki.expired_key, false),
    ] {
        let (port, server) = handshake_once(cert, key);
        let transport = Network
            .connect(&loopback_target(port, TransportKind::Gateway, &ca, None))
            .expect("a transport");
        let status = transport
            .call(path, Bytes::new(), &metadata)
            .expect_err(what);
        // The good server ends the connection after its handshake, so its call fails too (how,
        // is not asserted); a refused certificate is UNAVAILABLE.
        if !handshake_ends {
            assert_eq!(status.code(), Code::Unavailable, "{what}: {status:?}");
            assert!(
                status
                    .message()
                    .starts_with("the gateway connection failed: "),
                "{what}: {status:?}"
            );
        }
        let handshake = server.join().expect("the server thread");
        assert_eq!(handshake.is_ok(), handshake_ends, "{what}: {handshake:?}");
    }
}
