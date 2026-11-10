//! The rows that are the fetcher's own.

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use hostkeeper_package::PackageFailure;
use hostkeeper_package::entry::Pinned;
use hostkeeper_package::net::{NetFetcher, Response, Timeouts, Transport};
use hostkeeper_package::ports::NetError;
use hostkeeper_proto::hostkeeper::release::v1::Target;

use super::support::*;

// The rows below are the fetcher's own: the real `NetFetcher` over an in-memory transport.

/// One answer of the in-memory network.
struct Answer {
    status: u16,
    location: Option<String>,
    www_authenticate: Option<String>,
    body: Vec<u8>,
}

fn ok(body: &[u8]) -> Answer {
    Answer {
        status: 200,
        location: None,
        www_authenticate: None,
        body: body.to_vec(),
    }
}

fn moved(to: &str) -> Answer {
    Answer {
        status: 307,
        location: Some(to.to_owned()),
        www_authenticate: None,
        body: Vec::new(),
    }
}

fn challenge(header: &str) -> Answer {
    Answer {
        status: 401,
        location: None,
        www_authenticate: Some(header.to_owned()),
        body: Vec::new(),
    }
}

/// An answer per URL in order (404 after), and every request with its `Authorization`.
#[derive(Default)]
struct Network {
    answers: Mutex<HashMap<String, VecDeque<Answer>>>,
    asked: Mutex<Vec<(String, Option<String>)>>,
}

impl Network {
    fn with(self, url: &str, answers: Vec<Answer>) -> Network {
        self.answers
            .lock()
            .expect("answers")
            .insert(url.to_owned(), answers.into());
        self
    }
}

/// The network, shared with the test that reads its log.
struct Shared(Arc<Network>);

impl Transport for Shared {
    fn open(&self, url: &str, timeouts: Timeouts) -> Result<Response, NetError> {
        self.open_with(url, None, timeouts)
    }

    fn open_with(
        &self,
        url: &str,
        authorization: Option<&str>,
        _timeouts: Timeouts,
    ) -> Result<Response, NetError> {
        self.0
            .asked
            .lock()
            .expect("log")
            .push((url.to_owned(), authorization.map(str::to_owned)));
        let next = self
            .0
            .answers
            .lock()
            .expect("answers")
            .get_mut(url)
            .and_then(VecDeque::pop_front);
        let next = next.unwrap_or(Answer {
            status: 404,
            location: None,
            www_authenticate: None,
            body: Vec::new(),
        });
        Ok(Response {
            status: next.status,
            content_length: None,
            location: next.location,
            www_authenticate: next.www_authenticate,
            body: Box::new(Cursor::new(next.body)),
        })
    }
}

/// The `lan` store's URL for the repository `team/app` and the blob whose bytes are `bytes`.
fn lan(repository: &str, bytes: &[u8]) -> String {
    format!(
        "https://lan.example/v2/{repository}/blobs/sha256:{}",
        sha256(bytes)
    )
}

/// An entry with its artifact at the `lan` store (an oci store with a basic credential).
fn at_lan() -> Target {
    entry_of(ARCHIVE, &[("lan", "team/app")])
}

/// Runs the entry through `NetFetcher` over `network`; returns the result and the requests.
/// A result and the requests that led to it.
type Seen = (
    Result<Pinned, PackageFailure>,
    Vec<(String, Option<String>)>,
);

fn through_net(host: &Host, network: Network, target: &Target) -> Seen {
    let network = Arc::new(network);
    let fetcher = NetFetcher::new(Shared(Arc::clone(&network)), host.clock.clone());
    let result = host.run(&fetcher, target);
    let asked = network.asked.lock().expect("log").clone();
    (result, asked)
}

const BASIC: &str = "Basic dXNlcjpzZWNyZXQ=";

fn pair(url: &str, authorization: Option<&str>) -> (String, Option<String>) {
    (url.to_owned(), authorization.map(str::to_owned))
}

/// The credential file's value reaches the store's base origin as `Authorization`, and a
/// redirect inside that origin keeps it while one to another origin (the registry's blob storage)
/// drops it.
///
/// Catches a credential not sent, sent to the storage host, or lost on a same-origin hop.
#[test]
fn the_credential_goes_to_the_base_origin_and_not_past_a_cross_origin_redirect() {
    let host = Host::new("net-credential", false);
    let artifact = lan("team/app", ARCHIVE);
    let inside = "https://lan.example/inside/blob";
    let storage = "https://storage.example/blob/1";
    let network = Network::default()
        .with(&artifact, vec![moved(inside)])
        .with(inside, vec![moved(storage)])
        .with(storage, vec![ok(ARCHIVE)]);
    let (result, asked) = through_net(&host, network, &at_lan());
    result.expect("pinned");
    assert_eq!(
        asked,
        [
            pair(&artifact, Some(BASIC)),
            pair(inside, Some(BASIC)),
            pair(storage, None),
        ]
    );
}

