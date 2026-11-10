//! The read of a private repository's origin ([`crate::repository_origin::RepositoryOrigin`])
//! against an origin served in process ([`crate::repository_origin_support`]). The deadlines run on
//! tokio's paused clock, which moves only when every task waits, so a deadline is reached without
//! any time passing.

use http_body_util::BodyExt as _;
use hyper::body::Body as _;

use crate::channel_https::ChannelTlsError;
use crate::repository_origin::{BodyError, Origin, OriginError, RepositoryOrigin};
use crate::repository_origin_support::{FakeOrigin, Then, hanging_dial, refusing_dial, response};

const URI: &str = "https://origin.test/apt";
const PATH: &str = "dists/web/InRelease";
const CREDENTIAL: &str = "Bearer s3cret-token-value";

fn origin_over(fake: &FakeOrigin, then: Then) -> RepositoryOrigin {
    RepositoryOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(fake.dial_all(then))
}

/// The error of a read that must fail.
async fn failure(origin: &RepositoryOrigin, uri: &str, credential: &str) -> OriginError {
    match origin.get(uri, PATH, credential).await {
        Err(why) => why,
        Ok(_) => panic!("the read succeeded"),
    }
}

/// Proves: a 200 with a `Content-Length` is `Found` with that length and the body's exact bytes;
/// the request is one GET of the path under the repository's `uri` carrying `Host`, a
/// `Connection: close` and the `Authorization` header with the credential's exact line, on a
/// connection to the `uri`'s host and its default port 443.
///
/// Catches: a credential not sent, sent under another name, or a path not joined to the `uri`'s.
#[tokio::test]
async fn a_200_is_streamed_with_its_length_and_carries_the_credential() {
    let fake = FakeOrigin::new();
    let origin = origin_over(&fake, Then::Write(response("200 OK", b"Release bytes")));
    let Origin::Found { length, body } = origin.get(URI, PATH, CREDENTIAL).await.expect("found")
    else {
        panic!("not found");
    };
    assert_eq!(length, 13);
    assert_eq!(body.size_hint().exact(), Some(13));
    let bytes = body.collect().await.expect("the body").to_bytes();
    assert_eq!(&bytes[..], b"Release bytes");
    assert_eq!(fake.dialed(), [("origin.test".to_owned(), 443)]);
    let heads = fake.heard();
    assert_eq!(heads.len(), 1);
    let lines: Vec<&str> = heads[0].lines().collect();
    assert_eq!(lines[0], "GET /apt/dists/web/InRelease HTTP/1.1");
    assert!(
        lines.contains(&"authorization: Bearer s3cret-token-value"),
        "{lines:?}"
    );
    assert!(lines.contains(&"host: origin.test"), "{lines:?}");
    assert!(lines.contains(&"connection: close"), "{lines:?}");
    assert!(
        lines.contains(&"user-agent: hostkeeper-coordinator"),
        "{lines:?}"
    );
}

/// Proves: a `uri` with a port and a trailing slash is dialled at that port and the path is under
/// it; a 404 is `NotFound`.
///
/// Catches: a repository on another port read on 443, or a path joined with `//`.
#[tokio::test]
async fn a_port_and_a_base_path_are_kept_and_a_404_is_not_found() {
    let fake = FakeOrigin::new();
    let origin = origin_over(&fake, Then::Write(response("404 Not Found", b"")));
    let found = origin
        .get("https://origin.test:8443/apt/", PATH, CREDENTIAL)
        .await
        .expect("answered");
    assert!(matches!(found, Origin::NotFound));
    assert_eq!(fake.dialed(), [("origin.test".to_owned(), 8443)]);
    assert!(fake.heard()[0].starts_with("GET /apt/dists/web/InRelease HTTP/1.1\r\n"));
}

/// Proves: any status but 200 and 404 is `Status(n)`, a redirect included, which is not followed
/// (one connection only).
///
/// Catches: a redirect that sends the credential to another host.
#[tokio::test]
async fn another_status_is_an_error_and_a_redirect_is_not_followed() {
    for code in [500_u16, 403, 301] {
        let fake = FakeOrigin::new();
        let text = format!("{code} Whatever");
        let origin = origin_over(&fake, Then::Write(response(&text, b"x")));
        assert_eq!(
            failure(&origin, URI, CREDENTIAL).await,
            OriginError::Status(code)
        );
        assert_eq!(fake.dials(), 1);
    }
}

