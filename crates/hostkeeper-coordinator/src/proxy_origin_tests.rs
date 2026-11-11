//! The package proxy's read of a public origin ([`crate::proxy_origin`]): what it sends, what it
//! makes of each answer, which redirects it follows and which it refuses. The origin is served in
//! process ([`crate::repository_origin_support`]); no socket is opened, and the deadlines run on
//! tokio's paused clock.

use std::sync::Arc;

use futures::StreamExt as _;
use hostkeeper_oidc::Dial;
use hyper::Method;

use crate::origin_https::OriginUrl;
use crate::proxy_cache::{ByteRange, Condition, FetchError, Fetched, Key, Request, Slice};
use crate::proxy_origin::ProxyOrigin;
use crate::repository_origin_support::{
    FakeOrigin, Then, bare, hanging_dial, refusing_dial, reply,
};

const BASE: &str = "https://origin.test/apt";

fn read(path: &str) -> Request {
    Request {
        key: Key::new(BASE, path),
        range: None,
        condition: None,
    }
}

fn client(fake: &FakeOrigin, dial: Dial) -> ProxyOrigin {
    ProxyOrigin::new(fake.roots())
        .expect("a client")
        .with_dial(dial)
}

/// A client whose origin answers by the head of the request.
fn serving(
    answer: impl Fn(&str) -> Then + Send + Sync + 'static,
) -> (FakeOrigin, Arc<ProxyOrigin>) {
    let fake = FakeOrigin::new();
    let dial = fake.dial_heads(Arc::new(answer));
    let client = Arc::new(client(&fake, dial));
    (fake, client)
}

fn base() -> OriginUrl {
    OriginUrl::parse(BASE).expect("a base URL")
}

async fn get(
    client: &ProxyOrigin,
    request: &Request,
    redirects: u64,
) -> Result<Fetched, FetchError> {
    client.read(&base(), request, redirects, Method::GET).await
}

async fn body_of(mut fetched: Fetched) -> Result<Vec<u8>, FetchError> {
    let mut all = Vec::new();
    while let Some(frame) = fetched.body.next().await {
        all.extend_from_slice(&frame?);
    }
    Ok(all)
}

fn failure(result: Result<Fetched, FetchError>) -> String {
    match result {
        Err(FetchError::Failed(why)) => why,
        Err(other) => panic!("not a failure: {other}"),
        Ok(_) => panic!("an answer"),
    }
}

/// Proves: a GET goes to the path under the base URL with the host, the identity encoding and a
/// closing connection and nothing else (no range, no condition), and a 200 is its body streamed
/// with the length, the `ETag` as the validator and the `Last-Modified` beside it.
///
/// Catches: a request that leaks a header of the client's, or a body that is held whole.
#[tokio::test]
async fn a_get_streams_the_body_with_its_validators() {
    let (fake, client) = serving(|_| {
        reply(
            "200 OK",
            &[
                ("ETag", "\"abc\""),
                ("Last-Modified", "Mon, 02 Nov 2026 10:00:00 GMT"),
            ],
            b"hello world",
        )
    });
    let fetched = get(&client, &read("dists/stable/Release"), 3)
        .await
        .expect("an answer");
    assert_eq!(fetched.validator.as_deref(), Some("\"abc\""));
    assert_eq!(
        fetched.last_modified.as_deref(),
        Some("Mon, 02 Nov 2026 10:00:00 GMT")
    );
    assert_eq!(fetched.length, Some(11));
    assert!(!fetched.unchanged);
    assert_eq!(fetched.slice, None);
    assert_eq!(body_of(fetched).await.expect("the body"), b"hello world");
    let heard = fake.heard();
    assert_eq!(heard.len(), 1);
    let head = heard[0].to_ascii_lowercase();
    assert!(head.starts_with("get /apt/dists/stable/release http/1.1"));
    for line in [
        "host: origin.test",
        "connection: close",
        "accept-encoding: identity",
        "user-agent: hostkeeper-coordinator",
    ] {
        assert!(head.contains(line), "{line} in {head}");
    }
    for absent in [
        "range:",
        "if-none-match:",
        "if-modified-since:",
        "authorization:",
    ] {
        assert!(!head.contains(absent), "{absent} in {head}");
    }
}

