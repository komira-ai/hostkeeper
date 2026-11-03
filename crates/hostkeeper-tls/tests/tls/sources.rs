//! The ways of making a source (from files, from text, with a protocol list), what each
//! refuses, what it offers, and its metrics.

use std::time::Duration;

use hostkeeper_tls::{Alpn, Outcome, Reloads, TlsError, TlsSource, render_metrics};

use crate::support::{
    Files, ca, client_leaf, connect, expired_leaf, ping, put, served, server_leaf,
};

/// The counters and the expiry gauge: one header per family however many sources, a row
/// per listener and outcome, and nothing for no source.
#[test]
fn the_metrics_name_each_listener_and_outcome() {
    let ca = ca("metrics CA");
    let (a, b) = (server_leaf(&ca), server_leaf(&ca));
    let files = Files::write("metrics", &a, None);
    let first =
        TlsSource::files("reapi", files.files.clone(), Duration::from_secs(3600)).expect("source");
    let second =
        TlsSource::files("worker", files.files.clone(), Duration::from_secs(3600)).expect("source");

    assert_eq!(first.reload(), Outcome::Unchanged);
    files.install(&b);
    assert_eq!(first.reload(), Outcome::Applied);
    put(&files.files.cert, "garbage");
    assert_eq!(first.reload(), Outcome::Rejected);
    assert_eq!(
        first.reloads(),
        Reloads {
            applied: 1,
            rejected: 1,
            unchanged: 1
        }
    );

    let mut text = String::new();
    render_metrics([&first, &second], &mut text);
    for want in [
        "hostkeeper_tls_reloads_total{listener=\"reapi\",outcome=\"applied\"} 1\n",
        "hostkeeper_tls_reloads_total{listener=\"reapi\",outcome=\"rejected\"} 1\n",
        "hostkeeper_tls_reloads_total{listener=\"reapi\",outcome=\"unchanged\"} 1\n",
        "hostkeeper_tls_reloads_total{listener=\"worker\",outcome=\"applied\"} 0\n",
    ] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
    assert_eq!(
        text.matches("# TYPE hostkeeper_tls_reloads_total").count(),
        1
    );
    assert_eq!(
        text.matches("# TYPE hostkeeper_tls_leaf_not_after_seconds")
            .count(),
        1
    );
    let gauge = |listener: &str| -> i64 {
        let prefix = format!("hostkeeper_tls_leaf_not_after_seconds{{listener=\"{listener}\"}} ");
        let line = text
            .lines()
            .find(|l| l.starts_with(&prefix))
            .expect("gauge row");
        line[prefix.len()..].parse().expect("a number")
    };
    // The reapi source serves the pair it applied (b), the worker one still serves a: two
    // leaves made a moment apart, valid for years.
    assert!(gauge("reapi") > 1_800_000_000 && gauge("worker") > 1_800_000_000);

    let mut nothing = String::new();
    render_metrics(std::iter::empty(), &mut nothing);
    assert!(nothing.is_empty());
    assert!(format!("{first:?}").contains("reapi"));
}

/// A source made of PEM text has no files: it serves what it was given, a look at it
/// finds nothing new, and a bad text is refused when it is made.
#[tokio::test]
async fn a_fixed_source_serves_its_text_and_never_changes() {
    let ca = ca("fixed CA");
    let leaf = server_leaf(&ca);
    let source = TlsSource::fixed("test", leaf.cert.as_bytes(), leaf.key.as_bytes(), None)
        .expect("a fixed source");
    assert_eq!(source.reload(), Outcome::Unchanged);
    assert_eq!(source.reloads().unchanged, 1);
    let mut conn = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut conn).await.expect("ping");
    assert_eq!(served(&conn), leaf.der);

    let bad = TlsSource::fixed("test", b"", leaf.key.as_bytes(), Some(ca.pem.as_bytes()));
    assert!(matches!(bad, Err(TlsError::Pem { .. })));
    let named = bad.expect_err("an error").to_string();
    assert!(named.contains("<certificate>"), "{named}");
    let with_ca = TlsSource::fixed(
        "test",
        leaf.cert.as_bytes(),
        leaf.key.as_bytes(),
        Some(ca.pem.as_bytes()),
    );
    assert!(with_ca.is_ok());
}

