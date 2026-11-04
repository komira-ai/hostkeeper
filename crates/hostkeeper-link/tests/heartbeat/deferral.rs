//! The deferral: its file, its retry time in unix seconds, and the order it holds back.

use std::os::unix::fs::PermissionsExt;

use hostkeeper_link::deferral::{DEFERRAL_FILE, FILE_MAX, MAX_RETRY_SECS};
use hostkeeper_link::testing::{FakeSink, START_MONO, START_UNIX, World, verdict};
use hostkeeper_link::{Deferral, DeferralError, Deferrals, Feature, OrderResult, Outcome, Reason};
use serde::Deserialize;

use crate::{Rig, recorded, reply, service, update, version};

/// An executor that defers try 1 of every order for 600 seconds (`drain_bound`), and lets
/// every later try succeed.
fn deferring_once() -> World {
    World::with_orders(FakeSink::new(|order| {
        if order.order_try == 1 {
            let mut result = verdict(
                order,
                Outcome::Deferred {
                    retry_after_secs: 600,
                },
            );
            result.reason = Some(Reason::new("drain_bound").expect("a reason"));
            result
        } else {
            verdict(order, Outcome::Succeeded)
        }
    }))
}

/// Runs try 1 of the update of `x` to 2.0, which defers, and the beat that takes its verdict.
fn defer(rig: &Rig) -> hostkeeper_link::Heartbeater {
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", 1, "x", "2.0")),
        &[Feature::DeferredV1],
    )));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    heartbeater
}

/// The file of `x`, as JSON.
fn file(rig: &Rig) -> serde_json::Value {
    let text = std::fs::read(rig.service_dir("x").join(DEFERRAL_FILE)).expect("the file");
    serde_json::from_slice(&text).expect("JSON")
}

/// Catches a retry time kept on the monotonic clock (meaningless after a reboot) or as a
/// string, and an order run again before its retry time or held back after it.
#[test]
fn a_deferred_order_is_not_retried_before_its_retry_time_and_the_time_is_in_unix_seconds_on_disk() {
    let rig = Rig::with_world("deferral-time", deferring_once());
    let mut heartbeater = defer(&rig);
    let text = file(&rig);
    assert_eq!(
        text["retry_after_unix"].as_u64(),
        Some(START_UNIX + 600),
        "{text}"
    );
    assert_ne!(START_UNIX, START_MONO);
    assert_eq!(text["target"], "2.0");
    assert_eq!(text["reason"], "drain_bound");

    // One second before the retry time, a new try of the same update is not run.
    rig.world.clock.advance(599);
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", 2, "x", "2.0")),
        &[Feature::DeferredV1],
    )));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(
        rig.world.log.lines(),
        [format!(
            "order o-1 try 2 of x is not run: deferred until unix second {}",
            START_UNIX + 600
        )]
    );
    heartbeater.beat_once();
    let report = rig.world.link.beats()[3].services[0]
        .deferred
        .clone()
        .expect("the deferral");
    assert_eq!(report.retry_after_secs, 1);

    // At the retry time it runs, and its success clears the file.
    rig.world.clock.advance(1);
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", 3, "x", "2.0")),
        &[Feature::DeferredV1],
    )));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    assert!(!rig.service_dir("x").join(DEFERRAL_FILE).exists());
    assert_eq!(rig.world.orders.orders().len(), 2);
}

/// Catches a retry time kept only in memory: a new loop over the same state directory still
/// holds the target back; another target is not held back.
#[test]
fn retry_time_survives_a_restart() {
    let rig = Rig::with_world("deferral-restart", deferring_once());
    drop(defer(&rig));
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", 2, "x", "2.0")),
        &[Feature::DeferredV1],
    )));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    rig.world.link.answer(Ok(reply(
        Some(update("o-2", 2, "x", "2.1")),
        &[Feature::DeferredV1],
    )));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
}

/// Catches a deferral that holds a target back for a coordinator that did not advertise
/// `deferred-v1` (kbf's agent never declines then), or a forced order held back by one.
#[test]
fn a_deferral_holds_back_only_an_update_and_only_with_deferred_v1() {
    let rig = Rig::with_world("deferral-scope", deferring_once());
    let mut heartbeater = defer(&rig);
    rig.world
        .link
        .answer(Ok(reply(Some(update("o-1", 2, "x", "2.0")), &[])));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();

    let rig = Rig::with_world("deferral-forced", deferring_once());
    let mut heartbeater = defer(&rig);
    rig.world.link.answer(Ok(reply(
        Some(crate::forced("o-9", 1, "x", "2.0")),
        &[Feature::DeferredV1, Feature::ForcedV1],
    )));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
}

/// kbf's agent's deferral, as it reads and writes it: `defer-<unit>` in its state directory.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KbfDeferral {
    target: String,
    reason: String,
    retry_after_unix: u64,
}

