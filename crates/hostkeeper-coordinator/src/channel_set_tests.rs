//! A set of the desired state with a release channel (`--release-channel`, [`crate::channel`]):
//! refused before anything changes when the channel does not hold the version or holds another
//! artifact for it, and then the refusals it always had; without a channel, nothing is fetched.
//! The channel is a fake fetcher: no network.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::channel::{ChannelRefusal, FetchError, ReleaseChannel};
use crate::channel_tests::{D1, D2, FakeChannel, index};
use crate::desired::Desired;
use crate::http::{DESIRED_PATH, Surface, handle};
use crate::registry::Digest;
use crate::rollback_tests::{ONE, round, services_of, wanted};
use crate::service::SetError;
use crate::testutil::{LEGACY, OPERATOR, Rig, ci_token, version};
use hostkeeper_wire::wire::ServiceReply;

/// A desired-state body of `version`, naming `digest` as its package digest when there is one.
fn body(version: &str, digest: Option<&str>) -> String {
    let mut body = json!({"version": version, "min_up": {"storage": 0, "frontend": 0}});
    if let Some(digest) = digest {
        body["release_digest"] = json!(digest);
    }
    body.to_string()
}

/// A rig whose coordinator runs a service of six daemons (`extra`'s lines in its table) and
/// checks every set against `fake`, with 1.0 in the channel and desired, past the quiet start.
async fn rig(test: &str, extra: &str, fake: &Arc<FakeChannel>) -> Rig {
    let mut rig = Rig::new(test);
    rig.coordinator = Rig::start(&rig.dir, &rig.clock)
        .with_services(services_of(extra))
        .with_release_channel(ReleaseChannel::new(fake.clone()));
    fake.hold(LEGACY, "1.0", &index(LEGACY, "1.0", &[D1]));
    rig.set(ONE).await;
    rig.at(2_000);
    rig
}

/// CI's set of `body`, with a token no one has used.
async fn ci_sets(rig: &Rig, body: &str) -> Result<Desired, SetError> {
    rig.coordinator
        .set_desired(Some(&ci_token()), body.as_bytes())
        .await
}

/// The bytes of every file a set could change: the state file and the ops files.
fn files(rig: &Rig) -> Vec<Option<Vec<u8>>> {
    ["desired.json", "desired.json.ops", "desired.json.ops.audit"]
        .iter()
        .map(|name| std::fs::read(rig.dir.path().join(name)).ok())
        .collect()
}

/// Whether any host of a heartbeat round at `at` is ordered to update: the one live update
/// taken.
fn ordered(rig: &Rig, at: u64) -> Vec<ServiceReply> {
    round(rig, at, &[])
        .into_iter()
        .filter(|reply| *reply != ServiceReply::Nothing)
        .collect()
}

/// Catches: CI's set of a version the channel does not hold yet being accepted (the brief's
/// failure: the one live update goes to a rollout every host fails to fetch). The set is
/// refused `not_in_channel`, the desired state, its registry, the state and ops files stay as
/// they were and no host is ordered; once the channel holds the version, the same set is
/// accepted and one host is ordered to it.
#[tokio::test]
async fn a_version_not_yet_in_the_channel_is_refused_and_takes_no_slot() {
    let fake = Arc::new(FakeChannel::default());
    let rig = rig("channel-not-yet", "", &fake).await;
    let before = files(&rig);
    let two = body("2.0", Some(D2));
    let refused = ci_sets(&rig, &two).await.expect_err("refused");
    assert!(
        matches!(refused, SetError::Channel(ChannelRefusal::NotInChannel)),
        "{refused:?}"
    );
    assert_eq!(refused.reason(), Some("not_in_channel"));
    assert_eq!(wanted(&rig), version("1.0"));
    assert_eq!(
        rig.coordinator.desired.digests(LEGACY).get(&version("2.0")),
        None
    );
    assert_eq!(files(&rig), before);
    assert_eq!(ordered(&rig, 2_000), Vec::<ServiceReply>::new());

    fake.hold(LEGACY, "2.0", &index(LEGACY, "2.0", &[D2]));
    let set = ci_sets(&rig, &two)
        .await
        .expect("set once the channel holds it");
    assert_eq!(set.version, version("2.0"));
    assert_eq!(wanted(&rig), version("2.0"));
    assert_eq!(
        rig.coordinator.desired.digests(LEGACY).get(&version("2.0")),
        Some(&Digest::parse(D2).expect("a digest"))
    );
    assert_eq!(
        ordered(&rig, 2_010),
        [ServiceReply::Update {
            service: None,
            version: version("2.0"),
        }]
    );
    assert_eq!(
        fake.asked(),
        [
            "kbf/1.0/index.json",
            "kbf/2.0/index.json",
            "kbf/2.0/index.json"
        ]
    );
}

