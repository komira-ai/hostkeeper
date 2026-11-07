//! The deferral: its file, its retry time in unix seconds, and the order it holds back.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hostkeeper_link::deferral::{DEFERRAL_FILE, FILE_MAX, MAX_RETRY_SECS, TEMP_FILE};
use hostkeeper_link::testing::{FakeSink, START_MONO, START_UNIX, World};
use hostkeeper_link::{
    Deferral, DeferralError, Deferrals, Feature, Heartbeater, Order, OrderResult, Outcome, Reason,
    RecordedService,
};

use crate::verdict;
use crate::{Rig, forced, kbf, recorded, reply, service, update, version};

fn drain_bound() -> Reason {
    Reason::new("drain_bound").expect("a reason")
}

/// The verdict of `order`: deferred for 600 seconds (`drain_bound`).
fn deferred(order: &Order) -> OrderResult {
    let mut result = verdict(
        order,
        Outcome::Deferred {
            retry_after_secs: 600,
        },
    );
    result.reason = Some(drain_bound());
    result
}

/// An executor that defers try 1 of every update, and lets every later try succeed.
fn deferring_once() -> World {
    World::with_orders(FakeSink::new(|order, _| {
        if order.order_try == 1 {
            deferred(order)
        } else {
            verdict(order, Outcome::Succeeded)
        }
    }))
}

/// An update of `x` to `to`, try `order_try` of order `o-1`, from a coordinator that
/// advertises `deferred-v1`.
fn ask(rig: &Rig, order_try: u64, to: &str) {
    rig.world.link.answer(Ok(reply(
        Some(update("o-1", order_try, "x", to)),
        &[Feature::DeferredV1],
    )));
}

/// Runs try 1 of the update of `x` to 2.0, which defers, and the beat that takes its verdict.
fn defer(rig: &Rig) -> Heartbeater {
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    ask(rig, 1, "2.0");
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    heartbeater
}

fn file_of(rig: &Rig) -> std::path::PathBuf {
    rig.service_dir("x").join(DEFERRAL_FILE)
}

/// The file of `x`, as JSON.
fn file(rig: &Rig) -> serde_json::Value {
    let text = std::fs::read(file_of(rig)).expect("the file");
    serde_json::from_slice(&text).expect("JSON")
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write");
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
    ask(&rig, 2, "2.0");
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

    // At the retry time it runs, and its success ends the deferral.
    rig.world.clock.advance(1);
    ask(&rig, 3, "2.0");
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    assert!(!file_of(&rig).exists());
    assert_eq!(rig.world.orders.orders().len(), 2);
}

/// Catches a retry time kept only in memory: a new loop over the same state directory still
/// holds the target back; another target is not held back.
#[test]
fn retry_time_survives_a_restart() {
    let rig = Rig::with_world("deferral-restart", deferring_once());
    drop(defer(&rig));
    let mut heartbeater = rig.heartbeater();
    ask(&rig, 2, "2.0");
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

    let rig = Rig::with_world("deferral-forced", deferring_once());
    let mut heartbeater = defer(&rig);
    rig.world.link.answer(Ok(reply(
        Some(forced("o-9", 1, "x", "2.0")),
        &[Feature::DeferredV1, Feature::ForcedV1],
    )));
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
}

/// Catches a verdict on another target, or a forced verdict, ending the deferral of a target:
/// the deferred target is still held back until its retry time.
#[test]
fn a_verdict_on_another_target_or_a_forced_one_keeps_the_deferral() {
    let world = World::with_orders(FakeSink::new(|order, _| {
        if order.is_forced() | (order.target().expect("a target").version.as_str() == "2.1") {
            verdict(order, Outcome::Failed)
        } else {
            deferred(order)
        }
    }));
    let rig = Rig::with_world("deferral-other-target", world);
    let mut heartbeater = defer(&rig);
    rig.world.link.answer(Ok(reply(
        Some(update("o-2", 1, "x", "2.1")),
        &[Feature::DeferredV1],
    )));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    rig.world.link.answer(Ok(reply(
        Some(forced("o-3", 1, "x", "2.0")),
        &[Feature::DeferredV1, Feature::ForcedV1],
    )));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    ask(&rig, 2, "2.0");
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(rig.world.orders.orders().len(), 3);
    assert_eq!(
        file(&rig)["retry_after_unix"].as_u64(),
        Some(START_UNIX + 600)
    );
}

