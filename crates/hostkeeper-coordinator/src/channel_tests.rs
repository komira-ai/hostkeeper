//! The release channel's check of a set ([`crate::channel`]) over a fake fetcher, and the
//! channel's base URL ([`crate::channel_https::ChannelUrl`]): which answers of the channel
//! refuse a set with which token. Nothing here touches a network.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures::FutureExt as _;
use futures::future::BoxFuture;
use serde_json::json;

use crate::channel::{
    CHANNEL_DIGEST_MISMATCH, ChannelFetcher, ChannelRefusal, FetchError, Fetched, MAX_INDEX_BYTES,
    NOT_IN_CHANNEL, ReleaseChannel, index_path,
};
use crate::channel_https::{ChannelUrl, ChannelUrlError};
use crate::desired::Desired;
use crate::registry::Digest;
use crate::testutil::{desired, version};

pub(crate) const D1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
pub(crate) const D2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

/// A release channel in memory: what it answers for each path (404 for any other), and every
/// path it was asked for, in order.
#[derive(Default)]
pub(crate) struct FakeChannel {
    answers: Mutex<BTreeMap<String, Result<Fetched, FetchError>>>,
    asked: Mutex<Vec<String>>,
}

impl FakeChannel {
    /// Answers `answer` for `path` from now on.
    pub(crate) fn answer(&self, path: &str, answer: Result<Fetched, FetchError>) {
        self.answers
            .lock()
            .expect("the answers")
            .insert(path.to_owned(), answer);
    }

    /// Holds the index `index` of `version` of `service`.
    pub(crate) fn hold(&self, service: &str, at: &str, index: &serde_json::Value) {
        let body = serde_json::to_vec(index).expect("JSON");
        self.answer(&index_path(service, &version(at)), Ok(Fetched::Found(body)));
    }

    /// Every path asked for, in order.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("the paths").clone()
    }
}

impl ChannelFetcher for FakeChannel {
    fn fetch<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Fetched, FetchError>> {
        self.asked.lock().expect("the paths").push(path.to_owned());
        let answer = self
            .answers
            .lock()
            .expect("the answers")
            .get(path)
            .cloned()
            .unwrap_or(Ok(Fetched::NotFound));
        async move { answer }.boxed()
    }
}

/// The index the channel serves for `version` of `service`, one package per digest.
pub(crate) fn index(service: &str, version: &str, digests: &[&str]) -> serde_json::Value {
    let platforms = ["linux-arm64", "linux-x86_64", "macos-arm64"];
    let packages: Vec<serde_json::Value> = digests
        .iter()
        .zip(platforms)
        .map(|(digest, platform)| {
            json!({
                "bytes": 1024,
                "file": format!("{service}-{version}-{platform}.tar.zst"),
                "platform": platform,
                "sha256": digest,
            })
        })
        .collect();
    json!({"packages": packages, "schema": 1, "service": service, "version": version})
}

/// `target` desired, naming the package digest `digest` when there is one.
fn wanting(target: &str, digest: Option<&str>) -> Desired {
    let mut wanted = desired(target, 0, 0);
    wanted.release_digest = digest.map(|d| Digest::parse(d).expect("a digest"));
    wanted
}

fn url(path: &str) -> String {
    format!("https://channel.example/{path}")
}

/// Catches: an index of a version the channel holds being refused, or one with the set's
/// digest among its packages, in any case of hex, being refused; a set with no digest compared
/// against one; the index looked for anywhere but `{service}/{version}/index.json`.
#[tokio::test]
async fn a_version_the_channel_holds_with_the_sets_digest_passes() {
    let fake = Arc::new(FakeChannel::default());
    let channel = ReleaseChannel::new(fake.clone());
    fake.hold(
        "tierstore",
        "1.5.0",
        &index("tierstore", "1.5.0", &[D2, D1]),
    );
    for digest in [None, Some(D1), Some(D2)] {
        let wanted = wanting("1.5.0", digest);
        assert_eq!(
            channel.check("tierstore", &wanted).await,
            Ok(()),
            "{digest:?}"
        );
    }
    let upper = D1.to_ascii_uppercase().replace('1', "A");
    let lower = upper.to_ascii_lowercase();
    fake.hold(
        "tierstore",
        "1.6.0",
        &index("tierstore", "1.6.0", &[&upper]),
    );
    assert_eq!(
        channel
            .check("tierstore", &wanting("1.6.0", Some(&lower)))
            .await,
        Ok(())
    );
    assert_eq!(
        fake.asked(),
        [
            "tierstore/1.5.0/index.json",
            "tierstore/1.5.0/index.json",
            "tierstore/1.5.0/index.json",
            "tierstore/1.6.0/index.json",
        ]
    );
    assert_eq!(format!("{channel:?}"), "ReleaseChannel { .. }");
}

