use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::pem::PemObject;

use super::{OrderError, Plan, issue, open_account};
use crate::mdm_acme::dns::DnsError;
use crate::mdm_acme::https::Https;
use crate::mdm_acme::testkit::{DIRECTORY, FakeAcme, FakeClock, FakeDns, Mood};

const NAME: &str = "mdm.internal.example.net";
const TXT: &str = "_acme-challenge.mdm.internal";

struct Rig {
    dns: Arc<FakeDns>,
    ca: Arc<FakeAcme>,
    clock: Arc<FakeClock>,
    https: Arc<dyn Https>,
}

fn plan() -> Plan {
    Plan {
        name: NAME.to_owned(),
        zone: "example.net".to_owned(),
        directory: DIRECTORY.to_owned(),
        contact: Some("ops@example.net".to_owned()),
        ttl_s: 600,
        poll_s: 15,
        dns_timeout_s: 120,
        settle_s: 7,
        acme_timeout_s: 90,
    }
}

fn rig() -> Rig {
    let dns = FakeDns::new();
    let ca = FakeAcme::new(&dns, NAME, TXT, 1_800_000_000, 90 * 86_400);
    Rig {
        https: Arc::clone(&ca) as Arc<dyn Https>,
        dns,
        ca,
        clock: FakeClock::at(1_800_000_000),
    }
}

async fn run(rig: &Rig) -> Result<super::Issued, OrderError> {
    let (account, _) = open_account(&rig.https, &plan(), None)
        .await
        .expect("an account");
    issue(&account, &plan(), &*rig.dns, &*rig.clock).await
}

fn pause(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

/// Proves: an order publishes one challenge value at the record inside the zone, waits two polls
/// for the provider to show it, then the settle pause, answers the challenge, and returns a chain
/// and a key that make a TLS server configuration; the CA saw exactly the published value (a
/// base64url SHA-256, 43 characters) when it was told the challenge was ready, and the record is
/// gone afterwards.
///
/// Catches: a challenge answered before the record shows, a record left in the zone, and a key
/// that is not the one the certificate was issued for.
#[tokio::test]
async fn an_order_proves_the_name_by_dns_and_returns_a_chain_for_its_own_key() {
    let rig = rig();
    rig.dns.show_after(2);
    let issued = run(&rig).await.expect("issued");
    let seen = rig.ca.challenge_values();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].len(), 1);
    assert_eq!(seen[0][0].len(), 43);
    assert_eq!(rig.dns.values(TXT), Vec::<String>::new());
    assert_eq!(
        rig.dns.calls(),
        [
            "publish _acme-challenge.mdm.internal ttl 600",
            "look _acme-challenge.mdm.internal",
            "look _acme-challenge.mdm.internal",
            "look _acme-challenge.mdm.internal",
            "remove _acme-challenge.mdm.internal",
        ]
    );
    // Two polls of 15 s, the settle of 7 s, and no wait for the CA: it was ready at once.
    assert_eq!(&rig.clock.slept()[..3], [pause(15), pause(15), pause(7)]);
    let chain = rustls::pki_types::CertificateDer::pem_slice_iter(issued.chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("a chain");
    assert_eq!(chain.len(), 2);
    let key = PrivateKeyDer::from_pem_slice(issued.key_pem.as_bytes()).expect("a key");
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("the key is the certificate's");
}

/// Proves: the record is removed when the order fails after it was published: the CA ends the
/// order invalid, the order never leaves pending (it times out, polling every 15 s within 90 s),
/// and the record does not show within `dns_timeout_s`; each leaves nothing at the name, and the
/// answers are the refusal and the two timeouts.
///
/// Catches: a challenge record left in the zone by a failed order, where the next order would add
/// to it.
#[tokio::test]
async fn the_record_is_removed_when_the_order_fails() {
    let rig = rig();
    rig.ca.be(Mood::OrderInvalid);
    let why = run(&rig).await.err().expect("refused");
    assert_eq!(why.to_string(), "the CA ended the order invalid");
    assert!(rig.dns.values(TXT).is_empty());
    assert!(
        rig.dns
            .calls()
            .last()
            .is_some_and(|call| call.starts_with("remove"))
    );

    let rig = rig_again();
    rig.ca.be(Mood::NeverReady);
    let why = run(&rig).await.err().expect("timed out");
    assert_eq!(
        why.to_string(),
        "timed out waiting for the CA to check the challenge"
    );
    assert!(rig.dns.values(TXT).is_empty());
    assert!(
        rig.dns
            .calls()
            .last()
            .is_some_and(|call| call.starts_with("remove"))
    );

    let rig = rig_again();
    rig.dns.show_after(1000);
    let why = run(&rig).await.err().expect("timed out");
    assert_eq!(
        why.to_string(),
        "timed out waiting for the DNS record to show"
    );
    assert!(rig.dns.values(TXT).is_empty());
    assert!(rig.ca.challenge_values().is_empty());
    assert!(
        rig.dns
            .calls()
            .last()
            .is_some_and(|call| call.starts_with("remove"))
    );

    let rig = rig_again();
    rig.ca.be(Mood::OrderErrors);
    let why = run(&rig).await.err().expect("an error");
    assert!(matches!(&why, OrderError::Acme(_)));
    assert!(why.to_string().contains("the CA broke"), "{why}");
    assert!(rig.dns.values(TXT).is_empty());

    let rig = rig_again();
    rig.ca.be(Mood::NeverIssues);
    let why = run(&rig).await.err().expect("timed out");
    assert_eq!(
        why.to_string(),
        "timed out waiting for the CA to issue the certificate"
    );
    assert!(rig.dns.values(TXT).is_empty());
}

