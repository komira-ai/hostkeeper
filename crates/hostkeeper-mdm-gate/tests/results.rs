//! How the gate takes the results of the commands it sent: which results it reads, which it
//! passes over, what an error answer or a failed queue call leaves in the record, and when a
//! query is asked again.

mod support;

use std::sync::Arc;

use hostkeeper_enrollment::fs::{Fs, RealFs};
use hostkeeper_mdm_gate::{
    CommandUuid, DeviceInformation, DeviceRecord, DeviceStore, FileDeviceStore, Handled, Phase,
    SecurityInfo, Sent,
};
use plist::Value;
use serde_json::json;
use support::{
    Answer, OTHER_UDID, Rig, SERIAL, TestFs, UDID, authenticate, check_out, device_information,
    idle, result, retarget, security_info, token_update,
};

const SOME_UUID: &str = "ABCDEF01-0000-4000-8000-000000000000";

fn kinds(rig: &Rig) -> Vec<String> {
    rig.hub
        .commands()
        .into_iter()
        .map(|(kind, _)| kind)
        .collect()
}

async fn handled(rig: &Rig, body: &[u8]) -> Handled {
    rig.gate.webhook(body).await.expect("the event is taken")
}

/// Catches: a result for a UUID the gate never sent, or one it sent to another device, changing
/// a record (here: answering the other device's query, or recording its error).
#[tokio::test]
async fn a_result_for_a_command_the_gate_did_not_send_to_that_device_is_passed_over() {
    let rig = Rig::new("foreign-result").await;
    rig.post(&token_update(UDID, 1)).await;
    rig.post(&token_update(OTHER_UDID, 1)).await;
    let before = rig.gate.records();
    let heard = rig.hub.heard().len();

    for status in [
        "Acknowledged",
        "Error",
        "CommandFormatError",
        "NotNow",
        "Idle",
    ] {
        let body = result(UDID, Some(SOME_UUID), status, &[]);
        assert_eq!(handled(&rig, &body).await, Handled::PassedOver, "{status}");
    }
    // The UUID of OTHER_UDID's DeviceInformation, said by UDID.
    let theirs = rig.sent(OTHER_UDID, "DeviceInformation");
    let body = device_information(UDID, &theirs, Some(SERIAL), Some(true));
    assert_eq!(handled(&rig, &body).await, Handled::PassedOver);
    let body = result(UDID, Some(&theirs), "Error", &[]);
    assert_eq!(handled(&rig, &body).await, Handled::PassedOver);
    assert_eq!(rig.gate.records(), before);
    assert_eq!(rig.hub.heard().len(), heard);
}

/// Catches: a result replayed after the command was answered read a second time.
#[tokio::test]
async fn a_result_answered_once_is_passed_over_the_second_time() {
    let rig = Rig::new("replayed").await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "DeviceInformation");
    let body = device_information(UDID, &uuid, Some(SERIAL), Some(true));
    assert_eq!(handled(&rig, &body).await, Handled::Recorded);
    let before = rig.gate.records();
    let other = device_information(UDID, &uuid, Some("C02OTHER0001"), Some(false));
    assert_eq!(handled(&rig, &other).await, Handled::PassedOver);
    assert_eq!(rig.gate.records(), before);
}

/// Catches: `NotNow`, or a status the gate does not know, taking the command off the list or
/// reading its answer; the device sends the command again itself.
#[tokio::test]
async fn not_now_and_an_unknown_status_leave_the_command_unanswered() {
    let rig = Rig::new("not-now").await;
    rig.post(&token_update(UDID, 1)).await;
    let before = rig.gate.records();
    let uuid = rig.sent(UDID, "DeviceInformation");
    for status in ["NotNow", "Acknowledgedd", "", "acknowledged"] {
        let body = result(UDID, Some(&uuid), status, &[]);
        assert_eq!(
            handled(&rig, &body).await,
            Handled::PassedOver,
            "{status:?}"
        );
        assert_eq!(rig.gate.records(), before);
    }
    assert_eq!(rig.hub.commands().len(), 2);
    // The command is still answerable afterwards.
    let body = device_information(UDID, &uuid, Some(SERIAL), Some(true));
    assert_eq!(handled(&rig, &body).await, Handled::Recorded);
    assert_eq!(rig.record(UDID).serial.as_deref(), Some(SERIAL));
}

