use super::*;
use crate::testkit::{FakeApns, ManualClock};

const MAC: &str = "mac-1";
const TOPIC: &str = "com.apple.mgmt.External.x";
const START: u64 = 1_800_000_000;

fn settings() -> PushSettings {
    PushSettings {
        retry_s: 3600,
        expiry_s: 86_400,
        priority: 10,
    }
}

struct Rig {
    pusher: Pusher,
    apns: Arc<FakeApns>,
    clock: Arc<ManualClock>,
    token: DeviceToken,
}

fn rig() -> Rig {
    let apns = Arc::new(FakeApns::new());
    let clock = Arc::new(ManualClock::at(START));
    Rig {
        pusher: Pusher::new(apns.clone(), clock.clone(), TOPIC),
        apns,
        clock,
        token: DeviceToken::from_raw(&[3; 32]).unwrap(),
    }
}

impl Rig {
    fn target(&self) -> Target<'_> {
        Target {
            token: &self.token,
            magic: "magic-1",
        }
    }
}

#[tokio::test]
async fn a_push_carries_the_topic_the_token_the_magic_the_priority_and_an_absolute_expiration() {
    let rig = rig();
    let outcome = rig
        .pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    assert!(matches!(outcome, Outcome::Sent(_)));
    let sent = rig.apns.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].topic, TOPIC);
    assert_eq!(sent[0].token, rig.token);
    assert_eq!(sent[0].magic, "magic-1");
    assert_eq!(sent[0].priority, 10);
    // apns-expiration is the time APNs keeps the push until, not a number of seconds.
    assert_eq!(sent[0].expiration_unix, START + 86_400);
}

#[tokio::test]
async fn a_priority_of_five_and_an_expiry_of_zero_are_sent_as_set() {
    let rig = rig();
    let set = PushSettings {
        retry_s: 60,
        expiry_s: 0,
        priority: 5,
    };
    rig.pusher.command_queued(MAC, rig.target(), set).await;
    let sent = rig.apns.sent();
    assert_eq!(sent[0].priority, 5);
    assert_eq!(sent[0].expiration_unix, 0);
}

#[tokio::test]
async fn a_command_after_an_answered_push_is_pushed_at_once_and_one_before_it_is_held() {
    let rig = rig();
    rig.pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    rig.clock.advance(5);
    let held = rig
        .pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    assert_eq!(held, Outcome::Held);
    assert_eq!(rig.apns.sent().len(), 1);
    // The Mac connected: the push was answered. A command now is pushed, inside the floor.
    rig.pusher.mac_connected(MAC);
    rig.clock.advance(5);
    let again = rig
        .pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    assert!(matches!(again, Outcome::Sent(_)));
    assert_eq!(rig.apns.sent().len(), 2);
    assert_eq!(rig.apns.sent()[1].expiration_unix, START + 10 + 86_400);
}

#[tokio::test]
async fn not_now_arms_the_floor_and_the_command_is_pushed_once_the_window_passed_and_not_before() {
    let rig = rig();
    rig.pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    rig.pusher.mac_connected(MAC);
    rig.clock.advance(100);
    rig.pusher.command_deferred(MAC, settings());
    // The floor armed by the push is the earlier one, at START + 3600.
    assert_eq!(rig.pusher.next_floor(), Some(START + 3600));
    rig.clock.set(START + 3599);
    assert_eq!(
        rig.pusher
            .floor_check(MAC, rig.target(), true, settings())
            .await,
        Outcome::Held
    );
    rig.clock.set(START + 3600);
    assert!(matches!(
        rig.pusher
            .floor_check(MAC, rig.target(), true, settings())
            .await,
        Outcome::Sent(_)
    ));
    assert_eq!(rig.apns.sent().len(), 2);
}

#[tokio::test]
async fn a_timed_out_command_is_pushed_again() {
    let rig = rig();
    rig.pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    rig.clock.advance(86_400);
    let outcome = rig
        .pusher
        .command_timed_out(MAC, rig.target(), settings())
        .await;
    assert!(matches!(outcome, Outcome::Sent(_)));
    assert_eq!(rig.apns.sent().len(), 2);
}