fn rig_again() -> Rig {
    rig()
}

/// Proves: a provider that fails to publish fails the order with the provider's error and removes
/// nothing, a provider that refuses the credential is told apart from one that fails, and a
/// removal that fails after a good order is logged and leaves the order's answer a certificate.
///
/// Catches: a failed removal that throws away a certificate the CA already issued.
#[tokio::test]
async fn the_provider_failing_is_the_orders_error_except_for_the_cleanup() {
    let rig = rig();
    rig.dns.fail_publish();
    let why = run(&rig).await.err().expect("fails");
    assert!(matches!(&why, OrderError::Dns(DnsError::Failed(_))));
    assert!(!why.dns_rejected());
    assert!(
        !rig.dns
            .calls()
            .iter()
            .any(|call| call.starts_with("remove"))
    );

    let rig = rig_again();
    rig.dns.reject_with(403);
    let why = run(&rig).await.err().expect("rejected");
    assert!(why.dns_rejected());
    assert_eq!(
        why.to_string(),
        "the DNS provider refused the credential (status 403)"
    );

    let rig = rig_again();
    rig.dns.fail_remove();
    run(&rig).await.expect("the order stands");
}

/// Proves: an authorization that is already valid is not proven again (no record, no challenge),
/// one that is invalid refuses the order, one with no dns-01 challenge says so, and the name
/// outside the zone is refused before the CA is asked for anything.
#[tokio::test]
async fn authorizations_that_need_no_proof_or_cannot_be_proven() {
    let rig = rig();
    rig.ca.be(Mood::AuthorizationValid);
    run(&rig).await.expect("issued");
    assert!(rig.dns.calls().is_empty());
    assert!(rig.ca.challenge_values().is_empty());

    let rig = rig_again();
    rig.ca.be(Mood::AuthorizationInvalid);
    let why = run(&rig).await.err().expect("refused");
    assert_eq!(
        why.to_string(),
        "the CA ended the order with an authorization Invalid"
    );

    let rig = rig_again();
    rig.ca.be(Mood::NoDnsChallenge);
    let why = run(&rig).await.err().expect("no challenge");
    assert_eq!(
        why.to_string(),
        "the CA offers no dns-01 challenge for the name"
    );

    let rig = rig_again();
    let (account, _) = open_account(&rig.https, &plan(), None)
        .await
        .expect("an account");
    let mut outside = plan();
    outside.zone = "example.org".to_owned();
    let why = issue(&account, &outside, &*rig.dns, &*rig.clock)
        .await
        .err()
        .expect("outside");
    assert_eq!(
        why.to_string(),
        "the name mdm.internal.example.net is not inside the zone example.org"
    );
    assert_eq!(rig.ca.orders(), 0);
}

/// Proves: a CA that answers a problem, and one that cannot be reached, fail the order with the
/// ACME exchange's error, and the problem's text reaches the message.
#[tokio::test]
async fn a_ca_that_refuses_or_is_down_fails_the_order() {
    let rig = rig();
    rig.ca.be(Mood::RefusesOrder);
    let why = run(&rig).await.err().expect("refused");
    assert!(matches!(&why, OrderError::Acme(_)));
    assert!(why.to_string().contains("too many orders"), "{why}");

    let rig = rig_again();
    rig.ca.be(Mood::Down);
    let result = open_account(&rig.https, &plan(), None).await;
    let why = result.err().expect("down");
    assert!(
        why.to_string().contains("the CA cannot be reached"),
        "{why}"
    );
}

/// Proves: a new account is made with the contact as a `mailto:` address and its credentials come
/// back to be stored; stored credentials for the same directory load the account and make none;
/// credentials made at another directory make a new account; credentials for this directory that
/// do not load are reported as unreadable.
///
/// Catches: an account made again at every renewal, which the CA's rate limit on new accounts
/// counts, and a staging account used at the production directory.
#[tokio::test]
async fn the_account_is_made_once_per_directory() {
    let rig = rig();
    let (_, created) = open_account(&rig.https, &plan(), None).await.expect("made");
    let created = created.expect("credentials to store");
    assert_eq!(rig.ca.accounts(), 1);
    let sent = rig.ca.payloads("/account");
    assert!(sent[0].contains("mailto:ops@example.net"), "{}", sent[0]);
    assert!(sent[0].contains("termsOfServiceAgreed"));

    let (_, again) = open_account(&rig.https, &plan(), Some(&created))
        .await
        .expect("loaded");
    assert!(again.is_none());
    assert_eq!(rig.ca.accounts(), 1);

    let mut staging = plan();
    staging.directory = format!("{DIRECTORY}?staging");
    staging.contact = None;
    let (_, other) = open_account(&rig.https, &staging, Some(&created))
        .await
        .expect("made");
    assert!(other.is_some());
    assert_eq!(rig.ca.accounts(), 2);
    assert!(!rig.ca.payloads("/account")[1].contains("mailto"));

    let broken = br#"{"directory":"https://acme.test/directory"}"#;
    let result = open_account(&rig.https, &plan(), Some(broken)).await;
    assert!(matches!(result.err(), Some(OrderError::AccountUnreadable)));
    let garbage = open_account(&rig.https, &plan(), Some(b"garbage")).await;
    assert!(
        garbage.is_ok(),
        "text that names no directory is replaced, not trusted"
    );
}