/// Catches a file that kbf's agent, the previous schema, cannot read (a renamed or retyped key,
/// a key it does not know), and a file kbf's agent wrote, with or without a key added later,
/// that this loop cannot read.
#[test]
fn files_written_by_the_loop_are_read_by_the_previous_schema() {
    let rig = Rig::with_world("deferral-schema", deferring_once());
    drop(defer(&rig));
    let text = std::fs::read(rig.service_dir("x").join(DEFERRAL_FILE)).expect("the file");
    let old: KbfDeferral = serde_json::from_slice(&text).expect("kbf reads it");
    assert_eq!(old.target, "2.0");
    assert_eq!(old.reason, "drain_bound");
    assert_eq!(old.retry_after_unix, START_UNIX + 600);

    let deferrals = Deferrals::new(rig.state());
    let expected = Deferral {
        target: version("1.7"),
        reason: Reason::new("drain_bound").expect("a reason"),
        retry_after_unix: START_UNIX + 30,
    };
    for written in [
        format!(
            r#"{{"target":"1.7","reason":"drain_bound","retry_after_unix":{}}}"#,
            START_UNIX + 30
        ),
        format!(
            r#"{{"target":"1.7","reason":"drain_bound","retry_after_unix":{},"later":1}}"#,
            START_UNIX + 30
        ),
    ] {
        std::fs::write(rig.service_dir("x").join(DEFERRAL_FILE), written).expect("write");
        assert_eq!(
            deferrals.load(&service("x"), START_UNIX).expect("readable"),
            Some(expected.clone())
        );
    }
}

/// Catches a deferral that holds a host longer than the cap, from its verdict or from a file an
/// older agent wrote with a longer one, and a report or a decline past the retry time.
#[test]
fn a_deferral_lasts_at_most_the_cap_and_ends_at_its_retry_time() {
    let target = version("2.0");
    let reason = Reason::new("drain_bound").expect("a reason");
    let long = Deferral::starting(target.clone(), reason, 1000, 10 * MAX_RETRY_SECS);
    assert_eq!(long.retry_after_unix, 1000 + MAX_RETRY_SECS);
    let read = Deferral {
        retry_after_unix: u64::MAX,
        ..long.clone()
    }
    .read_at(1000);
    assert_eq!(read, long);
    assert!(long.declines(&target, 1000 + MAX_RETRY_SECS - 1));
    assert!(!long.declines(&target, 1000 + MAX_RETRY_SECS));
    assert!(!long.declines(&version("2.1"), 1000));
    assert_eq!(long.report(1000 + MAX_RETRY_SECS), None);
    assert_eq!(
        long.report(1000).map(|report| report.retry_after_secs),
        Some(MAX_RETRY_SECS)
    );
}

/// Catches a deferral file that cannot be used (a link, a directory, too long, not JSON, a word
/// outside its grammar) read as a deferral or as no error, and a message that does not name the
/// file.
#[test]
fn a_deferral_file_that_cannot_be_used_is_an_error_naming_it() {
    let rig = Rig::new("deferral-errors");
    rig.install(vec![recorded("x", "1.0")]);
    let deferrals = Deferrals::new(rig.state());
    let x = service("x");
    let path = deferrals.path(&x);
    assert_eq!(deferrals.load(&x, 0).expect("no file"), None);

    std::fs::write(
        &path,
        vec![b' '; usize::try_from(FILE_MAX).expect("small") + 1],
    )
    .expect("write");
    let error = deferrals.load(&x, 0).expect_err("too long");
    assert!(matches!(error, DeferralError::TooLong { .. }), "{error:?}");
    assert_eq!(
        error.to_string(),
        format!("{} is longer than 4096 bytes", path.display())
    );

    std::fs::write(&path, "{").expect("write");
    let error = deferrals.load(&x, 0).expect_err("not JSON");
    assert!(matches!(error, DeferralError::Json { .. }), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with(&format!("{} is not a deferral: ", path.display())),
        "{error}"
    );

    std::fs::write(
        &path,
        r#"{"target":".bad","reason":"x","retry_after_unix":1}"#,
    )
    .expect("write");
    let error = deferrals.load(&x, 0).expect_err("bad target");
    assert_eq!(
        error.to_string(),
        format!(
            "{} holds a target or a reason outside its grammar",
            path.display()
        )
    );
    std::fs::write(
        &path,
        r#"{"target":"1.0","reason":"Bad","retry_after_unix":1}"#,
    )
    .expect("write");
    assert!(matches!(
        deferrals.load(&x, 0),
        Err(DeferralError::Grammar { .. })
    ));

    std::fs::remove_file(&path).expect("remove");
    std::os::unix::fs::symlink(rig.state().join("elsewhere"), &path).expect("a link");
    let error = deferrals.load(&x, 0).expect_err("a link");
    assert!(matches!(error, DeferralError::Io { .. }), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with(&format!("cannot read {}: ", path.display())),
        "{error}"
    );
    deferrals.clear(&x).expect("the link goes");
    assert!(std::fs::symlink_metadata(&path).is_err());

    std::fs::create_dir(&path).expect("a directory");
    assert!(matches!(
        deferrals.load(&x, 0),
        Err(DeferralError::Io { .. })
    ));
}