/// Catches: a fetch that failed (here the total deadline, as the fetcher reports it) read as
/// the channel holding the version, or refused as non-retryable.
#[tokio::test]
async fn a_failed_fetch_is_not_in_channel_and_changes_nothing() {
    let fake = Arc::new(FakeChannel::default());
    let rig = rig("channel-fetch-failed", "", &fake).await;
    let before = files(&rig);
    fake.answer(
        "kbf/2.0/index.json",
        Err(FetchError::TimedOut {
            url: "https://channel.example/kbf/2.0/index.json".to_owned(),
            secs: 30,
        }),
    );
    let refused = ci_sets(&rig, &body("2.0", None))
        .await
        .expect_err("refused");
    assert_eq!(refused.reason(), Some("not_in_channel"));
    assert_eq!(files(&rig), before);
    assert_eq!(ordered(&rig, 2_000), Vec::<ServiceReply>::new());
}

/// Catches: a set whose digest the channel's index of the version does not list being
/// accepted (hosts would fetch another artifact than CI set), or refused as retryable; the
/// refusal changes nothing and records no digest.
#[tokio::test]
async fn a_digest_the_channel_index_does_not_list_is_a_mismatch() {
    let fake = Arc::new(FakeChannel::default());
    let rig = rig("channel-digest-mismatch", "", &fake).await;
    let before = files(&rig);
    fake.hold(LEGACY, "2.0", &index(LEGACY, "2.0", &[D1]));
    let refused = ci_sets(&rig, &body("2.0", Some(D2)))
        .await
        .expect_err("refused");
    assert!(
        matches!(refused, SetError::Channel(ChannelRefusal::DigestMismatch)),
        "{refused:?}"
    );
    assert_eq!(refused.reason(), Some("channel_digest_mismatch"));
    assert_eq!(wanted(&rig), version("1.0"));
    assert_eq!(
        rig.coordinator.desired.digests(LEGACY).get(&version("2.0")),
        None
    );
    assert_eq!(files(&rig), before);
    assert_eq!(ordered(&rig, 2_000), Vec::<ServiceReply>::new());
}

/// Catches: a set the channel holds skipping the refusals the coordinator always made: a
/// version below the service's `min_version` is still refused `below_min_version`, and a
/// version the channel holds above it is set.
#[tokio::test]
async fn a_version_the_channel_holds_goes_on_to_the_other_refusals() {
    let fake = Arc::new(FakeChannel::default());
    let rig = rig("channel-then-floor", "min_version = \"1.0\"", &fake).await;
    fake.hold(LEGACY, "0.9", &index(LEGACY, "0.9", &[D1]));
    let refused = ci_sets(&rig, &body("0.9", None))
        .await
        .expect_err("refused");
    assert!(matches!(refused, SetError::BelowMinVersion), "{refused:?}");
    fake.hold(LEGACY, "1.1", &index(LEGACY, "1.1", &[D2, D1]));
    let set = ci_sets(&rig, &body("1.1", Some(D1))).await.expect("set");
    assert_eq!(set.version, version("1.1"));
    assert_eq!(wanted(&rig), version("1.1"));
}

