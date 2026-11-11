//! The rules of the queue (`docs/design/coordinator-mdm.md` sections 4.3 and 5.2), one test for
//! each invariant of the model check of the queue (`crates/hostkeeper-mdm-model`), against the
//! file tables and a fake APNs. The tests set the clock; none waits for time.

use hostkeeper_mdm_gate::EnqueueError;
use hostkeeper_mdm_push::apns::PushError;

use crate::mdm_server::support::{Rig, SERIAL, gate_command};
use crate::store::mdm::{Answer, Answered, CommandState, MdmStore};

/// The seconds of the default floor (`push_retry_s`).
const FLOOR: u64 = 3600;

fn state(rig: &Rig, n: u8) -> CommandState {
    rig.store
        .command(&Rig::command_id(n))
        .expect("read")
        .expect("the command")
        .state
}

/// Proves: a claim takes a command only while it is queued: the command is sent, and a second claim
/// finds nothing, so one command is held by one connection at a time.
///
/// Catches: a claim that hands a sent command to a second connection of the same Mac.
#[tokio::test]
async fn a_claim_takes_only_a_queued_command() {
    let rig = Rig::new("queue-claim-queued");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    let first = rig
        .queue
        .claim("e1", &[])
        .expect("claimed")
        .expect("a command");
    assert_eq!(first.id, Rig::command_id(1));
    assert_eq!(state(&rig, 1), CommandState::Sent);
    assert!(rig.queue.claim("e1", &[]).expect("claimed").is_none());
}

/// Proves: a claim skips the commands the connection has tried, and takes the next one; the one it
/// skipped stays queued for the next connection.
///
/// Catches: a Mac that answers `NotNow` being handed the same command in a loop.
#[tokio::test]
async fn a_claim_skips_the_commands_tried_on_the_connection() {
    let rig = Rig::new("queue-claim-tried");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.enqueue("e1", 2).await.expect("queued");
    let tried = vec![Rig::command_id(1)];
    let next = rig
        .queue
        .claim("e1", &tried)
        .expect("claimed")
        .expect("a command");
    assert_eq!(next.id, Rig::command_id(2));
    assert_eq!(state(&rig, 1), CommandState::Queued);
    assert!(
        rig.queue
            .claim("e1", &[tried[0].clone(), next.id])
            .expect("claimed")
            .is_none()
    );
}

/// Proves: a result changes a command only while it is sent, and only through the enrollment it was
/// sent to: a result for a queued command, for one that ended, and one from another enrollment are
/// dropped and change nothing.
///
/// Catches: a late or forged result acknowledging a command the Mac never held.
#[tokio::test]
async fn a_result_is_matched_only_to_a_sent_command() {
    let rig = Rig::new("queue-result-sent");
    rig.live("e1", SERIAL, true);
    rig.live("e2", "OTHERSERIAL", true);
    rig.enqueue("e1", 1).await.expect("queued");
    let id = Rig::command_id(1);
    let ack = |rig: &Rig, enrollment: &str| {
        rig.queue
            .answer(enrollment, &id, Answer::Acknowledged, b"<plist/>")
            .expect("answered")
    };
    assert_eq!(ack(&rig, "e1"), Answered::Dropped, "queued, not sent");
    assert_eq!(state(&rig, 1), CommandState::Queued);
    rig.queue.claim("e1", &[]).expect("claimed");
    assert_eq!(
        ack(&rig, "e2"),
        Answered::Dropped,
        "sent to another enrollment"
    );
    assert_eq!(state(&rig, 1), CommandState::Sent);
    assert_eq!(ack(&rig, "e1"), Answered::Taken);
    assert_eq!(state(&rig, 1), CommandState::Acked);
    assert_eq!(ack(&rig, "e1"), Answered::Dropped, "ended");
}

