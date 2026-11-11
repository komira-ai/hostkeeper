//! Tests of a store's request ([`Fetcher::get_request`]) over an in-memory transport: redirects,
//! the credential, and the token challenge, with the header parsing and the URL resolution they
//! rest on.

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::Mutex;

use super::*;
use crate::ports::Stop;
use crate::testing::FakeClock;

const BLOB: &str = "https://registry.example/v2/app/blobs/sha256:abc";
const STORAGE: &str = "https://storage.example/blob/abc";

/// What the transport answers to one request.
struct Answer {
    status: u16,
    location: Option<String>,
    www_authenticate: Option<String>,
    body: Vec<u8>,
}

fn answer(status: u16) -> Answer {
    Answer {
        status,
        location: None,
        www_authenticate: None,
        body: Vec::new(),
    }
}

fn body(bytes: &[u8]) -> Answer {
    Answer {
        body: bytes.to_vec(),
        ..answer(200)
    }
}

fn redirect(status: u16, to: &str) -> Answer {
    Answer {
        location: Some(to.to_owned()),
        ..answer(status)
    }
}

fn challenge(header: &str) -> Answer {
    Answer {
        www_authenticate: Some(header.to_owned()),
        ..answer(401)
    }
}

/// An answer per URL, in order (a URL asked more often than it has answers gets a 404), and the
/// URL and `Authorization` value of every request, in order.
#[derive(Default)]
struct Routes {
    answers: Mutex<HashMap<String, VecDeque<Answer>>>,
    seen: Mutex<Vec<(String, Option<String>)>>,
    on_open: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Routes {
    fn with(mut self, url: &str, answers: Vec<Answer>) -> Routes {
        self.answers
            .get_mut()
            .expect("the answers")
            .insert(url.to_owned(), answers.into());
        self
    }

    fn seen(&self) -> Vec<(String, Option<String>)> {
        self.seen.lock().expect("the log").clone()
    }
}

impl Transport for Routes {
    fn open(&self, url: &str, timeouts: Timeouts) -> Result<Response, NetError> {
        self.open_with(url, None, timeouts)
    }

    fn open_with(
        &self,
        url: &str,
        authorization: Option<&str>,
        _timeouts: Timeouts,
    ) -> Result<Response, NetError> {
        self.seen
            .lock()
            .expect("the log")
            .push((url.to_owned(), authorization.map(str::to_owned)));
        if let Some(hook) = &self.on_open {
            hook();
        }
        let next = self
            .answers
            .lock()
            .expect("the answers")
            .get_mut(url)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| answer(404));
        if next.status == 0 {
            return Err(NetError::Connect);
        }
        Ok(Response {
            status: next.status,
            content_length: None,
            location: next.location,
            www_authenticate: next.www_authenticate,
            body: Box::new(Cursor::new(next.body)),
        })
    }
}

/// Collects the body.
#[derive(Default)]
struct Collect(Vec<u8>);

impl Sink for Collect {
    fn begin(&mut self, _: Option<u64>) -> Result<(), Stop> {
        Ok(())
    }

    fn chunk(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

fn limit_of(clock: &FakeClock) -> Limit {
    Limit {
        connect_timeout: Duration::from_secs(5),
        deadline: clock.stamp(clock.mono_secs() + 100),
    }
}

/// A request for `url` at the registry's origin.
fn request<'a>(
    url: &'a Url,
    authorization: Option<&'a str>,
    redirects: u32,
    answer_challenge: bool,
) -> Request<'a> {
    Request {
        url,
        authorization,
        origin: "https://registry.example:443",
        redirects,
        answer_challenge,
    }
}

/// The requests a transport saw: URL and `Authorization`.
type Log = Vec<(String, Option<String>)>;

/// Runs `request` against `routes`: what it gave the sink, or its error, and the requests seen.
fn run(routes: Routes, request: &Request<'_>) -> (Result<Vec<u8>, NetError>, Log) {
    let clock = Arc::new(FakeClock::at(1));
    let limit = limit_of(&clock);
    let fetcher = NetFetcher::new(routes, clock);
    let mut sink = Collect::default();
    let got = fetcher.get_request(request, &mut sink, limit);
    let result = got.map(|got| {
        assert_eq!(got, Got::Complete);
        sink.0
    });
    (result, fetcher.transport.seen())
}

fn url(text: &str) -> Url {
    Url::new(text.to_owned())
}

