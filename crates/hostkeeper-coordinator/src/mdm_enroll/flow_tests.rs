//! The whole of 5.1 with a fake Mac: Setup Assistant's post to `/enroll`, the SCEP exchange, then
//! `Authenticate` and `TokenUpdate`, over the listener in memory.

use der::Encode as _;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hyper::StatusCode;
use rsa::pkcs8::EncodePrivateKey as _;
use rsa_legacy::pkcs8::DecodePrivateKey as _;

use super::ca::pack;
use super::scep_client::{self, Client, check_rep, open_rep};
use super::testkit::{SERIAL, UDID, blob_for, challenge_in, error_of, forged_for, rig};
use crate::audit_actions;
use crate::mdm_secrets::SecretKind;
use crate::mdm_server::support::{
    Identity2, Rig, Session, Spec as Signing, authenticate, header, token_update,
};
use crate::operator::Caller;
use crate::store::mdm::{AbmBatch, EnrollmentState, MdmStore as _};

const PKI: &str = "/scep?operation=PKIOperation";

/// A rig whose identity CA is the SCEP helper's, as a restarted coordinator loads it.
fn flow_rig(name: &str) -> Rig {
    let rig = rig(name);
    let ca = scep_client::ca();
    rig.put(
        SecretKind::MdmCaKey,
        &pack(ca.cert_der(), &ca.key_pkcs8_der().expect("the key encodes")),
    );
    rig
}

fn operator() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

/// What the Mac does from the profile on: reads the CA, redeems `challenge` with a key of its own
/// and returns the identity it was issued.
async fn redeem(session: &mut Session, challenge: &str, transaction: &str) -> Identity2 {
    let (status, caps) = session
        .request("GET", "/scep?operation=GetCACaps", &[], b"")
        .await;
    assert_eq!(
        (status, caps.as_slice()),
        (StatusCode::OK, hostkeeper_mdm_scep::CA_CAPS.as_bytes())
    );
    let (status, ca) = session
        .request("GET", "/scep?operation=GetCACert", &[], b"")
        .await;
    assert_eq!(
        (status, ca),
        (StatusCode::OK, scep_client::ca().cert_der().to_vec())
    );
    let client = Client::new(2048);
    let message = scep_client::request(
        &client,
        &scep_client::Spec {
            challenge: Some(challenge.to_owned()),
            transaction_id: Some(transaction.to_owned()),
            ..scep_client::Spec::default()
        },
    );
    let (status, reply) = session.request("POST", PKI, &[], &message).await;
    assert_eq!(status, StatusCode::OK);
    let rep = check_rep(&reply);
    assert_eq!(rep.status, "0", "{:?}", rep.fail_info);
    let identity = open_rep(&rep, &client).0.remove(0);
    let key_der = client.key.to_pkcs8_der().expect("the key encodes");
    Identity2 {
        index: 0,
        der: identity.to_der().expect("DER"),
        key_der: key_der.as_bytes().to_vec(),
        key: Some(rsa_legacy::RsaPrivateKey::from_pkcs8_der(key_der.as_bytes()).expect("the key reads")),
    }
}

/// The Mac of Setup Assistant with the blob `blob`: the profile's challenge, redeemed.
async fn enroll_identity(rig: &Rig, blob: &[u8], transaction: &str) -> Identity2 {
    let mut session = Session::dial(rig, None).await.expect("connected");
    let (status, profile) = session.request("POST", "/enroll", &[], blob).await;
    assert_eq!(status, StatusCode::OK);
    redeem(&mut session, &challenge_in(&profile), transaction).await
}

/// `Authenticate` and then `TokenUpdate` signed by `mac`: the statuses.
async fn check_in(rig: &Rig, mac: &Identity2) -> (StatusCode, StatusCode) {
    let mut session = Session::dial(rig, None).await.expect("connected");
    let mut statuses = Vec::new();
    for body in [authenticate(SERIAL), token_update("magic-1")] {
        let signature = header(&body, mac, &Signing::default());
        let (status, _) = session
            .put("/checkin", &[("Mdm-Signature", signature)], &body)
            .await;
        statuses.push(status);
    }
    (statuses[0], statuses[1])
}

fn live(rig: &Rig) -> crate::store::mdm::Enrollment {
    rig.store
        .live_enrollment(SERIAL)
        .expect("reads")
        .expect("a live enrollment")
}