/// Catches a loop that trusts an unusable deferral file, stops on it, or keeps it: it reports no
/// deferral, runs the order, says what it did, and removes the file; a file it cannot remove is
/// said too.
#[test]
fn an_unusable_deferral_file_is_logged_removed_and_ignored() {
    let rig = Rig::new("deferral-unusable");
    rig.install(vec![recorded("x", "1.0")]);
    let path = rig.service_dir("x").join(DEFERRAL_FILE);
    std::fs::write(
        &path,
        r#"{"target":".bad","reason":"x","retry_after_unix":1}"#,
    )
    .expect("write");
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::DeferredV1])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.link.beats()[1].services[0].deferred, None);
    assert!(!path.exists());
    assert_eq!(
        rig.world.log.lines(),
        [format!(
            "error: the deferral of x is not used and is removed: {} holds a target or a \
             reason outside its grammar",
            path.display()
        )]
    );

    std::fs::create_dir(&path).expect("a directory");
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", 1, "x", "2.0")),
        &[Feature::DeferredV1],
    )));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    let lines = rig.world.log.lines();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[1].starts_with(&format!(
            "error: the deferral of x is not used and is removed: cannot read {}: ",
            path.display()
        )),
        "{lines:?}"
    );
    assert!(
        lines[2].starts_with("error: the deferral of x is not removed: "),
        "{lines:?}"
    );
}

/// Catches a deferral that is not written silently, or a write that stops the loop: the
/// verdict is still sent, and the line says why.
#[test]
fn a_deferral_that_cannot_be_written_is_logged_and_the_verdict_still_sent() {
    let rig = Rig::with_world("deferral-unwritable", deferring_once());
    rig.world.records.set(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", 1, "x", "2.0")),
        &[Feature::DeferredV1],
    )));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    assert_eq!(
        rig.world.log.lines(),
        ["error: the deferral of x is not written: No such file or directory (os error 2)"]
    );
    let shown: Option<OrderResult> = rig.world.link.beats()[1].services[0].last_update.clone();
    assert_eq!(
        shown.map(|result| result.outcome),
        Some(Outcome::Deferred {
            retry_after_secs: 600
        })
    );
}

/// Catches a temporary file left by a crash, or a link put in its place, written through or
/// stopping the write: it is removed, and the file is written new with mode 0600.
#[test]
fn a_deferral_is_written_new_beside_and_owner_only() {
    let rig = Rig::new("deferral-write");
    rig.install(vec![recorded("x", "1.0")]);
    let deferrals = Deferrals::new(rig.state());
    let x = service("x");
    let temp = rig.service_dir("x").join(".deferral.json.tmp");
    let elsewhere = rig.state().join("elsewhere");
    std::fs::write(&elsewhere, "kept").expect("write");
    std::os::unix::fs::symlink(&elsewhere, &temp).expect("a link");
    let deferral = Deferral::starting(
        version("2.0"),
        Reason::new("drain_bound").expect("a reason"),
        START_UNIX,
        60,
    );
    deferrals.save(&x, &deferral).expect("written");
    assert_eq!(std::fs::read_to_string(&elsewhere).expect("read"), "kept");
    assert!(!temp.exists());
    let mode = std::fs::metadata(deferrals.path(&x))
        .expect("the file")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        deferrals.load(&x, START_UNIX).expect("readable"),
        Some(deferral.clone())
    );

    // A temporary path that cannot be removed stops the write, and the file is left as it was.
    std::fs::create_dir(&temp).expect("a directory");
    assert!(deferrals.save(&x, &deferral).is_err());
    assert_eq!(
        deferrals.load(&x, START_UNIX).expect("readable"),
        Some(deferral)
    );
}

/// Catches a deferral whose verdict gives no reason being dropped, or written with a reason
/// outside the grammar kbf's agent reads: it is kept under the reason `deferred`.
#[test]
fn a_deferral_with_no_reason_is_kept_under_the_reason_deferred() {
    let world = World::with_orders(FakeSink::new(|order| {
        verdict(
            order,
            Outcome::Deferred {
                retry_after_secs: 60,
            },
        )
    }));
    let rig = Rig::with_world("deferral-no-reason", world);
    let mut heartbeater = defer(&rig);
    assert_eq!(file(&rig)["reason"], "deferred");
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::DeferredV1])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(
        rig.world.link.beats()[3].services[0]
            .deferred
            .clone()
            .map(|report| report.reason),
        Some(Reason::new("deferred").expect("a reason"))
    );
}