/// Catches a file that kbf's agent, the previous schema, cannot read, and a file kbf's agent
/// wrote, with or without a key added later, that this loop cannot read. kbf's reader is the one
/// of its release, pinned in `kbf.rs`. A target in hostkeeper's version grammar and not in kbf's
/// (`_`, a letter first) is refused by kbf's reader, which then goes on without the deferral:
/// the only loss is that it may run the target before the retry time.
#[test]
fn files_written_by_the_loop_are_read_by_the_previous_schema() {
    let rig = Rig::with_world("deferral-schema", deferring_once());
    drop(defer(&rig));
    let text = std::fs::read(file_of(&rig)).expect("the file");
    let old: kbf::Deferral = serde_json::from_slice(&text).expect("kbf reads it");
    assert_eq!(old.target.as_str(), "2.0");
    assert_eq!(old.reason.as_str(), "drain_bound");
    assert_eq!(old.retry_after_unix, START_UNIX + 600);

    let deferrals = Deferrals::new(rig.state());
    let x = service("x");
    let kbf_written = kbf::Deferral {
        target: kbf::Version::parse("1.7+g0123456789ab").expect("a kbf version"),
        reason: kbf::Token::parse("drain_bound").expect("a kbf token"),
        retry_after_unix: START_UNIX + 30,
    };
    let expected = Deferral {
        target: version("1.7+g0123456789ab"),
        reason: drain_bound(),
        retry_after_unix: START_UNIX + 30,
    };
    let mut later = serde_json::to_value(&kbf_written).expect("JSON");
    later["added_later"] = serde_json::json!(1);
    for written in [
        serde_json::to_string(&kbf_written).expect("JSON"),
        later.to_string(),
    ] {
        write(&file_of(&rig), &written);
        assert_eq!(
            deferrals.load(&x).expect("readable"),
            Some(expected.clone())
        );
    }

    let only_ours = Deferral {
        target: version("v2_0"),
        ..expected
    };
    deferrals.save(&x, &only_ours).expect("written");
    let text = std::fs::read(file_of(&rig)).expect("the file");
    assert!(serde_json::from_slice::<kbf::Deferral>(&text).is_err());
}

/// Catches a deferral that holds a host longer than the cap, from its verdict, and a report or
/// a decline past the retry time; and arithmetic that wraps at the end of time.
#[test]
fn a_deferral_lasts_at_most_the_cap_and_ends_at_its_retry_time() {
    let target = version("2.0");
    let long = Deferral::starting(target.clone(), drain_bound(), 1000, u64::MAX);
    assert_eq!(long.retry_after_unix, 1000 + MAX_RETRY_SECS);
    assert!(long.declines(&target, 1000 + MAX_RETRY_SECS - 1));
    assert!(!long.declines(&target, 1000 + MAX_RETRY_SECS));
    assert!(!long.declines(&version("2.1"), 1000));
    assert_eq!(long.report(1000 + MAX_RETRY_SECS), None);
    assert_eq!(
        long.report(1000).map(|report| report.retry_after_secs),
        Some(MAX_RETRY_SECS)
    );
    let none = Deferral::starting(target.clone(), drain_bound(), 1000, 0);
    assert!(!none.declines(&target, 1000));
    assert_eq!(none.report(1000), None);
    let late = Deferral::starting(target.clone(), drain_bound(), u64::MAX - 10, 60);
    assert_eq!(late.retry_after_unix, u64::MAX);
    assert_eq!(late.clone().read_at(u64::MAX - 10), late);
    let far = Deferral {
        retry_after_unix: u64::MAX,
        ..long.clone()
    };
    assert_eq!(far.read_at(1000), long);
}

