//! `/scep` over the coordinator's tables: the library's rules, with the challenges where the
//! coordinator keeps them.

use hostkeeper_mdm_scep::CA_CAPS;
use hyper::{Method, StatusCode};
use sha2::{Digest, Sha256};

use super::ca::pack;
use super::scep::challenge_hash;
use super::scep_client::{self, Client, Spec, check_rep, open_rep, request};
use super::testkit::{SERIAL, UDID, rig, scep_request};
use crate::mdm_secrets::SecretKind;
use crate::mdm_server::support::{NOW, Rig};
use crate::store::mdm::{Challenge, IdentityState, Issuance, MdmStore as _};

const PKI: Option<&str> = Some("operation=PKIOperation");

fn now() -> i64 {
    i64::try_from(NOW).expect("a time")
}

/// A rig whose identity CA is the SCEP helper's, so that the helper's client and checker speak to
/// it: the CA is loaded from the sealed secret, as a restarted coordinator loads it.
fn scep_rig(name: &str) -> Rig {
    let rig = rig(name);
    let ca = scep_client::ca();
    rig.put(
        SecretKind::MdmCaKey,
        &pack(ca.cert_der(), &ca.key_pkcs8_der().expect("the key encodes")),
    );
    rig
}

/// Issues the challenge `text` for `SERIAL` and `UDID`, valid for an hour.
fn challenge(rig: &Rig, text: &str) {
    rig.store
        .issue_challenge(&Challenge {
            challenge_sha256: challenge_hash(text),
            serial: SERIAL.to_owned(),
            udid: UDID.to_owned(),
            expires_unix: now() + 3600,
            confirmed: false,
        })
        .expect("the challenge is issued");
}

fn spec(text: &str, transaction: &str) -> Spec {
    Spec {
        challenge: Some(text.to_owned()),
        transaction_id: Some(transaction.to_owned()),
        ..Spec::default()
    }
}

/// Posts `message`: the status and the checked reply.
async fn pki(rig: &Rig, message: &[u8]) -> (StatusCode, scep_client::Rep) {
    let (status, kind, body) = scep_request(rig, &Method::POST, PKI, message).await;
    assert_eq!(kind.as_deref(), Some("application/x-pki-message"));
    (status, check_rep(&body))
}

fn key_hash(client: &Client) -> String {
    hex::encode(Sha256::digest(client.spki_der()))
}

/// Proves: `GetCACaps` answers the capabilities of the library as text, and `GetCACert` the DER of
/// the CA certificate the setup holds, the first need making the CA when none is held.
///
/// Catches: a Mac that cannot read what the server can do, or a CA certificate that is not the
/// one that signs.
#[tokio::test]
async fn the_caps_and_the_ca_certificate_are_answered() {
    let rig = scep_rig("scep-caps");
    let (status, kind, body) =
        scep_request(&rig, &Method::GET, Some("operation=GetCACaps"), b"").await;
    assert_eq!(
        (status, kind.as_deref()),
        (StatusCode::OK, Some("text/plain"))
    );
    assert_eq!(body, CA_CAPS.as_bytes());
    let (status, kind, body) =
        scep_request(&rig, &Method::GET, Some("x=1&operation=GetCACert"), b"").await;
    assert_eq!(
        (status, kind.as_deref()),
        (StatusCode::OK, Some("application/x-x509-ca-cert"))
    );
    assert_eq!(body, scep_client::ca().cert_der());
    let fresh = super::testkit::rig("scep-caps-fresh");
    let (status, _, body) =
        scep_request(&fresh, &Method::GET, Some("operation=GetCACert"), b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        Some(body),
        super::ca::held_certificate(&fresh.setup),
        "the first need made the CA that was answered"
    );
}

