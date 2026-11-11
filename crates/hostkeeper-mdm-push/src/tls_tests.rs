use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use super::*;
use crate::identity::PushIdentity;
use crate::request::pkcs1_pem;
use crate::testkit::{fixture_key, push_certificate_pem};

#[test]
fn an_endpoint_is_dialed_by_its_name_or_where_it_is_told() {
    let endpoint = Endpoint::new("api.push.apple.com", 443);
    assert_eq!(endpoint.dial, "api.push.apple.com:443");
    assert_eq!(endpoint.server_name, "api.push.apple.com");
    let moved = endpoint.dialing("127.0.0.1:9");
    assert_eq!(moved.dial, "127.0.0.1:9");
    assert_eq!(moved.server_name, "api.push.apple.com");
    assert!(!public_roots().roots.is_empty());
}

#[test]
fn a_root_that_is_not_a_certificate_and_a_key_that_does_not_sign_are_refused() {
    let junk = CertificateDer::from(vec![1, 2, 3]);
    assert!(matches!(
        roots_of(std::slice::from_ref(&junk)),
        Err(TlsError::Root(_))
    ));
    let key = fixture_key(0);
    let cert = push_certificate_pem(key, "com.apple.mgmt.External.x", 1_000_000, 4_000_000_000);
    let identity = PushIdentity::parse(cert.as_bytes(), pkcs1_pem(key).unwrap().expose()).unwrap();
    let broken = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(vec![1, 2, 3]));
    let error =
        client_config(public_roots(), Some((identity.chain, broken)), &[b"h2"]).unwrap_err();
    assert!(matches!(error, TlsError::Identity(_)), "{error}");
    assert!(
        error
            .to_string()
            .starts_with("the TLS client identity is not usable")
    );
    assert!(
        TlsError::Root("x".to_owned())
            .to_string()
            .starts_with("a root certificate")
    );
}

#[tokio::test]
async fn a_server_name_that_is_not_a_name_is_refused_before_a_dial() {
    let config = client_config(public_roots(), None, &[]).unwrap();
    let endpoint = Endpoint::new("", 443).dialing("127.0.0.1:9");
    let error = connect(&endpoint, config, Duration::from_secs(30))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

/// A server that takes the connection and never speaks TLS: whatever order the virtual clock and
/// the socket are served in, the only way out is the timeout.
#[tokio::test(start_paused = true)]
async fn a_handshake_that_never_finishes_times_out() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        Endpoint::new("localhost", 443).dialing(&listener.local_addr().unwrap().to_string());
    let config = client_config(public_roots(), None, &[]).unwrap();
    let error = connect(&endpoint, config, Duration::from_secs(30))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    drop(listener);
}