/// Proves: with only a `Last-Modified` the validator is that date.
///
/// Catches: a validator that is none for an origin without entity tags, which would make every
/// revalidation a full read.
#[tokio::test]
async fn a_last_modified_alone_is_the_validator() {
    let (_, client) = serving(|_| {
        reply(
            "200 OK",
            &[("Last-Modified", "Mon, 02 Nov 2026 10:00:00 GMT")],
            b"x",
        )
    });
    let fetched = get(&client, &read("a"), 3).await.expect("an answer");
    assert_eq!(
        fetched.validator.as_deref(),
        Some("Mon, 02 Nov 2026 10:00:00 GMT")
    );
}

/// Proves: a conditional read sends `If-None-Match` and `If-Modified-Since`, a 304 is `unchanged`
/// with no body, and a condition that is only a date sends only that.
///
/// Catches: a revalidation that cannot tell the origin what it holds.
#[tokio::test]
async fn a_conditional_read_sends_the_validators_and_a_304_is_unchanged() {
    let (fake, client) = serving(|_| bare(&["HTTP/1.1 304 Not Modified", "ETag: \"abc\""], b""));
    let mut request = read("dists/stable/Release");
    request.condition = Some(Condition {
        etag: Some("\"abc\"".to_owned()),
        last_modified: Some("Mon, 02 Nov 2026 10:00:00 GMT".to_owned()),
    });
    let fetched = get(&client, &request, 3).await.expect("an answer");
    assert!(fetched.unchanged);
    assert_eq!(fetched.validator.as_deref(), Some("\"abc\""));
    assert_eq!(body_of(fetched).await.expect("no body"), b"");
    request.condition = Some(Condition {
        etag: None,
        last_modified: Some("Mon, 02 Nov 2026 10:00:00 GMT".to_owned()),
    });
    get(&client, &request, 3).await.expect("an answer");
    request.condition = Some(Condition {
        etag: Some("\"abc\"".to_owned()),
        last_modified: None,
    });
    get(&client, &request, 3).await.expect("an answer");
    let heard: Vec<String> = fake
        .heard()
        .iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();
    assert!(heard[0].contains("if-none-match: \"abc\""));
    assert!(heard[0].contains("if-modified-since: mon, 02 nov 2026 10:00:00 gmt"));
    assert!(heard[2].contains("if-none-match: \"abc\""));
    assert!(!heard[2].contains("if-modified-since"));
    assert!(!heard[1].contains("if-none-match"));
    assert!(heard[1].contains("if-modified-since: mon, 02 nov 2026 10:00:00 gmt"));
}

/// Proves: a 304 to a read that was not conditional is a failure, and so is a validator that
/// cannot be sent as a header.
///
/// Catches: an empty body taken for an empty file.
#[tokio::test]
async fn a_304_to_an_unconditional_read_is_a_failure() {
    let (_, client) = serving(|_| bare(&["HTTP/1.1 304 Not Modified"], b""));
    assert_eq!(
        failure(get(&client, &read("a"), 3).await),
        "the origin answered 304"
    );
    let mut request = read("a");
    request.condition = Some(Condition {
        etag: Some("\"a\nb\"".to_owned()),
        last_modified: None,
    });
    assert_eq!(
        failure(get(&client, &request, 3).await),
        "a validator is not a header value"
    );
}