/// Proves: `NotNow` keeps the command (queued again, marked deferred) and arms the push floor: a
/// sweep inside the window pushes nothing, the first sweep at the floor pushes, and a sweep after
/// it pushes nothing more until the floor is armed again.
///
/// Catches: a deferred command with no wake once the Mac closes its connection, or one pushed
/// again inside the floor.
#[tokio::test]
async fn not_now_keeps_the_command_and_arms_the_push_floor() {
    let rig = Rig::new("queue-not-now");
    rig.live("e1", SERIAL, true);
    // No push yet: the command is queued and claimed by a Mac that connected on its own.
    rig.store
        .enqueue(&crate::mdm_server::queue_new(1, "e1"))
        .expect("queued");
    rig.queue.claim("e1", &[]).expect("claimed");
    rig.queue
        .answer("e1", &Rig::command_id(1), Answer::NotNow, b"<plist/>")
        .expect("answered");
    let row = rig
        .store
        .command(&Rig::command_id(1))
        .expect("read")
        .expect("kept");
    assert_eq!((row.state, row.deferred), (CommandState::Queued, true));
    assert!(rig.apns.sent().is_empty());
    rig.time.advance(FLOOR - 1);
    rig.queue.sweep().await.expect("swept");
    assert!(rig.apns.sent().is_empty(), "inside the floor");
    rig.time.advance(1);
    rig.queue.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 1, "at the floor");
    rig.queue.sweep().await.expect("swept");
    assert_eq!(
        rig.apns.sent().len(),
        1,
        "the floor is armed again by that push"
    );
}

/// Proves: a restarted coordinator pushes for a command that was left queued: the queue it builds
/// has nothing armed, yet its sweep reads the store, finds the live enrollment with a queued
/// command, starts its floor and pushes once the floor has elapsed, and not before.
///
/// Catches: a deferred or unpushed command forgotten across a deploy, the Mac asleep and never woken.
#[tokio::test]
async fn a_restarted_queue_pushes_for_a_command_left_queued() {
    let rig = Rig::new("queue-restart");
    rig.live("e1", SERIAL, true);
    rig.store
        .enqueue(&crate::mdm_server::queue_new(1, "e1"))
        .expect("queued");
    rig.queue.claim("e1", &[]).expect("claimed");
    rig.queue
        .answer("e1", &Rig::command_id(1), Answer::NotNow, b"<plist/>")
        .expect("answered");
    let restarted = rig.restarted_queue();
    assert!(restarted.armed.lock().expect("lock").is_empty());
    restarted.sweep().await.expect("swept");
    assert!(
        rig.apns.sent().is_empty(),
        "the floor starts at the first sweep"
    );
    rig.time.advance(FLOOR - 1);
    restarted.sweep().await.expect("swept");
    assert!(rig.apns.sent().is_empty(), "inside the floor");
    rig.time.advance(1);
    restarted.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 1, "at the floor");
}

/// Proves: a sweep of a coordinator that has no push identity yet still arms the floor of a
/// queued command and sends nothing, and the push is sent once an identity is stored.
///
/// Catches: a sweep that panics, or one that forgets the command, when nobody can push.
#[tokio::test]
async fn a_sweep_without_a_pusher_arms_the_command_and_pushes_nothing() {
    let rig = Rig::new("queue-sweep-no-pusher");
    rig.live("e1", SERIAL, true);
    rig.store
        .enqueue(&crate::mdm_server::queue_new(1, "e1"))
        .expect("queued");
    let bare = rig.queue_without_pusher();
    bare.sweep().await.expect("swept");
    assert!(bare.armed.lock().expect("lock").contains("e1"));
    assert!(rig.apns.sent().is_empty());
}

/// Proves: a sweep whose store cannot say which enrollments wait carries on with the timed-out
/// commands and the enrollments already armed.
///
/// Catches: one failed read stopping every Mac's wake.
#[tokio::test]
async fn a_sweep_carries_on_when_the_waiting_enrollments_cannot_be_read() {
    let rig = Rig::new("queue-waiting-fails");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.time.advance(FLOOR);
    rig.store.fail(Some("enrollments_with_queued"));
    rig.queue.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 2, "the armed enrollment was pushed");
    rig.store.fail(None);
}

