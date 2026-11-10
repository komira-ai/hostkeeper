//! The tests of `public`: the release list and assets read from GitHub with no token, the `ETag`
//! kept between runs, GitHub's limit of requests without a token; first against a scripted
//! [`Web`], then [`UreqWeb`] itself against the HTTPS fake of `https_tests` on loopback.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use super::Releases;
use super::https_tests::{Answer, Request, Server};
use super::public::*;
use crate::release::contract::Repo;
use crate::release::fakes::scratch;
use crate::release::ports::{PortError, Release, StoredAsset};

/// One GET as the scripted web saw it: URL, `Accept`, `If-None-Match`, limit.
type Seen = (String, String, Option<String>, Option<u64>);

/// One scripted answer: a head and the body of a 200, or a request that got no answer.
type Step = Result<(Head, Vec<u8>), PortError>;

/// A [`Web`] that answers from a script, in order, and records each GET.
#[derive(Default)]
struct Scripted {
    answers: RefCell<VecDeque<Step>>,
    seen: RefCell<Vec<Seen>>,
}

impl Scripted {
    fn then(self, status: u16, headers: &[(&str, &str)], body: &[u8]) -> Scripted {
        let mut head = Head {
            status,
            ..Head::default()
        };
        for (name, value) in headers {
            let value = Some((*value).to_owned());
            match *name {
                "etag" => head.etag = value,
                "x-ratelimit-remaining" => head.remaining = value,
                "x-ratelimit-reset" => head.reset = value,
                "retry-after" => head.retry_after = value,
                other => panic!("no such header in the script: {other}"),
            }
        }
        self.answers
            .borrow_mut()
            .push_back(Ok((head, body.to_vec())));
        self
    }

    fn fails(self, why: &str) -> Scripted {
        self.answers
            .borrow_mut()
            .push_back(Err(PortError::new("GET", why)));
        self
    }
}

impl Web for Scripted {
    fn get(&self, get: &Get<'_>, body: &mut dyn Write) -> Result<Head, PortError> {
        self.seen.borrow_mut().push((
            get.url.to_owned(),
            get.accept.to_owned(),
            get.if_none_match.map(str::to_owned),
            get.limit,
        ));
        let (head, bytes) = self
            .answers
            .borrow_mut()
            .pop_front()
            .expect("a scripted answer")?;
        if head.status == 200 {
            body.write_all(&bytes).expect("write the body");
        }
        Ok(head)
    }
}

const LIST_URL: &str = "https://api.example.com/repos/komira-ai/hostkeeper/releases?per_page=100";

/// The JSON of a list with the release v0.1.219.
fn list_json() -> Vec<u8> {
    json!([{
        "tag_name": "v0.1.219",
        "target_commitish": "0771c30f87f375ffd8b39e8b4630cd6ea377b764",
        "draft": false,
        "assets": [{"name": "SHA256SUMS", "digest": null}],
    }])
    .to_string()
    .into_bytes()
}

fn release() -> Release {
    Release {
        tag: "v0.1.219".to_owned(),
        commit: "0771c30f87f375ffd8b39e8b4630cd6ea377b764".to_owned(),
        draft: false,
        assets: vec![StoredAsset {
            name: "SHA256SUMS".to_owned(),
            sha256: None,
        }],
    }
}

fn source<'a>(web: &'a dyn Web, cache: &'a Path) -> PublicReleases<'a> {
    PublicReleases {
        web,
        api: "https://api.example.com",
        site: "https://site.example.com",
        cache,
    }
}

