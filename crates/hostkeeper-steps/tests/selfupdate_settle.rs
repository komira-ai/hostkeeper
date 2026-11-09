//! The updater's own update until its new binary has run: the step record waits for the restart,
//! the next start settles it, a fallback marks the version bad, and the files stay additive.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{
    AwaitingRestart, HookSel, Progress, REASON_SELF_UPDATE_FALLBACK, RECORD_FILE, SCHEMA,
    Settlement, State,
};
use hostkeeper_steps::selfupdate::settle_with;
use hostkeeper_steps::{
    BAD_VERSIONS_FILE, BadVersions, BadVersionsError, CrashPoint, Finished, FollowOn, ForceRecord,
    Layout, Loaded, MAX_BAD_VERSIONS, Record, RecordError, RecordStore, ResumeAction,
    SelfUpdateError, Settled, Staged, follow_on, resume, resume_action, settle,
};
use support::{FAIL, FakeClock, Rig, Scratch};

/// The updater binary of a release, in its own staged tree.
const UPDATER: &str = "bin/updater";

fn binary(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    path
}

/// A release tree under `dir` whose updater binary holds `text`, sealed.
fn release(dir: &Path, name: &str, text: &str) -> Staged {
    let tree = dir.join(format!("release-{name}"));
    fs::create_dir_all(tree.join("bin")).unwrap();
    fs::write(tree.join(UPDATER), text).unwrap();
    Staged::seal(&tree).unwrap()
}

fn ok() -> Finished {
    Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    }
}

/// The step record of the updater's own update to `version` whose steps ran, as the runner leaves
/// it when it holds the success back: `done` at `cleanup`, unfinished.
fn steps_ran(version: &str) -> Record {
    Record {
        schema: SCHEMA,
        service: "hostkeeper".to_owned(),
        target_version: version.to_owned(),
        artifact_digest: "ab".repeat(32),
        tarball_digest: "cd".repeat(32),
        previous_digest: Some("ef".repeat(32)),
        generation: 1,
        params: "/state/hostkeeper/params.toml".into(),
        step: Step::Cleanup,
        attempt: 1,
        state: State::Done {
            code: Some(0),
            last_line: None,
        },
        hook: HookSel::New,
        progress: Progress {
            committed: true,
            ..Progress::default()
        },
        order_try: Some(1),
        forced: false,
        force: None,
    }
}

/// `rec` waiting for the restart into `version`.
fn waiting(mut rec: Record, version: &str) -> Record {
    rec.progress.awaiting_restart = Some(AwaitingRestart {
        version: version.to_owned(),
        verdict_promotion_pending: false,
        order_id: None,
        next: None,
        starts: None,
    });
    rec
}

/// `rec` waiting for the restart into `version` by a hand-over: `next` names it, no start yet.
fn handed_over(mut rec: Record, version: &str) -> Record {
    rec = waiting(rec, version);
    if let Some(awaiting) = rec.progress.awaiting_restart.as_mut() {
        awaiting.next = Some(version.to_owned());
        awaiting.starts = Some(0);
    }
    rec
}

/// A host whose updater runs `v1` (staged, `current`), with a state directory and the release
/// of `v2`.
struct Host {
    s: Scratch,
    layout: Layout,
    state: PathBuf,
    v2: Staged,
}

impl Host {
    fn new(name: &str) -> Host {
        let s = Scratch::new(name);
        let layout = Layout::new(&s.path().join("bin"));
        layout.stage("v1", &binary(s.path(), "a", "a")).unwrap();
        layout.switch("v1").unwrap();
        let state = s.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let v2 = release(s.path(), "v2", "b");
        Host {
            s,
            layout,
            state,
            v2,
        }
    }

    fn store(&self) -> RecordStore {
        RecordStore::new(&self.state)
    }

    /// The store, every write counted in `count`.
    fn counted(&self, count: &Arc<AtomicU64>) -> RecordStore {
        let count = Arc::clone(count);
        self.store().with_crash_gate(Arc::new(move |point| {
            if let CrashPoint::BeforeWrite(_) = point {
                count.fetch_add(1, Ordering::SeqCst);
            }
            false
        }))
    }