fn pair(url: &str, authorization: Option<&str>) -> (String, Option<String>) {
    (url.to_owned(), authorization.map(str::to_owned))
}

/// Catches a credential not sent, or sent as another header value: the first request carries it.
#[test]
fn the_credential_is_sent_to_the_base_origin() {
    let at = url(BLOB);
    let routes = Routes::default().with(BLOB, vec![body(b"bytes")]);
    let (got, seen) = run(routes, &request(&at, Some("Basic dTpz"), 0, false));
    assert_eq!(got, Ok(b"bytes".to_vec()));
    assert_eq!(seen, [pair(BLOB, Some("Basic dTpz"))]);
}

/// Catches a transport that sends no credential header still being driven: the default
/// `open_with` ignores the header and calls `open`.
#[test]
fn a_transport_that_only_opens_ignores_the_header() {
    struct OpenOnly;
    impl Transport for OpenOnly {
        fn open(&self, _: &str, _: Timeouts) -> Result<Response, NetError> {
            Ok(Response {
                status: 200,
                content_length: Some(2),
                location: None,
                www_authenticate: None,
                body: Box::new(Cursor::new(b"ok".to_vec())),
            })
        }
    }
    let clock = Arc::new(FakeClock::at(1));
    let limit = limit_of(&clock);
    let fetcher = NetFetcher::new(OpenOnly, clock);
    let at = url(BLOB);
    let mut sink = Collect::default();
    let got = fetcher.get_request(&request(&at, Some("Basic x"), 0, false), &mut sink, limit);
    assert_eq!(got, Ok(Got::Complete));
    assert_eq!(sink.0, b"ok");
}

/// Catches the credential being dropped by a redirect inside the base origin, or a relative
/// `Location` not resolved against the URL it came from: an absolute path, a path relative to
/// the directory, a scheme-relative URL on the base origin and a fragment all keep the header.
#[test]
fn a_redirect_inside_the_base_origin_keeps_the_credential() {
    let hops = [
        ("/v2/other/blob", "https://registry.example/v2/other/blob"),
        (
            "next/blob",
            "https://registry.example/v2/app/blobs/next/blob",
        ),
        ("//registry.example/x", "https://registry.example/x"),
        (
            "https://REGISTRY.example:443/y#frag",
            "https://REGISTRY.example:443/y",
        ),
    ];
    for (location, expected) in hops {
        let at = url(BLOB);
        let routes = Routes::default()
            .with(BLOB, vec![redirect(307, location)])
            .with(expected, vec![body(b"after")]);
        let (got, seen) = run(routes, &request(&at, Some("Basic dTpz"), 3, false));
        assert_eq!(got, Ok(b"after".to_vec()), "{location}");
        assert_eq!(
            seen,
            [
                pair(BLOB, Some("Basic dTpz")),
                pair(expected, Some("Basic dTpz"))
            ],
            "{location}"
        );
    }
}

/// Catches a credential sent to another origin: after a redirect to the storage host no request
/// carries it, and a further redirect back to the base origin does not bring it back.
#[test]
fn the_credential_is_dropped_at_another_origin_and_never_added_back() {
    let at = url(BLOB);
    let back = "https://registry.example/v2/app/blobs/again";
    let routes = Routes::default()
        .with(BLOB, vec![redirect(302, STORAGE)])
        .with(STORAGE, vec![redirect(301, back)])
        .with(back, vec![body(b"late")]);
    let (got, seen) = run(routes, &request(&at, Some("Bearer t0k"), 3, false));
    assert_eq!(got, Ok(b"late".to_vec()));
    assert_eq!(
        seen,
        [
            pair(BLOB, Some("Bearer t0k")),
            pair(STORAGE, None),
            pair(back, None)
        ]
    );
}

/// Catches a redirect to a URL that is not `https://` being followed: `http://`, another scheme
/// and a path that leaves no URL are no connection, and nothing is requested after.
#[test]
fn a_redirect_to_anything_but_https_is_no_connection() {
    for location in [
        "http://registry.example/x",
        "ftp://registry.example/x",
        "file:///etc/passwd",
    ] {
        let at = url(BLOB);
        let routes = Routes::default().with(BLOB, vec![redirect(302, location)]);
        let (got, seen) = run(routes, &request(&at, Some("Basic x"), 3, false));
        assert_eq!(got, Err(NetError::Connect), "{location}");
        assert_eq!(seen.len(), 1, "{location}");
    }
}

