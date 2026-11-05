//! A listener's certificate, key and client CA, replaced on disk while it serves.
//!
//! Every test makes real TLS handshakes in-process and reads which certificate the client
//! was served, so what is checked is what a client sees and not what the source says it
//! holds. A look at the files is a direct call of `reload`.

use std::time::Duration;

use hostkeeper_tls::{Outcome, TlsError, TlsSource};

use crate::support::{
    Files, ca, client_leaf, connect, expired_leaf, ping, put, served, server_leaf,
};

/// The client sees the certificate that was on disk when its handshake began: the old
/// one before a renewal, the new one after.
///
/// Catches: a source that reads the files once at start.
#[tokio::test]
async fn a_renewed_certificate_serves_new_handshakes() {
    let ca = ca("reload CA");
    let (old, new) = (server_leaf(&ca), server_leaf(&ca));
    let files = Files::write("renewed", &old, None);
    let source = files.source();

    let mut before = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut before).await.expect("ping");
    assert_eq!(served(&before), old.der);

    files.install(&new);
    assert_eq!(source.reload(), Outcome::Applied);
    let mut after = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut after).await.expect("ping");
    assert_eq!(served(&after), new.der);
    assert_eq!(source.reloads().rejected, 0);
}

/// A connection that was open at the reload goes on working, on the certificate it was
/// accepted under, and new ones are served the new certificate beside it.
///
/// Catches: a reload that ends the connections of the old configuration.
#[tokio::test]
async fn an_open_connection_survives_a_reload() {
    let ca = ca("survive CA");
    let (old, new) = (server_leaf(&ca), server_leaf(&ca));
    let files = Files::write("survive", &old, None);
    let source = files.source();

    let mut open = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut open).await.expect("ping before");
    files.install(&new);
    assert_eq!(source.reload(), Outcome::Applied);
    ping(&mut open).await.expect("ping after the reload");
    assert_eq!(served(&open), old.der);

    let mut fresh = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut fresh).await.expect("ping");
    assert_eq!(served(&fresh), new.der);
    ping(&mut open).await.expect("the old one still answers");
}

/// A renewal written in two steps (certificate, then key) is a mismatch in between: the
/// old pair keeps serving, the mismatch is counted, and the whole pair is applied when
/// the second file lands.
///
/// Catches: a mismatched pair accepted; a swap before the pair is validated.
#[tokio::test]
async fn a_half_written_pair_keeps_the_old_one_until_it_is_whole() {
    let ca = ca("half CA");
    let (old, new) = (server_leaf(&ca), server_leaf(&ca));
    let files = Files::write("half", &old, None);
    let source = files.source();

    put(&files.files.cert, &new.cert);
    assert_eq!(source.reload(), Outcome::Rejected);
    let mut during = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut during).await.expect("ping");
    assert_eq!(served(&during), old.der, "a mismatched pair was served");
    assert_eq!(source.reloads().applied, 0);
    assert_eq!(source.reloads().rejected, 1);

    put(&files.files.key, &new.key);
    assert_eq!(source.reload(), Outcome::Applied);
    let mut after = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut after).await.expect("ping");
    assert_eq!(served(&after), new.der);
}

/// What each kind of damage to each file does: nothing to the service, one refusal in
/// the count, and the good files applied when they are back.
///
/// Catches: garbage, a missing file or a CA bundle with nothing usable applied; a bad state
/// counted again at every look.
#[tokio::test]
async fn damaged_files_keep_the_old_configuration_and_count_one_refusal() {
    let ca = ca("garbage CA");
    let client = client_leaf(&ca, "node-1");
    let old = server_leaf(&ca);
    let files = Files::write("garbage", &old, Some(&ca.pem));
    let source = files.source();
    let client_ca = files.files.client_ca.clone().expect("client CA file");

    // PEM with a body that is not base64, text with no PEM in it, a certificate-shaped
    // PEM that is not X.509, and a missing file.
    let not_base64 = "-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n";
    let not_x509 = "-----BEGIN CERTIFICATE-----\naGVsbG8=\n-----END CERTIFICATE-----\n";
    let key_not_base64 = "-----BEGIN PRIVATE KEY-----\n!!!!\n-----END PRIVATE KEY-----\n";
    let damage: [(&std::path::Path, &str, Option<&str>); 8] = [
        (&files.files.cert, "cert is text", Some("not a certificate")),
        (&files.files.cert, "cert is not base64", Some(not_base64)),
        (&files.files.cert, "cert is not X.509", Some(not_x509)),
        (&files.files.cert, "cert is missing", None),
        (&files.files.key, "key is text", Some("not a key")),
        (&files.files.key, "key is not base64", Some(key_not_base64)),
        (&client_ca, "CA is text", Some("not a CA")),
        (&client_ca, "CA holds no usable certificate", Some(not_x509)),
    ];
    let good: [(&std::path::Path, &str); 3] = [
        (&files.files.cert, old.cert.as_str()),
        (&files.files.key, old.key.as_str()),
        (&client_ca, ca.pem.as_str()),
    ];
    for (n, (path, what, bad)) in damage.into_iter().enumerate() {
        let good_text = good
            .iter()
            .find(|(p, _)| *p == path)
            .expect("a good file")
            .1;
        let before = source.reloads();
        match bad {
            Some(text) => put(path, text),
            None => std::fs::remove_file(path).expect("remove"),
        }
        assert_eq!(source.reload(), Outcome::Rejected, "{what}");

        // Several more looks at the same damage: not counted again, none applied.
        for _ in 0..3 {
            assert_eq!(source.reload(), Outcome::Unchanged, "{what}: looked again");
        }
        let after = source.reloads();
        assert_eq!(after.rejected, before.rejected + 1, "{what}: counted again");
        assert_eq!(after.applied, before.applied, "{what}: applied");

        let mut conn = connect(&source, &ca.pem, Some(&client))
            .await
            .expect("connect");
        ping(&mut conn).await.expect("ping");
        assert_eq!(
            served(&conn),
            old.der,
            "{what}: the served certificate moved"
        );

        put(path, good_text);
        assert_eq!(source.reload(), Outcome::Applied, "{what}: good file back");
        assert_eq!(source.reloads().applied, before.applied + 1, "case {n}");
        assert_eq!(source.reloads().rejected, before.rejected + 1, "case {n}");
    }
}