#[test]
fn the_list_is_kept_with_its_etag_and_a_304_is_answered_from_the_kept_list() {
    // Proves: the first run asks for the list whole (GitHub's JSON, bounded) and keeps it with
    // its ETag; the next asks with that ETag, and a 304 gives the same releases from the kept
    // list without changing it.
    let dir = scratch("cu-public-etag");
    let web = Scripted::default()
        .then(200, &[("etag", "W/\"one\"")], &list_json())
        .then(304, &[("etag", "W/\"one\"")], b"");
    let releases = source(&web, &dir);
    let repo = Repo::hostkeeper();
    assert_eq!(releases.list(&repo), Ok(vec![release()]));
    assert_eq!(
        std::fs::read(dir.join(LIST)).expect("the kept list"),
        list_json()
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(ETAG)).expect("the kept ETag"),
        "W/\"one\""
    );
    assert_eq!(releases.list(&repo), Ok(vec![release()]));
    assert_eq!(
        *web.seen.borrow(),
        vec![
            (
                LIST_URL.to_owned(),
                API_JSON.to_owned(),
                None,
                Some(MAX_LIST_BYTES)
            ),
            (
                LIST_URL.to_owned(),
                API_JSON.to_owned(),
                Some("W/\"one\"".to_owned()),
                Some(MAX_LIST_BYTES)
            ),
        ]
    );
    assert_eq!(
        std::fs::read(dir.join(LIST)).expect("the kept list"),
        list_json()
    );
}

#[test]
fn a_new_list_replaces_the_kept_one_and_one_without_an_etag_is_asked_for_whole_next() {
    // Proves: a 200 replaces the kept list; when it comes with no ETag the old ETag is gone, so
    // the next request has no If-None-Match (an old ETag beside a new list would make GitHub's
    // 304 answer with the wrong list); a list whose ETag file is missing is asked for whole.
    let dir = scratch("cu-public-replace");
    std::fs::write(dir.join(LIST), b"[]").expect("an old list");
    std::fs::write(dir.join(ETAG), "W/\"old\"\n").expect("its ETag");
    let web = Scripted::default()
        .then(200, &[], &list_json())
        .then(200, &[], b"[]");
    let releases = source(&web, &dir);
    let repo = Repo::hostkeeper();
    assert_eq!(releases.list(&repo), Ok(vec![release()]));
    assert!(!dir.join(ETAG).exists());
    assert_eq!(releases.list(&repo), Ok(Vec::new()));
    let sent: Vec<Option<String>> = web.seen.borrow().iter().map(|s| s.2.clone()).collect();
    assert_eq!(sent, [Some("W/\"old\"".to_owned()), None]);
}

/// The error of one list request answered by `script`.
fn list_error(at: &str, script: Scripted) -> FetchError {
    let dir = scratch(at);
    let releases = source(&script, &dir);
    releases
        .list(&Repo::hostkeeper())
        .expect_err("the list is refused")
}

#[test]
fn a_used_up_limit_is_rate_limited_and_any_other_refusal_is_a_failure() {
    // Proves: GitHub's answers for a used-up limit (403 with no requests remaining, 403 or 429
    // with Retry-After, any 429) are RateLimited, saying when to try again; a 403 that is not the
    // limit, a 404, a 304 to a request that kept no list, a body that is not GitHub's JSON, and a
    // request that got no answer are Failed, naming the request.
    let limited = |status: u16, headers: &[(&str, &str)]| -> String {
        let script = Scripted::default().then(status, headers, b"");
        match list_error("cu-public-limit", script) {
            FetchError::RateLimited(why) => why,
            other => panic!("not the limit: {other:?}"),
        }
    };
    let what = format!("GET {LIST_URL}");
    assert_eq!(
        limited(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1794337278")
            ]
        ),
        format!(
            "GitHub's limit of requests without a token is used up ({what}: HTTP 403, reset at 1794337278 (Unix time))"
        )
    );
    assert_eq!(
        limited(403, &[("retry-after", "60")]),
        format!(
            "GitHub's limit of requests without a token is used up ({what}: HTTP 403, retry after 60 seconds)"
        )
    );
    assert_eq!(
        limited(429, &[]),
        format!("GitHub's limit of requests without a token is used up ({what}: HTTP 429)")
    );

    let failed = |at: &str, script: Scripted| -> String {
        match list_error(at, script) {
            FetchError::Failed(e) => e.to_string(),
            other => panic!("not a failure: {other:?}"),
        }
    };
    let script = Scripted::default().then(403, &[("x-ratelimit-remaining", "12")], b"");
    assert_eq!(failed("cu-public-403", script), format!("{what}: HTTP 403"));
    let script = Scripted::default().then(404, &[], b"");
    assert_eq!(failed("cu-public-404", script), format!("{what}: HTTP 404"));
    let script = Scripted::default().then(304, &[], b"");
    assert_eq!(failed("cu-public-304", script), format!("{what}: HTTP 304"));
    let script = Scripted::default().fails("connection refused");
    assert_eq!(failed("cu-public-down", script), "GET: connection refused");
    let script = Scripted::default().then(200, &[], b"<html>");
    assert!(
        failed("cu-public-html", script).starts_with(&format!("{what}: not JSON: ")),
        "the body is named as not JSON"
    );
    let script = Scripted::default().then(200, &[], b"{}");
    assert_eq!(
        failed("cu-public-shape", script),
        format!("{what}: the reply is missing or not a list")
    );
}