/// Catches the redirect count being off by one or a redirect that is not one being followed: the
/// count is followed, the next redirect is its status, and a redirect without a `Location`, a
/// `300`, a `304` and a failing status are statuses.
#[test]
fn redirects_are_followed_up_to_the_count_and_the_next_is_the_status() {
    let second = "https://registry.example/second";
    let third = "https://registry.example/third";
    let at = url(BLOB);
    let chain = || {
        Routes::default()
            .with(BLOB, vec![redirect(301, second)])
            .with(second, vec![redirect(308, third)])
            .with(third, vec![body(b"end")])
    };
    let (got, seen) = run(chain(), &request(&at, None, 2, false));
    assert_eq!(got, Ok(b"end".to_vec()));
    assert_eq!(seen.len(), 3);
    let (got, seen) = run(chain(), &request(&at, None, 1, false));
    assert_eq!(got, Err(NetError::Status(308)));
    assert_eq!(seen.len(), 2);
    let (got, seen) = run(chain(), &request(&at, None, 0, false));
    assert_eq!(got, Err(NetError::Status(301)));
    assert_eq!(seen.len(), 1);
    for status in [300, 304, 500] {
        let routes = Routes::default().with(BLOB, vec![redirect(status, second)]);
        assert_eq!(
            run(routes, &request(&at, None, 3, false)).0,
            Err(NetError::Status(status))
        );
    }
    let routes = Routes::default().with(BLOB, vec![answer(302)]);
    assert_eq!(
        run(routes, &request(&at, None, 3, false)).0,
        Err(NetError::Status(302))
    );
}

/// The challenge of the registry in the tests.
const BEARER: &str = "Bearer realm=\"https://registry.example/token\",service=\"reg istry\",scope=\"repository:app:pull\"";

/// Catches a token challenge not answered, answered with the wrong query, without the basic
/// credential, or the retry sent without the token: the realm is asked with service and scope
/// percent-encoded and the basic credential, and the blob again with `Bearer <token>`. The
/// `access_token` key is read too.
#[test]
fn a_bearer_challenge_on_the_base_origin_is_answered_once() {
    let token_url =
        "https://registry.example/token?service=reg%20istry&scope=repository%3Aapp%3Apull";
    for key in ["token", "access_token"] {
        let at = url(BLOB);
        let json = format!("{{\"{key}\":\"abc.def\",\"expires_in\":60}}");
        let routes = Routes::default()
            .with(BLOB, vec![challenge(BEARER), body(b"blob")])
            .with(token_url, vec![body(json.as_bytes())]);
        let (got, seen) = run(routes, &request(&at, Some("Basic dTpz"), 3, true));
        assert_eq!(got, Ok(b"blob".to_vec()), "{key}");
        assert_eq!(
            seen,
            [
                pair(BLOB, Some("Basic dTpz")),
                pair(token_url, Some("Basic dTpz")),
                pair(BLOB, Some("Bearer abc.def")),
            ],
            "{key}"
        );
    }
}

/// Catches a second challenge being answered, a token asked for with a credential it should not
/// carry, or a bearer token kept across origins: an anonymous store asks anonymously, a second
/// 401 stands, and a redirect to the storage host after the retry drops the token.
#[test]
fn the_challenge_is_answered_once_anonymously_and_the_token_goes_no_further() {
    let token_url =
        "https://registry.example/token?service=reg%20istry&scope=repository%3Aapp%3Apull";
    let at = url(BLOB);
    let routes = Routes::default()
        .with(BLOB, vec![challenge(BEARER), challenge(BEARER)])
        .with(
            token_url,
            vec![body(b"{\"token\":\"t\"}"), body(b"{\"token\":\"u\"}")],
        );
    let (got, seen) = run(routes, &request(&at, None, 3, true));
    assert_eq!(got, Err(NetError::Status(401)));
    assert_eq!(
        seen,
        [
            pair(BLOB, None),
            pair(token_url, None),
            pair(BLOB, Some("Bearer t"))
        ]
    );

    let routes = Routes::default()
        .with(BLOB, vec![challenge(BEARER), redirect(307, STORAGE)])
        .with(token_url, vec![body(b"{\"token\":\"t\"}")])
        .with(STORAGE, vec![body(b"stored")]);
    let (got, seen) = run(routes, &request(&at, None, 3, true));
    assert_eq!(got, Ok(b"stored".to_vec()));
    assert_eq!(seen.last(), Some(&pair(STORAGE, None)));
}