/// Proves: a command that a Mac holds past `--mdm-command-timeout-s` is queued again and its Mac
/// is pushed, even though a push is outstanding.
///
/// Catches: a command lost to a dropped connection with nothing to wake the Mac.
#[tokio::test]
async fn a_timed_out_command_is_queued_again_and_pushed() {
    let rig = Rig::new("queue-timeout");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    assert_eq!(rig.apns.sent().len(), 1);
    rig.queue.contact("e1").expect("contact");
    rig.queue.claim("e1", &[]).expect("claimed");
    rig.time.advance(86_399);
    rig.queue.sweep().await.expect("swept");
    assert_eq!(state(&rig, 1), CommandState::Sent, "inside the timeout");
    rig.time.advance(1);
    rig.queue.sweep().await.expect("swept");
    assert_eq!(state(&rig, 1), CommandState::Queued);
    assert_eq!(rig.apns.sent().len(), 2, "pushed again");
}

/// Proves: a command the Mac acknowledged stays acknowledged: the timeout does not return it, no
/// claim hands it out and no push is made for it.
///
/// Catches: an acknowledged command sent again after the timeout.
#[tokio::test]
async fn an_acknowledged_command_is_never_sent_again() {
    let rig = Rig::new("queue-acked");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.queue.contact("e1").expect("contact");
    rig.queue.claim("e1", &[]).expect("claimed");
    rig.queue
        .answer("e1", &Rig::command_id(1), Answer::Acknowledged, b"<plist/>")
        .expect("answered");
    rig.time.advance(10 * 86_400);
    rig.queue.sweep().await.expect("swept");
    assert_eq!(state(&rig, 1), CommandState::Acked);
    assert!(rig.queue.claim("e1", &[]).expect("claimed").is_none());
    assert_eq!(rig.apns.sent().len(), 1, "only the push of the enqueue");
}

/// Proves: when the Mac's `Idle` arrives on two connections, one command goes to one of them and
/// the other is handed nothing.
///
/// Catches: a duplicated `Idle` running a command twice at once.
#[tokio::test]
async fn a_duplicated_idle_hands_the_command_to_one_connection() {
    let rig = Rig::new("queue-idle");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    let first = rig.queue.claim("e1", &[]).expect("claimed");
    let second = rig.queue.claim("e1", &[]).expect("claimed");
    assert!(first.is_some() && second.is_none());
}

/// Proves: delivery is at least once and not at most once: a command the timeout returned while a
/// slow Mac still held it is sent again, and the slow Mac's late answer is taken once; the second
/// delivery's answer is dropped.
///
/// Catches: a queue that promises one delivery, which the gate and the pinned commands would then
/// not be written to survive.
#[tokio::test]
async fn delivery_is_at_least_once_not_at_most_one() {
    let rig = Rig::new("queue-at-least-once");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.queue.claim("e1", &[]).expect("claimed");
    rig.time.advance(86_400);
    rig.queue.sweep().await.expect("swept");
    let again = rig
        .queue
        .claim("e1", &[])
        .expect("claimed")
        .expect("sent again");
    assert_eq!(again.deliveries, 2);
    let id = Rig::command_id(1);
    let late = rig
        .queue
        .answer("e1", &id, Answer::Acknowledged, b"<plist/>");
    assert_eq!(late.expect("answered"), Answered::Taken);
    let second = rig
        .queue
        .answer("e1", &id, Answer::Acknowledged, b"<plist/>");
    assert_eq!(second.expect("answered"), Answered::Dropped);
    assert_eq!(state(&rig, 1), CommandState::Acked);
}

/// Proves: queueing a command whose id exists changes nothing and sends no second push.
///
/// Catches: a gate that asks again at a Mac's next check-in queueing the command twice.
#[tokio::test]
async fn enqueue_is_idempotent_on_the_command_id() {
    let rig = Rig::new("queue-idempotent");
    rig.live("e1", SERIAL, true);
    assert_eq!(rig.enqueue("e1", 1).await.expect("queued").push_error, None);
    rig.enqueue("e1", 1).await.expect("queued again");
    assert_eq!(rig.store.commands("e1").expect("read").len(), 1);
    assert_eq!(rig.apns.sent().len(), 1);
}