#[test]
fn a_list_that_cannot_be_kept_fails_the_run() {
    // Proves: a download directory the list cannot be written to, or whose ETag cannot be
    // removed, is a failure that names the file, not a list quietly not kept.
    let dir = scratch("cu-public-keep");
    let gone = dir.join("missing");
    let web = Scripted::default().then(200, &[("etag", "x")], &list_json());
    let err = source(&web, &gone)
        .list(&Repo::hostkeeper())
        .expect_err("not kept");
    assert!(
        err.to_string()
            .starts_with(&format!("write {}: ", gone.join(LIST).display())),
        "{err}"
    );
    std::fs::create_dir(dir.join(ETAG)).expect("an ETag that is a directory");
    let web = Scripted::default().then(200, &[("etag", "x")], &list_json());
    let err = source(&web, &dir)
        .list(&Repo::hostkeeper())
        .expect_err("not kept");
    assert!(
        err.to_string()
            .starts_with(&format!("remove {}: ", dir.join(ETAG).display())),
        "{err}"
    );
}

#[test]
fn an_asset_is_fetched_from_its_public_download_url_and_a_refusal_leaves_no_file() {
    // Proves: an asset is GET <site>/<repo>/releases/download/<tag>/<name>, unbounded, written to
    // <dir>/<name>; a 404 fails, a used-up limit is RateLimited, and a request that got no
    // answer fails, each leaving no file; a file already there is never overwritten.
    let dir = scratch("cu-public-asset");
    let repo = Repo::hostkeeper();
    let web = Scripted::default()
        .then(200, &[], b"the sums")
        .then(404, &[], b"")
        .then(429, &[("retry-after", "30")], b"")
        .fails("reset by peer");
    let releases = source(&web, &dir);
    releases
        .download(&repo, "v0.1.219", "SHA256SUMS", &dir)
        .expect("the download");
    assert_eq!(
        std::fs::read(dir.join("SHA256SUMS")).expect("the file"),
        b"the sums"
    );
    let url = "https://site.example.com/komira-ai/hostkeeper/releases/download/v0.1.219/a.tar";
    assert_eq!(
        releases.download(&repo, "v0.1.219", "a.tar", &dir),
        Err(FetchError::Failed(PortError::new(
            format!("GET {url}"),
            "HTTP 404"
        )))
    );
    assert_eq!(
        releases.download(&repo, "v0.1.219", "a.tar", &dir),
        Err(FetchError::RateLimited(format!(
            "GitHub's limit of requests without a token is used up (GET {url}: HTTP 429, retry after 30 seconds)"
        )))
    );
    assert_eq!(
        releases.download(&repo, "v0.1.219", "a.tar", &dir),
        Err(FetchError::Failed(PortError::new("GET", "reset by peer")))
    );
    assert!(!dir.join("a.tar").exists());
    assert_eq!(
        web.seen.borrow()[0],
        (
            "https://site.example.com/komira-ai/hostkeeper/releases/download/v0.1.219/SHA256SUMS"
                .to_owned(),
            "application/octet-stream".to_owned(),
            None,
            None
        )
    );
    let err = releases
        .download(&repo, "v0.1.219", "SHA256SUMS", &dir)
        .expect_err("never overwrites");
    assert!(
        err.to_string()
            .starts_with(&format!("create {}: ", dir.join("SHA256SUMS").display())),
        "{err}"
    );
    assert_eq!(web.seen.borrow().len(), 4, "nothing was asked for it");
}