    fn record(&self) -> Record {
        match self.store().load().unwrap() {
            Loaded::Current(rec) => *rec,
            other => panic!("no current record: {other:?}"),
        }
    }

    fn current(&self) -> Option<String> {
        self.layout.current().unwrap()
    }

    fn bad(&self) -> Vec<String> {
        BadVersions::load(&self.state).unwrap().versions().to_vec()
    }

    fn follow(&self, store: &RecordStore) -> Result<FollowOn, SelfUpdateError> {
        follow_on(
            &ok(),
            store,
            &self.layout,
            "v1",
            "v2",
            &self.v2,
            Path::new(UPDATER),
            None,
        )
    }
}

fn fell_back() -> State {
    State::Finished {
        verdict: Verdict::Failed {
            phase: Phase::AfterCommit,
            class: Class::Release,
        },
        reason: Some(REASON_SELF_UPDATE_FALLBACK.to_owned()),
    }
}

fn confirmed() -> State {
    State::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    }
}

/// Catches a follow-on that switches `current` (the hand-over moves none), that finishes the
/// record itself, or that writes a wait that names no hand-over: after it, the record is still
/// `done` at `cleanup`, names the version it waits for with `next` and a start count of 0,
/// `next` names the staged new binary, and `current` still names the old one.
#[test]
fn the_follow_on_hands_over_and_moves_no_current() {
    let host = Host::new("su-wait");
    host.store().save(&steps_ran("v2")).unwrap();
    assert_eq!(host.follow(&host.store()).unwrap(), FollowOn::Restart);
    let rec = host.record();
    assert_eq!(rec, handed_over(steps_ran("v2"), "v2"));
    assert_eq!(host.current().as_deref(), Some("v1"));
    assert_eq!(host.layout.previous().unwrap(), None);
    assert_eq!(host.layout.next().unwrap().as_deref(), Some("v2"));
    assert!(host.layout.version_path("v2").is_file());
}

/// Catches a crash that leaves a `Succeeded` for a binary that never ran, or a link `next` that
/// outlives its wait: the updater dies before the record is written, after it, and after the
/// whole hand-over; each time the next start, by the old binary and (separately) by the new one,
/// settles the record `FellBack` or `Confirmed`, or finds nothing to settle with the update still
/// unfinished (and removes the link), and the record says `Succeeded` only where the new binary
/// runs and `current` names it.
#[test]
fn a_crash_between_the_link_and_the_record_settles_correctly() {
    // The point, and what a start of the old binary, then of the new one, settles there.
    let points = [
        (
            "before the record is written",
            Some(CrashPoint::BeforeWrite(1)),
            [Settled::NotPending, Settled::NotPending],
        ),
        (
            "after the record is written",
            Some(CrashPoint::AfterWrite(1)),
            [Settled::FellBack, Settled::Confirmed],
        ),
        (
            "after the hand-over, before the restart",
            None,
            [Settled::FellBack, Settled::Confirmed],
        ),
    ];
    for (n, (what, point, outcomes)) in points.into_iter().enumerate() {
        for (running, want) in ["v1", "v2"].into_iter().zip(outcomes) {
            let host = Host::new(&format!("su-crash-{n}-{running}"));
            host.store().save(&steps_ran("v2")).unwrap();
            let gated = host
                .store()
                .with_crash_gate(Arc::new(move |at| Some(at) == point));
            let followed = host.follow(&gated);
            match point {
                Some(at) => {
                    assert!(
                        matches!(followed, Err(SelfUpdateError::Record(RecordError::Crashed(p))) if p == at),
                        "{what}: {followed:?}"
                    );
                }
                None => assert_eq!(followed.unwrap(), FollowOn::Restart, "{what}"),
            }
            // The follow-on moves no `current`, whatever point it stopped at.
            assert_eq!(host.current().as_deref(), Some("v1"), "{what}");
            let settled = settle(&host.layout, &host.store(), running).unwrap();
            assert_eq!(settled, want, "{what}, {running} starts");
            let rec = host.record();
            let succeeded = matches!(
                rec.state,
                State::Finished {
                    verdict: Verdict::Succeeded { .. },
                    ..
                }
            );
            let new_runs = (running == "v2") & (host.current().as_deref() == Some("v2"));
            assert!(!succeeded | new_runs, "{what}, {running}: {rec:?}");
            let state = match want {
                Settled::NotPending => steps_ran("v2").state,
                Settled::FellBack => fell_back(),
                Settled::Confirmed => confirmed(),
            };
            assert_eq!(rec.state, state, "{what}, {running}");
            assert_eq!(rec.progress.awaiting_restart, None, "{what}, {running}");
            assert_eq!(host.layout.next().unwrap(), None, "{what}, {running}");
            let bad = match want {
                Settled::FellBack => vec!["v2".to_owned()],
                _ => Vec::new(),
            };
            assert_eq!(host.bad(), bad, "{what}, {running}");
        }
    }
}