/// Proves: a range read sends `Range` for a closed and an open span, a 206 gives the slice the
/// `Content-Range` says (whole length known or `*`), a 206 without a usable `Content-Range` is a
/// failure, and a 200 to a range read is the whole object with no slice.
///
/// Catches: a partial body taken for a whole one, which the cache would commit.
#[tokio::test]
async fn a_range_read_sends_the_range_and_a_206_gives_its_slice() {
    let (fake, client) = serving(|head| {
        let head = head.to_ascii_lowercase();
        if head.contains("range: bytes=5-9") {
            reply(
                "206 Partial Content",
                &[("Content-Range", "bytes 5-9/20")],
                b"56789",
            )
        } else if head.contains("range: bytes=15-") {
            reply(
                "206 Partial Content",
                &[("Content-Range", "bytes 15-19/*")],
                b"fghij",
            )
        } else if head.contains("range: bytes=1-") {
            reply(
                "206 Partial Content",
                &[("Content-Range", "bytes nonsense")],
                b"x",
            )
        } else if head.contains("range: bytes=2-") {
            reply(
                "206 Partial Content",
                &[("Content-Range", "bytes 9-3/20")],
                b"x",
            )
        } else {
            reply("200 OK", &[], b"0123456789abcdefghij")
        }
    });
    let ranged = |start, end| Request {
        range: Some(ByteRange { start, end }),
        ..read("pool/a.deb")
    };
    let closed = get(&client, &ranged(5, Some(9)), 3).await.expect("206");
    assert_eq!(
        closed.slice,
        Some(Slice {
            start: 5,
            end: 9,
            total: Some(20)
        })
    );
    assert_eq!(body_of(closed).await.expect("body"), b"56789");
    let open = get(&client, &ranged(15, None), 3).await.expect("206");
    assert_eq!(
        open.slice,
        Some(Slice {
            start: 15,
            end: 19,
            total: None
        })
    );
    let expected = "the origin gave a 206 without a usable Content-Range";
    assert_eq!(failure(get(&client, &ranged(1, None), 3).await), expected);
    assert_eq!(failure(get(&client, &ranged(2, None), 3).await), expected);
    let whole = get(&client, &ranged(0, Some(3)), 3).await.expect("200");
    assert_eq!(whole.slice, None);
    assert_eq!(whole.length, Some(20));
    let heard = fake.heard().join("\n").to_ascii_lowercase();
    assert!(heard.contains("range: bytes=5-9"));
    assert!(heard.contains("range: bytes=15-\r\n"));
}

/// Proves: 404 is `NotFound`, and any other status (a server error, a client error) is a failure
/// that names the status.
///
/// Catches: a missing index answered as a broken origin, or a broken origin answered as missing.
#[tokio::test]
async fn a_404_is_not_found_and_other_statuses_fail() {
    let (_, client) = serving(|head| {
        if head.contains("/gone") {
            reply("404 Not Found", &[], b"")
        } else if head.contains("/denied") {
            reply("403 Forbidden", &[], b"")
        } else {
            reply("500 Oops", &[], b"")
        }
    });
    assert_eq!(
        get(&client, &read("gone"), 3).await.err(),
        Some(FetchError::NotFound)
    );
    assert_eq!(
        failure(get(&client, &read("denied"), 3).await),
        "the origin answered 403"
    );
    assert_eq!(
        failure(get(&client, &read("other"), 3).await),
        "the origin answered 500"
    );
}

/// Proves: a 200 with neither a length nor chunks is refused (a body cut short could not be
/// told from a whole one), and a chunked body is read to its end.
///
/// Catches: a truncated body committed to the cache as whole.
#[tokio::test]
async fn a_body_needs_a_length_or_chunks() {
    let (_, client) = serving(|head| {
        if head.contains("/chunked") {
            bare(
                &["HTTP/1.1 200 OK", "Transfer-Encoding: chunked"],
                b"5\r\nhello\r\n0\r\n\r\n",
            )
        } else {
            bare(&["HTTP/1.1 200 OK"], b"hello")
        }
    });
    assert_eq!(
        failure(get(&client, &read("plain"), 3).await),
        "the origin gave neither a length nor chunks"
    );
    let chunked = get(&client, &read("chunked"), 3).await.expect("an answer");
    assert_eq!(chunked.length, None);
    assert_eq!(body_of(chunked).await.expect("body"), b"hello");
}

/// Proves: a HEAD asks the origin with the method `HEAD` and gives its length and validators with
/// an empty body, with or without a `Content-Length`.
///
/// Catches: a HEAD that downloads the file.
#[tokio::test]
async fn a_head_gives_the_headers_and_no_body() {
    let (fake, client) = serving(|head| {
        if head.contains("/nolength") {
            bare(&["HTTP/1.1 200 OK", "ETag: \"h\""], b"")
        } else {
            bare(
                &["HTTP/1.1 200 OK", "Content-Length: 4096", "ETag: \"h\""],
                b"",
            )
        }
    });
    let sized = client
        .head(&base(), &read("pool/a.deb"), 3)
        .await
        .expect("headers");
    assert_eq!(sized.length, Some(4096));
    assert_eq!(sized.validator.as_deref(), Some("\"h\""));
    assert_eq!(body_of(sized).await.expect("empty"), b"");
    let unsized_ = client
        .head(&base(), &read("nolength"), 3)
        .await
        .expect("headers");
    assert_eq!(unsized_.length, None);
    assert!(fake.heard().iter().all(|head| head.starts_with("HEAD ")));
}