/// Proves: a request that names no operation, one that is unknown, one with the wrong method for
/// its operation, and a message that names no transaction are refused with their statuses, and
/// nothing is consumed.
///
/// Catches: `PKIOperation` by GET answered as if it were a POST, which this route does not read.
#[tokio::test]
async fn requests_that_are_not_scep_are_refused_by_status() {
    let rig = scep_rig("scep-bad");
    for (method, query, status) in [
        (Method::GET, None, StatusCode::BAD_REQUEST),
        (Method::GET, Some("operation=Nope"), StatusCode::BAD_REQUEST),
        (
            Method::POST,
            Some("operation=Nope"),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            Method::POST,
            Some("operation=GetCACaps"),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (Method::GET, PKI, StatusCode::METHOD_NOT_ALLOWED),
        (Method::POST, PKI, StatusCode::BAD_REQUEST),
    ] {
        let (got, _, _) = scep_request(&rig, &method, query, b"not a message").await;
        assert_eq!(got, status, "{method} {query:?}");
    }
}

/// Proves: a Mac that redeems an open challenge with a good request gets a success reply that
/// holds its identity: named for the challenge's serial, signed by the CA, recorded under the
/// challenge with the hash of the key and of the certificate, so that `Authenticate` finds it.
///
/// Catches: an identity issued and not recorded, or recorded under the wrong serial.
#[tokio::test]
async fn an_open_challenge_is_redeemed_for_an_identity_of_its_serial() {
    let rig = scep_rig("scep-issue");
    challenge(&rig, "open-challenge");
    let client = Client::new(2048);
    let (status, rep) = pki(&rig, &request(&client, &spec("open-challenge", "T1"))).await;
    assert_eq!((status, rep.status.as_str()), (StatusCode::OK, "0"));
    let (certs, _) = open_rep(&rep, &client);
    let der = x509_cert_der(&certs[0]);
    let row = rig
        .store
        .challenge(&challenge_hash("open-challenge"))
        .expect("reads")
        .expect("row");
    assert_eq!(row.state, IdentityState::Issued);
    assert_eq!(row.key_sha256, Some(key_hash(&client)));
    let issued = row.issued.expect("what was issued");
    assert_eq!(issued.cert_der, der);
    assert_eq!(issued.cert_sha256, hex::encode(Sha256::digest(&der)));
    assert!(issued.not_after_unix > now() + 1000 * 86_400);
    let found = rig
        .store
        .identity(&issued.cert_sha256)
        .expect("reads")
        .expect("found by hash");
    assert_eq!(found.serial, SERIAL);
    let (_, parsed) = x509_parser::parse_x509_certificate(&der).expect("a certificate");
    assert!(
        parsed
            .subject()
            .to_string()
            .contains(&format!("CN={SERIAL}"))
    );
}

fn x509_cert_der(cert: &x509_cert::Certificate) -> Vec<u8> {
    use der::Encode as _;
    cert.to_der().expect("DER")
}

/// Proves: a request that is refused leaves the challenge open, so a Mac that retries in a new
/// session (a new `transactionID`) with the same key is redeemed; and once the challenge is
/// consumed the same key in yet another session gets the certificate stored, while another key is
/// refused and the first key keeps its certificate.
///
/// Catches: a retry that macOS makes after a failure turned into a second identity or a permanent
/// refusal, and a consumed challenge that a second key can take.
#[tokio::test]
async fn a_retry_with_a_new_transaction_gets_the_same_identity_and_another_key_is_refused() {
    let rig = scep_rig("scep-retry");
    challenge(&rig, "retry-challenge");
    let client = Client::new(2048);
    let broken = Spec {
        break_csr_signature: true,
        ..spec("retry-challenge", "T1")
    };
    let (_, rep) = pki(&rig, &request(&client, &broken)).await;
    assert_eq!(rep.status, "2");
    let row = rig
        .store
        .challenge(&challenge_hash("retry-challenge"))
        .expect("reads")
        .expect("row");
    assert_eq!(
        row.state,
        IdentityState::Challenged,
        "a refusal consumes nothing"
    );

    let (_, first) = pki(&rig, &request(&client, &spec("retry-challenge", "T2"))).await;
    assert_eq!(first.status, "0");
    let (_, again) = pki(&rig, &request(&client, &spec("retry-challenge", "T3"))).await;
    assert_eq!(again.status, "0");
    assert_ne!(first.transaction_id, again.transaction_id);
    assert_eq!(
        open_rep(&first, &client).0[0]
            .tbs_certificate()
            .serial_number(),
        open_rep(&again, &client).0[0]
            .tbs_certificate()
            .serial_number()
    );

    let thief = Client::new(2048);
    let (_, refused) = pki(&rig, &request(&thief, &spec("retry-challenge", "T4"))).await;
    assert_eq!(
        (refused.status.as_str(), refused.fail_info.as_deref()),
        ("2", Some("2"))
    );
    let row = rig
        .store
        .challenge(&challenge_hash("retry-challenge"))
        .expect("reads")
        .expect("row");
    assert_eq!(row.key_sha256, Some(key_hash(&client)));
    let (_, last) = pki(&rig, &request(&client, &spec("retry-challenge", "T5"))).await;
    assert_eq!(last.status, "0");
}

/// Proves: an unknown challenge, one past its time, and one whose enrollment ended are refused with
/// a failure reply, the expired one still open in the table.
///
/// Catches: a challenge that outlives its hour, or one of a Mac that checked out.
#[tokio::test]
async fn unknown_expired_and_ended_challenges_are_refused() {
    let rig = scep_rig("scep-refused");
    let client = Client::new(2048);
    let (_, rep) = pki(&rig, &request(&client, &spec("nobody-issued-this", "T1"))).await;
    assert_eq!(rep.status, "2");

    challenge(&rig, "late-challenge");
    rig.time.advance(3600);
    let (_, rep) = pki(&rig, &request(&client, &spec("late-challenge", "T2"))).await;
    assert_eq!(rep.status, "2");
    let row = rig
        .store
        .challenge(&challenge_hash("late-challenge"))
        .expect("reads")
        .expect("row");
    assert_eq!(row.state, IdentityState::Challenged);

    // An identity that was issued and whose enrollment then ended.
    let ended = Rig::with_apple("scep-ended", super::fake_apple::roots());
    ended.put(
        SecretKind::MdmCaKey,
        &pack(
            scep_client::ca().cert_der(),
            &scep_client::ca().key_pkcs8_der().expect("encodes"),
        ),
    );
    challenge(&ended, "ended-challenge");
    let (_, rep) = pki(&ended, &request(&client, &spec("ended-challenge", "T3"))).await;
    assert_eq!(rep.status, "0");
    let cert = ended
        .store
        .challenge(&challenge_hash("ended-challenge"))
        .expect("reads")
        .expect("row")
        .issued
        .expect("issued")
        .cert_sha256;
    ended.store.kill_identity(&cert).expect("killed");
    let (_, rep) = pki(&ended, &request(&client, &spec("ended-challenge", "T4"))).await;
    assert_eq!(rep.status, "2");
}

/// Proves: a store that fails while a challenge is read or consumed gives a failure reply and
/// consumes nothing, and the same request succeeds once the store is mended.
///
/// Catches: a certificate handed out whose record was not written.
#[tokio::test]
async fn a_failing_store_refuses_and_consumes_nothing() {
    let rig = scep_rig("scep-store");
    challenge(&rig, "store-challenge");
    let client = Client::new(2048);
    for name in ["challenge", "consume_challenge"] {
        rig.store.fail(Some(name));
        let (status, rep) = pki(&rig, &request(&client, &spec("store-challenge", "T1"))).await;
        assert_eq!(
            (status, rep.status.as_str()),
            (StatusCode::OK, "2"),
            "{name}"
        );
    }
    rig.store.fail(None);
    let row = rig
        .store
        .challenge(&challenge_hash("store-challenge"))
        .expect("reads")
        .expect("row");
    assert_eq!(row.state, IdentityState::Challenged);
    let (_, rep) = pki(&rig, &request(&client, &spec("store-challenge", "T2"))).await;
    assert_eq!(rep.status, "0");
}

/// Proves: when another request redeems the same challenge between this one's read and its write,
/// the same key is answered with the certificate that request stored, and another key is refused.
///
/// Catches: two certificates for one challenge when a Mac's retries overlap.
#[tokio::test]
async fn a_request_that_loses_a_race_is_answered_with_the_winners_certificate() {
    let rig = scep_rig("scep-race");
    let client = Client::new(2048);
    // An identity the CA issued for another Mac, to stand for what the winner stored.
    challenge(&rig, "race-seed");
    let seed = Client::new(2048);
    let (_, rep) = pki(&rig, &request(&seed, &spec("race-seed", "T0"))).await;
    let winning = x509_cert_der(&open_rep(&rep, &seed).0[0]);
    for (text, same_key) in [("race-same", true), ("race-other", false)] {
        challenge(&rig, text);
        let winner_key = if same_key {
            key_hash(&client)
        } else {
            key_hash(&Client::new(2048))
        };
        let hash = challenge_hash(text);
        let stored = Issuance {
            cert_sha256: hex::encode(Sha256::digest(&winning)),
            cert_der: winning.clone(),
            not_after_unix: now() + 1000,
        };
        rig.store.race_before_consume(move |tables| {
            use crate::store::mdm::MdmStore as _;
            tables
                .consume_challenge(&hash, &winner_key, &stored, now())
                .expect("the winner consumes it");
        });
        let (_, rep) = pki(&rig, &request(&client, &spec(text, "T1"))).await;
        if same_key {
            assert_eq!(rep.status, "0");
            assert_eq!(x509_cert_der(&open_rep(&rep, &client).0[0]), winning);
        } else {
            assert_eq!(
                (rep.status.as_str(), rep.fail_info.as_deref()),
                ("2", Some("2"))
            );
        }
        let row = rig
            .store
            .challenge(&challenge_hash(text))
            .expect("reads")
            .expect("row");
        assert_eq!(row.issued.expect("the winner's").cert_der, winning);
    }
}

/// Proves: a Mac past its rate is refused `429` before any RSA operation.
///
/// Catches: a stranger using the CA's private key as often as it likes.
#[tokio::test]
async fn a_peer_past_its_rate_is_refused_before_the_message_is_read() {
    let rig = scep_rig("scep-rate");
    rig.setup
        .set_setting("enroll_rate_per_min", "1")
        .expect("set");
    let (status, _, _) = scep_request(&rig, &Method::GET, Some("operation=GetCACaps"), b"").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = scep_request(&rig, &Method::POST, PKI, b"x").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(super::testkit::error_of(&body), "rate_limited");
}
