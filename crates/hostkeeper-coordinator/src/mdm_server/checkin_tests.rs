//! `PUT /checkin` (`docs/design/coordinator-mdm.md`, sections 4.2 and 5.1) against the file tables,
//! with a fake Mac that dials the listener in memory and offers the certificate of its identity.

use hyper::StatusCode;

use super::auth::SIGNATURE_HEADER;
use crate::mdm_server::support::{
    BODY, Identity2, Rig, SERIAL, Session, Spec, UDID, authenticate, checkin, header, token_update,
};
use crate::store::mdm::{AbmBatch, CommandState, EnrollmentState, IdentityState, MdmStore};

fn mac(rig: &Rig, index: usize) -> Identity2 {
    rig.mac_ca.issue(index, SERIAL)
}

async fn session(rig: &Rig, mac: &Identity2) -> Session {
    Session::dial(rig, Some(mac)).await.expect("connected")
}

/// An enrolled Mac: identity issued, `Authenticate` answered.
async fn enrolled(rig: &Rig, index: usize) -> (Identity2, Session) {
    let mac = mac(rig, index);
    rig.issue(&mac, SERIAL, UDID, false);
    let mut mac_session = session(rig, &mac).await;
    let (status, _) = mac_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    assert_eq!(status, StatusCode::OK);
    (mac, mac_session)
}

fn live_id(rig: &Rig) -> String {
    rig.store
        .live_enrollment(SERIAL)
        .expect("read")
        .expect("a live enrollment")
        .enrollment_id
}

/// Proves: `Authenticate` of an issued identity enters the enrollment, live and bound to the
/// certificate that presented it, and a repeat of the message changes nothing.
///
/// Catches: a Mac that retries `Authenticate` replacing its own enrollment, and with it the
/// commands queued for it.
#[tokio::test]
async fn authenticate_enters_the_enrollment_and_a_repeat_changes_nothing() {
    let rig = Rig::new("checkin-authenticate");
    let (mac, mut mac_session) = enrolled(&rig, 1).await;
    let first = rig
        .store
        .live_enrollment(SERIAL)
        .expect("read")
        .expect("live");
    assert_eq!(first.udid, UDID);
    assert_eq!(first.state, EnrollmentState::Live);
    assert_eq!(first.cert_sha256.as_deref(), Some(mac.sha256().as_str()));
    let (status, body) = mac_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    assert_eq!((status, body.len()), (StatusCode::OK, 0));
    assert_eq!(live_id(&rig), first.enrollment_id);
    assert!(rig.audited().is_empty(), "nothing was replaced");
}

/// Proves: under owner decision B a new enrollment of a serial in the ABM table replaces the live
/// one with no operator: the old row is replaced, its commands cancelled, its certificate no
/// longer accepted, the gate's enqueue for it refused, and an audit row says why.
///
/// Catches: a wiped Mac that never checked out waiting for an operator, or a superseded
/// certificate still taken.
#[tokio::test]
async fn a_new_enrollment_of_a_serial_in_the_abm_table_replaces_the_live_one() {
    let rig = Rig::new("checkin-replace");
    let (old_mac, mut old_session) = enrolled(&rig, 1).await;
    let old = live_id(&rig);
    rig.enqueue(&old, 1)
        .await
        .expect("queued for the old enrollment");
    rig.list_in_abm(SERIAL);
    let new_mac = mac(&rig, 2);
    rig.issue(&new_mac, SERIAL, UDID, false);
    let mut new_session = session(&rig, &new_mac).await;
    let (status, _) = new_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    assert_eq!(status, StatusCode::OK);
    let live = rig
        .store
        .live_enrollment(SERIAL)
        .expect("read")
        .expect("live");
    assert_ne!(live.enrollment_id, old);
    assert_eq!(live.cert_sha256.as_deref(), Some(new_mac.sha256().as_str()));
    assert_eq!(
        rig.store
            .enrollment(&old)
            .expect("read")
            .expect("kept")
            .state,
        EnrollmentState::Replaced
    );
    let commands = rig.store.commands(&old).expect("read");
    assert_eq!(commands[0].state, CommandState::Cancelled);
    assert_eq!(
        rig.store
            .identity(&old_mac.sha256())
            .expect("read")
            .expect("kept")
            .state,
        IdentityState::Retired
    );
    let (status, _) = old_session
        .put(
            "/mdm",
            &[],
            &crate::mdm_server::support::result("Idle", None),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the superseded certificate is not accepted"
    );
    assert!(
        rig.enqueue(&old, 2).await.is_err(),
        "the gate's enqueue for it is refused"
    );
    assert_eq!(rig.audited(), ["mdm_enrollment_replaced"]);
}