/// Catches a confirmation that is skipped, written twice, or written before what must precede it:
/// the new binary that `current` names confirms the update in one write, after its hook ran (and
/// while the record still waited, so that a crash in between confirms again); a hook that fails
/// leaves the record waiting; the second start writes nothing; the promotion the update recorded
/// is kept.
#[test]
fn the_new_binary_confirms_once_after_its_hook() {
    let host = Host::new("su-confirm");
    let mut rec = waiting(steps_ran("v2"), "v2");
    host.store().save(&rec).unwrap();
    host.layout
        .stage("v2", &binary(host.s.path(), "b", "b"))
        .unwrap();
    host.layout.switch("v2").unwrap();

    let refused = settle_with(&host.layout, &host.store(), "v2", &mut || {
        Err(SelfUpdateError::NoUpdate {
            version: "hook".to_owned(),
        })
    });
    assert!(matches!(refused, Err(SelfUpdateError::NoUpdate { .. })));
    assert_eq!(host.record(), rec);

    let writes = Arc::new(AtomicU64::new(0));
    let store = host.counted(&writes);
    let mut seen = Vec::new();
    let mut hook = || -> Result<(), SelfUpdateError> {
        seen.push(host.record().progress.awaiting_restart.clone());
        Ok(())
    };
    let first = settle_with(&host.layout, &store, "v2", &mut hook).unwrap();
    assert_eq!(first, Settled::Confirmed);
    assert_eq!(seen, [rec.progress.awaiting_restart.clone()]);
    assert_eq!(
        settle(&host.layout, &store, "v2").unwrap(),
        Settled::NotPending
    );
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    rec.progress.awaiting_restart = None;
    rec.state = confirmed();
    assert_eq!(host.record(), rec);
    assert!(host.bad().is_empty());

    // A promotion still pending when the steps ended is what the confirmation reports.
    let mut pending = waiting(steps_ran("v2"), "v2");
    pending.progress.awaiting_restart = Some(AwaitingRestart {
        version: "v2".to_owned(),
        verdict_promotion_pending: true,
        order_id: None,
        next: None,
        starts: None,
    });
    host.store().save(&pending).unwrap();
    assert!(
        host.store()
            .settle_awaiting_restart(Settlement::Confirmed)
            .unwrap()
    );
    assert_eq!(
        host.record().state,
        State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: true
            },
            reason: None,
        }
    );
}

/// Catches a bad mark lost to a crash, or written twice: the version goes on the list before the
/// record is closed, so a start that dies between the two leaves the record waiting and the mark
/// there, and the next start closes the record without adding the version again.
#[test]
fn a_fallback_marks_the_version_bad_before_it_closes_the_record() {
    let host = Host::new("su-bad-first");
    host.store().save(&waiting(steps_ran("v2"), "v2")).unwrap();
    let dies = host
        .store()
        .with_crash_gate(Arc::new(|at| at == CrashPoint::BeforeWrite(1)));
    let died = settle(&host.layout, &dies, "v1");
    assert!(
        matches!(died, Err(SelfUpdateError::Record(RecordError::Crashed(_)))),
        "{died:?}"
    );
    assert_eq!(host.bad(), ["v2"]);
    assert_eq!(host.record(), waiting(steps_ran("v2"), "v2"));
    let list = fs::read(host.state.join(BAD_VERSIONS_FILE)).unwrap();

    assert_eq!(
        settle(&host.layout, &host.store(), "v1").unwrap(),
        Settled::FellBack
    );
    assert_eq!(host.record().state, fell_back());
    assert_eq!(fs::read(host.state.join(BAD_VERSIONS_FILE)).unwrap(), list);
    assert_eq!(
        settle(&host.layout, &host.store(), "v1").unwrap(),
        Settled::NotPending
    );
}