/// Catches: a set refused for the wrong reason when the channel holds another artifact: an
/// index of another version or another service, or one none of whose packages has the set's
/// digest, refused as retryable (`not_in_channel`) or let through.
#[tokio::test]
async fn an_index_that_disagrees_with_the_set_is_a_digest_mismatch() {
    let fake = Arc::new(FakeChannel::default());
    let channel = ReleaseChannel::new(fake.clone());
    let cases = [
        ("2.0", index("kbf", "2.0", &[D1]), Some(D2)),
        ("2.0", index("kbf", "2.0", &[D1, D1]), Some(D2)),
        ("2.1", index("kbf", "2.0", &[D1]), None),
        ("2.2", index("tierstore", "2.2", &[D1]), Some(D1)),
        ("2.3", index("kbf", "2.30", &[D1]), Some(D1)),
    ];
    for (at, served, digest) in cases {
        fake.hold("kbf", at, &served);
        let refused = channel
            .check("kbf", &wanting(at, digest))
            .await
            .expect_err(at);
        assert_eq!(refused, ChannelRefusal::DigestMismatch, "{at}");
        assert_eq!(refused.token(), CHANNEL_DIGEST_MISMATCH);
        assert_eq!(
            refused.to_string(),
            "channel_digest_mismatch: the release channel's index of the version does not \
             match the set"
        );
    }
}

/// Catches: a channel that does not hold the version, holds something that is no index or an
/// index of no package, or cannot be read in any way (each error of the fetcher: a refused or
/// broken connection, the connect deadline, the total deadline, an answer over the size cap,
/// a redirect or another status, HTTP failing mid-answer) letting the set through or refusing
/// it as non-retryable.
#[tokio::test]
async fn a_version_the_channel_does_not_hold_or_cannot_say_is_not_in_channel() {
    let fake = Arc::new(FakeChannel::default());
    let channel = ReleaseChannel::new(fake.clone());
    let path = index_path("kbf", &version("2.0"));
    let at = url(&path);
    let answers = [
        Ok(Fetched::NotFound),
        Ok(Fetched::Found(b"not json".to_vec())),
        Ok(Fetched::Found(b"{\"service\":\"kbf\"}".to_vec())),
        Ok(Fetched::Found(
            serde_json::to_vec(&index("kbf", "2.0", &[])).expect("JSON"),
        )),
        Err(FetchError::Connect {
            url: at.clone(),
            why: "connection refused".to_owned(),
        }),
        Err(FetchError::ConnectTimedOut {
            url: at.clone(),
            secs: 10,
        }),
        Err(FetchError::TimedOut {
            url: at.clone(),
            secs: 30,
        }),
        Err(FetchError::TooLarge { url: at.clone() }),
        Err(FetchError::Status {
            url: at.clone(),
            status: 302,
        }),
        Err(FetchError::Status {
            url: at.clone(),
            status: 500,
        }),
        Err(FetchError::Http {
            url: at.clone(),
            why: "connection closed before message completed".to_owned(),
        }),
    ];
    for answer in answers {
        fake.answer(&path, answer.clone());
        for digest in [None, Some(D1)] {
            let refused = channel
                .check("kbf", &wanting("2.0", digest))
                .await
                .expect_err("refused");
            assert_eq!(refused, ChannelRefusal::NotInChannel, "{answer:?}");
        }
    }
    let refused = ChannelRefusal::NotInChannel;
    assert_eq!(refused.token(), NOT_IN_CHANNEL);
    assert_eq!(
        refused.to_string(),
        "not_in_channel: the release channel does not hold the version; set it again once it does"
    );
}