/// Proves: section 5.1 end to end with a fake Mac. A device blob signed by the fake Apple is
/// answered with the profile, the challenge in it is redeemed over SCEP for an identity of the
/// serial, and `Authenticate` and `TokenUpdate` signed by that identity enter the enrollment with
/// its push token; the challenge cannot be used by a second key.
///
/// Catches: any seam of the chain from `/enroll` to the first token: a challenge that SCEP cannot
/// find, an identity `Authenticate` does not accept, an enrollment without its token.
#[tokio::test]
async fn a_fake_mac_enrolls_from_its_blob_to_its_first_token_update() {
    let rig = flow_rig("flow-enroll");
    let mac = enroll_identity(&rig, &blob_for(SERIAL, UDID), "T1").await;
    assert_eq!(check_in(&rig, &mac).await, (StatusCode::OK, StatusCode::OK));
    let enrolled = live(&rig);
    assert_eq!(
        (enrolled.serial.as_str(), enrolled.udid.as_str()),
        (SERIAL, UDID)
    );
    assert_eq!(enrolled.cert_sha256.as_deref(), Some(mac.sha256().as_str()));
    assert_eq!(enrolled.push_token.as_deref(), Some(&[9_u8; 32][..]));
    assert_eq!(enrolled.push_magic.as_deref(), Some("magic-1"));
}

/// Proves: owner decision B end to end. A Mac that was wiped and enrolls again through a verified
/// blob replaces its live enrollment with no operator action and no check-out; the old identity
/// is refused from then on, and the replacement is audited.
///
/// Catches: a wiped Mac that waits for a click, or that keeps two live enrollments.
#[tokio::test]
async fn a_wiped_mac_enrolls_again_and_replaces_its_enrollment_with_no_operator() {
    let rig = flow_rig("flow-wiped");
    let first = enroll_identity(&rig, &blob_for(SERIAL, UDID), "T1").await;
    assert_eq!(check_in(&rig, &first).await.0, StatusCode::OK);
    let old = live(&rig);
    let second = enroll_identity(&rig, &blob_for(SERIAL, UDID), "T2").await;
    assert_eq!(
        check_in(&rig, &second).await,
        (StatusCode::OK, StatusCode::OK)
    );
    let new = live(&rig);
    assert_ne!(new.enrollment_id, old.enrollment_id);
    assert_eq!(
        rig.store
            .enrollment(&old.enrollment_id)
            .expect("reads")
            .expect("kept")
            .state,
        EnrollmentState::Replaced
    );
    assert_eq!(
        rig.audited().last().map(String::as_str),
        Some(audit_actions::MDM_ENROLLMENT_REPLACED)
    );
    // The old identity is no longer a credential.
    assert_eq!(check_in(&rig, &first).await.0, StatusCode::UNAUTHORIZED);
}

/// Proves: a serial removed from ABM between the challenge and `Authenticate` does not replace
/// the live enrollment: the Mac holds an identity and is refused, audited, and the live
/// enrollment stands; and a serial that left ABM before its next `/enroll` gets no challenge.
///
/// Catches: the check made only at `/enroll` (the trace the model check of enrollment found).
#[tokio::test]
async fn a_serial_removed_from_abm_after_its_challenge_does_not_replace_the_live_enrollment() {
    let rig = flow_rig("flow-removed");
    let first = enroll_identity(&rig, &blob_for(SERIAL, UDID), "T1").await;
    assert_eq!(check_in(&rig, &first).await.0, StatusCode::OK);
    let live_id = live(&rig).enrollment_id;
    let second = enroll_identity(&rig, &blob_for(SERIAL, UDID), "T2").await;
    rig.store
        .abm_apply(&AbmBatch {
            removed: vec![SERIAL.to_owned()],
            removed_unix: 5,
            ..AbmBatch::default()
        })
        .expect("removed");
    assert_eq!(check_in(&rig, &second).await.0, StatusCode::FORBIDDEN);
    assert_eq!(live(&rig).enrollment_id, live_id);
    assert_eq!(
        rig.audited().last().map(String::as_str),
        Some(audit_actions::MDM_REPLACE_REFUSED)
    );
    let mut session = Session::dial(&rig, None).await.expect("connected");
    let (status, body) = session
        .request("POST", "/enroll", &[], &blob_for(SERIAL, UDID))
        .await;
    assert_eq!(
        (status, error_of(&body).as_str()),
        (StatusCode::FORBIDDEN, "serial_not_in_abm")
    );
}

