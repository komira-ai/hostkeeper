use std::sync::Arc;
use std::time::Duration;

use super::{BASE, GoDaddyProviders, RateLimit};
use crate::mdm_acme::Clock;
use crate::mdm_acme::dns::{
    CredentialError, DnsCredential, DnsError, DnsProvider, DnsProviders, TxtRecord,
};
use crate::mdm_acme::https::Https;
use crate::mdm_acme::testkit::{FakeClock, FakeGoDaddyApi};

const AT: &str = "example.net/_acme-challenge.mdm.internal";

fn record(value: &str) -> TxtRecord {
    TxtRecord {
        zone: "example.net".to_owned(),
        name: "_acme-challenge.mdm.internal".to_owned(),
        value: value.to_owned(),
        ttl_s: 600,
    }
}

fn credential(json: &str) -> DnsCredential {
    DnsCredential::parse(json.as_bytes()).expect("a credential")
}

const SSO: &str = r#"{"provider":"godaddy","kind":"sso-key","key":"KEY1","secret":"SEC2"}"#;

fn provider(api: &Arc<FakeGoDaddyApi>, json: &str) -> Arc<dyn DnsProvider> {
    let providers = GoDaddyProviders::new(
        Arc::clone(api) as Arc<dyn Https>,
        FakeClock::at(1_000) as Arc<dyn Clock>,
    );
    providers.open(&credential(json)).expect("a provider")
}

/// Proves: publishing a value at a name that already holds another TXT value writes both back,
/// because a `PUT` replaces every record at the name, with the new one at the zone's TTL and the
/// old one at its own; the request carries the credential's header and the zone and name in its
/// path.
///
/// Catches: a publish that replaces the records at the challenge name and deletes one an operator
/// or another order keeps there.
#[tokio::test]
async fn publishing_keeps_the_other_text_records_at_the_name() {
    let api = FakeGoDaddyApi::new();
    api.hold(AT, "keep-me", 3600);
    let dns = provider(&api, SSO);
    dns.publish(&record("proof")).await.expect("published");
    assert_eq!(api.data(AT), ["keep-me", "proof"]);
    assert_eq!(
        api.calls(),
        [
            "GET example.net/_acme-challenge.mdm.internal",
            "PUT example.net/_acme-challenge.mdm.internal"
        ]
    );
    let requests = api.requests();
    assert_eq!(
        requests[1].url,
        format!("{BASE}/v1/domains/example.net/records/TXT/_acme-challenge.mdm.internal")
    );
    assert!(
        requests[1]
            .headers
            .contains(&("authorization".to_owned(), "sso-key KEY1:SEC2".to_owned()))
    );
    let body: serde_json::Value = serde_json::from_slice(&requests[1].body).expect("json");
    assert_eq!(body[0]["ttl"], 3600);
    assert_eq!(body[1], serde_json::json!({"data": "proof", "ttl": 600}));
}

/// Proves: publishing a value the provider already holds, with or without the quotes some zones
/// return, writes nothing.
#[tokio::test]
async fn publishing_a_value_that_is_there_writes_nothing() {
    let api = FakeGoDaddyApi::new();
    api.hold(AT, "\"proof\"", 600);
    let dns = provider(&api, SSO);
    dns.publish(&record("proof")).await.expect("published");
    assert_eq!(
        api.calls(),
        ["GET example.net/_acme-challenge.mdm.internal"]
    );
}

/// Proves: the provider's view is read back: a value is published when the records at the name
/// hold it, quoted or not, and not when they hold others or none.
#[tokio::test]
async fn is_published_reads_the_records_back() {
    let api = FakeGoDaddyApi::new();
    let dns = provider(&api, SSO);
    assert!(!dns.is_published(&record("proof")).await.expect("read"));
    api.hold(AT, "other", 600);
    assert!(!dns.is_published(&record("proof")).await.expect("read"));
    api.hold(AT, "\"proof\"", 600);
    assert!(dns.is_published(&record("proof")).await.expect("read"));
}

/// Proves: removing one of several values writes the rest back, removing the only value deletes
/// the name, and removing a value that is not there writes nothing.
///
/// Catches: a removal that deletes the whole name and takes another order's challenge with it.
#[tokio::test]
async fn removing_keeps_the_rest_and_deletes_an_empty_name() {
    let api = FakeGoDaddyApi::new();
    api.hold(AT, "keep-me", 3600);
    api.hold(AT, "proof", 600);
    let dns = provider(&api, SSO);
    dns.remove(&record("proof")).await.expect("removed");
    assert_eq!(api.data(AT), ["keep-me"]);
    dns.remove(&record("proof"))
        .await
        .expect("nothing to remove");
    assert_eq!(
        api.calls(),
        [
            "GET example.net/_acme-challenge.mdm.internal",
            "PUT example.net/_acme-challenge.mdm.internal",
            "GET example.net/_acme-challenge.mdm.internal",
        ]
    );
    dns.remove(&record("keep-me")).await.expect("removed");
    assert!(api.data(AT).is_empty());
    assert_eq!(
        api.calls().last().map(String::as_str),
        Some("DELETE example.net/_acme-challenge.mdm.internal")
    );
}