#[test]
fn the_roots_are_the_ca_bundles_certificates() {
    // Proves: --ca-bundle's certificates are the roots (its other PEM items ignored); a bundle
    // that cannot be read or holds no certificate is refused by name.
    let dir = scratch("cu-public-roots");
    let server = Server::start(Arc::new(|_: &str, _: &Request| Answer::new(404, "")));
    let pem = dir.join("ca.pem");
    let key = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIHVk\n-----END PRIVATE KEY-----\n";
    std::fs::write(&pem, format!("{}{key}", server.ca_pem())).expect("the bundle");
    assert_eq!(roots(&pem).expect("the roots").len(), 1);
    let missing = dir.join("missing.pem");
    let err = roots(&missing).expect_err("no file");
    assert!(
        err.to_string()
            .starts_with(&format!("--ca-bundle {}: ", missing.display())),
        "{err}"
    );
    let empty = dir.join("empty.pem");
    std::fs::write(&empty, key).expect("no certificate");
    assert_eq!(
        roots(&empty).map(|r| r.len()),
        Err(PortError::new(
            format!("--ca-bundle {}", empty.display()),
            "no certificate could be read"
        ))
    );
}

/// A client of the fake: it trusts the fake's CA only.
fn client(server: &Server) -> UreqWeb {
    let dir = scratch("cu-public-client");
    let pem = dir.join("ca.pem");
    std::fs::write(&pem, server.ca_pem()).expect("the CA");
    UreqWeb::new(&roots(&pem).expect("the roots"))
}

#[test]
fn the_https_client_sends_no_credential_reads_the_headers_and_writes_only_a_200_body() {
    // Proves: the real client sends Accept, If-None-Match and its User-Agent and no
    // Authorization; it reads the status, the ETag and the limit's headers; it writes the body
    // of a 200 and of nothing else; and it follows an https redirect to its target.
    let server = Server::start(Arc::new(|base: &str, request: &Request| {
        match request.path.as_str() {
            "/list" => Answer::new(200, "the body")
                .with("etag", "W/\"e\"")
                .with("x-ratelimit-remaining", "41")
                .with("x-ratelimit-reset", "1794337278"),
            "/limited" => Answer::new(403, "rate limited")
                .with("x-ratelimit-remaining", "0")
                .with("retry-after", "60"),
            "/moved" => Answer::new(302, "").with("location", &format!("{base}/list")),
            _ => Answer::new(404, "not here"),
        }
    }));
    let web = client(&server);
    let url = |path: &str| format!("{}{path}", server.url());
    let mut body = Vec::new();
    let list = url("/list");
    let head = web
        .get(
            &Get {
                url: &list,
                accept: API_JSON,
                if_none_match: Some("W/\"d\""),
                limit: Some(64),
            },
            &mut body,
        )
        .expect("an answer");
    assert_eq!(
        (head, body.as_slice()),
        (
            Head {
                status: 200,
                etag: Some("W/\"e\"".to_owned()),
                remaining: Some("41".to_owned()),
                reset: Some("1794337278".to_owned()),
                retry_after: None,
            },
            b"the body".as_slice()
        )
    );
    let first = &server.requests()[0];
    assert_eq!(first.path, "/list");
    assert_eq!(first.header("accept"), Some(API_JSON));
    assert_eq!(first.header("if-none-match"), Some("W/\"d\""));
    assert_eq!(first.header("user-agent"), Some(USER_AGENT));
    assert_eq!(first.header("authorization"), None);

    let mut body = Vec::new();
    let limited = url("/limited");
    let get = Get {
        url: &limited,
        accept: "application/octet-stream",
        if_none_match: None,
        limit: None,
    };
    let head = web.get(&get, &mut body).expect("an answer");
    assert_eq!(
        (head.status, head.remaining, head.retry_after, body.len()),
        (403, Some("0".to_owned()), Some("60".to_owned()), 0)
    );
    assert_eq!(server.requests()[1].header("if-none-match"), None);

    let mut body = Vec::new();
    let moved = url("/moved");
    let get = Get { url: &moved, ..get };
    assert_eq!(web.get(&get, &mut body).expect("an answer").status, 200);
    assert_eq!(body, b"the body");
    let paths: Vec<String> = server.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, ["/list", "/limited", "/moved", "/list"]);
}