/// Catches: an error answer read as an answer; one that is asked again at once (a loop against
/// a device that refuses the query), or never; the refusal's text.
#[tokio::test]
async fn an_error_answer_refuses_the_query_until_the_next_token_update() {
    for status in ["Error", "CommandFormatError"] {
        let rig = Rig::new("error-answer").await;
        rig.post(&token_update(UDID, 1)).await;
        let uuid = rig.sent(UDID, "DeviceInformation");
        let body = result(UDID, Some(&uuid), status, &[]);
        assert_eq!(handled(&rig, &body).await, Handled::Recorded);
        let record = rig.record(UDID);
        assert_eq!(
            record.last_error.as_deref(),
            Some(format!("DeviceInformation: the device answered {status}").as_str())
        );
        assert_eq!(record.refused, ["DeviceInformation"]);
        assert_eq!(record.device_information_unix, None);
        assert_eq!(record.sent.len(), 1, "only the SecurityInfo is unanswered");

        // An `Idle` sends nothing: the refused query is not due.
        assert_eq!(handled(&rig, &idle(UDID)).await, Handled::PassedOver);
        assert_eq!(kinds(&rig), ["DeviceInformation", "SecurityInfo"]);

        // The next `TokenUpdate` of the same enrollment asks again.
        rig.post(&token_update(UDID, 2)).await;
        assert_eq!(
            kinds(&rig),
            ["DeviceInformation", "SecurityInfo", "DeviceInformation"]
        );
        let record = rig.record(UDID);
        assert!(record.refused.is_empty());
        assert_eq!(record.enrollment, 1);
        assert_eq!(record.sent.len(), 2);
    }
}

/// Catches: a refusal recorded twice for one query type, or an error answer to a `SecurityInfo`
/// refusing the other query.
#[tokio::test]
async fn a_security_info_error_refuses_only_that_query() {
    let rig = Rig::new("security-error").await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "SecurityInfo");
    rig.post(&result(UDID, Some(&uuid), "Error", &[])).await;
    let record = rig.record(UDID);
    assert_eq!(record.refused, ["SecurityInfo"]);
    assert_eq!(
        record.last_error.as_deref(),
        Some("SecurityInfo: the device answered Error")
    );
    assert_eq!(record.sent.len(), 1);

    // Refused again (as a record can be told twice): one entry.
    let mut record = record;
    record.refuse("SecurityInfo", "again");
    assert_eq!(record.refused, ["SecurityInfo"]);
    assert_eq!(record.last_error.as_deref(), Some("SecurityInfo: again"));
}

/// Catches: an `Idle` that does not send a query due (after NanoHUB failed to queue it), or one
/// that sends when nothing is due or the device is not enrolled.
#[tokio::test]
async fn an_idle_sends_the_queries_that_are_due_and_only_those() {
    let rig = Rig::new("idle").await;
    rig.hub.answer(Answer::Status(
        500,
        json!({"command_error": "queue is down"}).to_string(),
    ));
    rig.post(&token_update(UDID, 1)).await;
    let record = rig.record(UDID);
    assert!(record.sent.is_empty(), "NanoHUB queued none of them");
    assert_eq!(kinds(&rig), ["DeviceInformation", "SecurityInfo"]);

    rig.hub.answer(Answer::Queued);
    assert_eq!(handled(&rig, &idle(UDID)).await, Handled::Recorded);
    assert_eq!(rig.record(UDID).sent.len(), 2);
    assert_eq!(
        kinds(&rig),
        [
            "DeviceInformation",
            "SecurityInfo",
            "DeviceInformation",
            "SecurityInfo"
        ]
    );

    // Both are sent now: nothing is due.
    assert_eq!(handled(&rig, &idle(UDID)).await, Handled::PassedOver);
    assert_eq!(rig.hub.commands().len(), 4);

    // An `Idle` that names no command but is not `Idle` is passed over too; so is an `Idle` of
    // a device that has not sent its `TokenUpdate`.
    let odd = result(UDID, None, "Acknowledged", &[]);
    assert_eq!(handled(&rig, &odd).await, Handled::PassedOver);
    rig.post(&authenticate(OTHER_UDID, None)).await;
    assert_eq!(handled(&rig, &idle(OTHER_UDID)).await, Handled::PassedOver);
    assert_eq!(rig.hub.commands().len(), 4);
}