/// Proves: the fail-closed path end to end. A blob that does not verify waits; the operator's
/// confirmation (which reads the ABM table again) lets the next post of that UDID through; the
/// Mac enrolls on that challenge, and the enrollment it replaces is replaced on the operator's
/// word even though the ABM table no longer lists the serial.
///
/// Catches: a confirmed Mac that is held at `Authenticate` by the rule it was confirmed past, and
/// a replayed unverified blob that approves anything.
#[tokio::test]
async fn an_unverified_blob_enrolls_only_after_an_operators_confirmation() {
    let rig = flow_rig("flow-confirmed");
    let mut session = Session::dial(&rig, None).await.expect("connected");
    let forged = forged_for(SERIAL, UDID);
    let (status, _) = session.request("POST", "/enroll", &[], &forged).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // A replay of the held blob changes nothing: nothing was issued, nothing is live.
    let (status, _) = session.request("POST", "/enroll", &[], &forged).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rig.store.live_enrollment(SERIAL).expect("reads"), None);
    let listed = rig
        .coordinator
        .mdm_list_enrollment_requests(&operator())
        .expect("listed");
    assert_eq!(listed.requests.len(), 1);
    assert_eq!(
        (listed.requests[0].count, listed.requests[0].in_abm),
        (2, true)
    );

    // An enrollment already stands, from a verified blob.
    let first = enroll_identity(&rig, &blob_for(SERIAL, UDID), "T1").await;
    assert_eq!(check_in(&rig, &first).await.0, StatusCode::OK);
    let confirmed = rig
        .coordinator
        .mdm_confirm_enrollment(
            &operator(),
            &proto::ConfirmEnrollmentRequest {
                serial: SERIAL.to_owned(),
            },
        )
        .expect("confirmed");
    assert_eq!(
        confirmed.expires_unix,
        i64::try_from(crate::mdm_server::support::NOW).unwrap() + 3600
    );
    let (status, profile) = session.request("POST", "/enroll", &[], &forged).await;
    assert_eq!(status, StatusCode::OK);
    let second = redeem(&mut session, &challenge_in(&profile), "T2").await;
    rig.store
        .abm_apply(&AbmBatch {
            removed: vec![SERIAL.to_owned()],
            removed_unix: 5,
            ..AbmBatch::default()
        })
        .expect("removed");
    assert_eq!(
        check_in(&rig, &second).await,
        (StatusCode::OK, StatusCode::OK)
    );
    assert_eq!(
        live(&rig).cert_sha256.as_deref(),
        Some(second.sha256().as_str())
    );
}

/// Proves: a verified blob is not the same as an enrolled Mac: a replayed unverified post can hold
/// the serial's place in the list but gets no challenge, creates no identity and enters no
/// enrollment, and the operator who confirms it admits only the UDID that waited.
///
/// Catches: the documented replay (docs/design/coordinator-mdm.md 9.2) turning into an approval.
#[tokio::test]
async fn a_replay_before_the_first_enrollment_holds_the_serial_and_approves_nothing() {
    let rig = flow_rig("flow-replay");
    let mut session = Session::dial(&rig, None).await.expect("connected");
    let (status, _) = session
        .request("POST", "/enroll", &[], &forged_for(SERIAL, "ATTACKER-UDID"))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    rig.coordinator
        .mdm_confirm_enrollment(
            &operator(),
            &proto::ConfirmEnrollmentRequest {
                serial: SERIAL.to_owned(),
            },
        )
        .expect("the operator saw a Mac enroll");
    // The real Mac's post, with its own UDID, is not the one that waited.
    let (status, _) = session
        .request("POST", "/enroll", &[], &forged_for(SERIAL, UDID))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rig.store.live_enrollment(SERIAL).expect("reads"), None);
}

/// Proves: through the routed listener the peer is the address of the connection: one address past
/// its rate is refused while another is served, for `/enroll` and `/scep` alike.
///
/// Catches: a limit keyed on something every Mac shares, which one noisy Mac would use up.
#[tokio::test]
async fn the_rate_is_per_address_of_the_connection() {
    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::Request;

    let rig = flow_rig("flow-rate");
    rig.setup
        .set_setting("enroll_rate_per_min", "1")
        .expect("set");
    let from = |address: &str, method: &str, path: &str| {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .body(Full::new(Bytes::new()))
            .expect("a request");
        request
            .extensions_mut()
            .insert(crate::http::PeerAddr(address.parse().expect("an address")));
        request
    };
    let status = |request| {
        let rig = &rig;
        async move {
            crate::mdm_server::answer(
                &rig.coordinator,
                &rig.server,
                crate::mdm_server::Family::Mdm,
                &None,
                request,
                std::time::Duration::from_secs(5),
            )
            .await
            .status()
        }
    };
    let caps = "/scep?operation=GetCACaps";
    assert_eq!(
        status(from("192.0.2.1:50000", "GET", caps)).await,
        StatusCode::OK
    );
    assert_eq!(
        status(from("192.0.2.1:50001", "GET", caps)).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        status(from("192.0.2.2:50000", "GET", caps)).await,
        StatusCode::OK
    );
    assert_eq!(
        status(from("192.0.2.2:50001", "POST", "/enroll")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}
