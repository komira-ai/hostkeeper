//! `--endpoint`, and the TLS material read before a transport is made.

use super::tls::Material;
use super::{Connect as _, Endpoint, Gateway, Network, TransportKind};
use crate::testutil::{file, loopback_target, pki, scratch};

/// Catches an endpoint that is not plain `https://host[:port]` accepted (plain HTTP, a path, a
/// query, user information), the default port wrong, or an IPv6 host kept in brackets.
#[test]
fn an_endpoint_is_https_host_and_port_only() {
    let ok = Endpoint::parse("https://coordinator.example").expect("plain");
    assert_eq!((ok.host(), ok.port()), ("coordinator.example", 443));
    assert_eq!(ok.uri().to_string(), "https://coordinator.example/");
    let slash = Endpoint::parse("https://coordinator.example/").expect("a slash");
    assert_eq!(slash.port(), 443);
    let v6 = Endpoint::parse("https://[::1]:8443").expect("IPv6, parsed only");
    assert_eq!((v6.host(), v6.port()), ("::1", 8443));
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