/// Catches a challenge answered when it must not be: a realm on another origin (or another
/// scheme), a store that does not answer challenges, a bearer credential already sent, a
/// challenge that is not `Bearer`, has no realm, or comes from another origin, and no header.
#[test]
fn a_challenge_that_may_not_be_answered_leaves_the_401() {
    let at = url(BLOB);
    let cases: Vec<(&str, Option<&str>, bool, Option<&str>)> = vec![
        (
            "a realm on another origin",
            Some("Bearer realm=\"https://auth.example/token\",service=\"s\""),
            true,
            None,
        ),
        (
            "a realm on another port",
            Some("Bearer realm=\"https://registry.example:8443/token\""),
            true,
            None,
        ),
        (
            "a realm over http",
            Some("Bearer realm=\"http://registry.example/token\""),
            true,
            None,
        ),
        ("a store that answers none", Some(BEARER), false, None),
        (
            "a bearer credential already sent",
            Some(BEARER),
            true,
            Some("Bearer old"),
        ),
        (
            "a basic challenge",
            Some("Basic realm=\"registry\""),
            true,
            None,
        ),
        (
            "a challenge with no realm",
            Some("Bearer service=\"s\""),
            true,
            None,
        ),
        ("a scheme with no parameters", Some("Bearer"), true, None),
        ("no header", None, true, None),
    ];
    for (case, header, answer_challenge, authorization) in cases {
        let first = Answer {
            www_authenticate: header.map(str::to_owned),
            ..answer(401)
        };
        let routes = Routes::default().with(BLOB, vec![first]);
        let (got, seen) = run(routes, &request(&at, authorization, 3, answer_challenge));
        assert_eq!(got, Err(NetError::Status(401)), "{case}");
        assert_eq!(seen.len(), 1, "{case}");
    }
    let routes = Routes::default()
        .with(BLOB, vec![redirect(302, STORAGE)])
        .with(STORAGE, vec![challenge(BEARER)]);
    let (got, seen) = run(routes, &request(&at, None, 3, true));
    assert_eq!(got, Err(NetError::Status(401)));
    assert_eq!(
        seen.len(),
        2,
        "a challenge from another origin is not answered"
    );
}

/// Catches a token response trusted when it is not one: a failing status, JSON without a
/// usable token, a token that cannot be a header value, text that is not JSON and a body over
/// 64 KiB leave the 401; a token request that gets no connection is that error.
#[test]
fn a_token_response_that_is_not_a_token_leaves_the_401() {
    let token_url =
        "https://registry.example/token?service=reg%20istry&scope=repository%3Aapp%3Apull";
    let huge = format!("{{\"token\":\"t\",\"pad\":\"{}\"}}", "x".repeat(65_536));
    let responses: Vec<(&str, Answer)> = vec![
        ("a 500", answer(500)),
        ("a redirect", redirect(302, STORAGE)),
        ("no token", body(b"{\"expires_in\":5}")),
        ("a number", body(b"{\"token\":5}")),
        ("an empty token", body(b"{\"token\":\"\"}")),
        ("a token with a space", body(b"{\"token\":\"a b\"}")),
        ("not JSON", body(b"nope")),
        ("too much", body(huge.as_bytes())),
    ];
    for (case, response) in responses {
        let at = url(BLOB);
        let routes = Routes::default()
            .with(BLOB, vec![challenge(BEARER)])
            .with(token_url, vec![response]);
        let (got, seen) = run(routes, &request(&at, None, 3, true));
        assert_eq!(got, Err(NetError::Status(401)), "{case}");
        assert_eq!(seen.len(), 2, "{case}");
    }
    let at = url(BLOB);
    let routes = Routes::default()
        .with(BLOB, vec![challenge(BEARER)])
        .with(token_url, vec![answer(0)]);
    assert_eq!(
        run(routes, &request(&at, None, 3, true)).0,
        Err(NetError::Connect)
    );
}