#[tokio::test]
async fn the_push_floor_is_read_from_the_injected_clock_and_the_settings_not_from_real_time() {
    let rig = rig();
    let short = PushSettings {
        retry_s: 10,
        ..settings()
    };
    rig.pusher.command_queued(MAC, rig.target(), short).await;
    assert_eq!(rig.pusher.next_floor(), Some(START + 10));
    rig.clock.advance(10);
    assert!(matches!(
        rig.pusher.floor_check(MAC, rig.target(), true, short).await,
        Outcome::Sent(_)
    ));
    // Nothing queued when the floor elapses: no push, and the floor is gone.
    rig.clock.advance(10);
    assert_eq!(
        rig.pusher
            .floor_check(MAC, rig.target(), false, short)
            .await,
        Outcome::Held
    );
    assert_eq!(rig.pusher.next_floor(), None);
}

#[tokio::test]
async fn a_failed_push_is_reported_with_its_error_and_the_floor_stays_for_the_next_try() {
    let rig = rig();
    rig.apns.answer_next(Err(PushError::Unavailable(503)));
    let outcome = rig
        .pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    assert_eq!(outcome, Outcome::Failed(PushError::Unavailable(503)));
    assert_eq!(rig.pusher.next_floor(), Some(START + 3600));
    // Nothing is outstanding after a failure: the next command is pushed.
    let next = rig
        .pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    assert!(matches!(next, Outcome::Sent(_)));
}

#[tokio::test]
async fn a_forgotten_enrollment_is_pushed_as_a_new_one() {
    let rig = rig();
    rig.pusher
        .command_queued(MAC, rig.target(), settings())
        .await;
    rig.pusher.forget(MAC);
    assert_eq!(rig.pusher.next_floor(), None);
    assert!(matches!(
        rig.pusher
            .command_queued(MAC, rig.target(), settings())
            .await,
        Outcome::Sent(_)
    ));
}

/// Proves: a test push is sent at once whatever the policy holds, with the topic, the token, the
/// magic and the priority of a push and an expiration of 0, and it neither arms a floor nor
/// leaves a push outstanding, so the next queued command is still pushed at once.
#[tokio::test]
async fn a_test_push_is_sent_outside_the_policy_and_leaves_it_untouched() {
    let rig = rig();
    // A push is outstanding: the policy holds the next one.
    assert!(matches!(
        rig.pusher
            .command_queued(MAC, rig.target(), settings())
            .await,
        Outcome::Sent(_)
    ));
    assert!(matches!(
        rig.pusher
            .command_queued(MAC, rig.target(), settings())
            .await,
        Outcome::Held
    ));
    let floor = rig.pusher.next_floor();
    let delivered = rig
        .pusher
        .test_push(rig.target(), settings())
        .await
        .expect("sent");
    assert_eq!(delivered.apns_id.as_deref(), Some("fake-apns-id"));
    let sent = rig.apns.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].topic, TOPIC);
    assert_eq!(sent[1].token, rig.token);
    assert_eq!(sent[1].magic, "magic-1");
    assert_eq!(sent[1].priority, 10);
    assert_eq!(sent[1].expiration_unix, 0);
    // The policy is as it was: still one outstanding, and the test push armed no floor.
    assert!(matches!(
        rig.pusher
            .command_queued(MAC, rig.target(), settings())
            .await,
        Outcome::Held
    ));
    assert_eq!(rig.pusher.next_floor(), floor);
}

/// Proves: what APNs refuses a test push with is returned as it is, and the policy is not told
/// the push failed (a failed test push must not arm the retry floor of a Mac).
#[tokio::test]
async fn a_test_push_that_fails_returns_the_error_and_arms_nothing() {
    let rig = rig();
    rig.apns.answer_next(Err(PushError::TopicDisallowed));
    let error = rig
        .pusher
        .test_push(rig.target(), settings())
        .await
        .expect_err("refused");
    assert_eq!(error, PushError::TopicDisallowed);
    assert_eq!(rig.pusher.next_floor(), None);
    // A queued command is pushed at once afterwards.
    assert!(matches!(
        rig.pusher
            .command_queued(MAC, rig.target(), settings())
            .await,
        Outcome::Sent(_)
    ));
}