/// A fixed source takes a new certificate and key in memory: new handshakes are served the
/// replacement, a connection open at the swap goes on under the old one, and a pair that
/// does not match, a source that reads files and a source that checks client certificates
/// refuse the call and leave what was served as it was.
///
/// Catches: a replacement that does not reach the next handshake (a certificate that
/// expires while the process runs), one that ends open connections, a mismatched pair
/// applied, and a swap that drops a file source's reload or a client CA.
#[tokio::test]
async fn a_fixed_source_serves_a_replacement_to_new_handshakes() {
    let ca = ca("replace CA");
    let (old, new) = (server_leaf(&ca), server_leaf(&ca));
    let source = TlsSource::fixed("test", old.cert.as_bytes(), old.key.as_bytes(), None)
        .expect("a fixed source");

    let mut open = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut open).await.expect("ping");
    assert_eq!(served(&open), old.der);

    source
        .replace(new.cert.as_bytes(), new.key.as_bytes())
        .expect("a matching pair");
    let mut fresh = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut fresh).await.expect("ping");
    assert_eq!(served(&fresh), new.der);
    ping(&mut open)
        .await
        .expect("the open connection still answers");
    assert_eq!(source.reloads().applied, 1);

    let mismatch = source.replace(old.cert.as_bytes(), new.key.as_bytes());
    assert!(matches!(mismatch, Err(TlsError::Config(_))), "{mismatch:?}");
    let mut after = connect(&source, &ca.pem, None).await.expect("connect");
    ping(&mut after).await.expect("ping");
    assert_eq!(served(&after), new.der);

    let expired = expired_leaf(&ca);
    let late = source.replace(expired.cert.as_bytes(), expired.key.as_bytes());
    assert!(matches!(late, Err(TlsError::Expired { .. })), "{late:?}");
    assert_eq!(source.reloads().applied, 1);

    let files = Files::write("replace-files", &old, None);
    let watched = files
        .source()
        .replace(new.cert.as_bytes(), new.key.as_bytes());
    assert!(matches!(watched, Err(TlsError::Config(_))), "{watched:?}");
    let with_ca = TlsSource::fixed(
        "test",
        old.cert.as_bytes(),
        old.key.as_bytes(),
        Some(ca.pem.as_bytes()),
    )
    .expect("a source with a client CA");
    let guarded = with_ca.replace(new.cert.as_bytes(), new.key.as_bytes());
    assert!(matches!(guarded, Err(TlsError::Config(_))), "{guarded:?}");
}

/// A source offers `h2` only unless it is made to offer `http/1.1` too, a fixed one and a
/// file one alike, and a file source goes on offering what it was made to offer after the
/// files are replaced.
#[test]
fn a_source_offers_h2_only_unless_made_to_offer_http1_too() {
    let ca = ca("alpn CA");
    let (old, new) = (server_leaf(&ca), server_leaf(&ca));
    let offered = |source: &TlsSource| source.config().alpn_protocols.clone();
    let h2 = vec![b"h2".to_vec()];
    let both = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let plain = TlsSource::fixed("test", old.cert.as_bytes(), old.key.as_bytes(), None)
        .expect("a fixed source");
    assert_eq!(offered(&plain), h2);
    let wide = TlsSource::fixed_alpn(
        "test",
        old.cert.as_bytes(),
        old.key.as_bytes(),
        None,
        Alpn::H2AndHttp1,
    )
    .expect("a fixed source");
    assert_eq!(offered(&wide), both);
    wide.replace(new.cert.as_bytes(), new.key.as_bytes())
        .expect("a matching pair");
    assert_eq!(offered(&wide), both);

    let files = Files::write("alpn-files", &old, None);
    assert_eq!(offered(&files.source()), h2);
    let watched = TlsSource::files_alpn(
        "test",
        files.files.clone(),
        Duration::ZERO,
        Alpn::H2AndHttp1,
    )
    .expect("a file source");
    assert_eq!(offered(&watched), both);
    files.install(&new);
    assert_eq!(watched.reload(), Outcome::Applied);
    assert_eq!(offered(&watched), both);
}