/// Catches a request made after its deadline: a deadline that passes during a redirect ends the
/// fetch with the deadline error before the next request is sent, and so does a token request.
#[test]
fn the_deadline_is_checked_before_every_request() {
    let clock = Arc::new(FakeClock::at(1));
    let late = Arc::clone(&clock);
    let mut routes = Routes::default().with(BLOB, vec![redirect(302, STORAGE)]);
    routes.on_open = Some(Box::new(move || late.advance(1000)));
    let limit = limit_of(&clock);
    let fetcher = NetFetcher::new(routes, clock);
    let at = url(BLOB);
    let got = fetcher.get_request(
        &request(&at, None, 3, false),
        &mut Collect::default(),
        limit,
    );
    assert_eq!(got, Err(NetError::DeadlineExceeded));
    assert_eq!(fetcher.transport.seen().len(), 1);

    let clock = Arc::new(FakeClock::at(1));
    let late = Arc::clone(&clock);
    let mut routes = Routes::default().with(BLOB, vec![challenge(BEARER)]);
    routes.on_open = Some(Box::new(move || late.advance(1000)));
    let limit = limit_of(&clock);
    let fetcher = NetFetcher::new(routes, clock);
    let got = fetcher.get_request(&request(&at, None, 3, true), &mut Collect::default(), limit);
    assert_eq!(got, Err(NetError::DeadlineExceeded));
    assert_eq!(fetcher.transport.seen().len(), 1);
}

/// Catches a credential shown in `Debug`: a request prints its URL and settings, never its
/// `Authorization` value.
#[test]
fn a_request_does_not_show_its_credential() {
    let at = url(BLOB);
    let shown = format!("{:?}", request(&at, Some("Basic c2VjcmV0"), 2, true));
    assert!(shown.contains("<redacted>"), "{shown}");
    assert!(!shown.contains("c2VjcmV0"), "{shown}");
    let plain = Request::plain(&at);
    assert_eq!(
        (
            plain.authorization,
            plain.origin,
            plain.redirects,
            plain.answer_challenge
        ),
        (None, "", 0, false)
    );
}

/// Catches a challenge read wrongly: quoted and bare values, commas inside quotes, escapes, any
/// case of the scheme and of the keys, and values in any order; and the token URL built from
/// it, with a realm that has a query already, or no service or scope.
#[test]
fn challenges_are_read_and_their_token_urls_built() {
    let read = |text: &str| token_request(text).expect(text);
    let full = read("bearer Scope=\"a,b c\", SERVICE=reg, realm=\"https://r.example/t\", extra=1");
    assert_eq!(
        full,
        TokenRequest {
            realm: "https://r.example/t".to_owned(),
            service: Some("reg".to_owned()),
            scope: Some("a,b c".to_owned()),
        }
    );
    assert_eq!(
        full.url(),
        "https://r.example/t?service=reg&scope=a%2Cb%20c"
    );
    let escaped = read("Bearer realm=\"https://r.example/t?x=1\",scope=\"say \\\"hi\\\"\"");
    assert_eq!(escaped.scope.as_deref(), Some("say \"hi\""));
    assert_eq!(
        escaped.url(),
        "https://r.example/t?x=1&scope=say%20%22hi%22"
    );
    let bare = read("Bearer realm=\"https://r.example/t\"");
    assert_eq!(bare.url(), "https://r.example/t");
    let unterminated = read("Bearer realm=\"https://r.example/t\",scope=\"open");
    assert_eq!(unterminated.scope.as_deref(), Some("open"));
    assert!(token_request("Basic realm=\"x\"").is_none());
    assert!(token_request("Bearer").is_none());
    assert!(token_request("Bearer service=\"s\"").is_none());
    assert!(token_request("").is_none());
}

/// Catches a `Location` resolved to the wrong URL: every form, and the ones that are no URL.
#[test]
fn locations_are_resolved_against_the_url_they_came_from() {
    let base = "https://h.example:8443/a/b/c?q=1";
    let cases = [
        ("https://other.example/x", Some("https://other.example/x")),
        ("//other.example/x", Some("https://other.example/x")),
        ("/x/y", Some("https://h.example:8443/x/y")),
        ("d", Some("https://h.example:8443/a/b/d")),
        ("d#frag", Some("https://h.example:8443/a/b/d")),
        ("http://h.example/", None),
        ("ftp://h.example/", None),
        ("//user@h.example/", None),
    ];
    for (location, expected) in cases {
        assert_eq!(resolve(base, location).as_deref(), expected, "{location}");
    }
    assert_eq!(
        resolve("https://h.example", "x").as_deref(),
        Some("https://h.example/x")
    );
    assert_eq!(resolve("not a url", "x"), None);
}
