//! A hook that defers `prepare` with the reason `leases_live` (a Mac holding live leases): the
//! runner ends the order as deferred, with the hook's retry time and reason, and never cuts over.

mod support;

use std::sync::{Arc, Mutex};

use hostkeeper_hook::{Step, Verdict};
use hostkeeper_steps::record::State;
use hostkeeper_steps::{Finished, Loaded, StepRunner, Switch, Toward, UpdateOrder};
use hostkeeper_testkit::HookMode;
use support::{Reply, Rig};

const HUGE_LEASES_LIVE: Reply = Reply::Exit(
    75,
    Some(r#"{"class":"environment","reason":"leases_live","retry_after_secs":1000000000}"#),
);

type Switched = Arc<Mutex<Vec<Toward>>>;

/// A runner of `rig` whose switch writes each direction it is called with to the returned log.
fn switching(rig: &Rig) -> (StepRunner, Switched) {
    let log = Switched::default();
    let seen = Arc::clone(&log);
    let switch: Switch = Arc::new(move |toward: Toward, _order: &UpdateOrder| {
        seen.lock().expect("the log").push(toward);
        Ok(())
    });
    let runner =
        StepRunner::new(rig.fake.clone(), rig.clock.clone(), rig.store()).with_switch(switch);
    (runner, log)
}

/// What the order ended as, from the record on disk.
fn recorded(rig: &Rig) -> Finished {
    let Loaded::Current(rec) = rig.store().load().expect("the record loads") else {
        panic!("a record")
    };
    let State::Finished { verdict, reason } = rec.state else {
        panic!("a finished record")
    };
    Finished { verdict, reason }
}

/// Catches a deferral for live leases that cuts over anyway, loses the hook's retry time or its
/// reason, or leaves the service switched toward the target: the order ends deferred with the
/// retry of 60 seconds and the reason `leases_live`, only `prepare` and the `rollback` that
/// follows a deferred `prepare` run, the switch is made back and never toward the target, and the
/// record on disk holds the same verdict.
#[test]
fn a_hook_deferral_with_leases_live_is_a_deferred_order_with_the_hooks_retry_time() {
    let rig = Rig::new("leases-live-defers");
    let mode = HookMode::DeferLeasesLive;
    rig.fake.say(
        Step::Prepare,
        &[Reply::Exit(mode.exit_code(), Some(mode.last_line()))],
    );
    let (runner, switched) = switching(&rig);
    let got = runner.run(&rig.order()).expect("the order runs");
    let want = Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(60),
        },
        reason: Some("leases_live".to_owned()),
    };
    assert_eq!(got, want);
    assert_eq!(rig.fake.steps(), [Step::Prepare, Step::Rollback]);
    assert_eq!(*switched.lock().expect("the log"), [Toward::Back]);
    assert_eq!(recorded(&rig), want);
}

/// Catches a retry time that the runner rewrites in the verdict: a `retry_after_secs` above the
/// longest wait (`MAX_RETRY_SECS`) is clamped only where the runner itself waits, so the
/// deferred verdict keeps what the hook said, the runner sleeps nothing, and the record holds
/// the same value.
#[test]
fn a_leases_live_retry_is_recorded_as_the_hook_gave_it() {
    let rig = Rig::new("leases-live-retry");
    let asked = 1_000_000_000;
    assert!(asked > hostkeeper_steps::runner::MAX_RETRY_SECS);
    rig.fake.say(Step::Prepare, &[HUGE_LEASES_LIVE]);
    let got = rig.runner().run(&rig.order()).expect("the order runs");
    let want = Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(asked),
        },
        reason: Some("leases_live".to_owned()),
    };
    assert_eq!(got, want);
    assert!(rig.clock.slept().is_empty());
    assert_eq!(recorded(&rig), want);
}

/// The keys of a JSON object, sorted.
fn keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

/// Catches a field added to or renamed in the step record by the macOS work (the `leases_live`
/// token, the quarantine and signature checks, the start log beside the record): the record a
/// release wrote before (the committed `before-order-try` fixture) is read and written back byte
/// for byte, and the record this release writes for a `leases_live` deferral has the same keys,
/// at the top and in `progress`, as that record (plus `order_try`, which a release since added),
/// the token being a value of the existing `reason` field.
#[test]
fn new_reason_tokens_and_verification_leave_every_written_format_unchanged() {
    let fixture = include_str!("fixtures/before-order-try/finished.json");
    let scratch = support::Scratch::new("formats-unchanged");
    std::fs::write(
        scratch.path().join(hostkeeper_steps::record::RECORD_FILE),
        fixture,
    )
    .expect("the fixture");
    let store = hostkeeper_steps::RecordStore::new(scratch.path());
    let Loaded::Current(rec) = store.load().expect("it loads") else {
        panic!("the fixture is a current record");
    };
    store.save(&rec).expect("written back");
    let written =
        std::fs::read_to_string(scratch.path().join(hostkeeper_steps::record::RECORD_FILE))
            .expect("read back");
    assert_eq!(written, fixture.trim_end_matches('\n'));

    let rig = Rig::new("formats-leases-live");
    let mode = HookMode::DeferLeasesLive;
    rig.fake.say(
        Step::Prepare,
        &[Reply::Exit(mode.exit_code(), Some(mode.last_line()))],
    );
    rig.runner().run(&rig.order()).expect("the order runs");
    let path = rig
        .store()
        .dir()
        .join(hostkeeper_steps::record::RECORD_FILE);
    let now: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("the record")).expect("JSON");
    let before: serde_json::Value = serde_json::from_str(fixture).expect("JSON");
    let mut allowed = keys(&before);
    allowed.push("order_try".to_owned());
    allowed.sort();
    let top = keys(&now);
    assert!(
        top.iter().all(|key| allowed.contains(key)),
        "{top:?} is not within {allowed:?}"
    );
    assert_eq!(keys(&now["progress"]), keys(&before["progress"]));
    assert_eq!(keys(&now["state"]), keys(&before["state"]));
    assert_eq!(now["state"]["reason"], serde_json::json!("leases_live"));
}