/// Catches a far retry time read again as far at every read (it would never expire), a cap
/// that is not kept across a restart, and a clock set back that holds the target for the whole
/// jump: the cap is applied once and written, and moves only nearer.
#[test]
fn a_far_retry_time_is_capped_once_and_expires_after_the_cap() {
    let rig = Rig::with_world("deferral-far", deferring_once());
    rig.install(vec![recorded("x", "1.0")]);
    write(
        &file_of(&rig),
        &format!(
            r#"{{"target":"2.0","reason":"drain_bound","retry_after_unix":{}}}"#,
            u64::MAX
        ),
    );
    let mut heartbeater = rig.heartbeater();
    ask(&rig, 2, "2.0");
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(
        file(&rig)["retry_after_unix"].as_u64(),
        Some(START_UNIX + MAX_RETRY_SECS)
    );

    // After a restart half-way, the file still says the capped time, not one cap from now.
    rig.world.clock.advance(MAX_RETRY_SECS / 2);
    drop(heartbeater);
    let mut heartbeater = rig.heartbeater();
    ask(&rig, 3, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    rig.world.clock.advance(MAX_RETRY_SECS / 2);
    ask(&rig, 4, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);

    // A clock set back holds the target for one cap from the new time, not for the jump.
    let rig = Rig::with_world("deferral-clock-back", deferring_once());
    let mut heartbeater = defer(&rig);
    let back = START_UNIX - 100_000;
    rig.world.clock.set_unix(back);
    ask(&rig, 2, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    rig.world.clock.set_unix(back + MAX_RETRY_SECS);
    ask(&rig, 3, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
}

/// Catches a deferral file that cannot be used (a link, a FIFO, a directory, too long, not
/// JSON, a word outside its grammar, a service directory that is a link or open to others) read
/// as a deferral, read through, waited on, or reported without the file's name.
#[test]
fn a_deferral_file_that_cannot_be_used_is_an_error_naming_it() {
    let rig = Rig::new("deferral-errors");
    rig.install(vec![recorded("x", "1.0")]);
    let deferrals = Deferrals::new(rig.state());
    let x = service("x");
    let path = deferrals.path(&x);
    assert_eq!(path, file_of(&rig));
    assert_eq!(deferrals.load(&x).expect("no file"), None);
    assert_eq!(
        deferrals.load(&service("nowhere")).expect("no directory"),
        None
    );
    deferrals
        .clear(&service("nowhere"))
        .expect("nothing to clear");

    write(
        &path,
        &" ".repeat(usize::try_from(FILE_MAX).expect("small") + 1),
    );
    let error = deferrals.load(&x).expect_err("too long");
    assert_eq!(
        error.to_string(),
        format!("{} is longer than 4096 bytes", path.display())
    );

    write(&path, "{");
    let error = deferrals.load(&x).expect_err("not JSON");
    assert!(matches!(error, DeferralError::Json { .. }), "{error:?}");
    assert!(
        error
            .to_string()
            .starts_with(&format!("{} is not a deferral: ", path.display())),
        "{error}"
    );

    write(
        &path,
        r#"{"target":".bad","reason":"x","retry_after_unix":1}"#,
    );
    assert_eq!(
        deferrals.load(&x).expect_err("bad target").to_string(),
        format!(
            "{} holds a target or a reason outside its grammar",
            path.display()
        )
    );
    write(
        &path,
        r#"{"target":"1.0","reason":"Bad","retry_after_unix":1}"#,
    );
    assert!(matches!(
        deferrals.load(&x),
        Err(DeferralError::Grammar { .. })
    ));

    std::fs::remove_file(&path).expect("remove");
    std::os::unix::fs::symlink(rig.state().join("elsewhere"), &path).expect("a link");
    let error = deferrals.load(&x).expect_err("a link");
    assert!(matches!(error, DeferralError::Io { .. }), "{error:?}");
    deferrals.clear(&x).expect("the link goes");
    assert!(std::fs::symlink_metadata(&path).is_err());

    // A FIFO with no writer is refused at once, not waited on.
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path.as_path(),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .expect("a FIFO");
    assert_eq!(
        deferrals.load(&x).expect_err("a FIFO").to_string(),
        format!("{} is not a regular file", path.display())
    );
    deferrals.clear(&x).expect("the FIFO goes");

    std::fs::create_dir(&path).expect("a directory");
    assert!(matches!(
        deferrals.load(&x),
        Err(DeferralError::NotRegular { .. })
    ));
    std::fs::remove_dir(&path).expect("remove");

    // A service directory open to the group is refused, read or written.
    let dir = rig.service_dir("x");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o770)).expect("chmod");
    assert_eq!(
        deferrals.load(&x).expect_err("open to others").to_string(),
        format!(
            "cannot read {}: {} is not a directory of this user alone",
            path.display(),
            dir.display()
        )
    );
    assert!(deferrals.save(&x, &a_deferral()).is_err());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
}

fn a_deferral() -> Deferral {
    Deferral::starting(version("2.0"), drain_bound(), START_UNIX, 60)
}

/// Catches a service directory that is a link being followed out of the state directory, for a
/// read, a write or a removal: each is refused and the file it points at is left alone.
#[test]
fn a_service_directory_that_is_a_link_is_not_followed() {
    let rig = Rig::new("deferral-dir-link");
    let elsewhere = rig.state().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("a directory");
    std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let kept = r#"{"target":"2.0","reason":"drain_bound","retry_after_unix":1}"#;
    write(&elsewhere.join(DEFERRAL_FILE), kept);
    std::os::unix::fs::symlink(&elsewhere, rig.service_dir("x")).expect("a link");
    let deferrals = Deferrals::new(rig.state());
    let x = service("x");
    assert!(matches!(deferrals.load(&x), Err(DeferralError::Io { .. })));
    assert!(deferrals.save(&x, &a_deferral()).is_err());
    assert!(deferrals.clear(&x).is_err());
    assert_eq!(
        std::fs::read_to_string(elsewhere.join(DEFERRAL_FILE)).expect("read"),
        kept
    );
}

/// Catches a loop that trusts an unusable deferral file, waits on a FIFO, stops on it, or keeps
/// it: it reports no deferral, runs the order, says what it did, and removes the file at the
/// next heartbeat; a file it cannot remove is said at every heartbeat until it can.
#[test]
fn an_unusable_deferral_file_is_logged_removed_and_ignored() {
    let rig = Rig::new("deferral-unusable");
    rig.install(vec![recorded("x", "1.0")]);
    let path = file_of(&rig);
    write(
        &path,
        r#"{"target":".bad","reason":"x","retry_after_unix":1}"#,
    );
    let mut heartbeater = rig.heartbeater();
    rig.world
        .link
        .answer(Ok(reply(None, &[Feature::DeferredV1])));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(rig.world.link.beats()[1].services[0].deferred, None);
    heartbeater.beat_once();
    assert!(!path.exists());
    assert_eq!(
        rig.world.log.lines(),
        [format!(
            "error: the deferral of x is not used and is removed: {} holds a target or a \
             reason outside its grammar",
            path.display()
        )]
    );

    // A FIFO in its place: the heartbeat goes on, the order runs, the FIFO goes.
    let rig = Rig::new("deferral-fifo");
    rig.install(vec![recorded("x", "1.0")]);
    let path = file_of(&rig);
    rustix::fs::mknodat(
        rustix::fs::CWD,
        path.as_path(),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .expect("a FIFO");
    let mut heartbeater = rig.heartbeater();
    ask(&rig, 1, "2.0");
    assert_eq!(heartbeater.beat_once(), hostkeeper_link::BeatOutcome::Sent);
    assert_eq!(rig.world.dispatch.pending(), 1);
    heartbeater.beat_once();
    assert!(std::fs::symlink_metadata(&path).is_err());

    // A directory in its place cannot be removed: said at every heartbeat.
    let rig = Rig::new("deferral-directory");
    rig.install(vec![recorded("x", "1.0")]);
    let path = file_of(&rig);
    std::fs::create_dir(&path).expect("a directory");
    let mut heartbeater = rig.heartbeater();
    ask(&rig, 1, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    heartbeater.beat_once();
    heartbeater.beat_once();
    let not_written = "error: the deferral of x is not written: Is a directory (os error 21)";
    assert_eq!(
        rig.world.log.lines(),
        [
            format!(
                "error: the deferral of x is not used and is removed: {} is not a regular file",
                path.display()
            ),
            not_written.to_owned(),
            not_written.to_owned(),
        ]
    );
}

/// Catches a deferral whose write failed letting its target run at once (the loop would forget
/// the retry time), a failed write not tried again, or not said: the target is held back from
/// memory, and the file is written at the first heartbeat after the directory is back.
#[test]
fn a_deferral_that_cannot_be_written_holds_the_target_back_and_is_written_later() {
    let rig = Rig::with_world("deferral-unwritable", deferring_once());
    rig.world.records.set(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    ask(&rig, 1, "2.0");
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    let shown = rig.world.link.beats()[1].services[0].last_update.clone();
    assert_eq!(
        shown.map(|result| result.outcome),
        Some(Outcome::Deferred {
            retry_after_secs: 600
        })
    );
    ask(&rig, 2, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    let not_written =
        "error: the deferral of x is not written: No such file or directory (os error 2)";
    assert_eq!(
        rig.world.log.lines(),
        [
            not_written.to_owned(),
            not_written.to_owned(),
            format!(
                "order o-1 try 2 of x is not run: deferred until unix second {}",
                START_UNIX + 600
            ),
        ]
    );
    rig.make_service_dir("x");
    heartbeater.beat_once();
    assert_eq!(
        file(&rig)["retry_after_unix"].as_u64(),
        Some(START_UNIX + 600)
    );
}

/// Catches a removal that failed being forgotten (the deferral would come back from the file
/// after a restart) or holding the target back: the target runs, and the file goes once the
/// directory takes it.
#[test]
fn a_deferral_whose_removal_fails_is_removed_later() {
    let rig = Rig::with_world("deferral-unremovable", deferring_once());
    let mut heartbeater = defer(&rig);
    let dir = rig.service_dir("x");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    rig.world
        .link
        .answer(Ok(reply(Some(update("o-1", 2, "x", "2.0")), &[])));
    heartbeater.beat_once();
    assert!(rig.world.dispatch.run_next());
    heartbeater.beat_once();
    ask(&rig, 3, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(file_of(&rig).exists());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    heartbeater.beat_once();
    assert!(!file_of(&rig).exists());
    let lines = rig.world.log.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    for line in lines {
        assert_eq!(
            line,
            "error: the deferral of x is not written: Permission denied (os error 13)"
        );
    }
}

/// Catches a daemon that died after the executor recorded a deferral and before the loop wrote
/// its retry time: the new loop resends the verdict and holds the target back from now.
#[test]
fn a_deferred_verdict_whose_deferral_was_never_written_holds_the_target_after_a_restart() {
    let rig = Rig::new("deferral-crash");
    let order = update("o-1", 1, "x", "2.0");
    rig.install(vec![RecordedService {
        last_result: Some(deferred(&order)),
        ..recorded("x", "1.0")
    }]);
    let mut heartbeater = rig.heartbeater();
    ask(&rig, 2, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(
        file(&rig)["retry_after_unix"].as_u64(),
        Some(START_UNIX + 600)
    );
    assert_eq!(
        rig.world.link.beats()[0].services[0]
            .last_update
            .clone()
            .map(|result| result.order),
        Some(order.reference())
    );

    // Another restart, later: the deferral on disk is the one kept, not started again from now.
    drop(heartbeater);
    rig.world.clock.advance(300);
    let mut heartbeater = rig.heartbeater();
    ask(&rig, 3, "2.0");
    heartbeater.beat_once();
    assert_eq!(rig.world.dispatch.pending(), 0);
    assert_eq!(
        file(&rig)["retry_after_unix"].as_u64(),
        Some(START_UNIX + 600)
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
    let temp = rig.service_dir("x").join(TEMP_FILE);
    let elsewhere = rig.state().join("elsewhere");
    write(&elsewhere, "kept");
    std::os::unix::fs::symlink(&elsewhere, &temp).expect("a link");
    let deferral = a_deferral();
    deferrals.save(&x, &deferral).expect("written");
    assert_eq!(std::fs::read_to_string(&elsewhere).expect("read"), "kept");
    assert!(std::fs::symlink_metadata(&temp).is_err());
    let mode = std::fs::metadata(deferrals.path(&x))
        .expect("the file")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        deferrals.load(&x).expect("readable"),
        Some(deferral.clone())
    );

    // A temporary path that cannot be removed stops the write, and the file is left as it was.
    std::fs::create_dir(&temp).expect("a directory");
    assert!(deferrals.save(&x, &deferral).is_err());
    assert_eq!(deferrals.load(&x).expect("readable"), Some(deferral));
}

/// Catches a deferral whose verdict gives no reason being dropped, or written with a reason
/// outside the grammar kbf's agent reads: it is kept under the reason `deferred`.
#[test]
fn a_deferral_with_no_reason_is_kept_under_the_reason_deferred() {
    let world = World::with_orders(FakeSink::new(|order, _| {
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

/// Catches a retry time on record that has passed being started again by a restart (the wait
/// would renew at every restart): at the retry time and after it, the recovered deferred verdict
/// leaves the record as it is and the target runs.
#[test]
fn a_restart_at_or_after_the_retry_time_on_record_does_not_start_the_wait_again() {
    let rig = Rig::new("deferral-ended");
    let order = update("o-1", 1, "x", "2.0");
    rig.install(vec![RecordedService {
        last_result: Some(deferred(&order)),
        ..recorded("x", "1.0")
    }]);
    let on_record = format!(
        r#"{{"target":"2.0","reason":"drain_bound","retry_after_unix":{}}}"#,
        START_UNIX + 600
    );
    write(&file_of(&rig), &on_record);
    for (order_try, at) in [(2, START_UNIX + 600), (3, START_UNIX + 5000)] {
        rig.world.clock.set_unix(at);
        let mut heartbeater = rig.heartbeater();
        ask(&rig, order_try, "2.0");
        heartbeater.beat_once();
        assert_eq!(rig.world.dispatch.pending(), 1, "at {at}");
        assert!(rig.world.dispatch.run_next());
        drop(heartbeater);
        assert_eq!(
            std::fs::read_to_string(file_of(&rig)).expect("the file"),
            on_record
        );
    }
}

/// Catches a deferral left standing beside the verdict that ended it, when the verdict comes
/// back after a restart (the settled update of the daemon itself, whose binary before the
/// restart wrote the deferral): a hooked verdict of the deferral's target, given back by the
/// records, ends it as it does when the order runs in the loop; a forced one keeps it.
#[test]
fn a_verdict_given_back_after_a_restart_ends_the_deferral_of_its_target() {
    for (forced_verdict, kept) in [(false, false), (true, true)] {
        let rig = Rig::with_world(
            &format!("deferral-recover-{forced_verdict}"),
            deferring_once(),
        );
        drop(defer(&rig));
        assert!(file_of(&rig).exists());
        let order = update("o-1", 1, "x", "2.0");
        let mut result = verdict(&order, Outcome::Succeeded);
        result.forced = forced_verdict;
        rig.install(vec![RecordedService {
            last_result: Some(result),
            ..recorded("x", "1.0")
        }]);
        let mut heartbeater = rig.heartbeater();
        rig.world
            .link
            .answer(Ok(reply(None, &[Feature::DeferredV1])));
        heartbeater.beat_once();
        heartbeater.beat_once();
        let beats = rig.world.link.beats();
        let last = beats.last().expect("a beat");
        assert_eq!(
            last.services[0].deferred.is_some(),
            kept,
            "forced: {forced_verdict}"
        );
    }
}