/// Catches: an answer read whose own `CommandUUID` is not the command's, or that holds no
/// `QueryResponses` or no `SecurityInfo`, taken as an answer; a refusal without its exact text.
#[tokio::test]
async fn an_answer_that_is_not_the_commands_is_refused() {
    let rig = Rig::new("bad-answers").await;
    rig.post(&token_update(UDID, 1)).await;
    let di = rig.sent(UDID, "DeviceInformation");
    let si = rig.sent(UDID, "SecurityInfo");

    // The event names the command, the plist another.
    let body = retarget(
        &device_information(UDID, SOME_UUID, Some(SERIAL), Some(true)),
        &di,
    );
    assert_eq!(handled(&rig, &body).await, Handled::Recorded);
    let record = rig.record(UDID);
    assert_eq!(
        record.last_error.as_deref(),
        Some("DeviceInformation: the answer is not a DeviceInformation result")
    );
    assert_eq!(
        (record.serial.clone(), record.device_information_unix),
        (None, None)
    );
    assert_eq!(record.refused, ["DeviceInformation"]);
    assert_eq!(record.sent.len(), 1);

    // A SecurityInfo answer with no `SecurityInfo` key.
    let body = result(UDID, Some(&si), "Acknowledged", &[]);
    rig.post(&body).await;
    let record = rig.record(UDID);
    assert_eq!(
        record.last_error.as_deref(),
        Some("SecurityInfo: the answer is not a SecurityInfo result")
    );
    assert_eq!(record.refused, ["DeviceInformation", "SecurityInfo"]);
    assert!(record.sent.is_empty());

    // Both asked again at the next TokenUpdate; a DeviceInformation with no QueryResponses, and
    // a SecurityInfo whose key is not a dictionary.
    rig.post(&token_update(UDID, 2)).await;
    let di = rig.sent(UDID, "DeviceInformation");
    let si = rig.sent(UDID, "SecurityInfo");
    rig.post(&result(
        UDID,
        Some(&di),
        "Acknowledged",
        &[("UDID", Value::from(UDID))],
    ))
    .await;
    rig.post(&result(
        UDID,
        Some(&si),
        "Acknowledged",
        &[("SecurityInfo", Value::from("x"))],
    ))
    .await;
    let record = rig.record(UDID);
    assert_eq!(record.refused, ["DeviceInformation", "SecurityInfo"]);
    assert_eq!(record.serial, None);
    assert_eq!(record.security_info_unix, None);

    // QueryResponses that is not a dictionary.
    rig.post(&token_update(UDID, 3)).await;
    let di = rig.sent(UDID, "DeviceInformation");
    let body = result(
        UDID,
        Some(&di),
        "Acknowledged",
        &[("QueryResponses", Value::from("x"))],
    );
    rig.post(&body).await;
    assert_eq!(rig.record(UDID).device_information_unix, None);
}

/// Catches: a `SecurityInfo` answer without `ManagementStatus`, or with keys missing, refused or
/// read as false; the flags it does give lost.
#[tokio::test]
async fn a_security_info_answer_keeps_the_flags_it_gives_and_none_it_does_not() {
    let rig = Rig::new("security-flags").await;
    rig.post(&token_update(UDID, 1)).await;
    let si = rig.sent(UDID, "SecurityInfo");
    let info = support::dict(&[]);
    let keys = [("SecurityInfo", Value::Dictionary(info))];
    rig.post(&result(UDID, Some(&si), "Acknowledged", &keys))
        .await;
    let record = rig.record(UDID);
    assert_eq!(
        (
            record.enrolled_via_dep,
            record.user_approved,
            record.security_info_unix
        ),
        (None, None, Some(1_000))
    );
    assert_eq!(record.last_error, None);

    rig.post(&token_update(OTHER_UDID, 1)).await;
    let si = rig.sent(OTHER_UDID, "SecurityInfo");
    let status = support::dict(&[("EnrolledViaDEP", Value::Boolean(false))]);
    let info = support::dict(&[("ManagementStatus", Value::Dictionary(status))]);
    let keys = [("SecurityInfo", Value::Dictionary(info))];
    rig.post(&result(OTHER_UDID, Some(&si), "Acknowledged", &keys))
        .await;
    let record = rig.record(OTHER_UDID);
    assert_eq!(
        (record.enrolled_via_dep, record.user_approved),
        (Some(false), None)
    );
}

