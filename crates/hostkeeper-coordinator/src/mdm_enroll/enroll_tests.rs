//! `/enroll`: which blobs get a challenge and which do not.

use hyper::{Method, StatusCode};

use super::ca::held_certificate;
use super::testkit::{
    SERIAL, UDID, blob_for, challenge_in, error_of, forged_for, post, request, rig,
};
use super::{fake_apple, scep};
use crate::audit_actions;
use crate::mdm_server::support::{NOW, Rig};
use crate::operator::Caller;
use crate::store::mdm::MdmStore as _;

fn now() -> i64 {
    i64::try_from(NOW).expect("a time")
}

fn challenge_row(rig: &Rig, profile: &[u8]) -> crate::store::mdm::Identity {
    rig.store
        .challenge(&scep::challenge_hash(&challenge_in(profile)))
        .expect("the tables read")
        .expect("the challenge is kept")
}

/// Proves: a blob that verifies against Apple's root, for a serial in the ABM table, is answered
/// with the enrollment profile; its challenge is 32 random bytes in hex, kept only as a hash, bound
/// to the serial and the blob's UDID, valid for the challenge ttl, not confirmed; the profile
/// names the server, the topic of the push certificate and the check-in URL.
///
/// Catches: a challenge that is not bound to the blob, or kept in the clear.
#[tokio::test]
async fn a_verified_blob_for_a_serial_in_abm_is_given_a_challenge_bound_to_it() {
    let rig = rig("enroll-verified");
    let (status, profile) = post(&rig, &blob_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::OK);
    let challenge = challenge_in(&profile);
    assert_eq!(challenge.len(), 64);
    assert!(challenge.bytes().all(|b| b.is_ascii_hexdigit()));
    let row = challenge_row(&rig, &profile);
    assert_eq!((row.serial.as_str(), row.udid.as_str()), (SERIAL, UDID));
    assert_eq!(row.expires_unix, now() + 3600);
    assert!(!row.confirmed);
    assert_eq!(row.state, crate::store::mdm::IdentityState::Challenged);
    let text = String::from_utf8(profile).expect("XML");
    assert!(text.contains("https://localhost/scep"));
    assert!(text.contains("https://localhost/checkin"));
    assert!(text.contains("https://localhost/mdm"));
    assert!(text.contains(crate::mdm_server::support::TOPIC));
    assert!(text.contains("dev.hostkeeper.mdm"));
    assert_eq!(rig.audited(), [audit_actions::MDM_CA_CREATED]);
}

/// Proves: each post of the same blob gets a challenge of its own, so a Mac that retries is not
/// handed one that a lost reply may have consumed.
///
/// Catches: a challenge reused across posts.
#[tokio::test]
async fn each_post_gets_a_new_challenge() {
    let rig = rig("enroll-new-challenges");
    let blob = blob_for(SERIAL, UDID);
    let (_, first) = post(&rig, &blob).await;
    let (_, second) = post(&rig, &blob).await;
    assert_ne!(challenge_in(&first), challenge_in(&second));
}

/// Proves: a serial that the ABM table does not list, or lists as removed, is declined
/// `serial_not_in_abm` whether its blob verifies or not, is audited once for the window and not
/// again, no challenge is made, and nothing waits for an operator.
///
/// Catches: a blob proving a serial is a Mac's, taken for proof that the Mac is ours.
#[tokio::test]
async fn a_serial_outside_abm_is_declined_whatever_its_blob_says() {
    let rig = rig("enroll-not-in-abm");
    for body in [blob_for("OTHER1", UDID), forged_for("OTHER1", UDID)] {
        let (status, body) = post(&rig, &body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error_of(&body), "serial_not_in_abm");
    }
    assert_eq!(rig.audited(), [audit_actions::MDM_ENROLL_DECLINED]);
    assert!(rig.server.enroll.pending.list().is_empty());
    assert_eq!(rig.store.challenge("anything").expect("reads"), None);
    // Removed from ABM since the sync listed it.
    rig.store
        .abm_apply(&crate::store::mdm::AbmBatch {
            removed: vec![SERIAL.to_owned()],
            removed_unix: 5,
            ..Default::default()
        })
        .expect("removed");
    let (status, _) = post(&rig, &blob_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        rig.audited(),
        [
            audit_actions::MDM_ENROLL_DECLINED,
            audit_actions::MDM_ENROLL_DECLINED
        ]
    );
    rig.time.advance(3601);
    post(&rig, &blob_for(SERIAL, UDID)).await;
    assert_eq!(rig.audited().len(), 3, "audited again after the window");
}

