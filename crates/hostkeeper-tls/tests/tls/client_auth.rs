//! What a listener asks of a client's certificate beyond its client CA file ([`ClientAuth`]):
//! a client with no certificate let in when it is optional, and only then; a certificate that
//! chains to no CA refused either way; an extra CA certificate admitting a client the file does
//! not; and a reload keeping both.

use hostkeeper_tls::{Alpn, ClientAuth, Outcome, TlsSource};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

use crate::support::{Files, INTERVAL, ca, client_leaf, connect, ping, server_leaf};

/// The DER of the first certificate of `pem`.
fn der_of(pem: &str) -> Vec<u8> {
    CertificateDer::from_pem_slice(pem.as_bytes())
        .expect("a certificate")
        .as_ref()
        .to_vec()
}

fn source(files: &Files, client: ClientAuth) -> TlsSource {
    TlsSource::files_with("test", files.files.clone(), INTERVAL, Alpn::H2, client)
        .expect("a source")
}

/// Catches a listener whose client certificate is optional refusing a client that has none,
/// one whose certificate is required letting such a client in, and an optional one letting in
/// a certificate that chains to no CA it trusts.
#[tokio::test]
async fn a_client_without_a_certificate_is_let_in_only_when_it_is_optional() {
    let server_ca = ca("server CA");
    let client_ca = ca("client CA");
    let stranger = ca("stranger CA");
    let files = Files::write("optional", &server_leaf(&server_ca), Some(&client_ca.pem));
    let optional = source(
        &files,
        ClientAuth {
            optional: true,
            extra_roots: Vec::new(),
        },
    );

    let mut anonymous = connect(&optional, &server_ca.pem, None)
        .await
        .expect("connect");
    ping(&mut anonymous)
        .await
        .expect("let in without a certificate");
    assert_eq!((&mut anonymous.seen).await.expect("seen"), None);

    let member = client_leaf(&client_ca, "host-1");
    let mut known = connect(&optional, &server_ca.pem, Some(&member))
        .await
        .expect("connect");
    ping(&mut known).await.expect("a chaining certificate");
    assert_eq!(
        (&mut known.seen).await.expect("seen"),
        Some(vec![member.der.clone()])
    );

    let foreign = client_leaf(&stranger, "host-2");
    let refused = match connect(&optional, &server_ca.pem, Some(&foreign)).await {
        Ok(mut conn) => ping(&mut conn).await.is_err(),
        Err(_) => true,
    };
    assert!(refused, "a certificate of an unknown CA was let in");

    let required = source(&files, ClientAuth::default());
    let refused = match connect(&required, &server_ca.pem, None).await {
        Ok(mut conn) => ping(&mut conn).await.is_err(),
        Err(_) => true,
    };
    assert!(refused, "a required certificate was not required");
}

/// Catches an extra CA certificate that does not admit the clients it signs, or that a reload
/// of the files drops together with the optional client certificate.
#[tokio::test]
async fn an_extra_root_admits_its_clients_and_a_reload_keeps_it() {
    let server_ca = ca("server CA");
    let file_ca = ca("file CA");
    let extra_ca = ca("extra CA");
    let files = Files::write("extra-root", &server_leaf(&server_ca), Some(&file_ca.pem));
    let client = ClientAuth {
        optional: true,
        extra_roots: vec![der_of(&extra_ca.pem)],
    };
    let source = source(&files, client);
    let enrolled = client_leaf(&extra_ca, "host-3");

    let mut conn = connect(&source, &server_ca.pem, Some(&enrolled))
        .await
        .expect("connect");
    ping(&mut conn).await.expect("the extra root admits it");

    files.install(&server_leaf(&server_ca));
    assert_eq!(source.reload(), Outcome::Applied);
    let mut again = connect(&source, &server_ca.pem, Some(&enrolled))
        .await
        .expect("connect");
    ping(&mut again)
        .await
        .expect("the reload kept the extra root");
    let mut anonymous = connect(&source, &server_ca.pem, None)
        .await
        .expect("connect");
    ping(&mut anonymous)
        .await
        .expect("the reload kept the optional certificate");
}