/// Catches: a `DeviceInformation` answer naming another UDID than the device's taken (a Mac
/// answering with another Mac's identity); a device whose UDID the gate has not heard, or an
/// answer with no UDID, refused for it.
#[tokio::test]
async fn a_device_information_answer_naming_another_udid_is_refused() {
    let rig = Rig::new("other-udid").await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "DeviceInformation");
    let body = device_information(OTHER_UDID, &uuid, Some(SERIAL), Some(true));
    // `device_information` posts as OTHER_UDID; the event is for the device UDID.
    let body = retarget(&body, &uuid);
    let body = swap_ids(&body, UDID);
    assert_eq!(handled(&rig, &body).await, Handled::Recorded);
    let record = rig.record(UDID);
    assert_eq!(
        record.last_error.as_deref(),
        Some("DeviceInformation: the answer names another UDID")
    );
    assert_eq!(
        (
            record.serial,
            record.supervised,
            record.device_information_unix
        ),
        (None, None, None)
    );
    assert_eq!(record.refused, ["DeviceInformation"]);
}

/// The event `body` made as the device `id`'s.
fn swap_ids(body: &[u8], id: &str) -> Vec<u8> {
    let mut event: serde_json::Value = serde_json::from_slice(body).expect("json");
    event["acknowledge_event"]["ids"]["id"] = json!(id);
    event["acknowledge_event"]["udid"] = json!(id);
    serde_json::to_vec(&event).expect("json")
}

/// Catches: an answer with no `UDID` refused, or any answer refused for a device whose UDID the
/// check-ins did not give.
#[tokio::test]
async fn an_answer_with_no_udid_or_for_a_device_with_none_is_read() {
    let rig = Rig::new("no-udid").await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "DeviceInformation");
    let answers = Value::Dictionary(support::dict(&[("SerialNumber", Value::from(SERIAL))]));
    let keys = [("QueryResponses", answers)];
    rig.post(&result(UDID, Some(&uuid), "Acknowledged", &keys))
        .await;
    let record = rig.record(UDID);
    assert_eq!(record.serial.as_deref(), Some(SERIAL));
    assert_eq!(record.supervised, Some(false));

    // A TokenUpdate with no `udid`: the record has none, so any UDID in the answer is read.
    let ids = json!({"id": OTHER_UDID, "type": "Device"});
    let raw = base64_of_token_update();
    let body = json!({
        "topic": "mdm.TokenUpdate",
        "checkin_event": {"ids": ids, "raw_payload": raw},
    });
    rig.post(&serde_json::to_vec(&body).expect("json")).await;
    assert_eq!(rig.record(OTHER_UDID).udid, "");
    let uuid = rig.sent(OTHER_UDID, "DeviceInformation");
    let body = swap_ids(
        &device_information(UDID, &uuid, Some("C02THIRD0001"), Some(true)),
        OTHER_UDID,
    );
    rig.post(&retarget(&body, &uuid)).await;
    assert_eq!(
        rig.record(OTHER_UDID).serial.as_deref(),
        Some("C02THIRD0001")
    );
}

fn base64_of_token_update() -> String {
    use base64::Engine as _;
    let dict = support::dict(&[("MessageType", Value::from("TokenUpdate"))]);
    base64::engine::general_purpose::STANDARD.encode(support::plist(dict))
}

/// Catches: an event of a device the gate has no record of creating one when it begins nothing
/// (a `CheckOut`, a result, an `Idle`); an `Authenticate` or a `TokenUpdate` not creating one.
#[tokio::test]
async fn an_event_that_begins_nothing_creates_no_record() {
    let rig = Rig::new("unknown-device").await;
    for body in [
        check_out(UDID),
        result(UDID, Some(SOME_UUID), "Acknowledged", &[]),
        idle(UDID),
    ] {
        assert_eq!(handled(&rig, &body).await, Handled::PassedOver);
    }
    assert!(rig.gate.records().is_empty());
    assert!(rig.hub.heard().is_empty());
    assert!(!rig.dir().join(format!("{UDID}.pb")).exists());

    assert_eq!(
        handled(&rig, &authenticate(UDID, None)).await,
        Handled::Recorded
    );
    assert_eq!(
        handled(&rig, &token_update(OTHER_UDID, 1)).await,
        Handled::Recorded
    );
    assert_eq!(rig.gate.records().len(), 2);
}

