//! The read of a pkg's origin ([`crate::pkg_origin::PkgOrigin`]) against an origin served in
//! process ([`crate::repository_origin_support`]): the request, the redirects it follows and the
//! ones it refuses, and each way a read fails. The deadlines run on tokio's paused clock, which
//! moves only when every task waits.

use std::sync::Arc;

use futures::StreamExt as _;
use hostkeeper_oidc::Dial;

use super::{PkgOrigin, PkgOriginError, Target};
use crate::origin_https::OriginTlsError;
use crate::proxy_cache::{FetchError, Fetched};
use crate::repository_origin_support::{
    Answer, FakeOrigin, Then, hanging_dial, refusing_dial, response,
};

const URL: &str = "https://origin.test/releases/hostkeeper.pkg";

fn redirect(status: &str, location: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n").into_bytes()
}

fn client(fake: &FakeOrigin, dial: Dial) -> PkgOrigin {
    PkgOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(dial)
}

/// A client whose origin answers `answer` by path.
fn answering(
    fake: &FakeOrigin,
    answer: impl Fn(&str) -> Then + Send + Sync + 'static,
) -> PkgOrigin {
    client(fake, fake.dial(Arc::new(answer) as Answer))
}

async fn failure(origin: &PkgOrigin, url: &str, redirects: u64) -> PkgOriginError {
    match origin.get(url, redirects).await {
        Err(why) => why,
        Ok(_) => panic!("the read succeeded"),
    }
}

async fn bytes_of(fetched: Fetched) -> Result<Vec<u8>, FetchError> {
    let mut body = fetched.body;
    let mut all = Vec::new();
    while let Some(frame) = body.next().await {
        all.extend_from_slice(&frame?);
    }
    Ok(all)
}

/// Proves: a 200 is the body's exact bytes with the origin's `ETag` and `Content-Length`; the
/// request is one `GET` of the URL's path and query on a connection to its host and port, with
/// `Host`, `Connection: close` and no `Authorization`.
///
/// Catches: a credential sent to an origin that wants none, a query dropped from the path, and a
/// port ignored.
#[tokio::test]
async fn a_200_is_streamed_with_its_validator_and_length_and_no_credential() {
    let fake = FakeOrigin::new();
    let ok = b"HTTP/1.1 200 OK\r\nETag: \"abc\"\r\nContent-Length: 6\r\n\r\npkgpkg".to_vec();
    let origin = answering(&fake, move |_| Then::Write(ok.clone()));
    let fetched = origin
        .get("https://origin.test:8443/a/b.pkg?x=1", 3)
        .await
        .expect("a read");
    assert_eq!(fetched.validator.as_deref(), Some("\"abc\""));
    assert_eq!(fetched.length, Some(6));
    assert_eq!(bytes_of(fetched).await.expect("a body"), b"pkgpkg");
    assert_eq!(fake.dialed(), [("origin.test".to_owned(), 8443)]);
    let heads = fake.heard();
    let lines: Vec<&str> = heads[0].lines().collect();
    assert_eq!(lines[0], "GET /a/b.pkg?x=1 HTTP/1.1");
    assert!(lines.contains(&"host: origin.test:8443"), "{lines:?}");
    assert!(lines.contains(&"connection: close"), "{lines:?}");
    assert!(
        !lines
            .iter()
            .any(|line| line.to_lowercase().starts_with("authorization")),
        "{lines:?}"
    );
}

