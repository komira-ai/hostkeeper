//! The route that serves a private repository ([`crate::repository_serve`]) over the real TLS
//! path of a heartbeat listener: [`crate::listen::connection`] on an in-memory stream, a client
//! certificate of the farm's CA that names a host, an HTTP/1.1 `GET` by hand, and an origin served
//! in process ([`crate::repository_origin_support`]). No socket is opened.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_tls::{Alpn, TlsSource};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_rustls::TlsConnector;

use crate::http::Surface;
use crate::listen::{CLIENT_PATIENCE, connection};
use crate::marks_support::operator;
use crate::repositories_support::{credential_in, private};
use crate::repository_origin::RepositoryOrigin;
use crate::repository_origin_support::{FakeOrigin, Then, response};
use crate::testutil::Rig;

/// A certificate of `authority` for `sans`, usable for `usage`.
fn issued(
    authority: &CertifiedIssuer<'_, KeyPair>,
    sans: Vec<SanType>,
    usage: ExtendedKeyUsagePurpose,
) -> (CertificateDer<'static>, KeyPair) {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = sans;
    params.extended_key_usages = vec![usage];
    let key = KeyPair::generate().expect("a key");
    let cert = params.signed_by(&key, authority).expect("signed");
    (cert.der().clone(), key)
}

fn uri(text: &str) -> SanType {
    SanType::URI(text.try_into().expect("a URI"))
}

/// Proves: over a mutual-TLS connection of the heartbeat listener, a host whose certificate names
/// the role `daemon` gets its group's pool file from the private repository: 200, the origin's
/// exact bytes (more than the stream's buffer), and the `Content-Length` of the origin; the same
/// host asking for another group's path is the 404 of a path that does not exist, on the same
/// listener.
///
/// Catches: a route that works in the router but is lost on the listener's path, or a body that
/// is buffered whole or cut on the wire.
#[tokio::test]
async fn a_host_reads_its_groups_file_over_the_listeners_mutual_tls() {
    let mut authority = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    authority.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let issuer = CertifiedIssuer::self_signed(authority, KeyPair::generate().expect("a CA key"))
        .expect("a CA certificate");
    let dns = |name: &str| SanType::DnsName(name.to_owned().try_into().expect("a DNS name"));
    let (server_cert, server_key) = issued(
        &issuer,
        vec![dns("localhost")],
        ExtendedKeyUsagePurpose::ServerAuth,
    );
    let (client_cert, client_key) = issued(
        &issuer,
        vec![
            uri("urn:kbf:role:updater"),
            uri("urn:kbf:host-role:daemon"),
            dns("d1"),
        ],
        ExtendedKeyUsagePurpose::ClientAuth,
    );
    let pem = |der: &CertificateDer<'_>| {
        let body = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, der.as_ref());
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).expect("ASCII"))
            .collect();
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            lines.join("\n")
        )
    };
    let source = TlsSource::fixed_alpn(
        "heartbeat",
        pem(&server_cert).as_bytes(),
        server_key.serialize_pem().as_bytes(),
        Some(pem(&CertificateDer::from(issuer.der().to_vec())).as_bytes()),
        Alpn::Http1,
    )
    .expect("a TLS source");

    let package: Vec<u8> = (0..400_000_u32)
        .map(|n| u8::try_from(n % 239).expect("a byte"))
        .collect();
    let fake = FakeOrigin::new();
    let served = package.clone();
    let client = RepositoryOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(fake.dial(Arc::new(move |path| {
            if path == "/apt/pool/daemon/main/w/web_1.deb" {
                Then::Write(response("200 OK", &served))
            } else {
                Then::Write(response("404 Not Found", b""))
            }
        })));
    let rig = Rig::new("repository-listen");
    let file = credential_in(rig.dir.path(), "credential", "Bearer s3cret-token-value\n");
    let coordinator = Arc::new(rig.coordinator.with_repository_origin(client));
    coordinator
        .repositories_put(
            &operator(),
            proto::PutRepositoryRequest {
                repository: Some(private(
                    "priv",
                    "https://origin.test/apt",
                    &["daemon", "storage"],
                    1,
                    &file,
                )),
                reason: "for a test".to_owned(),
            },
        )
        .expect("put");

    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(issuer.der().to_vec()))
        .expect("a root");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_client_auth_cert(
            vec![client_cert],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(client_key.serialize_der())),
        )
        .expect("a client certificate");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = TlsConnector::from(Arc::new(config));

    let get = |path: &'static str| {
        let (client, server) = tokio::io::duplex(64 * 1024);
        tokio::spawn(connection(
            server,
            source.clone(),
            Surface::Heartbeat,
            Arc::clone(&coordinator),
            CLIENT_PATIENCE,
        ));
        let connector = connector.clone();
        async move {
            let mut tls = connector
                .connect(ServerName::try_from("localhost").expect("a name"), client)
                .await
                .expect("a handshake");
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            tls.write_all(request.as_bytes()).await.expect("sent");
            let mut answer = Vec::new();
            tls.read_to_end(&mut answer)
                .await
                .expect("the whole answer");
            answer
        }
    };
    let answer = get("/repositories/priv/pool/daemon/main/w/web_1.deb").await;
    let split = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a head");
    let head = String::from_utf8(answer[..split].to_vec()).expect("text");
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "{head}");
    assert!(
        head.lines()
            .any(|l| l.eq_ignore_ascii_case(&format!("content-length: {}", package.len()))),
        "{head}"
    );
    assert!(answer[split + 4..] == package[..], "the bytes differ");

    let other = get("/repositories/priv/pool/storage/main/w/web_1.deb").await;
    let text = String::from_utf8(other).expect("text");
    assert!(text.starts_with("HTTP/1.1 404 Not Found\r\n"), "{text}");
    assert!(text.ends_with(r#"{"error":"not_found"}"#), "{text}");
}