/// Proves: a redirect to an `https://` URL (another port, the query kept) and to a path of the
/// same host is followed, the second request is made to the place named, and the body is the last
/// one's.
///
/// Catches: a release asset that cannot be fetched because it is served from elsewhere.
#[tokio::test]
async fn a_redirect_is_followed_to_an_https_url_and_to_a_path() {
    let (fake, client) = serving(|head| {
        if head.contains("GET /apt/pool/a.deb ") {
            bare(
                &[
                    "HTTP/1.1 302 Found",
                    "Location: https://origin.test:8443/store/a.deb?token=1",
                ],
                b"",
            )
        } else if head.contains("GET /store/a.deb?token=1 ") {
            bare(
                &["HTTP/1.1 307 Temporary Redirect", "Location: /final/a.deb"],
                b"",
            )
        } else {
            reply("200 OK", &[("ETag", "\"end\"")], b"the file")
        }
    });
    let fetched = get(&client, &read("pool/a.deb"), 3)
        .await
        .expect("an answer");
    assert_eq!(fetched.validator.as_deref(), Some("\"end\""));
    assert_eq!(body_of(fetched).await.expect("body"), b"the file");
    assert_eq!(
        fake.dialed(),
        [
            ("origin.test".to_owned(), 443),
            ("origin.test".to_owned(), 8443),
            ("origin.test".to_owned(), 8443),
        ]
    );
    let heard = fake.heard();
    assert!(heard[1].contains("host: origin.test:8443"));
    assert!(heard[2].starts_with("GET /final/a.deb "));
}

/// Proves: a redirect to `http://` is refused with no request made to it, a chain longer than the
/// limit is refused (and one of the limit's length is followed), a limit of none follows none, and
/// a redirect without a `Location` is a failure.
///
/// Catches: a downgrade to plain HTTP, and a redirect loop.
#[tokio::test]
async fn a_redirect_to_http_or_past_the_limit_is_refused() {
    let (fake, client) = serving(|head| {
        let hop = |n: u8| format!("Location: /hop{n}");
        let redirect = |location: &str| bare(&["HTTP/1.1 301 Moved", location], b"");
        if head.contains("/apt/plain ") {
            redirect("Location: http://origin.test/insecure")
        } else if head.contains("/apt/none ") {
            bare(&["HTTP/1.1 302 Found"], b"")
        } else if head.contains("/apt/chain ") {
            redirect(&hop(1))
        } else if head.contains("/hop1 ") {
            redirect(&hop(2))
        } else if head.contains("/hop2 ") {
            redirect(&hop(3))
        } else {
            reply("200 OK", &[], b"end")
        }
    });
    assert_eq!(
        failure(get(&client, &read("plain"), 3).await),
        "a redirect to http was refused"
    );
    assert_eq!(fake.heard().len(), 1);
    assert_eq!(
        failure(get(&client, &read("none"), 3).await),
        "a redirect has no Location"
    );
    assert_eq!(
        failure(get(&client, &read("chain"), 2).await),
        "too many redirects"
    );
    assert!(get(&client, &read("chain"), 3).await.is_ok());
    assert_eq!(
        failure(get(&client, &read("chain"), 0).await),
        "too many redirects"
    );
}