/// Proves: a GET (a manual enrollment, which proves no device) is declined `serial_not_in_abm`,
/// and a method that is neither is `405`.
///
/// Catches: a profile with a challenge served to a browser.
#[tokio::test]
async fn a_manual_enrollment_is_declined() {
    let rig = rig("enroll-manual");
    let (status, body) = request(&rig, &Method::GET, b"").await;
    assert_eq!(
        (status, error_of(&body).as_str()),
        (StatusCode::FORBIDDEN, "serial_not_in_abm")
    );
    let (status, body) = request(&rig, &Method::PUT, b"x").await;
    assert_eq!(
        (status, error_of(&body).as_str()),
        (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
    );
}

/// Proves: fail closed. A blob that does not verify (a bad signature, a chain to another root)
/// gets no challenge even for a serial in the ABM table; it is answered `503
/// waiting_for_operator`, waits in the list with the verifier's reason, and its first post is
/// audited and its later posts count.
///
/// Catches: "the serial is in the ABM table" taken as enough, which would let a peer that knows a
/// serial obtain an identity.
#[tokio::test]
async fn an_unverified_blob_gets_no_challenge_and_waits_for_an_operator() {
    let rig = rig("enroll-held");
    for _ in 0..2 {
        let (status, body) = post(&rig, &forged_for(SERIAL, UDID)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error_of(&body), "waiting_for_operator");
    }
    let rows = rig.server.enroll.pending.list();
    assert_eq!(rows.len(), 1);
    let (serial, row) = &rows[0];
    assert_eq!(
        (serial.as_str(), row.udid.as_str(), row.count),
        (SERIAL, UDID, 2)
    );
    assert_eq!(row.product.as_deref(), Some("Mac14,13"));
    assert_eq!(
        row.reason,
        "the blob does not verify: a signature does not verify"
    );
    assert_eq!(rig.audited(), [audit_actions::MDM_ENROLL_HELD]);
    assert_eq!(held_certificate(&rig.setup), None, "no CA was made for it");
    // The same blob signed by the fake Apple but trusted by nobody.
    let strangers = Rig::with_apple("enroll-held-root", vec![vec![0x30, 0]]);
    strangers.list_in_abm(SERIAL);
    let (status, _) = post(&strangers, &blob_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(strangers.server.enroll.pending.list().len(), 1);
}

/// Proves: an operator's confirmation lets the next post of the same serial and UDID through with a
/// challenge marked confirmed, once; the post after it waits again, and a post from another UDID
/// does not spend it.
///
/// Catches: a confirmation that admits a different Mac, or every post after it.
#[tokio::test]
async fn a_confirmation_admits_the_waiting_udid_once() {
    let rig = rig("enroll-confirmed");
    let (status, _) = post(&rig, &forged_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    rig.server
        .enroll
        .pending
        .confirm(SERIAL, now() + 600)
        .expect("waits");
    let (status, _) = post(&rig, &forged_for(SERIAL, "00008112-OTHER")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "another UDID");
    let (status, profile) = post(&rig, &forged_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::OK);
    let row = challenge_row(&rig, &profile);
    assert!(row.confirmed);
    assert_eq!((row.serial.as_str(), row.udid.as_str()), (SERIAL, UDID));
    let (status, _) = post(&rig, &forged_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "spent");
    assert_eq!(
        rig.audited(),
        [
            audit_actions::MDM_ENROLL_HELD,
            audit_actions::MDM_CA_CREATED,
            audit_actions::MDM_ENROLL_HELD
        ],
        "the row was spent, so the post after it is a new request"
    );
}

/// Proves: a request that is not a blob (not CMS, no serial in its plist, a serial that is not
/// letters and digits) is `400`, and a store that cannot be read is `500`, with nothing made.
///
/// Catches: a malformed body turned into a row, or a store failure that shows as a decline.
#[tokio::test]
async fn bodies_that_name_no_serial_are_bad_requests_and_a_failing_store_is_500() {
    let rig = rig("enroll-bad");
    for body in [
        Vec::new(),
        b"hello".to_vec(),
        blob_for("A-1", UDID),
        blob_for("", UDID),
        blob_for(&"A".repeat(65), UDID),
    ] {
        let (status, _) = post(&rig, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    rig.store.fail(Some("abm_device"));
    let (status, _) = post(&rig, &blob_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    rig.store.fail(Some("issue_challenge"));
    let (status, _) = post(&rig, &blob_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// Proves: with no name, no push identity or no master key the route answers `503 unavailable`
/// and changes nothing, and with no random bytes it is `500`.
///
/// Catches: a profile without a server name or a topic handed to a Mac.
#[tokio::test]
async fn a_coordinator_that_is_not_set_up_answers_unavailable() {
    let rig = rig("enroll-unset");
    rig.setup
        .set_setting("mdm_name", "not a host name")
        .expect("set");
    let (status, body) = post(&rig, &blob_for(SERIAL, UDID)).await;
    assert_eq!(
        (status, error_of(&body).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    );
    let bare = Rig::bare("enroll-unset-push");
    bare.store_tls();
    bare.setup
        .set_setting("mdm_name", "localhost")
        .expect("set");
    let server_blob = blob_for(SERIAL, UDID);
    let answer = super::enroll::answer(
        &bare.coordinator,
        &bare.server,
        None,
        &Method::POST,
        &server_blob,
    )
    .await;
    assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);
    let noisy =
        Rig::with_apple_random("enroll-no-random", fake_apple::roots(), Box::new(|_| false));
    noisy.list_in_abm(SERIAL);
    let (status, _) = post(&noisy, &blob_for(SERIAL, UDID)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// Proves: one peer is limited to `enroll_rate_per_min` requests a minute, and the limit lifts with
/// the minute.
///
/// Catches: a route that does an RSA operation for a stranger as often as the stranger asks.
#[tokio::test]
async fn a_peer_past_its_rate_is_refused_until_the_next_minute() {
    let rig = rig("enroll-rate");
    rig.setup
        .set_setting("enroll_rate_per_min", "2")
        .expect("set");
    let blob = blob_for(SERIAL, UDID);
    assert_eq!(post(&rig, &blob).await.0, StatusCode::OK);
    assert_eq!(post(&rig, &blob).await.0, StatusCode::OK);
    let (status, body) = post(&rig, &blob).await;
    assert_eq!(
        (status, error_of(&body).as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "rate_limited")
    );
    rig.time.advance(60);
    assert_eq!(post(&rig, &blob).await.0, StatusCode::OK);
}

/// Proves: a coordinator whose settings cannot be read refuses `/enroll` and `/scep` `503` and
/// the operator's confirmation `store_failed`, and issues and consumes nothing.
///
/// Catches: an enrollment that runs on defaults it made up when the setup is unreadable.
#[tokio::test]
async fn a_coordinator_that_cannot_read_its_settings_serves_nothing() {
    use std::sync::atomic::Ordering;

    use hostkeeper_proto::hostkeeper::operator::v1 as proto;
    use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

    use crate::mdm_secrets::MasterKey;
    use crate::mdm_setup::MdmSetup;
    use crate::operator_mdm_push_tests::Flaky;

    let store = Flaky::new();
    let shared: std::sync::Arc<dyn crate::store::mdm_setup::SetupStore> = store.clone();
    let setup = MdmSetup::new(
        shared,
        Some(MasterKey::from_bytes(&[3; 32]).expect("a key")),
        crate::enrollment::os_random(),
    );
    let rig = Rig::with_apple_over("enroll-unreadable", fake_apple::roots(), setup);
    rig.list_in_abm(SERIAL);
    post(&rig, &forged_for(SERIAL, UDID)).await;
    store.reads_fail.store(true, Ordering::SeqCst);
    let (status, body) = post(&rig, &blob_for(SERIAL, UDID)).await;
    assert_eq!(
        (status, error_of(&body).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    );
    let (status, _, _) =
        super::testkit::scep_request(&rig, &Method::GET, Some("operation=GetCACaps"), b"").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let confirmed = rig.coordinator.mdm_confirm_enrollment(
        &Caller::Operator("alice".parse().expect("a token name")),
        &proto::ConfirmEnrollmentRequest {
            serial: SERIAL.to_owned(),
        },
    );
    assert_eq!(confirmed, Err(MdmRefusal::StoreFailed));
    assert_eq!(rig.server.enroll.pending.list()[0].1.confirmed_until, None);
}

/// Proves: with every waiting row confirmed and the list full, a request of another serial is
/// answered `503 unavailable` and not recorded, so a confirmed row is never pushed out.
///
/// Catches: confirmations dropped by a flood of serials that ABM lists.
#[tokio::test]
async fn a_full_list_of_confirmed_requests_takes_no_more() {
    let rig = rig("enroll-full");
    rig.setup
        .set_setting("enroll_pending_max", "1")
        .expect("set");
    rig.list_in_abm("OTHER2");
    post(&rig, &forged_for(SERIAL, UDID)).await;
    rig.server
        .enroll
        .pending
        .confirm(SERIAL, now() + 600)
        .expect("waits");
    let (status, body) = post(&rig, &forged_for("OTHER2", UDID)).await;
    assert_eq!(
        (status, error_of(&body).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    );
    let serials: Vec<String> = rig
        .server
        .enroll
        .pending
        .list()
        .into_iter()
        .map(|(s, _)| s)
        .collect();
    assert_eq!(serials, [SERIAL]);
}

/// Proves: a CA that does not open makes both routes `503 unavailable` and does not replace it.
///
/// Catches: a damaged CA answered with a profile whose identities nothing could sign.
#[tokio::test]
async fn a_ca_that_does_not_open_makes_the_routes_unavailable() {
    let rig = rig("enroll-ca-damaged");
    rig.put(crate::mdm_secrets::SecretKind::MdmCaKey, b"damaged");
    let (status, body) = post(&rig, &blob_for(SERIAL, UDID)).await;
    assert_eq!(
        (status, error_of(&body).as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
    );
    let (status, _, _) =
        super::testkit::scep_request(&rig, &Method::GET, Some("operation=GetCACert"), b"").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rig.audited(), Vec::<String>::new());
}
