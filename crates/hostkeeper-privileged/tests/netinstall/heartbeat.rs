//! A heartbeat sent with a host's certificate and key, as its daemon sends its first one: the JSON
//! route `POST /v1/heartbeat` of the coordinator's LAN listener, over mutual TLS. The
//! network-install test sends one with the certificate the guest held before its reinstall, which
//! the `--replace` approval of the new key must have made worthless.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

/// The status and body of `POST /v1/heartbeat` to `lan`, the listener's certificate checked
/// against the CA `ca_der`, the client authenticated by `cert_pem` and the PKCS#8 key `key_der`.
pub fn post(lan: SocketAddr, ca_der: &[u8], cert_pem: &[u8], key_der: &[u8]) -> (u16, String) {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca_der.to_vec()))
        .expect("the coordinator's CA");
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem).expect("a PEM certificate");
    let chain = vec![CertificateDer::from(pem.contents)];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der.to_vec()));
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("the protocol versions")
            .with_root_certificates(roots)
            .with_client_auth_cert(chain, key)
            .expect("the client's certificate");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let name = ServerName::IpAddress(lan.ip().into());
    let conn = ClientConnection::new(Arc::new(config), name).expect("a TLS client");
    let stream = TcpStream::connect(lan).expect("connect to the LAN listener");
    let mut tls = StreamOwned::new(conn, stream);
    let body = "{}";
    write!(
        tls,
        "POST /v1/heartbeat HTTP/1.1\r\nHost: {lan}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("send the heartbeat");
    let mut reply = Vec::new();
    // The server may end without a close notification; what it sent is the answer.
    let _ = tls.read_to_end(&mut reply);
    let reply = String::from_utf8_lossy(&reply).into_owned();
    let status = reply
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no HTTP status in {reply:?}"));
    let body = reply
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}