#[test]
fn the_https_client_refuses_plain_http_an_untrusted_server_and_a_body_over_its_limit() {
    // Proves: a redirect to http://, a server whose certificate does not chain to the roots it
    // was given, and a body one byte longer than the request's limit are errors naming the
    // request, not answers (a body of exactly the limit is read); a URL that is not https:// is
    // never fetched.
    let server = Server::start(Arc::new(|_: &str, request: &Request| {
        match request.path.as_str() {
            "/plain" => Answer::new(302, "").with("location", "http://127.0.0.1:9/x"),
            _ => Answer::new(200, "0123456789"),
        }
    }));
    let web = client(&server);
    let get = |url: &str, limit: Option<u64>| {
        let mut body = Vec::new();
        web.get(
            &Get {
                url,
                accept: API_JSON,
                if_none_match: None,
                limit,
            },
            &mut body,
        )
    };
    let plain = format!("{}/plain", server.url());
    let err = get(&plain, None).expect_err("no plain http");
    assert!(
        err.to_string().starts_with(&format!("GET {plain}: ")),
        "{err}"
    );
    let err = get("http://127.0.0.1:9/x", None).expect_err("no plain http");
    assert!(
        err.to_string().starts_with("GET http://127.0.0.1:9/x: "),
        "{err}"
    );
    let long = format!("{}/long", server.url());
    let err = get(&long, Some(9)).expect_err("one byte over the limit");
    assert!(
        err.to_string().starts_with(&format!("GET {long}: ")),
        "{err}"
    );
    assert_eq!(
        get(&long, Some(10)).map(|h| h.status),
        Ok(200),
        "exactly the limit"
    );

    let other = Server::start(Arc::new(|_: &str, _: &Request| Answer::new(200, "")));
    let stranger = client(&other);
    let mut body = Vec::new();
    let err = stranger
        .get(
            &Get {
                url: &long,
                accept: API_JSON,
                if_none_match: None,
                limit: None,
            },
            &mut body,
        )
        .expect_err("not trusted");
    assert!(
        err.to_string().starts_with(&format!("GET {long}: ")),
        "{err}"
    );
}

#[test]
fn the_public_releases_read_github_over_https_with_no_token() {
    // Proves, end to end over TLS on loopback: the list's 200 and its ETag, the 304 of the next
    // run with nothing kept changed, and an asset whose download URL redirects to storage, with
    // no request carrying a credential.
    let server = Server::start(Arc::new(|base: &str, request: &Request| {
        let list = "/repos/komira-ai/hostkeeper/releases?per_page=100";
        match request.path.as_str() {
            path if path == list && request.header("if-none-match") == Some("W/\"1\"") => {
                Answer::new(304, "").with("etag", "W/\"1\"")
            }
            path if path == list => Answer::new(200, list_json()).with("etag", "W/\"1\""),
            "/komira-ai/hostkeeper/releases/download/v0.1.219/SHA256SUMS" => {
                Answer::new(302, "").with("location", &format!("{base}/storage/sums"))
            }
            "/storage/sums" => Answer::new(200, "the sums"),
            _ => Answer::new(404, ""),
        }
    }));
    let web = client(&server);
    let dir = scratch("cu-public-e2e");
    let base = server.url();
    let releases = PublicReleases {
        web: &web,
        api: &base,
        site: &base,
        cache: &dir,
    };
    let repo = Repo::hostkeeper();
    assert_eq!(releases.list(&repo), Ok(vec![release()]));
    assert_eq!(releases.list(&repo), Ok(vec![release()]));
    releases
        .download(&repo, "v0.1.219", "SHA256SUMS", &dir)
        .expect("the download");
    assert_eq!(
        std::fs::read(dir.join("SHA256SUMS")).expect("the file"),
        b"the sums"
    );
    let requests = server.requests();
    let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/repos/komira-ai/hostkeeper/releases?per_page=100",
            "/repos/komira-ai/hostkeeper/releases?per_page=100",
            "/komira-ai/hostkeeper/releases/download/v0.1.219/SHA256SUMS",
            "/storage/sums",
        ]
    );
    assert!(requests.iter().all(|r| r.header("authorization").is_none()));
}