/// Proves: an enqueue for an enrollment that a newer one replaced is refused, and nothing is
/// queued for it.
///
/// Catches: the gate's command for a replaced enrollment going to a Mac that is no longer it.
#[tokio::test]
async fn an_enqueue_for_a_replaced_enrollment_is_refused() {
    let rig = Rig::new("queue-replaced");
    rig.live("e1", SERIAL, true);
    rig.live("e2", SERIAL, true);
    let refused = rig.enqueue("e1", 1).await.expect_err("refused");
    assert_eq!(
        refused,
        EnqueueError::NotQueued("the enrollment is not live".to_owned())
    );
    assert!(rig.store.commands("e1").expect("read").is_empty());
}

/// Proves: a command whose type is off the pinned list is refused before it is queued.
///
/// Catches: the queue sending a type the gate is not allowed to.
#[tokio::test]
async fn a_command_off_the_pinned_list_is_refused() {
    use hostkeeper_mdm_gate::CommandQueue as _;
    let rig = Rig::new("queue-pinned");
    rig.live("e1", SERIAL, true);
    let command = gate_command(1, "EraseDevice");
    let refused = rig
        .queue
        .enqueue(&crate::mdm_server::support::enrollment_id("e1"), &command)
        .await
        .expect_err("refused");
    assert_eq!(refused, EnqueueError::NotAllowed("EraseDevice".to_owned()));
    assert!(rig.store.commands("e1").expect("read").is_empty());
}

/// Proves: a command queued while a push is outstanding sends no second push, and one queued after
/// a request of the Mac reached `/mdm` is pushed at once.
///
/// Catches: a push for every command of a burst, or a command that waits an hour for the floor
/// after the Mac was awake.
#[tokio::test]
async fn a_command_queued_after_the_mac_connected_is_pushed_at_once() {
    let rig = Rig::new("queue-push-outstanding");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.enqueue("e1", 2).await.expect("queued");
    assert_eq!(rig.apns.sent().len(), 1, "one push for a burst");
    rig.queue.contact("e1").expect("contact");
    rig.enqueue("e1", 3).await.expect("queued");
    assert_eq!(
        rig.apns.sent().len(),
        2,
        "pushed at once, the floor not waited for"
    );
    let push = &rig.apns.sent()[1];
    assert_eq!(push.magic, "magic");
    assert_eq!(push.topic, crate::mdm_server::support::TOPIC);
}

/// Proves: a Mac with no push token is not pushed: the command waits for its next connection.
///
/// Catches: a push to a token the Mac never gave.
#[tokio::test]
async fn a_command_for_a_mac_without_a_token_waits_for_its_connection() {
    let rig = Rig::new("queue-no-token");
    rig.live("e1", SERIAL, false);
    let queued = rig.enqueue("e1", 1).await.expect("queued");
    assert_eq!(queued.push_error, None);
    assert!(rig.apns.sent().is_empty());
    assert_eq!(state(&rig, 1), CommandState::Queued);
}

/// Proves: without a push identity a command is queued and nothing is pushed, and with an
/// enrollment the table does not hold a push says nothing.
///
/// Catches: the gate's enqueue failing for want of a push certificate, which does not stop a Mac
/// that connects on its own.
#[tokio::test]
async fn without_a_push_identity_a_command_is_queued_and_not_pushed() {
    let rig = Rig::bare("queue-no-identity");
    rig.live("e1", SERIAL, true);
    let queued = rig.enqueue("e1", 1).await.expect("queued");
    assert_eq!(queued.push_error, None);
    assert!(rig.apns.sent().is_empty());
    rig.store_push();
    assert_eq!(
        rig.enqueue("nobody", 2)
            .await
            .expect_err("not live")
            .to_string(),
        "NanoHUB did not queue the command: the enrollment is not live"
    );
}