/// Catches a start that writes something when no update waits for it: no record, a finished
/// record, an unfinished one that waits for nothing, and one of a newer schema are each left as
/// they are, and no list of bad versions is made.
#[test]
fn a_start_with_nothing_waiting_writes_nothing() {
    let host = Host::new("su-nothing");
    assert_eq!(
        settle(&host.layout, &host.store(), "v1").unwrap(),
        Settled::NotPending
    );
    let finished = Record {
        state: confirmed(),
        ..steps_ran("v2")
    };
    for rec in [finished, steps_ran("v2")] {
        host.store().save(&rec).unwrap();
        let before = fs::read(host.state.join(RECORD_FILE)).unwrap();
        assert_eq!(
            settle(&host.layout, &host.store(), "v1").unwrap(),
            Settled::NotPending
        );
        assert!(
            !host
                .store()
                .settle_awaiting_restart(Settlement::Confirmed)
                .unwrap()
        );
        assert_eq!(fs::read(host.state.join(RECORD_FILE)).unwrap(), before);
    }
    fs::write(host.state.join(RECORD_FILE), r#"{"schema":2}"#).unwrap();
    assert_eq!(
        settle(&host.layout, &host.store(), "v1").unwrap(),
        Settled::NotPending
    );
    for outcome in [Settlement::Confirmed, Settlement::FellBack] {
        assert!(!host.store().settle_awaiting_restart(outcome).unwrap());
    }
    fs::remove_file(host.state.join(RECORD_FILE)).unwrap();
    assert!(
        !host
            .store()
            .settle_awaiting_restart(Settlement::FellBack)
            .unwrap()
    );
    assert!(!host.state.join(BAD_VERSIONS_FILE).exists());
}

/// Catches a list of bad versions that cannot be read being taken for an empty one: a fallback
/// whose list is garbage is an error naming the file, and the record still waits.
#[test]
fn a_garbage_list_of_bad_versions_stops_the_fallback_naming_the_file() {
    let host = Host::new("su-garbage");
    host.store().save(&waiting(steps_ran("v2"), "v2")).unwrap();
    let path = host.state.join(BAD_VERSIONS_FILE);
    fs::write(&path, "not json").unwrap();
    let err = settle(&host.layout, &host.store(), "v1").unwrap_err();
    assert!(
        matches!(&err, SelfUpdateError::BadVersions(BadVersionsError::Garbage { path: p, .. }) if *p == path),
        "{err:?}"
    );
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "{err}"
    );
    assert_eq!(host.record(), waiting(steps_ran("v2"), "v2"));
}

/// Catches a follow-on that hands over with no record to wait in: no record, a finished one and
/// an update to another version are each `NoUpdate`, and nothing is staged or linked, `current`
/// and the record are as they were.
#[test]
fn the_follow_on_needs_the_unfinished_update_to_its_version() {
    let host = Host::new("su-no-update");
    let finished = Record {
        state: confirmed(),
        ..steps_ran("v2")
    };
    for rec in [None, Some(finished), Some(steps_ran("v3"))] {
        let _ = fs::remove_file(host.state.join(RECORD_FILE));
        if let Some(rec) = &rec {
            host.store().save(rec).unwrap();
        }
        let err = host.follow(&host.store()).unwrap_err();
        assert!(
            matches!(&err, SelfUpdateError::NoUpdate { version } if version == "v2"),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            "the step record holds no unfinished update to v2"
        );
        assert_eq!(host.current().as_deref(), Some("v1"));
        assert_eq!(host.layout.next().unwrap(), None);
        assert!(!host.layout.version_path("v2").exists());
        match rec {
            Some(rec) => assert_eq!(host.record(), rec),
            None => assert_eq!(host.store().load().unwrap(), Loaded::Absent),
        }
    }
}