/// Proves: a 200 without a `Content-Length` (chunked, or ended by closing) is `NoLength`, one whose
/// length is not a number too.
///
/// Catches: a body the daemon cannot size streamed to it.
#[tokio::test]
async fn an_answer_without_a_length_is_an_error() {
    for head in [
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"[..],
        b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabc",
    ] {
        let fake = FakeOrigin::new();
        let origin = origin_over(&fake, Then::Write(head.to_vec()));
        assert_eq!(
            failure(&origin, URI, CREDENTIAL).await,
            OriginError::NoLength
        );
    }
}

/// Proves: each way the read cannot start or finish has its own error: a `uri` that is not
/// https, a refused connection, a certificate no root trusts, an origin that never completes the
/// handshake or the headers (after the connect deadline), bytes that are no HTTP, and a credential
/// that cannot be a header value.
///
/// Catches: a failure reported as another, or a read that waits for ever.
#[tokio::test(start_paused = true)]
async fn every_way_a_read_fails_has_its_own_error() {
    let fake = FakeOrigin::new();
    let ok = origin_over(&fake, Then::Write(response("200 OK", b"x")));
    assert_eq!(
        failure(&ok, "http://origin.test/apt", CREDENTIAL).await,
        OriginError::Uri
    );
    assert_eq!(failure(&ok, URI, "Bearer a\nb").await, OriginError::Http);

    let refused = RepositoryOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(refusing_dial());
    assert_eq!(
        failure(&refused, URI, CREDENTIAL).await,
        OriginError::Connect
    );

    let other = FakeOrigin::new();
    let untrusted = RepositoryOrigin::new(other.roots())
        .expect("a client")
        .with_dial(fake.dial_all(Then::Write(response("200 OK", b"x"))));
    assert_eq!(
        failure(&untrusted, URI, CREDENTIAL).await,
        OriginError::Connect
    );

    let hanging = RepositoryOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(hanging_dial());
    assert_eq!(
        failure(&hanging, URI, CREDENTIAL).await,
        OriginError::ConnectTimedOut
    );

    let silent = origin_over(&fake, Then::Hang);
    assert_eq!(
        failure(&silent, URI, CREDENTIAL).await,
        OriginError::ConnectTimedOut
    );

    let garbage = origin_over(&fake, Then::Write(b"not http at all\r\n\r\n".to_vec()));
    assert_eq!(failure(&garbage, URI, CREDENTIAL).await, OriginError::Http);
}

/// Proves: a body that stops giving frames for the idle deadline ends with `Idle` after the frames
/// it gave, and one cut short of its length with `Broken`; there is no deadline over the whole.
///
/// Catches: a stalled origin that holds a host's connection for ever, or a truncated file that
/// ends as if it were whole.
#[tokio::test(start_paused = true)]
async fn a_body_that_stalls_or_breaks_ends_with_an_error() {
    let fake = FakeOrigin::new();
    let head = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabcd".to_vec();
    let stalled = origin_over(&fake, Then::Stall(head.clone()));
    let Origin::Found { body, .. } = stalled.get(URI, PATH, CREDENTIAL).await.expect("found")
    else {
        panic!("not found");
    };
    let mut body = Box::pin(body);
    let first = body.frame().await.expect("a frame").expect("data");
    assert_eq!(first.into_data().expect("data"), &b"abcd"[..]);
    let error = body.frame().await.expect("a result").expect_err("idle");
    assert_eq!(error, BodyError::Idle);

    let cut = origin_over(&fake, Then::Write(head));
    let Origin::Found { body, .. } = cut.get(URI, PATH, CREDENTIAL).await.expect("found") else {
        panic!("not found");
    };
    assert_eq!(body.collect().await.expect_err("cut"), BodyError::Broken);
}

/// Proves: an origin client is refused when its roots hold no certificate.
///
/// Catches: a coordinator that starts with an origin client that trusts nothing.
#[test]
fn a_client_needs_a_root() {
    let error =
        RepositoryOrigin::new(hostkeeper_oidc::Roots::Given(Vec::new())).expect_err("no roots");
    assert!(matches!(error, ChannelTlsError::NoRoots));
    assert_eq!(
        format!(
            "{:?}",
            RepositoryOrigin::new(FakeOrigin::new().roots()).expect("a client")
        ),
        "RepositoryOrigin { .. }"
    );
}