/// Catches: a second `TokenUpdate`, while a query is unanswered, sending it again: a Mac that is
/// off would pile commands up in NanoHUB's queue.
#[tokio::test]
async fn a_second_token_update_sends_nothing_while_the_queries_are_unanswered() {
    let rig = Rig::new("no-pile-up").await;
    rig.post(&token_update(UDID, 1)).await;
    for tally in [2, 3, 9] {
        rig.post(&token_update(UDID, tally)).await;
    }
    rig.post(&support::checkin("TokenUpdate", UDID, &[], None))
        .await;
    assert_eq!(kinds(&rig), ["DeviceInformation", "SecurityInfo"]);
    assert_eq!(rig.record(UDID).enrollment, 1);
}

/// Catches: a re-sent first `TokenUpdate` (tally 1) the gate hears while it holds the device
/// enrolled not starting a new enrollment, or a `TokenUpdate` after a `CheckOut` not asking again.
#[tokio::test]
async fn a_first_token_update_of_an_enrolled_device_begins_a_new_enrollment() {
    let rig = Rig::new("tally-one").await;
    rig.enroll(UDID, SERIAL, true).await;
    assert_eq!(kinds(&rig).len(), 2);
    rig.clock.set(2_000);
    rig.post(&token_update(UDID, 1)).await;
    let record = rig.record(UDID);
    assert_eq!(
        (record.enrollment, record.serial.clone(), record.supervised),
        (2, None, None)
    );
    assert_eq!(record.enrolled_since_unix, Some(2_000));
    assert_eq!(record.sent.len(), 2);
    assert_eq!(kinds(&rig).len(), 4);

    rig.post(&check_out(UDID)).await;
    assert_eq!(rig.record(UDID).phase, Phase::CheckedOut);
    rig.post(&token_update(UDID, 5)).await;
    assert_eq!(rig.record(UDID).enrollment, 3);
    assert_eq!(kinds(&rig).len(), 6);
}

/// Catches: no random bytes for a UUID sending a command with a made-up one, panicking, or
/// leaving no reason; the first unavailable draw stops the loop.
#[tokio::test]
async fn no_random_bytes_sends_nothing_and_says_why() {
    let rig = Rig::with_random("no-random", support::no_random()).await;
    assert_eq!(
        handled(&rig, &token_update(UDID, 1)).await,
        Handled::Recorded
    );
    let record = rig.record(UDID);
    assert!(rig.hub.heard().is_empty());
    assert!(record.sent.is_empty());
    assert_eq!(record.phase, Phase::Enrolled);
    assert_eq!(
        record.last_error.as_deref(),
        Some("DeviceInformation: no random bytes for a command UUID")
    );
}

/// Catches: an `Acknowledged` for a sent command of a type the enrollment side sends (a profile)
/// left on the list, read as one of the queries, or changing anything else of the record.
#[tokio::test]
async fn an_acknowledged_profile_command_is_taken_off_the_list_and_changes_nothing_else() {
    let rig = Rig::new("profile-ack").await;
    let ack_uuid = CommandUuid::from_random([7; 16]);
    let other_uuid = CommandUuid::from_random([8; 16]);
    let mut record = DeviceRecord::new(support::id(UDID), 900);
    record.phase = Phase::Enrolled;
    record.enrollment = 1;
    record.device_information_unix = Some(950);
    record.security_info_unix = Some(950);
    record.sent = vec![
        Sent {
            uuid: ack_uuid.clone(),
            request_type: "InstallProfile".to_owned(),
            sent_unix: 960,
        },
        Sent {
            uuid: other_uuid,
            request_type: "RemoveProfile".to_owned(),
            sent_unix: 961,
        },
    ];
    let fs: Arc<dyn Fs> = Arc::new(RealFs);
    FileDeviceStore::open(fs, &rig.dir())
        .expect("the store")
        .save(&record)
        .expect("saved");
    let mut rig = rig;
    rig.restart().expect("the gate opens");
    assert_eq!(rig.gate.records(), [record.clone()]);

    let body = result(UDID, Some(ack_uuid.as_str()), "Acknowledged", &[]);
    assert_eq!(handled(&rig, &body).await, Handled::Recorded);
    record.sent.remove(0);
    assert_eq!(rig.gate.records(), [record]);
    assert!(rig.hub.heard().is_empty());
}