/// Proves: a `Location` that is not an `https://` URL or a path of the host (another scheme, a
/// network-path reference, a URL with a user, a URL with no host, one that is not a URL, one over
/// 4096 bytes) is refused.
///
/// Catches: a redirect that sends the proxy somewhere the origin's own answer should not.
#[tokio::test]
async fn a_redirect_to_anything_else_is_refused() {
    let long = format!("/{}", "a".repeat(5000));
    for (location, why) in [
        (
            "ftp://origin.test/x",
            "a redirect is not an https URL or a path",
        ),
        (
            "//elsewhere.test/x",
            "a redirect is not an https URL or a path",
        ),
        ("relative/x", "a redirect is not an https URL or a path"),
        ("https://user@origin.test/x", "a redirect has a user"),
        ("https:///x", "a redirect is not a URL"),
        (
            "https://[::1]/x",
            "a redirect names no host a certificate can be checked for",
        ),
        ("https://example.test mple/x", "a redirect is not a URL"),
        (long.as_str(), "a redirect is too long"),
        ("/a path with spaces", "the request failed"),
    ] {
        let location = location.to_owned();
        let (_, client) = serving(move |_| {
            bare(
                &["HTTP/1.1 302 Found", &format!("Location: {location}")],
                b"",
            )
        });
        assert_eq!(failure(get(&client, &read("a"), 3).await), why);
    }
}

/// Proves: a refused connection, a handshake the origin's certificate fails, an origin that closes
/// without answering and one that never answers are each a failure naming the step; the last on
/// the paused clock.
///
/// Catches: a read that waits for ever on a dead origin.
#[tokio::test(start_paused = true)]
async fn a_dead_origin_is_a_failure_that_names_the_step() {
    let fake = FakeOrigin::new();
    let refused = client(&fake, refusing_dial());
    assert_eq!(
        failure(get(&refused, &read("a"), 3).await),
        "the connection failed"
    );
    let other = FakeOrigin::new();
    let distrusting = client(&other, fake.dial_all(reply("200 OK", &[], b"x")));
    assert_eq!(
        failure(get(&distrusting, &read("a"), 3).await),
        "the TLS handshake failed"
    );
    let silent = client(&fake, fake.dial_all(Then::Write(Vec::new())));
    assert_eq!(
        failure(get(&silent, &read("a"), 3).await),
        "the HTTP exchange failed"
    );
    let hanging = client(&fake, hanging_dial());
    assert_eq!(
        failure(get(&hanging, &read("a"), 3).await),
        "the connection timed out"
    );
    let never = client(&fake, fake.dial_all(Then::Hang));
    assert_eq!(
        failure(get(&never, &read("a"), 3).await),
        "the connection timed out"
    );
}

/// Proves: a body that stops giving frames fails after the idle deadline with the text of the
/// deadline, and the frames before it were given.
///
/// Catches: a fill held open for ever by an origin that went quiet.
#[tokio::test(start_paused = true)]
async fn a_body_that_goes_quiet_fails_after_the_idle_deadline() {
    let (_, client) =
        serving(|_| Then::Stall(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".to_vec()));
    let mut fetched = get(&client, &read("a"), 3).await.expect("headers");
    assert_eq!(
        fetched.body.next().await.expect("a frame").expect("data"),
        &b"abc"[..]
    );
    let why = fetched
        .body
        .next()
        .await
        .expect("an end")
        .expect_err("a failure");
    assert_eq!(
        why,
        FetchError::Failed("the origin gave no data for 30 seconds".to_owned())
    );
}

/// Proves: `Content-Range` is read as `bytes first-last/total` or `/*` and nothing else.
///
/// Catches: a slice taken from a malformed header.
#[test]
fn a_content_range_is_read_whole_or_not_at_all() {
    use crate::proxy_origin::slice_of;
    let slice = |start, end, total| Some(Slice { start, end, total });
    assert_eq!(slice_of("bytes 0-9/10"), slice(0, 9, Some(10)));
    assert_eq!(slice_of("bytes 5-6/*"), slice(5, 6, None));
    for bad in [
        "items 0-9/10",
        "bytes 0-9",
        "bytes 0/10",
        "bytes a-9/10",
        "bytes 0-b/10",
        "bytes 0-9/x",
        "bytes 9-0/10",
    ] {
        assert_eq!(slice_of(bad), None, "{bad}");
    }
}

/// Proves: the client prints as itself and shows no dial or root.
///
/// Catches: a debug print that spills the TLS configuration into a log.
#[test]
fn the_client_prints_without_its_configuration() {
    let fake = FakeOrigin::new();
    let client = ProxyOrigin::new(fake.roots()).expect("a client");
    assert_eq!(format!("{client:?}"), "ProxyOrigin { .. }");
}