/// Catches: an error of the fetcher that drops the URL asked or the reason from the operator's
/// log line.
#[test]
fn every_fetch_error_names_the_url_and_why() {
    let at = url("kbf/2.0/index.json");
    let cases = [
        (
            FetchError::Connect {
                url: at.clone(),
                why: "connection refused".to_owned(),
            },
            "the connection failed: connection refused",
        ),
        (
            FetchError::ConnectTimedOut {
                url: at.clone(),
                secs: 10,
            },
            "not connected within 10 s",
        ),
        (
            FetchError::TimedOut {
                url: at.clone(),
                secs: 30,
            },
            "not answered within 30 s",
        ),
        (
            FetchError::TooLarge { url: at.clone() },
            &format!("the answer is longer than {MAX_INDEX_BYTES} bytes"),
        ),
        (
            FetchError::Status {
                url: at.clone(),
                status: 302,
            },
            "status 302",
        ),
        (
            FetchError::Http {
                url: at.clone(),
                why: "closed".to_owned(),
            },
            "closed",
        ),
    ];
    for (error, why) in cases {
        assert_eq!(error.to_string(), format!("{at}: {why}"));
    }
}

/// Catches: a channel URL of another scheme taken (so a set fetches over plain HTTP), a URL
/// with credentials, a query or a fragment taken as a base, a host TLS cannot check a
/// certificate for, or a base path joined with a doubled or missing `/`.
#[test]
fn the_channel_url_is_an_https_base_url() {
    let taken = [
        (
            "https://channel.example",
            "https://channel.example/kbf/2.0/index.json",
        ),
        (
            "https://channel.example/",
            "https://channel.example/kbf/2.0/index.json",
        ),
        (
            "https://channel.example:8443/releases/",
            "https://channel.example:8443/releases/kbf/2.0/index.json",
        ),
        (
            "https://192.0.2.7/a/b",
            "https://192.0.2.7/a/b/kbf/2.0/index.json",
        ),
    ];
    for (text, wanted) in taken {
        let parsed = ChannelUrl::parse(text).expect(text);
        assert_eq!(parsed.url_of("kbf/2.0/index.json"), wanted, "{text}");
        assert_eq!(
            format!("{parsed}/kbf/2.0/index.json"),
            wanted,
            "{text} as shown"
        );
    }
    let refused = [
        (
            "http://channel.example",
            ChannelUrlError::NotHttps as fn(String) -> ChannelUrlError,
        ),
        ("ftp://channel.example", ChannelUrlError::NotHttps),
        ("/releases", ChannelUrlError::NotHttps),
        ("channel.example", ChannelUrlError::NotHttps),
        ("not a url", ChannelUrlError::NotUrl),
        ("https://", ChannelUrlError::NotUrl),
        ("https://[::1]/releases", ChannelUrlError::Host),
        ("https://user@channel.example", ChannelUrlError::NotBase),
        ("https://channel.example/?v=1", ChannelUrlError::NotBase),
        ("https://channel.example/#top", ChannelUrlError::NotBase),
    ];
    for (text, kind) in refused {
        let why = ChannelUrl::parse(text).expect_err(text);
        assert_eq!(why, kind(text.to_owned()), "{text}");
    }
    assert_eq!(
        ChannelUrlError::NotHttps("http://channel.example".to_owned()).to_string(),
        "\"http://channel.example\" is not an https:// URL: the release channel is fetched over \
         HTTPS only"
    );
    assert_eq!(
        ChannelUrlError::NotUrl("not a url".to_owned()).to_string(),
        "\"not a url\" is not a URL"
    );
    assert_eq!(
        ChannelUrlError::Host("https://[::1]".to_owned()).to_string(),
        "\"https://[::1]\" names no host a certificate can be checked for"
    );
    assert_eq!(
        ChannelUrlError::NotBase("https://u@channel.example".to_owned()).to_string(),
        "\"https://u@channel.example\" has a user, a query or a fragment: the release channel is a base URL"
    );
}