/// Catches: a failed queue call leaving the command on the list (so it is never asked again),
/// or leaving no reason; a retry that sends nothing at the next check-in.
#[tokio::test]
async fn a_query_nanohub_did_not_queue_is_taken_back_and_asked_at_the_next_check_in() {
    let rig = Rig::new("not-queued").await;
    rig.post(&token_update(UDID, 1)).await;
    let uuid = rig.sent(UDID, "SecurityInfo");
    rig.post(&security_info(UDID, &uuid, true, true)).await;
    let uuid = rig.sent(UDID, "DeviceInformation");
    rig.post(&result(UDID, Some(&uuid), "Error", &[])).await;
    assert_eq!(rig.record(UDID).refused, ["DeviceInformation"]);

    rig.hub.answer(Answer::Status(
        500,
        json!({"command_error": "boom"}).to_string(),
    ));
    rig.post(&token_update(UDID, 2)).await;
    let record = rig.record(UDID);
    assert!(record.sent.is_empty());
    assert_eq!(
        record.last_error.as_deref(),
        Some("DeviceInformation: NanoHUB did not queue the command: boom")
    );
    let before = rig.hub.commands().len();

    rig.hub.answer(Answer::Queued);
    assert_eq!(handled(&rig, &idle(UDID)).await, Handled::Recorded);
    assert_eq!(rig.record(UDID).sent.len(), 1);
    assert_eq!(rig.hub.commands().len(), before + 1);
    assert_eq!(rig.hub.commands()[before].0, "DeviceInformation");
}

/// Catches: both queries failing to queue leaving the first one's reason (the last failure is
/// the one kept), or either command left as sent.
#[tokio::test]
async fn when_both_queries_fail_the_last_reason_is_kept() {
    let rig = Rig::new("both-fail").await;
    rig.hub.answer(Answer::Status(
        200,
        json!({"status": {UDID: {"command_error": "no"}}}).to_string(),
    ));
    rig.post(&token_update(UDID, 1)).await;
    let record = rig.record(UDID);
    assert!(record.sent.is_empty());
    assert_eq!(
        record.last_error.as_deref(),
        Some("SecurityInfo: NanoHUB did not queue the command: no")
    );
}

/// Catches: a reason longer than 200 characters kept whole, or one with a character outside
/// printable ASCII kept as it came (it is shown in logs and kept in the record); NanoHUB's
/// non-text error reduced to nothing.
#[tokio::test]
async fn a_reason_is_kept_printable_and_at_most_two_hundred_characters() {
    let rig = Rig::new("long-reason").await;
    let long = format!("é\n{}", "x".repeat(300));
    rig.hub.answer(Answer::Status(
        500,
        json!({"command_error": long}).to_string(),
    ));
    rig.post(&token_update(UDID, 1)).await;
    let prefix = "SecurityInfo: NanoHUB did not queue the command: ";
    let wanted = format!("{prefix}??{}", "x".repeat(200 - prefix.len() - 2));
    let kept = rig.record(UDID).last_error.expect("a reason");
    assert_eq!(kept.chars().count(), 200);
    assert_eq!(kept, wanted);

    rig.hub.answer(Answer::Status(
        500,
        json!({"command_error": {"code": 5}}).to_string(),
    ));
    rig.post(&token_update(UDID, 2)).await;
    assert_eq!(
        rig.record(UDID).last_error.as_deref(),
        Some("SecurityInfo: NanoHUB did not queue the command: an error")
    );
}

/// Catches: a push that failed counted as not queued (the command is queued all the same), so
/// the query is sent twice; the record losing the command.
#[tokio::test]
async fn a_push_error_keeps_the_command_as_sent() {
    let (_log, logs) = support::capture_logs();
    let rig = Rig::new("push-error").await;
    rig.hub.answer(Answer::Status(
        500,
        json!({"push_error": "APNs said no"}).to_string(),
    ));
    rig.post(&token_update(UDID, 1)).await;
    let record = rig.record(UDID);
    assert_eq!(record.sent.len(), 2);
    assert_eq!(record.last_error, None);
    rig.post(&idle(UDID)).await;
    assert_eq!(rig.hub.commands().len(), 2, "nothing is sent again");
    let warned = logs.events();
    assert_eq!(warned.len(), 2, "one warning for each command");
    assert_eq!(warned[0]["level"], "WARN");
    assert_eq!(warned[0]["push"], "APNs said no");
    assert_eq!(
        warned[0]["message"],
        "NanoHUB queued the command, and could not push the device; it fetches the command at its next check-in"
    );
}