/// Proves: an answer with no `ETag` and no length (read to its close) has neither, and its body is
/// all that came.
///
/// Catches: a length invented for a body that declares none.
#[tokio::test]
async fn an_answer_with_no_validator_and_no_length_has_neither() {
    let fake = FakeOrigin::new();
    let origin = answering(&fake, |_| {
        Then::Write(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabc".to_vec())
    });
    let fetched = origin.get(URL, 3).await.expect("a read");
    assert_eq!((fetched.validator.clone(), fetched.length), (None, None));
    assert_eq!(bytes_of(fetched).await.expect("a body"), b"abc");
}

/// Proves: every redirect status is followed, to an absolute `https://` URL (with its signed
/// query) or to a path of the same host, one connection per hop, up to `redirects` of them; the
/// chain's last answer is the body.
///
/// Catches: a release asset (which GitHub redirects to a storage URL with a signature) that is
/// not fetched, and a redirect that drops its query.
#[tokio::test]
async fn redirects_are_followed_to_https_and_to_a_path_of_the_same_host() {
    for status in [
        "301 Moved",
        "302 Found",
        "303 See Other",
        "307 Temporary",
        "308 Permanent",
    ] {
        let fake = FakeOrigin::new();
        let (first, second) = (
            redirect(status, "/second/file.pkg"),
            redirect(status, "https://origin.test:9443/third?sig=a%2Fb&exp=1"),
        );
        let origin = answering(&fake, move |path| match path {
            "/releases/hostkeeper.pkg" => Then::Write(first.clone()),
            "/second/file.pkg" => Then::Write(second.clone()),
            _ => Then::Write(response("200 OK", b"the pkg")),
        });
        let fetched = origin.get(URL, 2).await.expect("a read");
        assert_eq!(
            bytes_of(fetched).await.expect("a body"),
            b"the pkg",
            "{status}"
        );
        assert_eq!(
            fake.dialed(),
            [
                ("origin.test".to_owned(), 443),
                ("origin.test".to_owned(), 443),
                ("origin.test".to_owned(), 9443)
            ]
        );
        assert!(fake.heard()[2].starts_with("GET /third?sig=a%2Fb&exp=1 HTTP/1.1\r\n"));
    }
}

/// Proves: more redirects than `redirects` is `Redirects(n)` with exactly `n` followed (a limit of
/// 0 follows none), and a chain of exactly `n` is read.
///
/// Catches: a redirect loop that never ends, and a limit that is off by one.
#[tokio::test]
async fn redirects_past_the_limit_are_refused() {
    let fake = FakeOrigin::new();
    let origin = answering(&fake, |_| Then::Write(redirect("302 Found", "/again")));
    assert_eq!(failure(&origin, URL, 0).await, PkgOriginError::Redirects(0));
    assert_eq!(fake.dials(), 1);
    assert_eq!(failure(&origin, URL, 3).await, PkgOriginError::Redirects(3));
    assert_eq!(fake.dials(), 1 + 4);
    let fake = FakeOrigin::new();
    let origin = answering(&fake, |path| match path {
        "/releases/hostkeeper.pkg" => Then::Write(redirect("302 Found", "/last")),
        _ => Then::Write(response("200 OK", b"x")),
    });
    assert!(origin.get(URL, 1).await.is_ok());
}

/// Proves: a redirect to `http://`, to a protocol-relative `//host/..`, to a relative path with no
/// leading slash, to a path a request line cannot carry, one with no `Location` and one whose
/// `Location` is not text is `Location`, and a redirect to a URL with a user is too; no second
/// connection is made.
///
/// Catches: a downgrade to plain HTTP, and a redirect that leaves the host by a spelling the check
/// does not know.
#[tokio::test]
async fn a_redirect_that_leaves_https_or_is_not_a_location_is_refused() {
    for location in [
        "http://origin.test/x.pkg",
        "//other.test/x.pkg",
        "x.pkg",
        "/with space",
        "https://user@origin.test/x.pkg",
        "ftp://origin.test/x.pkg",
        "",
    ] {
        let fake = FakeOrigin::new();
        let head = redirect("302 Found", location);
        let origin = answering(&fake, move |_| Then::Write(head.clone()));
        assert_eq!(
            failure(&origin, URL, 3).await,
            PkgOriginError::Location,
            "{location:?}"
        );
        assert_eq!(fake.dials(), 1, "{location:?}");
    }
    let fake = FakeOrigin::new();
    let origin = answering(&fake, |_| {
        Then::Write(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n".to_vec())
    });
    assert_eq!(failure(&origin, URL, 3).await, PkgOriginError::Location);
    let origin = answering(&fake, |_| {
        Then::Write(b"HTTP/1.1 302 Found\r\nLocation: \xff\r\nContent-Length: 0\r\n\r\n".to_vec())
    });
    assert_eq!(failure(&origin, URL, 3).await, PkgOriginError::Location);
}

/// Proves: a URL that is not `https://`, has a user, no host or is not a URL is `Url` and nothing
/// is dialled; a status but 200 and the redirects is `Status(n)`; the text of no error holds a URL.
///
/// Catches: a read over plain HTTP, a credential in a URL, and a failure text that leaks a
/// release host's signed query.
#[tokio::test]
async fn a_url_that_is_not_https_and_another_status_are_errors() {
    let fake = FakeOrigin::new();
    let origin = answering(&fake, |_| Then::Write(response("404 Not Found", b"")));
    for url in [
        "http://origin.test/x.pkg",
        "https://user@origin.test/x.pkg",
        "https:///x.pkg",
        "not a url",
        "/x.pkg",
    ] {
        assert_eq!(
            failure(&origin, url, 3).await,
            PkgOriginError::Url,
            "{url:?}"
        );
    }
    assert_eq!(fake.dials(), 0);
    for code in [404_u16, 500, 304] {
        let text = format!("{code} Whatever");
        let origin = answering(&fake, move |_| Then::Write(response(&text, b"")));
        let why = failure(&origin, URL, 3).await;
        assert_eq!(why, PkgOriginError::Status(code));
        assert_eq!(why.to_string(), format!("the origin answered {code}"));
    }
}

/// Proves: a refused connection, a handshake with a certificate the roots do not trust and an
/// answer that is not HTTP are each `Connect` or `Http`, and a connection that never comes and an
/// origin that never answers are `TimedOut`.
///
/// Catches: a read that hangs for ever on a dead origin, and one that trusts a certificate that
/// does not chain to the roots.
#[tokio::test(start_paused = true)]
async fn a_read_that_cannot_start_has_its_own_error() {
    let fake = FakeOrigin::new();
    let refused = client(&fake, refusing_dial());
    assert_eq!(failure(&refused, URL, 3).await, PkgOriginError::Connect);
    let stranger = FakeOrigin::new();
    let untrusted = PkgOrigin::new(stranger.roots())
        .expect("a client")
        .with_dial(fake.dial_all(Then::Write(response("200 OK", b"x"))));
    assert_eq!(failure(&untrusted, URL, 3).await, PkgOriginError::Connect);
    let garbage = answering(&fake, |_| Then::Write(b"not http at all\r\n\r\n".to_vec()));
    assert_eq!(failure(&garbage, URL, 3).await, PkgOriginError::Http);
    let closed = answering(&fake, |_| Then::Write(Vec::new()));
    assert_eq!(failure(&closed, URL, 3).await, PkgOriginError::Http);
    let hanging = client(&fake, hanging_dial());
    assert_eq!(failure(&hanging, URL, 3).await, PkgOriginError::TimedOut);
    let silent = answering(&fake, |_| Then::Hang);
    assert_eq!(failure(&silent, URL, 3).await, PkgOriginError::TimedOut);
}

/// Proves: a body that stops coming ends in `Failed` after the frames that did, and one that is cut
/// short of its length ends in `Failed` too, so the cache does not commit either.
///
/// Catches: a truncated pkg that ends as if whole.
#[tokio::test(start_paused = true)]
async fn a_body_that_stalls_or_breaks_ends_in_an_error() {
    let fake = FakeOrigin::new();
    let head = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".to_vec();
    for then in [Then::Stall(head.clone()), Then::Write(head)] {
        let origin = answering(&fake, move |_| then.clone());
        let fetched = origin.get(URL, 3).await.expect("headers");
        let mut body = fetched.body;
        assert_eq!(
            body.next().await,
            Some(Ok(bytes::Bytes::from_static(b"abc")))
        );
        assert!(matches!(
            body.next().await,
            Some(Err(FetchError::Failed(_)))
        ));
    }
}

/// Proves: `Target::follow` keeps the host, port and name for a path and takes the whole of an
/// `https://` URL; a client with no usable root is refused at its making, and the coordinator
/// holds the reader it is given.
///
/// Catches: a path redirect that moves to another port, and a start with a reader that trusts
/// nothing.
#[test]
fn a_path_redirect_keeps_the_host_and_a_client_needs_roots() {
    let at = Target::parse("https://origin.test:8443/a").expect("a target");
    let next = at.follow("/b?c=d").expect("a path");
    assert_eq!((next.host.as_str(), next.port), ("origin.test", 8443));
    assert_eq!(next.authority, "origin.test:8443");
    assert_eq!(next.path.as_str(), "/b?c=d");
    assert_eq!(at.follow("https://x.test/y").expect("a url").host, "x.test");
    assert_eq!(
        Target::parse("https://origin.test")
            .expect("a target")
            .path
            .as_str(),
        "/"
    );
    assert!(matches!(
        PkgOrigin::new(hostkeeper_oidc::Roots::Given(Vec::new())),
        Err(OriginTlsError::NoRoots)
    ));
    let fake = FakeOrigin::new();
    let origin = PkgOrigin::new(fake.roots()).expect("a client");
    assert_eq!(format!("{origin:?}"), "PkgOrigin { .. }");
}