/// A renewed certificate whose validity has already ended is not applied; the pair that
/// is still valid keeps serving.
///
/// Catches: an expired certificate replacing a working one.
#[tokio::test]
async fn an_expired_certificate_is_not_applied() {
    let ca = ca("expiry CA");
    let old = server_leaf(&ca);
    let expired = expired_leaf(&ca);
    let files = Files::write("expired", &old, None);
    let source = files.source();

    put(&files.files.key, &expired.key);
    assert_eq!(source.reload(), Outcome::Rejected, "the mismatch");
    put(&files.files.cert, &expired.cert);
    assert_eq!(source.reload(), Outcome::Rejected, "the expired pair");
    assert_eq!(source.reloads().rejected, 2);
    assert_eq!(source.reloads().applied, 0);
    let mut conn = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut conn).await.expect("ping");
    assert_eq!(served(&conn), old.der);
}

/// The server starts with the files it is given, an expired certificate included, as it
/// always has; files that do not make a configuration stop it.
///
/// Catches: a start that refuses an expired certificate; a start that accepts bad files.
#[test]
fn the_first_load_must_work_but_may_be_expired() {
    let ca = ca("start CA");
    let files = Files::write("start-expired", &expired_leaf(&ca), None);
    TlsSource::files("test", files.files.clone(), Duration::from_secs(1))
        .expect("an expired certificate starts");

    let good = server_leaf(&ca);
    let files = Files::write("start-bad", &good, Some(&ca.pem));
    let (cert, other) = (good.cert.clone(), server_leaf(&ca));
    let start =
        |files: &Files| TlsSource::files("test", files.files.clone(), Duration::from_secs(1));
    assert!(start(&files).is_ok());

    put(&files.files.key, &other.key);
    assert!(matches!(start(&files), Err(TlsError::Config(_))));
    put(&files.files.key, &good.key);
    put(&files.files.cert, "nothing");
    assert!(matches!(start(&files), Err(TlsError::Pem { .. })));
    put(&files.files.cert, &cert);
    std::fs::remove_file(&files.files.key).expect("remove");
    assert!(matches!(start(&files), Err(TlsError::Read { .. })));
    // The message names the file.
    let message = start(&files).expect_err("an error").to_string();
    assert!(message.contains("server.key"), "{message}");
}

/// The trust roots are reloaded the same way: after the client CA file changes, a
/// certificate of the old CA is refused on a new connection and one of the new CA is
/// accepted, while the connection that was open keeps working; the server sees the
/// client's chain on its connection.
///
/// Catches: a source that reloads the identity but keeps the client verifier.
#[tokio::test]
async fn a_reloaded_client_ca_changes_who_may_connect() {
    let (server_ca, ca_one, ca_two) = (ca("server CA"), ca("client CA one"), ca("client CA two"));
    let (one, two) = (
        client_leaf(&ca_one, "node-1"),
        client_leaf(&ca_two, "node-2"),
    );
    let files = Files::write("mtls", &server_leaf(&server_ca), Some(&ca_one.pem));
    let source = files.source();
    let client_ca = files.files.client_ca.clone().expect("client CA file");

    let mut kept = connect(&source, &server_ca.pem, Some(&one))
        .await
        .expect("connect");
    ping(&mut kept).await.expect("one is accepted");
    let chain = (&mut kept.seen)
        .await
        .expect("the server's handshake ended")
        .expect("the client's chain");
    assert_eq!(chain[0], one.der);
    let mut none = connect(&source, &server_ca.pem, None)
        .await
        .expect("connect");
    assert!(
        ping(&mut none).await.is_err(),
        "a client with no certificate"
    );
    assert!(
        (&mut none.seen).await.is_err(),
        "the server's handshake did not end"
    );

    put(&client_ca, &ca_two.pem);
    assert_eq!(source.reload(), Outcome::Applied);
    let mut old_ca = connect(&source, &server_ca.pem, Some(&one))
        .await
        .expect("the handshake ends on the client's side");
    assert!(
        ping(&mut old_ca).await.is_err(),
        "the old CA is still trusted"
    );
    let mut new_ca = connect(&source, &server_ca.pem, Some(&two))
        .await
        .expect("connect");
    ping(&mut new_ca).await.expect("two is accepted");
    ping(&mut kept).await.expect("the open connection goes on");

    // A bundle of both CAs trusts both.
    put(&client_ca, &format!("{}{}", ca_one.pem, ca_two.pem));
    assert_eq!(source.reload(), Outcome::Applied);
    for who in [&one, &two] {
        let mut conn = connect(&source, &server_ca.pem, Some(who))
            .await
            .expect("connect");
        ping(&mut conn).await.expect("accepted by the bundle");
    }
}