/// Catches: a record write that fails while a failed queue call is taken back crashing the
/// event or losing the in-memory record; the command stays on the list in memory.
#[tokio::test]
async fn a_write_failing_when_a_command_is_taken_back_is_logged_and_the_event_taken() {
    let (_log, logs) = support::capture_logs();
    let fs = Arc::new(TestFs::default());
    let scratch = support::scratch("takeback-write");
    let hub = support::FakeNanoHub::start().await;
    let clock = support::Clock::at(1_000);
    let dir = support::dir_of(&scratch);
    let gate =
        support::open_on(fs.clone(), &dir, &hub, &clock, support::counting()).expect("opens");
    hub.answer(Answer::Status(
        500,
        json!({"command_error": "down"}).to_string(),
    ));
    // The first write (the record with its two commands) works; the takings back do not.
    fs.fail_writes_from(2);
    let body = token_update(UDID, 1);
    assert_eq!(gate.webhook(&body).await.expect("taken"), Handled::Recorded);
    let record = &gate.records()[0];
    assert_eq!(record.sent.len(), 2);
    assert_eq!(record.last_error, None);
    let lines = logs.lines();
    assert_eq!(
        lines.iter().filter(|(level, _)| level == "ERROR").count(),
        2
    );
    assert_eq!(
        lines.last().expect("a line"),
        &(
            "ERROR".to_owned(),
            "cannot write a device record".to_owned()
        )
    );
}

/// Catches: a write that fails when the first command is taken back losing the second's
/// taking back: it is made from what is kept, so the first stays on the list and the second
/// goes.
#[tokio::test]
async fn a_command_is_taken_back_from_what_was_kept() {
    let fs = Arc::new(TestFs::default());
    let scratch = support::scratch("takeback-twice");
    let hub = support::FakeNanoHub::start().await;
    let clock = support::Clock::at(1_000);
    let dir = support::dir_of(&scratch);
    let gate =
        support::open_on(fs.clone(), &dir, &hub, &clock, support::counting()).expect("opens");
    hub.answer(Answer::Status(
        500,
        json!({"command_error": "down"}).to_string(),
    ));
    fs.fail_write(2);
    gate.webhook(&token_update(UDID, 1)).await.expect("taken");
    let record = &gate.records()[0];
    let left: Vec<&str> = record
        .sent
        .iter()
        .map(|sent| sent.request_type.as_str())
        .collect();
    assert_eq!(left, ["DeviceInformation"]);
    assert_eq!(
        record.last_error.as_deref(),
        Some("SecurityInfo: NanoHUB did not queue the command: down")
    );
}

/// Catches: a device that is not enrolled being asked queries, an answered query asked again, a
/// malformed answer read as an answer, or an answer's fields lost.
#[test]
fn a_record_asks_what_is_due_and_keeps_what_the_device_answered() {
    let mut record = DeviceRecord::new(support::id(UDID), 5);
    assert!(!record.is_enrolled());
    assert!(
        record.due().is_empty(),
        "a device that is not enrolled is asked nothing"
    );
    assert!(record.take_sent(SOME_UUID).is_none());
    record.token_update(UDID, None, 6);
    assert_eq!(record.due(), ["DeviceInformation", "SecurityInfo"]);
    record.answer_device_information(
        Some(DeviceInformation {
            serial: Some(SERIAL.to_owned()),
            udid: Some(UDID.to_owned()),
            supervised: true,
        }),
        7,
    );
    record.answer_security_info(
        Some(SecurityInfo {
            enrolled_via_dep: Some(true),
            user_approved: None,
        }),
        8,
    );
    assert!(record.due().is_empty());
    assert_eq!(
        (
            record.serial.as_deref(),
            record.supervised,
            record.enrolled_via_dep,
            record.user_approved
        ),
        (Some(SERIAL), Some(true), Some(true), None)
    );
    record.answer_device_information(None, 9);
    record.answer_security_info(None, 9);
    assert_eq!(record.refused, ["DeviceInformation", "SecurityInfo"]);
    assert_eq!(record.device_information_unix, Some(7));
}