/// Catches: the channel asked for a body that is no desired state or for a service the token
/// may not set (an outbound fetch any caller with a token could cause, and a refusal reported
/// as the channel's rather than as what it is).
#[tokio::test]
async fn a_set_refused_before_the_channel_fetches_nothing() {
    let fake = Arc::new(FakeChannel::default());
    let mut rig = rig("channel-admitted-first", "", &fake).await;
    let malformed = ci_sets(&rig, "{\"version\":").await.expect_err("refused");
    assert!(matches!(malformed, SetError::Invalid(_)), "{malformed:?}");
    let list = format!(
        "{}services = [\"other\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    rig.coordinator = Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
        .with_services(services_of(""))
        .with_release_channel(ReleaseChannel::new(fake.clone()));
    let forbidden = ci_sets(&rig, &body("2.0", None))
        .await
        .expect_err("refused");
    assert!(matches!(forbidden, SetError::Forbidden), "{forbidden:?}");
    assert_eq!(fake.asked(), ["kbf/1.0/index.json"]);
}

/// Catches: an operator's set passing by the channel, which CI's may not: the operator's set
/// of a version the channel does not hold is refused `not_in_channel`, with no audit entry.
#[tokio::test]
async fn an_operators_set_is_checked_against_the_channel_too() {
    let fake = Arc::new(FakeChannel::default());
    let rig = rig("channel-operator", "", &fake).await;
    let before = files(&rig);
    let refused = rig
        .coordinator
        .set_desired_operator(OPERATOR, body("2.0", None).as_bytes())
        .await
        .expect_err("refused");
    assert_eq!(refused.reason(), Some("not_in_channel"));
    assert_eq!(files(&rig), before);
    fake.hold(LEGACY, "2.0", &index(LEGACY, "2.0", &[D1]));
    rig.coordinator
        .set_desired_operator(OPERATOR, body("2.0", Some(D1)).as_bytes())
        .await
        .expect("set");
    assert_eq!(wanted(&rig), version("2.0"));
}

/// Catches: a coordinator without `--release-channel` refusing a set for a channel, or one
/// with it letting the same set through: the set of a version no channel holds is refused
/// `not_in_channel` by a coordinator given an empty channel (whose fetcher it asks once), and
/// accepted as before by one on the same files given none, which holds no fetcher to ask.
#[tokio::test]
async fn without_a_channel_nothing_is_fetched_and_the_set_is_as_before() {
    let fake = Arc::new(FakeChannel::default());
    let mut rig = Rig::new("channel-unset");
    rig.set(ONE).await;
    let two = body("2.0", Some(D2));
    rig.coordinator = Rig::start(&rig.dir, &rig.clock)
        .with_services(services_of(""))
        .with_release_channel(ReleaseChannel::new(fake.clone()));
    let refused = ci_sets(&rig, &two)
        .await
        .expect_err("refused with a channel");
    assert_eq!(refused.reason(), Some("not_in_channel"));
    assert_eq!(fake.asked(), ["kbf/2.0/index.json"]);
    assert_eq!(wanted(&rig), version("1.0"));

    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_services(services_of(""));
    assert!(rig.coordinator.channel.is_none());
    let set = ci_sets(&rig, &two).await.expect("set without a channel");
    assert_eq!(set.version, version("2.0"));
    assert_eq!(wanted(&rig), version("2.0"));
    assert_eq!(fake.asked(), ["kbf/2.0/index.json"]);
}

/// Catches: the JSON route answering a channel refusal as anything but a 409 with exactly the
/// token, or a retryable refusal sharing the non-retryable one's token.
#[tokio::test]
async fn the_json_route_answers_a_channel_refusal_409_with_its_token() {
    let fake = Arc::new(FakeChannel::default());
    let rig = rig("channel-json", "", &fake).await;
    fake.hold(LEGACY, "3.0", &index(LEGACY, "3.0", &[D1]));
    for (at, digest, token) in [
        ("2.0", None, "not_in_channel"),
        ("3.0", Some(D2), "channel_digest_mismatch"),
    ] {
        let request = hyper::Request::builder()
            .method("POST")
            .uri(DESIRED_PATH)
            .header("authorization", format!("Bearer {}", ci_token()))
            .body(http_body_util::Full::new(bytes::Bytes::from(body(
                at, digest,
            ))))
            .expect("a request");
        let response = handle(
            &rig.coordinator,
            Surface::Control,
            &None,
            request,
            std::time::Duration::from_secs(30),
        )
        .await;
        assert_eq!(response.status().as_u16(), 409, "{at}");
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("a body")
            .to_bytes();
        let answer: Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(answer, json!({"error": token}), "{at}");
    }
    assert_eq!(wanted(&rig), version("1.0"));
}