/// Proves: a serial that is not in the ABM table does not replace a live enrollment by itself: the
/// request is refused and audited, and the live enrollment stays, until an operator confirmed the
/// identity, which then replaces it; and a serial that ABM removed since is refused as one that
/// was never there.
///
/// Catches: a Mac outside our ABM taking over the enrollment of another, and the table being read
/// only when `/enroll` issued the challenge (the model check's counterexample).
#[tokio::test]
async fn a_serial_outside_the_abm_table_replaces_only_when_an_operator_confirmed() {
    let rig = Rig::new("checkin-not-in-abm");
    let (_, _session) = enrolled(&rig, 1).await;
    let live = live_id(&rig);
    let second = mac(&rig, 2);
    rig.issue(&second, SERIAL, UDID, false);
    let mut second_session = session(&rig, &second).await;
    let (status, _) = second_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(live_id(&rig), live, "the live enrollment stays");
    // Listed, then removed by a later sync: refused again.
    rig.list_in_abm(SERIAL);
    rig.store
        .abm_apply(&AbmBatch {
            removed: vec![SERIAL.to_owned()],
            removed_unix: 5,
            ..AbmBatch::default()
        })
        .expect("removed");
    let (status, _) = second_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "removed from ABM since");
    assert_eq!(live_id(&rig), live);
    assert_eq!(
        rig.audited(),
        ["mdm_replace_refused", "mdm_replace_refused"]
    );
    // An operator confirmed a third identity: it replaces without the table.
    let third = mac(&rig, 3);
    rig.issue(&third, SERIAL, UDID, true);
    let mut third_session = session(&rig, &third).await;
    let (status, _) = third_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(live_id(&rig), live);
    assert_eq!(rig.audited()[2], "mdm_enrollment_replaced");
}