/// Catches: a source made for plain HTTPS clients that also offers `h2` (a client that
/// prefers it would then speak HTTP/2 to a server that does not), or that offers nothing, and
/// one that forgets what it offers after its files are replaced.
#[test]
fn a_source_can_offer_http1_alone() {
    let ca = ca("http1 CA");
    let (old, new) = (server_leaf(&ca), server_leaf(&ca));
    let http1 = vec![b"http/1.1".to_vec()];
    let fixed = TlsSource::fixed_alpn(
        "test",
        old.cert.as_bytes(),
        old.key.as_bytes(),
        None,
        Alpn::Http1,
    )
    .expect("a fixed source");
    assert_eq!(fixed.config().alpn_protocols, http1);
    let files = Files::write("alpn-http1", &old, None);
    let watched = TlsSource::files_alpn("test", files.files.clone(), Duration::ZERO, Alpn::Http1)
        .expect("a file source");
    assert_eq!(watched.config().alpn_protocols, http1);
    files.install(&new);
    assert_eq!(watched.reload(), Outcome::Applied);
    assert_eq!(watched.config().alpn_protocols, http1);
}

/// Text that does not make a configuration is refused, each for its own reason.
///
/// Catches: an error path that swallows the reason or accepts the text.
#[test]
fn text_that_does_not_make_a_configuration_is_refused() {
    let ca = ca("refusal CA");
    let server = server_leaf(&ca);
    let other = server_leaf(&ca);
    let make = |cert: &str, key: &str, roots: &str| {
        TlsSource::fixed(
            "test",
            cert.as_bytes(),
            key.as_bytes(),
            Some(roots.as_bytes()),
        )
    };

    assert!(matches!(
        make("", &server.key, &ca.pem),
        Err(TlsError::Pem { .. })
    ));
    assert!(matches!(
        make(&server.cert, "not a key", &ca.pem),
        Err(TlsError::Pem { .. })
    ));
    assert!(matches!(
        make(&server.cert, &server.key, ""),
        Err(TlsError::Pem { .. })
    ));
    // A bundle of one certificate that is not a CA certificate parses but is no trust
    // anchor: no usable CA.
    let garbage = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    assert!(matches!(
        make(&server.cert, &server.key, garbage),
        Err(TlsError::Pem { .. })
    ));
    // The key of another leaf.
    assert!(matches!(
        make(&server.cert, &other.key, &ca.pem),
        Err(TlsError::Config(_))
    ));
    // The "leaf" is not an X.509 certificate.
    assert!(matches!(
        make(garbage, &server.key, &ca.pem),
        Err(TlsError::Pem { .. })
    ));
}

/// A client certificate is asked for when the source has a client CA, and a client with a
/// certificate of that CA is served; one without is not.
///
/// Catches: a source with a client CA that does not ask for a certificate.
#[tokio::test]
async fn a_source_with_a_client_ca_requires_a_certificate_of_it() {
    let ca = ca("required CA");
    let (server, client) = (server_leaf(&ca), client_leaf(&ca, "node-1"));
    let source = TlsSource::fixed(
        "test",
        server.cert.as_bytes(),
        server.key.as_bytes(),
        Some(ca.pem.as_bytes()),
    )
    .expect("a fixed source");
    let mut with = connect(&source, &ca.pem, Some(&client))
        .await
        .expect("connect");
    ping(&mut with).await.expect("a certificate of the CA");
    let mut without = connect(&source, &ca.pem, None).await.expect("connect");
    assert!(ping(&mut without).await.is_err(), "no certificate");
}
