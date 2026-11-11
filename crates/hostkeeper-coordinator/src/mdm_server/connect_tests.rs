//! `PUT /mdm` (`docs/design/coordinator-mdm.md`, sections 4.3 and 5.2) and the whole of 5.1 but
//! `/enroll`, against the file tables, a fake APNs and a fake Mac that dials the listener in memory.

use std::sync::Arc;

use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_mdm_gate::{FileDeviceStore, Gate};
use hyper::StatusCode;

use super::auth::SIGNATURE_HEADER;
use crate::mdm_server::support::{
    BODY, Identity2, Rig, SERIAL, Session, Spec, UDID, authenticate, command_in, header, result,
    result_with, token_update,
};
use crate::store::mdm::{CommandState, MdmStore};

/// A Mac that is enrolled and has a push token, with a connection.
async fn mac(rig: &Rig) -> (Identity2, Session) {
    let mac = rig.mac_ca.issue(1, SERIAL);
    rig.issue(&mac, SERIAL, UDID, false);
    let mut session = Session::dial(rig, Some(&mac)).await.expect("connected");
    assert_eq!(
        session.put("/checkin", &[], &authenticate(SERIAL)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        session.put("/checkin", &[], &token_update("magic")).await.0,
        StatusCode::OK
    );
    (mac, session)
}

fn live_id(rig: &Rig) -> String {
    rig.store
        .live_enrollment(SERIAL)
        .expect("read")
        .expect("live")
        .enrollment_id
}

fn state(rig: &Rig, n: u8) -> CommandState {
    rig.store
        .command(&Rig::command_id(n))
        .expect("read")
        .expect("the command")
        .state
}

/// Proves: an `Idle` is answered with the oldest queued command, the Mac's answer to it is taken and
/// answered with the next one, and the end of the queue is an empty answer; a request of the Mac
/// answers its push, so the next command is pushed at once.
///
/// Catches: a Mac that has to be pushed once per command, or handed a command twice on one
/// connection.
#[tokio::test]
async fn an_idle_is_handed_the_commands_in_order_and_an_empty_queue_ends_the_session() {
    let rig = Rig::new("connect-drain");
    let (_, mut mac) = mac(&rig).await;
    let id = live_id(&rig);
    rig.enqueue(&id, 1).await.expect("queued");
    rig.enqueue(&id, 2).await.expect("queued");
    assert_eq!(rig.apns.sent().len(), 1, "one push for the burst");
    let (status, body) = mac.put("/mdm", &[], &result("Idle", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(command_in(&body).uuid, Rig::command_id(1));
    assert_eq!(command_in(&body).request_type, "DeviceInformation");
    let ack = result("Acknowledged", Some(&Rig::command_id(1)));
    let (_, body) = mac.put("/mdm", &[], &ack).await;
    assert_eq!(command_in(&body).uuid, Rig::command_id(2));
    let (status, body) = mac
        .put(
            "/mdm",
            &[],
            &result("Acknowledged", Some(&Rig::command_id(2))),
        )
        .await;
    assert_eq!((status, body.len()), (StatusCode::OK, 0));
    assert_eq!(
        (state(&rig, 1), state(&rig, 2)),
        (CommandState::Acked, CommandState::Acked)
    );
    rig.enqueue(&id, 3).await.expect("queued");
    assert_eq!(
        rig.apns.sent().len(),
        2,
        "pushed at once after the Mac was awake"
    );
    let kept = rig.store.commands(&id).expect("read");
    assert!(
        kept[0].response.is_some(),
        "the Mac's answer is kept with the command"
    );
}

/// Proves: `NotNow` keeps the command and does not hand it back on the connection that deferred it,
/// a new connection is handed it, and the push floor is armed: the sweep at the floor pushes.
///
/// Catches: a deferred command in a loop on one connection, or lost when the Mac hangs up.
#[tokio::test]
async fn not_now_is_not_handed_back_on_the_connection_and_waits_for_the_next() {
    let rig = Rig::new("connect-not-now");
    let (mac_identity, mut first) = mac(&rig).await;
    let id = live_id(&rig);
    rig.enqueue(&id, 1).await.expect("queued");
    let (_, body) = first.put("/mdm", &[], &result("Idle", None)).await;
    assert_eq!(command_in(&body).uuid, Rig::command_id(1));
    let (status, body) = first
        .put("/mdm", &[], &result("NotNow", Some(&Rig::command_id(1))))
        .await;
    assert_eq!((status, body.len()), (StatusCode::OK, 0));
    let (_, body) = first.put("/mdm", &[], &result("Idle", None)).await;
    assert!(body.is_empty(), "not handed back on this connection");
    let row = rig
        .store
        .command(&Rig::command_id(1))
        .expect("read")
        .expect("kept");
    assert_eq!((row.state, row.deferred), (CommandState::Queued, true));
    rig.time.advance(3599);
    rig.queue.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 1, "never inside the floor");
    rig.time.advance(1);
    rig.queue.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 2, "the floor pushed the Mac again");
    let mut second = Session::dial(&rig, Some(&mac_identity))
        .await
        .expect("connected");
    let (_, body) = second.put("/mdm", &[], &result("Idle", None)).await;
    assert_eq!(
        command_in(&body).uuid,
        Rig::command_id(1),
        "a new connection is handed it"
    );
}

/// Proves: an `Error` and a `CommandFormatError` end the command; a result for a command that is not
/// sent to the Mac, or that is unknown, changes nothing and is answered with the next command (or
/// none).
///
/// Catches: a late or forged result acknowledging a command the Mac never held.
#[tokio::test]
async fn an_error_ends_the_command_and_a_result_for_a_command_not_sent_is_dropped() {
    let rig = Rig::new("connect-results");
    let (_, mut mac) = mac(&rig).await;
    let id = live_id(&rig);
    rig.enqueue(&id, 1).await.expect("queued");
    rig.enqueue(&id, 2).await.expect("queued");
    rig.enqueue(&id, 3).await.expect("queued");
    mac.put("/mdm", &[], &result("Idle", None)).await;
    let chain = plist::Value::Array(vec![plist::Value::Dictionary(
        [
            ("ErrorCode".to_owned(), plist::Value::Integer(12.into())),
            ("ErrorDomain".to_owned(), "MCMDMErrorDomain".into()),
        ]
        .into_iter()
        .collect(),
    )]);
    let failed = result_with(
        "Error",
        Some(&Rig::command_id(1)),
        &[("ErrorChain", chain.clone())],
    );
    let (_, body) = mac.put("/mdm", &[], &failed).await;
    assert_eq!(state(&rig, 1), CommandState::Errored);
    assert_eq!(command_in(&body).uuid, Rig::command_id(2));
    // Command 3 is queued: a result for it is dropped, and it is handed out as the next one.
    let (_, body) = mac
        .put(
            "/mdm",
            &[],
            &result("Acknowledged", Some(&Rig::command_id(3))),
        )
        .await;
    assert_eq!(
        state(&rig, 3),
        CommandState::Sent,
        "not acknowledged by a result for a queued command"
    );
    assert_eq!(command_in(&body).uuid, Rig::command_id(3));
    let malformed = result_with(
        "CommandFormatError",
        Some(&Rig::command_id(2)),
        &[("ErrorChain", chain)],
    );
    let (_, body) = mac.put("/mdm", &[], &malformed).await;
    assert_eq!(state(&rig, 2), CommandState::Errored);
    assert!(body.is_empty());
    let unknown = result("Acknowledged", Some("9f3c1a7e-5b42-4d86-a0c9-7e2d84b61f35"));
    let (_, body) = mac.put("/mdm", &[], &unknown).await;
    assert!(body.is_empty());
    assert_eq!(state(&rig, 3), CommandState::Sent);
}

/// Proves: a request is refused when it has no accepted credential (`401`), names another device than
/// its identity (`403`) or is no result (`400`), and a Mac that signs its messages is accepted by
/// its signature alone.
///
/// Catches: a peer on the LAN draining another Mac's commands.
#[tokio::test]
async fn a_request_must_come_from_the_live_identity_of_the_device_it_names() {
    let rig = Rig::new("connect-credentials");
    let (identity, mut mac) = mac(&rig).await;
    rig.enqueue(&live_id(&rig), 1).await.expect("queued");
    let idle = result("Idle", None);
    let mut bare = Session::dial(&rig, None).await.expect("connected");
    assert_eq!(
        bare.put("/mdm", &[], &idle).await.0,
        StatusCode::UNAUTHORIZED
    );
    let wrong = [(SIGNATURE_HEADER, header(BODY, &identity, &Spec::default()))];
    assert_eq!(
        bare.put("/mdm", &wrong, &idle).await.0,
        StatusCode::UNAUTHORIZED
    );
    let mut other_device = plist::Dictionary::new();
    other_device.insert("Status".to_owned(), "Idle".into());
    other_device.insert("UDID".to_owned(), "SOMEONEELSE".into());
    let mut body = Vec::new();
    plist::Value::Dictionary(other_device)
        .to_writer_xml(&mut body)
        .expect("a plist");
    assert_eq!(mac.put("/mdm", &[], &body).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        mac.put("/mdm", &[], b"not a plist").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        state(&rig, 1),
        CommandState::Queued,
        "nothing was handed out"
    );
    let signed = [(SIGNATURE_HEADER, header(&idle, &identity, &Spec::default()))];
    let (status, body) = bare.put("/mdm", &signed, &idle).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(command_in(&body).uuid, Rig::command_id(1));
}

/// Proves: a command handed out and not answered before the timeout is handed out again to the next
/// connection: delivery is at least once.
///
/// Catches: a Mac that dropped its connection with a command in hand never getting it again.
#[tokio::test]
async fn a_command_the_timeout_returned_is_handed_to_the_next_connection() {
    let rig = Rig::new("connect-timeout");
    let (identity, mut first) = mac(&rig).await;
    rig.enqueue(&live_id(&rig), 1).await.expect("queued");
    let (_, body) = first.put("/mdm", &[], &result("Idle", None)).await;
    assert_eq!(command_in(&body).uuid, Rig::command_id(1));
    drop(first);
    rig.time.advance(86_400);
    rig.queue.sweep().await.expect("swept");
    let mut second = Session::dial(&rig, Some(&identity))
        .await
        .expect("connected");
    let (_, body) = second.put("/mdm", &[], &result("Idle", None)).await;
    assert_eq!(command_in(&body).uuid, Rig::command_id(1));
    assert_eq!(
        rig.store
            .command(&Rig::command_id(1))
            .expect("read")
            .expect("kept")
            .deliveries,
        2
    );
}

/// Proves: a store that fails is a `500` for each step of a request on `/mdm`, nothing is handed
/// out that was not recorded as sent, and the Mac asks again.
///
/// Catches: a command handed out whose claim was not stored.
#[tokio::test]
async fn a_store_that_fails_is_a_server_error_on_mdm() {
    let rig = Rig::new("connect-store-fails");
    let (_, mut mac) = mac(&rig).await;
    let id = live_id(&rig);
    rig.enqueue(&id, 1).await.expect("queued");
    for method in ["identity", "live_enrollment", "record_contact", "claim"] {
        rig.store.fail(Some(method));
        let (status, _) = mac.put("/mdm", &[], &result("Idle", None)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{method}");
    }
    assert_eq!(state(&rig, 1), CommandState::Queued);
    rig.store.fail(Some("answer"));
    let (status, _) = mac
        .put(
            "/mdm",
            &[],
            &result("Acknowledged", Some(&Rig::command_id(1))),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    rig.store.fail(None);
    let (_, body) = mac.put("/mdm", &[], &result("Idle", None)).await;
    assert_eq!(command_in(&body).uuid, Rig::command_id(1));
}

/// Proves: with the gate on, the whole of 5.1 but `/enroll` runs against a fake Mac: `Authenticate`
/// and `TokenUpdate` reach the gate, which queues `DeviceInformation` and `SecurityInfo` in the
/// coordinator's queue and a push goes to the fake APNs; the Mac connects, takes both commands,
/// answers, and the gate then says the serial is enrolled and supervised; a wiped Mac that enrolls
/// again replaces the old enrollment in the gate's records too, so the serial is not ambiguous.
///
/// Catches: any seam between the listener, the queue and the gate that the pieces' own tests miss.
#[tokio::test]
async fn a_fake_mac_enrolls_drains_the_gates_commands_and_re_enrolls() {
    let rig = Rig::with_gate("connect-end-to-end", |queue, dir, time| {
        let store =
            FileDeviceStore::open(Arc::new(RealFs), &dir.path().join("devices")).expect("records");
        Arc::new(
            Gate::open(
                Box::new(store),
                queue,
                time,
                crate::mdm_server::support::counting(),
            )
            .expect("opens"),
        )
    });
    let gate = rig.server.gate.clone().expect("the gate");
    let (identity, mut mac) = mac(&rig).await;
    let id = live_id(&rig);
    assert_eq!(
        rig.apns.sent().len(),
        1,
        "the gate's first command was pushed"
    );
    let (_, body) = mac.put("/mdm", &[], &result("Idle", None)).await;
    let first = command_in(&body);
    assert_eq!(first.request_type, "DeviceInformation");
    let responses = plist::Value::Dictionary(
        [
            ("SerialNumber".to_owned(), plist::Value::from(SERIAL)),
            ("UDID".to_owned(), plist::Value::from(UDID)),
            ("IsSupervised".to_owned(), plist::Value::Boolean(true)),
        ]
        .into_iter()
        .collect(),
    );
    let answer = result_with(
        "Acknowledged",
        Some(&first.uuid),
        &[("QueryResponses", responses)],
    );
    let (_, body) = mac.put("/mdm", &[], &answer).await;
    let second = command_in(&body);
    assert_eq!(second.request_type, "SecurityInfo");
    let (_, body) = mac
        .put(
            "/mdm",
            &[],
            &result_with("Acknowledged", Some(&second.uuid), &[]),
        )
        .await;
    assert!(body.is_empty());
    let status = gate.status(SERIAL).expect("known");
    assert!(status.enrolled && status.supervised);
    // The Mac is wiped and enrolls again; its serial is in ABM.
    rig.list_in_abm(SERIAL);
    let again = rig.mac_ca.issue(2, SERIAL);
    rig.issue(&again, SERIAL, UDID, false);
    let mut fresh = Session::dial(&rig, Some(&again)).await.expect("connected");
    assert_eq!(
        fresh.put("/checkin", &[], &authenticate(SERIAL)).await.0,
        StatusCode::OK
    );
    assert_ne!(live_id(&rig), id);
    assert_eq!(
        fresh.put("/checkin", &[], &token_update("magic-2")).await.0,
        StatusCode::OK
    );
    let (_, body) = fresh.put("/mdm", &[], &result("Idle", None)).await;
    assert_eq!(
        command_in(&body).request_type,
        "DeviceInformation",
        "the gate asks the new enrollment again"
    );
    assert!(
        gate.status(SERIAL).is_ok(),
        "one enrollment of the serial in the gate, not two"
    );
    let (status, _) = Session::dial(&rig, Some(&identity))
        .await
        .expect("connected")
        .put("/mdm", &[], &result("Idle", None))
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the old certificate is not accepted"
    );
}