/// Proves: an `Authenticate` that names another serial or another device than its identity's is
/// refused (the first audited), and nothing is entered.
///
/// Catches: a Mac enrolling under a serial that is not the one its identity was issued for.
#[tokio::test]
async fn authenticate_must_name_the_serial_and_the_device_of_its_identity() {
    let rig = Rig::new("checkin-names");
    let mac = mac(&rig, 1);
    rig.issue(&mac, SERIAL, UDID, false);
    let mut mac_session = session(&rig, &mac).await;
    let (status, _) = mac_session
        .put("/checkin", &[], &authenticate("SOMEONEELSE"))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = mac_session
        .put(
            "/checkin",
            &[],
            &checkin(
                "Authenticate",
                &[
                    ("SerialNumber", SERIAL.into()),
                    ("UDID", "OTHERUDID".into()),
                ],
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = mac_session
        .put(
            "/checkin",
            &[],
            &checkin("Authenticate", &[("UDID", UDID.into())]),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "no serial at all");
    assert!(rig.store.live_enrollment(SERIAL).expect("read").is_none());
    assert_eq!(
        rig.audited(),
        ["mdm_serial_mismatch", "mdm_serial_mismatch"]
    );
}

/// Proves: `TokenUpdate` stores the push token, magic and topic on the live enrollment; a user
/// channel's update is passed over; a certificate whose enrollment was replaced is refused.
///
/// Catches: the user channel's token overwriting the Mac's, and a replaced identity setting a token.
#[tokio::test]
async fn token_update_sets_the_push_token_of_the_live_enrollment_only() {
    let rig = Rig::new("checkin-token");
    let (_, mut mac_session) = enrolled(&rig, 1).await;
    let (status, _) = mac_session
        .put("/checkin", &[], &token_update("magic-1"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let live = rig
        .store
        .live_enrollment(SERIAL)
        .expect("read")
        .expect("live");
    assert_eq!(live.push_token, Some(vec![9; 32]));
    assert_eq!(live.push_magic.as_deref(), Some("magic-1"));
    assert_eq!(
        live.topic.as_deref(),
        Some(crate::mdm_server::support::TOPIC)
    );
    let user = checkin(
        "TokenUpdate",
        &[
            ("Token", plist::Value::Data(vec![1; 32])),
            ("PushMagic", "user-magic".into()),
            ("EnrollmentUserID", "user".into()),
        ],
    );
    let (status, _) = mac_session.put("/checkin", &[], &user).await;
    assert_eq!(status, StatusCode::OK);
    let console = checkin(
        "TokenUpdate",
        &[
            ("Token", plist::Value::Data(vec![1; 32])),
            ("PushMagic", "user-magic".into()),
            ("NotOnConsole", true.into()),
        ],
    );
    mac_session.put("/checkin", &[], &console).await;
    let by_user_id = checkin(
        "TokenUpdate",
        &[
            ("Token", plist::Value::Data(vec![2; 32])),
            ("PushMagic", "user-id-magic".into()),
            ("UserID", "ABCD-1234".into()),
        ],
    );
    let (status, _) = mac_session.put("/checkin", &[], &by_user_id).await;
    assert_eq!(status, StatusCode::OK);
    let kept = rig
        .store
        .live_enrollment(SERIAL)
        .expect("read")
        .expect("live");
    assert_eq!(kept.push_magic.as_deref(), Some("magic-1"));
    let other = checkin(
        "TokenUpdate",
        &[
            ("Token", plist::Value::Data(vec![9; 32])),
            ("PushMagic", "m".into()),
            ("UDID", "OTHER".into()),
        ],
    );
    let (status, _) = mac_session.put("/checkin", &[], &other).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Proves: `CheckOut` ends the enrollment: it is checked out, its commands are cancelled, its
/// identity is retired and the certificate is refused from then on.
///
/// Catches: commands still queued for a Mac that left management.
#[tokio::test]
async fn check_out_ends_the_enrollment_and_retires_its_certificate() {
    let rig = Rig::new("checkin-check-out");
    let (mac, mut mac_session) = enrolled(&rig, 1).await;
    let id = live_id(&rig);
    rig.enqueue(&id, 1).await.expect("queued");
    let (status, _) = mac_session
        .put("/checkin", &[], &checkin("CheckOut", &[]))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        rig.store
            .enrollment(&id)
            .expect("read")
            .expect("kept")
            .state,
        EnrollmentState::CheckedOut
    );
    assert_eq!(
        rig.store.commands(&id).expect("read")[0].state,
        CommandState::Cancelled
    );
    assert_eq!(
        rig.store
            .identity(&mac.sha256())
            .expect("read")
            .expect("kept")
            .state,
        IdentityState::Retired
    );
    let (status, _) = mac_session.put("/checkin", &[], &token_update("m")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Proves: the check-ins the server keeps nothing for are answered without changing state: a
/// bootstrap token set is taken, a get and a declarative request are `404`; a body that is no
/// check-in is a bad request.
///
/// Catches: a Mac stuck on a message the server cannot answer, or a crash on a message it ignores.
#[tokio::test]
async fn the_other_check_ins_change_nothing() {
    let rig = Rig::new("checkin-others");
    let (_, mut mac_session) = enrolled(&rig, 1).await;
    let before = rig.store.live_enrollment(SERIAL).expect("read");
    let set = checkin(
        "SetBootstrapToken",
        &[("BootstrapToken", plist::Value::Data(vec![1, 2]))],
    );
    assert_eq!(
        mac_session.put("/checkin", &[], &set).await.0,
        StatusCode::OK
    );
    let get = checkin("GetBootstrapToken", &[]);
    assert_eq!(
        mac_session.put("/checkin", &[], &get).await.0,
        StatusCode::NOT_FOUND
    );
    let declarative = checkin("DeclarativeManagement", &[("Endpoint", "tokens".into())]);
    assert_eq!(
        mac_session.put("/checkin", &[], &declarative).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        mac_session.put("/checkin", &[], b"not a plist").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(rig.store.live_enrollment(SERIAL).expect("read"), before);
}

/// Proves: a check-in with no certificate and no signature, with a signature that does not verify,
/// with a certificate the coordinator did not issue, and with one whose identity is dead, is `401`
/// and changes nothing.
///
/// Catches: a peer on the LAN enrolling or checking in without an identity.
#[tokio::test]
async fn a_check_in_without_an_accepted_credential_is_unauthorized() {
    let rig = Rig::new("checkin-unauthorized");
    let mac = mac(&rig, 1);
    rig.issue(&mac, SERIAL, UDID, false);
    let body = authenticate(SERIAL);
    let mut bare = Session::dial(&rig, None).await.expect("connected");
    assert_eq!(
        bare.put("/checkin", &[], &body).await.0,
        StatusCode::UNAUTHORIZED
    );
    let garbage = [(SIGNATURE_HEADER, "not base64!".to_owned())];
    assert_eq!(
        bare.put("/checkin", &garbage, &body).await.0,
        StatusCode::UNAUTHORIZED
    );
    // A signature over another body.
    let other = [(SIGNATURE_HEADER, header(BODY, &mac, &Spec::default()))];
    assert_eq!(
        bare.put("/checkin", &other, &body).await.0,
        StatusCode::UNAUTHORIZED
    );
    // A certificate of the CA that no identity row holds.
    let stranger = rig.mac_ca.issue(2, SERIAL);
    let mut unknown = session(&rig, &stranger).await;
    assert_eq!(
        unknown.put("/checkin", &[], &body).await.0,
        StatusCode::UNAUTHORIZED
    );
    // An identity that was killed.
    rig.store.kill_identity(&mac.sha256()).expect("killed");
    let mut dead = session(&rig, &mac).await;
    assert_eq!(
        dead.put("/checkin", &[], &body).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(rig.store.live_enrollment(SERIAL).expect("read").is_none());
}

/// Proves: a Mac whose profile has `SignMessage` and that presents no client certificate is
/// accepted by its `Mdm-Signature` alone, for a check-in.
///
/// Catches: the live profile's Macs (`SignMessage true`) locked out by a listener that reads only
/// the TLS certificate.
#[tokio::test]
async fn the_mdm_signature_authenticates_a_mac_that_presents_no_certificate() {
    let rig = Rig::new("checkin-signature");
    let mac = mac(&rig, 1);
    rig.issue(&mac, SERIAL, UDID, false);
    let body = authenticate(SERIAL);
    let signed = [(SIGNATURE_HEADER, header(&body, &mac, &Spec::default()))];
    let mut bare = Session::dial(&rig, None).await.expect("connected");
    assert_eq!(bare.put("/checkin", &signed, &body).await.0, StatusCode::OK);
    let live = rig
        .store
        .live_enrollment(SERIAL)
        .expect("read")
        .expect("live");
    assert_eq!(live.cert_sha256.as_deref(), Some(mac.sha256().as_str()));
}

/// Proves: a store that fails is a `500` for each step of a check-in, and nothing is half done: a
/// failure to read the identity, the live enrollment, the ABM table, to replace, to set the
/// token or to check out is told to the Mac, which retries.
///
/// Catches: a failed write answered `200`, which the Mac would not send again.
#[tokio::test]
async fn a_store_that_fails_is_a_server_error_for_the_mac_to_retry() {
    let rig = Rig::new("checkin-store-fails");
    let (_, mut mac_session) = enrolled(&rig, 1).await;
    let second = mac(&rig, 2);
    rig.issue(&second, SERIAL, UDID, false);
    let mut second_session = session(&rig, &second).await;
    // (the method that fails, whether the second identity asks, the message)
    for (method, second, body) in [
        ("identity", false, token_update("m")),
        ("live_enrollment", false, token_update("m")),
        ("set_push", false, token_update("m")),
        ("check_out", false, checkin("CheckOut", &[])),
        ("live_enrollment", true, authenticate(SERIAL)),
        ("abm_device", true, authenticate(SERIAL)),
    ] {
        rig.store.fail(Some(method));
        let asking = if second {
            &mut second_session
        } else {
            &mut mac_session
        };
        let (status, _) = asking.put("/checkin", &[], &body).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{method}");
    }
    rig.list_in_abm(SERIAL);
    rig.store.fail(Some("replace_live"));
    assert_eq!(
        second_session
            .put("/checkin", &[], &authenticate(SERIAL))
            .await
            .0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    rig.store.fail(None);
    assert_eq!(
        second_session
            .put("/checkin", &[], &authenticate(SERIAL))
            .await
            .0,
        StatusCode::OK
    );
}

/// Proves: when the audit row of a replacement cannot be written the replacement is not made and the
/// Mac is told to try again (`503`), with the live enrollment still the old one.
///
/// Catches: a replacement with no audit row, which the threat model counts on to show a takeover.
#[tokio::test]
async fn a_replacement_that_cannot_be_audited_is_not_made() {
    let rig = Rig::new("checkin-audit-fails");
    let (_, _session) = enrolled(&rig, 1).await;
    let live = live_id(&rig);
    rig.list_in_abm(SERIAL);
    let second = mac(&rig, 2);
    rig.issue(&second, SERIAL, UDID, false);
    let mut second_session = session(&rig, &second).await;
    rig.fake.refuse(|_| Some(crate::store::audit_failure()));
    let (status, _) = second_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(live_id(&rig), live);
}

/// Proves: the system giving no random bytes for an enrollment id is a server error and nothing is
/// entered.
///
/// Catches: an enrollment entered under an id of zeros.
#[tokio::test]
async fn no_random_bytes_for_an_enrollment_id_is_a_server_error() {
    let rig = Rig::with_random("checkin-no-random", Box::new(|_| false));
    let mac = mac(&rig, 1);
    rig.issue(&mac, SERIAL, UDID, false);
    let mut mac_session = session(&rig, &mac).await;
    let (status, _) = mac_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(rig.store.live_enrollment(SERIAL).expect("read").is_none());
}

/// Proves: an enrollment that ends between the read and the write of its token is told to the Mac as
/// unauthorized, and a gate that cannot write its record does not stop the Mac being answered.
///
/// Catches: a token stored for an enrollment that is gone, and a Mac left unanswered by the gate's disk.
#[tokio::test]
async fn an_enrollment_that_ended_mid_request_and_a_gate_that_cannot_write_are_handled() {
    let rig = Rig::new("checkin-race-and-gate");
    let (_, mut mac_session) = enrolled(&rig, 1).await;
    rig.store.end_enrollments_before_set_push();
    assert_eq!(
        mac_session.put("/checkin", &[], &token_update("m")).await.0,
        StatusCode::UNAUTHORIZED
    );

    use std::os::unix::fs::PermissionsExt as _;
    let rig = Rig::with_gate("checkin-gate-disk", |queue, dir, time| {
        let store = hostkeeper_mdm_gate::FileDeviceStore::open(
            std::sync::Arc::new(hostkeeper_enrollment::fs::RealFs),
            &dir.path().join("devices"),
        )
        .expect("records");
        std::sync::Arc::new(
            hostkeeper_mdm_gate::Gate::open(
                Box::new(store),
                queue,
                time,
                crate::mdm_server::support::counting(),
            )
            .expect("opens"),
        )
    });
    let devices = rig.store_dir().join("devices");
    std::fs::set_permissions(&devices, std::fs::Permissions::from_mode(0o500)).expect("mode");
    let mac = mac(&rig, 1);
    rig.issue(&mac, SERIAL, UDID, false);
    let mut mac_session = session(&rig, &mac).await;
    let (status, _) = mac_session
        .put("/checkin", &[], &authenticate(SERIAL))
        .await;
    std::fs::set_permissions(&devices, std::fs::Permissions::from_mode(0o700)).expect("mode");
    assert_eq!(
        status,
        StatusCode::OK,
        "the Mac is answered; the gate logged its failure"
    );
}