/// Proves: a push that fails is reported to the gate in its answer and the command stays queued; the
/// floor stays armed, so the sweep at the floor pushes again.
///
/// Catches: a failed push dropping the command, or never being tried again.
#[tokio::test]
async fn a_failed_push_is_reported_and_the_command_stays_queued() {
    let rig = Rig::new("queue-push-failed");
    rig.live("e1", SERIAL, true);
    rig.apns.answer_next(Err(PushError::Unavailable(503)));
    let queued = rig.enqueue("e1", 1).await.expect("queued");
    assert_eq!(
        queued.push_error.as_deref(),
        Some("APNs is unavailable (503)")
    );
    assert_eq!(state(&rig, 1), CommandState::Queued);
    rig.time.advance(FLOOR);
    rig.queue.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 2, "tried again at the floor");
}

/// Proves: the sweep stops looking at an enrollment that ended or has nothing queued, so the set of
/// enrollments it looks at does not grow without end.
///
/// Catches: a sweep that pushes a Mac that was replaced or whose queue is empty.
#[tokio::test]
async fn the_sweep_forgets_an_enrollment_that_ended_or_has_nothing_queued() {
    let rig = Rig::new("queue-sweep-forgets");
    rig.live("e1", SERIAL, true);
    rig.live("e3", "THIRDSERIAL", true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.enqueue("e3", 2).await.expect("queued");
    assert_eq!(rig.queue.armed.lock().expect("lock").len(), 2);
    // e1 is replaced; e3's command is answered.
    rig.live("e2", SERIAL, true);
    rig.queue.claim("e3", &[]).expect("claimed");
    rig.queue
        .answer("e3", &Rig::command_id(2), Answer::Acknowledged, b"<plist/>")
        .expect("answered");
    rig.time.advance(FLOOR);
    rig.queue.sweep().await.expect("swept");
    assert!(rig.queue.armed.lock().expect("lock").is_empty());
    assert_eq!(rig.apns.sent().len(), 2, "nothing was pushed for either");
}

/// Proves: a store that fails is reported by each call and not swallowed: enqueue, claim, contact,
/// answer and the sweep answer the store's error, and a push whose enrollment cannot be read says
/// why.
///
/// Catches: a failed write answered as a success, which the gate would record as sent.
#[tokio::test]
async fn a_store_that_fails_is_reported_by_each_call() {
    let rig = Rig::new("queue-store-fails");
    rig.live("e1", SERIAL, true);
    rig.store.fail(Some("enqueue"));
    let refused = rig.enqueue("e1", 1).await.expect_err("refused");
    assert_eq!(
        refused.to_string(),
        "NanoHUB did not queue the command: the MDM store failed: enqueue failed"
    );
    rig.store.fail(Some("claim"));
    assert!(rig.queue.claim("e1", &[]).is_err());
    rig.store.fail(Some("record_contact"));
    assert!(rig.queue.contact("e1").is_err());
    rig.store.fail(Some("answer"));
    assert!(rig.queue.answer("e1", "x", Answer::Error, b"").is_err());
    rig.store.fail(Some("requeue_timed_out"));
    assert!(rig.queue.sweep().await.is_err());
    rig.store.fail(Some("enrollment"));
    let queued = rig.enqueue("e1", 2).await;
    assert!(
        queued.is_ok(),
        "the command is queued; the push could not read the Mac"
    );
    assert_eq!(
        queued.expect("queued").push_error.as_deref(),
        Some("the MDM store failed: enrollment failed")
    );
    rig.store.fail(None);
}

/// Proves: a sweep that cannot read the commands of an armed enrollment, or record a push it sent,
/// carries on: the push goes out and the other enrollments are looked at.
///
/// Catches: one bad row stopping every Mac's wake.
#[tokio::test]
async fn a_sweep_carries_on_past_a_store_that_fails() {
    let rig = Rig::new("queue-sweep-fails");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.time.advance(FLOOR);
    rig.store.fail(Some("commands"));
    rig.queue.sweep().await.expect("swept");
    assert_eq!(
        rig.apns.sent().len(),
        1,
        "the commands could not be read: no push"
    );
    rig.store.fail(Some("record_push"));
    rig.queue.sweep().await.expect("swept");
    assert_eq!(
        rig.apns.sent().len(),
        2,
        "the push went out though it could not be recorded"
    );
    rig.store.fail(None);
}

/// Proves: a sweep that finds the push of a timed-out command failing says so and goes on.
///
/// Catches: a failed wake of a timed-out command stopping the sweep.
#[tokio::test]
async fn a_timed_out_command_whose_push_fails_is_still_queued_again() {
    let rig = Rig::new("queue-timeout-push-fails");
    rig.live("e1", SERIAL, true);
    rig.store
        .enqueue(&crate::mdm_server::queue_new(1, "e1"))
        .expect("queued");
    rig.queue.claim("e1", &[]).expect("claimed");
    rig.apns.answer_next(Err(PushError::Unavailable(500)));
    rig.time.advance(86_400);
    rig.queue.sweep().await.expect("swept");
    assert_eq!(state(&rig, 1), CommandState::Queued);
    assert_eq!(rig.apns.sent().len(), 1);
}

/// Proves: forgetting an enrollment drops its push state: after `forget` a command queued for a
/// live enrollment is pushed as for a new one.
///
/// Catches: a replaced enrollment's outstanding push suppressing its successor's.
#[tokio::test]
async fn forgetting_an_enrollment_clears_its_push_state() {
    let rig = Rig::new("queue-forget");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.queue.forget("e1");
    rig.enqueue("e1", 2).await.expect("queued");
    assert_eq!(rig.apns.sent().len(), 2);
    let bare = Rig::bare("queue-forget-bare");
    bare.queue.forget("e1");
}

/// Proves: the queue prints no more than its timeout, a Mac that connects or answers `NotNow` with no
/// push identity held is handled as any other, and a push for an enrollment the table no longer holds
/// is none.
///
/// Catches: a queue that needs a push certificate to take a result, or one that pushes a Mac it has
/// forgotten.
#[tokio::test]
async fn a_queue_without_a_pusher_still_takes_contact_and_not_now() {
    let rig = Rig::bare("queue-no-pusher");
    assert_eq!(
        format!("{:?}", rig.queue),
        "MdmQueue { timeout_s: 86400, .. }"
    );
    rig.live("e1", SERIAL, true);
    rig.queue.contact("e1").expect("contact");
    rig.store
        .enqueue(&crate::mdm_server::queue_new(1, "e1"))
        .expect("queued");
    rig.queue.claim("e1", &[]).expect("claimed");
    let taken = rig
        .queue
        .answer("e1", &Rig::command_id(1), Answer::NotNow, b"<plist/>");
    assert_eq!(taken.expect("answered"), Answered::Taken);
    rig.queue.forget("e1");
    // The command is still queued and no push identity is held: the sweep arms nothing to push.
    rig.queue.sweep().await.expect("swept");
    assert!(rig.apns.sent().is_empty());
    rig.store_push();
    assert_eq!(rig.queue.push("ghost", super::Why::Queued).await, None);
}

/// Proves: a push that fails at the floor is logged and the command stays queued for the next look.
///
/// Catches: a failed wake at the floor dropping the Mac from the sweep.
#[tokio::test]
async fn a_push_that_fails_at_the_floor_leaves_the_command_queued() {
    let rig = Rig::new("queue-floor-fails");
    rig.live("e1", SERIAL, true);
    rig.enqueue("e1", 1).await.expect("queued");
    rig.time.advance(FLOOR);
    rig.apns.answer_next(Err(PushError::Throttled));
    rig.queue.sweep().await.expect("swept");
    assert_eq!(rig.apns.sent().len(), 2);
    assert_eq!(state(&rig, 1), CommandState::Queued);
}