/// Catches a link that fails being reported as done, or leaving a record that the next start
/// would settle as a hand-over that failed: with the name `next` is made through taken by a
/// directory, the follow-on fails, `current` and `next` are unchanged and the record waits for
/// nothing; once the way is clear, a repeat hands over and the record waits for the restart.
#[test]
fn a_failed_link_stops_before_the_wait_and_is_repeated() {
    let host = Host::new("su-link-fails");
    host.store().save(&steps_ran("v2")).unwrap();
    let blocker = host.layout.dir().join("next.tmp");
    fs::create_dir(&blocker).unwrap();
    let err = host.follow(&host.store()).unwrap_err();
    assert!(matches!(err, SelfUpdateError::Io(_)), "{err:?}");
    assert_eq!(host.current().as_deref(), Some("v1"));
    assert_eq!(host.layout.next().unwrap(), None);
    assert_eq!(host.record(), steps_ran("v2"));
    assert_eq!(
        settle(&host.layout, &host.store(), "v1").unwrap(),
        Settled::NotPending
    );
    fs::remove_dir(&blocker).unwrap();
    assert_eq!(host.follow(&host.store()).unwrap(), FollowOn::Restart);
    assert_eq!(host.current().as_deref(), Some("v1"));
    assert_eq!(host.layout.next().unwrap().as_deref(), Some("v2"));
    assert_eq!(host.record(), handed_over(steps_ran("v2"), "v2"));
}

/// Catches a record that cannot be written leaving the link `next` standing, or the update
/// reported as handed over: a record that is a directory fails the write after the link was made;
/// the follow-on fails with the link removed again and `current` unmoved.
#[test]
fn a_record_that_cannot_be_written_takes_the_link_back() {
    let host = Host::new("su-record-fails");
    host.store().save(&steps_ran("v2")).unwrap();
    let gated = host
        .store()
        .with_crash_gate(Arc::new(|at| at == CrashPoint::BeforeWrite(1)));
    let err = host.follow(&gated).unwrap_err();
    assert!(
        matches!(err, SelfUpdateError::Record(RecordError::Crashed(_))),
        "{err:?}"
    );
    assert_eq!(host.layout.next().unwrap(), None);
    assert_eq!(host.current().as_deref(), Some("v1"));
    assert_eq!(host.record(), steps_ran("v2"));
}

/// Catches a repeat of the follow-on writing the record again: once the record waits for the
/// restart, a second follow-on (a resend before the exit) writes nothing.
#[test]
fn a_repeated_follow_on_writes_the_record_once() {
    let host = Host::new("su-follow-twice");
    host.store().save(&steps_ran("v2")).unwrap();
    let writes = Arc::new(AtomicU64::new(0));
    for _ in 0..2 {
        assert_eq!(
            host.follow(&host.counted(&writes)).unwrap(),
            FollowOn::Restart
        );
    }
    assert_eq!(writes.load(Ordering::SeqCst), 1);
}

/// Catches an update to the version that already runs being left unfinished, or another record
/// being changed: the binary that runs is the target, so its unfinished update is finished as the
/// steps ended, the reason kept; a record of another version is left alone; a record that cannot
/// be read is an error.
#[test]
fn an_update_to_the_running_binary_is_finished_at_once() {
    let host = Host::new("su-running");
    host.store().save(&steps_ran("v1")).unwrap();
    let with_reason = Finished {
        reason: Some("kept".to_owned()),
        ..ok()
    };
    let run = |finished: &Finished| {
        follow_on(
            finished,
            &host.store(),
            &host.layout,
            "v1",
            "v1",
            &host.v2,
            Path::new(UPDATER),
            None,
        )
    };
    assert_eq!(run(&with_reason).unwrap(), FollowOn::NotNeeded);
    assert_eq!(
        host.record().state,
        State::Finished {
            verdict: with_reason.verdict.clone(),
            reason: Some("kept".to_owned()),
        }
    );
    host.store().save(&steps_ran("v9")).unwrap();
    assert_eq!(run(&ok()).unwrap(), FollowOn::NotNeeded);
    assert_eq!(host.record(), steps_ran("v9"));
    fs::write(host.state.join(RECORD_FILE), "{").unwrap();
    let err = run(&ok()).unwrap_err();
    assert!(
        matches!(err, SelfUpdateError::Record(RecordError::Corrupt(_))),
        "{err:?}"
    );
    assert_eq!(host.current().as_deref(), Some("v1"));
}