/// Proves: a bearer credential sends `Bearer <token>`.
#[tokio::test]
async fn a_bearer_credential_sends_the_bearer_header() {
    let api = FakeGoDaddyApi::new();
    let dns = provider(
        &api,
        r#"{"provider":"godaddy","kind":"bearer","token":"TOK3"}"#,
    );
    dns.is_published(&record("x")).await.expect("read");
    assert!(
        api.requests()[0]
            .headers
            .contains(&("authorization".to_owned(), "Bearer TOK3".to_owned()))
    );
}

/// Proves: a 401 and a 403 are the provider refusing the credential and name the status, any
/// other status is a failure that names the status and none of the reply's body, a reply that is
/// not a list of records is a failure, and a call that gets no reply is a failure.
///
/// Catches: a refused credential retried as if the provider were down, which is what the alert
/// `mdm_dns_credential_rejected` tells apart.
#[tokio::test]
async fn the_provider_saying_no_is_told_from_the_provider_failing() {
    let api = FakeGoDaddyApi::new();
    let dns = provider(&api, SSO);
    for status in [401, 403] {
        api.answer_with(status, Some("SECRET-BODY"));
        assert_eq!(
            dns.publish(&record("p")).await,
            Err(DnsError::Rejected { status })
        );
    }
    api.answer_with(500, Some("SECRET-BODY"));
    assert_eq!(
        dns.remove(&record("p")).await,
        Err(DnsError::Failed(
            "the provider answered status 500".to_owned()
        ))
    );
    api.answer_with(429, None);
    assert_eq!(
        dns.is_published(&record("p")).await,
        Err(DnsError::Failed(
            "the provider answered status 429".to_owned()
        ))
    );
    api.answer_with(200, Some("not records"));
    assert_eq!(
        dns.is_published(&record("p")).await,
        Err(DnsError::Failed(
            "the records are not understood".to_owned()
        ))
    );
    let down = FakeGoDaddyApi::new();
    down.go_down();
    let dns = provider(&down, SSO);
    assert_eq!(
        dns.publish(&record("p")).await,
        Err(DnsError::Failed("the API cannot be reached".to_owned()))
    );
}

/// Proves: a zone or a record name with a character a path cannot hold sends no request.
///
/// Catches: a setting that smuggles a path or a query into the provider's URL.
#[tokio::test]
async fn a_zone_or_name_that_is_not_path_safe_sends_nothing() {
    let api = FakeGoDaddyApi::new();
    let dns = provider(&api, SSO);
    let mut bad = record("p");
    bad.zone = "example.net/../x".to_owned();
    assert!(matches!(dns.publish(&bad).await, Err(DnsError::Failed(_))));
    let mut bad = record("p");
    bad.name = String::new();
    assert!(matches!(dns.publish(&bad).await, Err(DnsError::Failed(_))));
    assert!(api.requests().is_empty());
}

/// Proves: a credential for another provider gets none.
#[test]
fn a_credential_for_another_provider_opens_nothing() {
    let providers = GoDaddyProviders::new(
        FakeGoDaddyApi::new() as Arc<dyn Https>,
        FakeClock::at(1) as Arc<dyn Clock>,
    );
    let other = credential(r#"{"provider":"route53","kind":"bearer","token":"t"}"#);
    assert_eq!(
        providers.open(&other).err(),
        Some(CredentialError::UnknownProvider("route53".to_owned()))
    );
}

/// Proves: the limit lets the allowed calls through at once, makes the next wait until the oldest
/// call is a minute old, counts calls that are older than a minute as free, and sleeps at least a
/// second.
///
/// Catches: a client that goes over the provider's 60 calls a minute and has its credential
/// throttled or banned.
#[tokio::test]
async fn the_rate_limit_waits_for_the_oldest_call_to_age_out() {
    let clock = FakeClock::at(1_000);
    let limit = RateLimit::new(2);
    limit.take(&*clock).await;
    clock.advance(10);
    limit.take(&*clock).await;
    assert!(clock.slept().is_empty());
    limit.take(&*clock).await;
    assert_eq!(clock.slept(), [Duration::from_secs(50)]);
    assert_eq!(clock.now_unix(), 1_060);
    clock.advance(100);
    limit.take(&*clock).await;
    limit.take(&*clock).await;
    assert_eq!(clock.slept().len(), 1);
}

/// Proves: the provider's own limit is 60 calls a minute.
#[test]
fn the_limit_is_sixty_calls_a_minute() {
    assert_eq!(super::CALLS_PER_MINUTE, 60);
}