/// A redirect to `http://` is no connection (the source is down), a redirect past the store's
/// count is its status (the source is down too), the count itself is followed: an `oci` store
/// follows 3 and the `https` store `b` follows 1.
///
/// Catches a redirect off HTTPS followed, and a count that is off by one in either direction.
#[test]
fn redirects_are_https_only_and_bounded_by_the_stores_count() {
    let host = Host::new("net-redirects", false);
    let artifact = lan("team/app", ARCHIVE);
    let hop = |n: u32| format!("https://storage.example/hop/{n}");

    let network = Network::default().with(&artifact, vec![moved("http://storage.example/blob")]);
    let (result, asked) = through_net(&host, network, &at_lan());
    assert_eq!(result.expect_err("refused"), deferred("fetch_failed"));
    assert_eq!(asked.len(), 1);

    let chain = |hops: u32| {
        let mut network = Network::default().with(&artifact, vec![moved(&hop(1))]);
        for n in 1..=hops {
            let next = if n == hops {
                ok(ARCHIVE)
            } else {
                moved(&hop(n + 1))
            };
            network = network.with(&hop(n), vec![next]);
        }
        network
    };
    let (result, _) = through_net(&host, chain(3), &at_lan());
    result.expect("three redirects are followed");
    let (result, asked) = through_net(&host, chain(4), &at_lan());
    assert_eq!(
        result.expect_err("one past the count"),
        deferred("fetch_failed")
    );
    assert_eq!(asked.len(), 4);

    let on_b = entry_of(ARCHIVE, &[("b", ARTIFACT_AT)]);
    let b_url = format!("https://b.example/{ARTIFACT_AT}");
    let network = Network::default()
        .with(&b_url, vec![moved(&hop(1))])
        .with(&hop(1), vec![ok(ARCHIVE)]);
    through_net(&host, network, &on_b)
        .0
        .expect("one redirect is followed");
    let network = Network::default()
        .with(&b_url, vec![moved(&hop(1))])
        .with(&hop(1), vec![moved(&hop(2))])
        .with(&hop(2), vec![ok(ARCHIVE)]);
    let (result, asked) = through_net(&host, network, &on_b);
    assert_eq!(
        result.expect_err("one past the count"),
        deferred("fetch_failed")
    );
    assert_eq!(asked.len(), 2);
}

/// An OCI `401` with a `Bearer` challenge whose realm is on the store's origin is answered with
/// one token request carrying the basic credential, and the blob is asked again with the token;
/// a realm on another origin is not answered: the source is down and no token is asked for.
///
/// Catches a token fetched from a host the registry did not name... the store did not trust, and
/// a challenge that is not answered at all.
#[test]
fn a_token_challenge_is_answered_on_the_stores_origin_only() {
    let host = Host::new("net-challenge", false);
    let artifact = lan("team/app", ARCHIVE);
    let own = "Bearer realm=\"https://lan.example/token\",service=\"lan\",scope=\"repository:team/app:pull\"";
    let token_url = "https://lan.example/token?service=lan&scope=repository%3Ateam%2Fapp%3Apull";
    let network = Network::default()
        .with(&artifact, vec![challenge(own), ok(ARCHIVE)])
        .with(token_url, vec![ok(b"{\"token\":\"tok\"}")]);
    let (result, asked) = through_net(&host, network, &at_lan());
    result.expect("pinned");
    assert_eq!(
        asked[..3],
        [
            pair(&artifact, Some(BASIC)),
            pair(token_url, Some(BASIC)),
            pair(&artifact, Some("Bearer tok")),
        ]
    );

    let foreign = "Bearer realm=\"https://auth.example/token\",service=\"lan\",scope=\"x\"";
    let network = Network::default().with(&artifact, vec![challenge(foreign)]);
    let (result, asked) = through_net(&host, network, &at_lan());
    assert_eq!(result.expect_err("not answered"), deferred("fetch_failed"));
    assert_eq!(asked, [pair(&artifact, Some(BASIC))]);
}