/// Catches the runner recording the updater's own success itself: with the success held, an
/// update that replaces a release returns `Succeeded` and leaves the record `done` at `cleanup`,
/// unfinished; a second run answers the same from the record and launches nothing. A failure is
/// written as always. The success of a first install is held as well: the updater's install
/// swaps the binary that runs as its update does, so only the binary of the version may record
/// it.
#[test]
fn a_held_success_is_returned_and_left_for_the_new_binary() {
    let rig = Rig::new("su-held");
    let held = rig.runner().with_held_success();
    let order = rig.order();
    assert_eq!(held.run(&order).unwrap(), ok());
    let Loaded::Current(rec) = rig.store().load().unwrap() else {
        panic!("no record");
    };
    assert_eq!(rec.step, Step::Cleanup);
    assert!(
        matches!(rec.state, State::Done { code: Some(0), .. }),
        "{rec:?}"
    );
    let ran = rig.fake.steps();
    assert_eq!(held.run(&order).unwrap(), ok());
    assert_eq!(rig.fake.steps(), ran);

    let rig = Rig::new("su-held-fails");
    rig.fake.say(Step::Prepare, &[FAIL]);
    let finished = rig.runner().with_held_success().run(&rig.order()).unwrap();
    assert!(matches!(finished.verdict, Verdict::Failed { .. }));
    assert!(matches!(
        rig.store().load().unwrap(),
        Loaded::Current(rec) if matches!(rec.state, State::Finished { .. })
    ));

    let rig = Rig::new("su-held-first");
    let first = hostkeeper_steps::UpdateOrder {
        previous: None,
        previous_version: None,
        ..rig.order()
    };
    assert_eq!(rig.runner().with_held_success().run(&first).unwrap(), ok());
    let Loaded::Current(rec) = rig.store().load().unwrap() else {
        panic!("no record");
    };
    assert_eq!(rec.step, Step::Cleanup);
    assert!(
        matches!(rec.state, State::Done { code: Some(0), .. }),
        "{rec:?}"
    );
}

/// Catches the start pass resuming an update that waits for the restart (its steps would run
/// again, or it would be finished `Succeeded` by a binary that is not the new one): the resume
/// table has no row for it, after a death of the daemon or a reboot, whatever else the record
/// says, and nothing in it is rewritten.
#[test]
fn a_record_that_waits_for_the_restart_is_not_an_unfinished_update() {
    let clock = FakeClock::new();
    let mut watching = waiting(steps_ran("v2"), "v2");
    watching.progress.watching = Some(hostkeeper_steps::Stamp::now(&*clock));
    for rec in [waiting(steps_ran("v2"), "v2"), watching] {
        for rebooted in [false, true] {
            assert_eq!(resume_action(&rec, rebooted), ResumeAction::NothingFinished);
            let mut again = rec.clone();
            assert_eq!(
                resume(&mut again, rebooted, &*clock),
                (ResumeAction::NothingFinished, false)
            );
            assert_eq!(again, rec);
        }
    }
    // Without the field the same record is resumed as it always was.
    assert_eq!(
        resume_action(&steps_ran("v2"), false),
        ResumeAction::RecomputeNext
    );
}

/// Catches a list of bad versions that loses a version, forgets one past the cap in the wrong
/// order, is open to others, or is written when nothing changed: a version added is there and
/// on disk (mode 0600, the pinned shape), adding it again writes nothing, and past the cap the
/// oldest go first.
#[test]
fn the_list_of_bad_versions_keeps_the_last_versions_added() {
    let s = Scratch::new("su-bad-list");
    let mut list = BadVersions::load(s.path()).unwrap();
    assert!(list.versions().is_empty());
    assert_eq!(list.path(), s.path().join(BAD_VERSIONS_FILE));
    assert!(!list.contains("1.0.0"));
    assert!(list.add("1.0.0").unwrap());
    let path = s.path().join(BAD_VERSIONS_FILE);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        r#"{"schema":1,"versions":["1.0.0"]}"#
    );
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!list.add("1.0.0").unwrap());
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644, "nothing was written");

    let mut list = BadVersions::load(s.path()).unwrap();
    assert!(list.contains("1.0.0"));
    for n in 1..=MAX_BAD_VERSIONS {
        assert!(list.add(&format!("1.0.{n}")).unwrap());
    }
    let read = BadVersions::load(s.path()).unwrap();
    assert_eq!(read.versions().len(), MAX_BAD_VERSIONS);
    assert_eq!(read.versions()[0], "1.0.1");
    assert!(!read.contains("1.0.0"));
    assert!(read.contains(&format!("1.0.{MAX_BAD_VERSIONS}")));
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "written again closed");
}

