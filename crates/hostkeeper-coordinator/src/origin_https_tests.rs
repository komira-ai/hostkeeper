//! The URL of a repository origin, the TLS client that trusts the roots it is given, and the
//! production dial ([`crate::origin_https`]).

use hostkeeper_oidc::Roots;
use rustls::pki_types::CertificateDer;

use crate::origin_https::{OriginTlsError, OriginUrl, OriginUrlError, connector, tcp_dial};

/// Proves the URLs an origin may have: `https://` with a host a certificate can be checked for
/// and no user, query or fragment; the port defaults to 443; a request's target is the base path,
/// a `/` and the path asked for, whatever final `/` the base had.
///
/// Catches a base that doubles or drops a `/`, an authority that loses its port, and a default
/// port other than 443.
#[test]
fn an_origin_url_names_the_host_the_port_and_the_target_of_a_path() {
    let taken = [
        (
            "https://origin.example",
            "origin.example",
            443,
            "origin.example",
            "/x/y",
        ),
        (
            "https://origin.example/",
            "origin.example",
            443,
            "origin.example",
            "/x/y",
        ),
        (
            "https://origin.example:8443/debian/",
            "origin.example",
            8443,
            "origin.example:8443",
            "/debian/x/y",
        ),
        (
            "https://192.0.2.7/a/b",
            "192.0.2.7",
            443,
            "192.0.2.7",
            "/a/b/x/y",
        ),
    ];
    for (text, host, port, authority, target) in taken {
        let parsed = OriginUrl::parse(text).expect(text);
        assert_eq!(parsed.host(), host, "{text}");
        assert_eq!(parsed.port(), port, "{text}");
        assert_eq!(parsed.authority(), authority, "{text}");
        assert_eq!(parsed.target("x/y"), target, "{text}");
        let _ = parsed.server();
    }
}

/// Proves every URL an origin may not have is refused with the reason that names it, and the
/// message of each reason.
///
/// Catches a plain `http://` origin, a user, query or fragment taken into the base, and a host
/// TLS cannot verify a certificate for.
#[test]
fn an_origin_url_that_is_not_an_https_base_is_refused() {
    let refused = [
        (
            "http://origin.example",
            OriginUrlError::NotHttps as fn(String) -> OriginUrlError,
        ),
        ("ftp://origin.example", OriginUrlError::NotHttps),
        ("/debian", OriginUrlError::NotHttps),
        ("origin.example", OriginUrlError::NotHttps),
        ("not a url", OriginUrlError::NotUrl),
        ("https://", OriginUrlError::NotUrl),
        ("https://[::1]/debian", OriginUrlError::Host),
        ("https://user@origin.example", OriginUrlError::NotBase),
        ("https://origin.example/?v=1", OriginUrlError::NotBase),
        ("https://origin.example/#top", OriginUrlError::NotBase),
    ];
    for (text, kind) in refused {
        let why = OriginUrl::parse(text).expect_err(text);
        assert_eq!(why, kind(text.to_owned()), "{text}");
    }
    assert_eq!(
        OriginUrlError::NotHttps("http://origin.example".to_owned()).to_string(),
        "\"http://origin.example\" is not an https:// URL: an origin is fetched over HTTPS only"
    );
    assert_eq!(
        OriginUrlError::NotUrl("not a url".to_owned()).to_string(),
        "\"not a url\" is not a URL"
    );
    assert_eq!(
        OriginUrlError::Host("https://[::1]".to_owned()).to_string(),
        "\"https://[::1]\" names no host a certificate can be checked for"
    );
    assert_eq!(
        OriginUrlError::NotBase("https://u@origin.example".to_owned()).to_string(),
        "\"https://u@origin.example\" has a user, a query or a fragment: an origin's URL is a base URL"
    );
}

/// Proves a TLS client with no root it can trust is refused at its making (every fetch would fail
/// at the handshake, which a start should say instead).
///
/// Catches a client made over an empty root store.
#[test]
fn a_tls_client_with_no_usable_root_is_refused() {
    for roots in [
        Roots::Given(Vec::new()),
        Roots::Given(vec![CertificateDer::from(b"not a certificate".to_vec())]),
    ] {
        let Err(why) = connector(roots) else {
            panic!("a connector with no roots");
        };
        assert!(matches!(why, OriginTlsError::NoRoots));
        assert_eq!(
            why.to_string(),
            "no trusted root certificates for fetching from a repository origin"
        );
    }
}

/// Proves the production dial opens a TCP connection to the address it is given and fails where
/// nothing listens.
///
/// Catches a dial that does not reach its address, and one that hides a refused connection.
#[tokio::test]
async fn the_production_dial_connects_to_a_listener_and_fails_where_none_listens() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback socket");
    let port = listener.local_addr().expect("its address").port();
    let dial = tcp_dial();
    let (stream, accepted) = tokio::join!(dial("127.0.0.1".to_owned(), port), listener.accept());
    assert!(stream.is_ok());
    assert!(accepted.is_ok());
    drop(listener);
    assert!(dial("127.0.0.1".to_owned(), port).await.is_err());
}