/// Catches a list that cannot be read or written being taken for an empty one: a file that is
/// not the list's object, and one that is a directory, are errors naming the file; a list that
/// cannot be written is an error and changes nothing.
#[test]
fn a_list_of_bad_versions_that_cannot_be_used_is_an_error_naming_it() {
    let s = Scratch::new("su-bad-errors");
    let path = s.path().join(BAD_VERSIONS_FILE);
    for text in [
        "not json",
        "[]",
        r#"{"versions":"1.0.0"}"#,
        "{}",
        r#"{"versions":[1]}"#,
    ] {
        fs::write(&path, text).unwrap();
        let err = BadVersions::load(s.path()).unwrap_err();
        assert!(
            matches!(&err, BadVersionsError::Garbage { path: p, .. } if *p == path),
            "{text}: {err:?}"
        );
        assert!(err.to_string().contains(&path.display().to_string()));
    }
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let err = BadVersions::load(s.path()).unwrap_err();
    assert!(
        matches!(&err, BadVersionsError::Io { path: p, .. } if *p == path),
        "{err:?}"
    );
    assert!(err.to_string().contains(&path.display().to_string()));

    let s = Scratch::new("su-bad-unwritable");
    let mut list = BadVersions::load(s.path()).unwrap();
    fs::create_dir(s.path().join(format!("{BAD_VERSIONS_FILE}.tmp"))).unwrap();
    let err = list.add("1.0.0").unwrap_err();
    assert!(matches!(err, BadVersionsError::Io { .. }), "{err:?}");
    assert!(!s.path().join(BAD_VERSIONS_FILE).exists());
}

/// The step record as it was before `awaiting_restart` existed: what an older binary reads.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct OldRecord {
    schema: u32,
    service: String,
    target_version: String,
    artifact_digest: String,
    tarball_digest: String,
    previous_digest: Option<String>,
    generation: u64,
    params: PathBuf,
    step: Step,
    attempt: u32,
    state: State,
    hook: HookSel,
    progress: OldProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order_try: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    forced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    force: Option<ForceRecord>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `Progress` as it was before `awaiting_restart` existed.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct OldProgress {
    committed: bool,
    promotion_pending: bool,
    then: Option<Verdict>,
    retry: u32,
    verify_started: Option<hostkeeper_steps::Stamp>,
    watching: Option<hostkeeper_steps::Stamp>,
    #[serde(default)]
    reason: Option<String>,
}

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/awaiting-restart/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(path).unwrap()
}

/// Catches `awaiting_restart` made a field an older binary cannot read, or one it reads as a
/// finished update: the committed record of an update waiting for the restart loads in the shape
/// the record had before the field (still `done`, so no older binary reports it `Succeeded`),
/// loads in the current shape with the field, and is written back as it was; a record of the
/// older shape loads in the current one without the field and is written back without it.
#[test]
fn a_record_with_awaiting_restart_loads_in_the_old_reader_shape() {
    let text = fixture("record.json");
    let old: OldRecord = serde_json::from_str(&text).unwrap();
    assert!(
        matches!(old.state, State::Done { code: Some(0), .. }),
        "{old:?}"
    );
    assert_eq!(old.target_version, "1.5.0");

    let s = Scratch::new("su-old-shape");
    fs::write(s.path().join(RECORD_FILE), &text).unwrap();
    let Loaded::Current(rec) = RecordStore::new(s.path()).load().unwrap() else {
        panic!("the fixture did not load");
    };
    assert_eq!(
        rec.progress.awaiting_restart,
        Some(AwaitingRestart {
            version: "1.5.0".to_owned(),
            verdict_promotion_pending: false,
            order_id: None,
            next: None,
            starts: None,
        })
    );
    let written = serde_json::to_value(&*rec).unwrap();
    let committed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(written, committed);

    let older = serde_json::to_string(&old).unwrap();
    assert!(!older.contains("awaiting_restart"));
    let rec: Record = serde_json::from_str(&older).unwrap();
    assert_eq!(rec.progress.awaiting_restart, None);
    let again = serde_json::to_value(&rec).unwrap();
    assert_eq!(
        again,
        serde_json::from_str::<serde_json::Value>(&older).unwrap()
    );
}

/// Catches a reader that a key of a later release breaks: a record that waits for the restart
/// with keys no release writes, in the record and in the field, still loads; the list of bad
/// versions of a later release (a newer schema, a key this one does not know) is read for its
/// versions, and written again with that key and that schema kept.
#[test]
fn a_record_and_a_bad_list_with_unknown_keys_still_load() {
    let mut value: serde_json::Value = serde_json::from_str(&fixture("record.json")).unwrap();
    value["written_by_a_later_release"] = serde_json::Value::from(1);
    value["progress"]["awaiting_restart"]["later"] = serde_json::Value::from("x");
    let s = Scratch::new("su-unknown");
    fs::write(
        s.path().join(RECORD_FILE),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let Loaded::Current(rec) = RecordStore::new(s.path()).load().unwrap() else {
        panic!("the record did not load");
    };
    assert_eq!(
        rec.progress
            .awaiting_restart
            .as_ref()
            .map(|a| a.version.as_str()),
        Some("1.5.0")
    );

    fs::write(
        s.path().join(BAD_VERSIONS_FILE),
        fixture("bad-versions-later.json"),
    )
    .unwrap();
    let mut list = BadVersions::load(s.path()).unwrap();
    assert_eq!(list.versions(), ["1.4.0", "1.5.0"]);
    assert!(list.add("1.6.0").unwrap());
    let written: serde_json::Value =
        serde_json::from_slice(&fs::read(s.path().join(BAD_VERSIONS_FILE)).unwrap()).unwrap();
    assert_eq!(
        written,
        serde_json::json!({
            "schema": 2,
            "versions": ["1.4.0", "1.5.0", "1.6.0"],
            "lifted_by": "a key of a later release"
        })
    );
}

/// Catches a confirmed update clearing more than the deferrals the old updater caused: only a
/// `Deferred` record whose reason is `updater_old` or `daemon_too_old` is removed; a deferral for
/// another reason or none, another verdict with the same reason, an unfinished record and a newer
/// one are left byte for byte; no record is fine; a record that cannot be read is an error.
#[test]
fn only_the_deferrals_of_an_old_updater_are_cleared() {
    let deferred = |reason: Option<&str>| State::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: reason.map(str::to_owned),
    };
    let failed = State::Finished {
        verdict: Verdict::Failed {
            phase: Phase::BeforeCommit,
            class: Class::Environment,
        },
        reason: Some("updater_old".to_owned()),
    };
    let rows = [
        (deferred(Some("updater_old")), true),
        (deferred(Some("daemon_too_old")), true),
        (deferred(Some("rebooted")), false),
        (deferred(None), false),
        (failed, false),
        (State::Pending, false),
    ];
    for (n, (state, cleared)) in rows.into_iter().enumerate() {
        let s = Scratch::new(&format!("su-clear-{n}"));
        let store = RecordStore::new(s.path());
        let rec = Record {
            state: state.clone(),
            ..steps_ran("v2")
        };
        store.save(&rec).unwrap();
        let before = fs::read(s.path().join(RECORD_FILE)).unwrap();
        assert_eq!(store.clear_updater_old().unwrap(), cleared, "{state:?}");
        match cleared {
            true => assert_eq!(store.load().unwrap(), Loaded::Absent, "{state:?}"),
            false => assert_eq!(
                fs::read(s.path().join(RECORD_FILE)).unwrap(),
                before,
                "{state:?}"
            ),
        }
    }
    let s = Scratch::new("su-clear-other");
    let store = RecordStore::new(s.path());
    assert!(!store.clear_updater_old().unwrap());
    fs::write(s.path().join(RECORD_FILE), r#"{"schema":2}"#).unwrap();
    assert!(!store.clear_updater_old().unwrap());
    fs::write(s.path().join(RECORD_FILE), "{").unwrap();
    assert!(matches!(
        store.clear_updater_old(),
        Err(RecordError::Corrupt(_))
    ));
}
