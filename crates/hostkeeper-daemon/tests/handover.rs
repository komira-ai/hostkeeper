//! The hand-over at the start of `hostkeeper daemon` (docs/design/two-programs.md, section 4.3):
//! before its config, the old binary execs the one the hand-over waits for, under the state
//! directory's lock and with the count raised, and settles the update failed when it cannot; the
//! new binary takes no lock here, and its start confirms the hand-over. The exec is a fake that records what it was given; the real
//! exec is run by `crates/hostkeeper/tests/binary.rs`.

mod support;

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hostkeeper_daemon::Reply;
use hostkeeper_daemon::handover::{
    Exec, HandOverError, Handed, ProcessExec, at_daemon_start, hand_over,
};
use hostkeeper_daemon::start::{self, LOCK_FILE, StartError};
use hostkeeper_hook::Verdict;
use hostkeeper_steps::SelfUpdateError;
use hostkeeper_steps::record::{RECORD_FILE, State};
use hostkeeper_steps::{BadVersions, Settled};
use support::selfupdate::{HK, NEW, OLD, SelfRig, awaiting, fell_back, own};
use support::snapshot;

/// Takes the state directory's lock as a daemon holds it: an exclusive lock on its lock file,
/// held until the file is dropped; `None` when another holds it.
fn lock(state: &Path) -> Option<fs::File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join(LOCK_FILE))
        .expect("the lock file");
    file.try_lock().ok().map(|()| file)
}

/// What the fake exec saw when it was called: the binary, the count the record held then, and
/// whether the state directory's lock was held.
#[derive(Debug, PartialEq, Eq)]
struct Call {
    binary: PathBuf,
    starts: Option<u32>,
    locked: bool,
}

/// An exec that runs nothing: it records each call and returns an error, as an exec that could
/// not be made returns.
struct FakeExec {
    state: PathBuf,
    calls: Mutex<Vec<Call>>,
}

impl FakeExec {
    fn new(s: &SelfRig) -> FakeExec {
        FakeExec {
            state: s.rig.state.clone(),
            calls: Mutex::default(),
        }
    }

    fn calls(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.lock().expect("the calls"))
    }
}

impl Exec for FakeExec {
    fn exec(&self, binary: &Path) -> io::Error {
        let store = hostkeeper_steps::RecordStore::new(&self.state.join(HK));
        let starts = hostkeeper_steps::handover::pending(&store)
            .expect("the record")
            .and_then(|wait| wait.starts);
        let locked = lock(&self.state).is_none();
        self.calls.lock().expect("the calls").push(Call {
            binary: binary.to_path_buf(),
            starts,
            locked,
        });
        io::Error::other("the fake exec runs nothing")
    }
}

/// A host of [`OLD`] that an update to [`NEW`] handed over to, as the daemon's own update leaves
/// it: [`NEW`] staged as `versions/0.2.0`, `next` naming it, `current` still [`OLD`] with no
/// `previous`, and the record waiting for [`NEW`] with `next` and a start count of `starts`.
fn handed_over(name: &str, starts: u32) -> SelfRig {
    let s = SelfRig::new(name);
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
    let store = hostkeeper_steps::RecordStore::new(&s.hk_state());
    let mut rec = s.rig.record(HK).expect("the record");
    let wait = rec.progress.awaiting_restart.as_mut().expect("the wait");
    assert_eq!((wait.next.as_deref(), wait.starts), (Some(NEW), Some(0)));
    wait.starts = Some(starts);
    store.save(&rec).expect("the hand-over record");
    s
}

fn links(s: &SelfRig) -> [Option<String>; 3] {
    let layout = s.layout();
    [
        layout.current().expect("current"),
        layout.previous().expect("previous"),
        layout.next().expect("next"),
    ]
}

fn some(version: &str) -> Option<String> {
    Some(version.to_owned())
}

/// The update settled as the hand-over's step 3 settles it: `Failed` after the commit,
/// `self_update_fallback`, [`NEW`] bad, `next` removed and `current` still [`OLD`].
fn assert_fell_back(s: &SelfRig) {
    let rec = s.rig.record(HK).expect("the record");
    let (verdict, reason) = fell_back();
    assert_eq!(rec.state, State::Finished { verdict, reason });
    assert_eq!(rec.progress.awaiting_restart, None);
    assert!(
        BadVersions::load(&s.hk_state())
            .expect("the list")
            .contains(NEW)
    );
    assert_eq!(links(s), [some(OLD), None, None]);
}

/// Catches an old daemon that starts on with a hand-over waiting, execs before the count is
/// durable, execs without the lock, or retries an exec that came back: its start execs
/// `versions/0.2.0` once, with the count 1 in the record and the state directory's lock held,
/// and, the exec having returned, settles the update `Failed`, marks 0.2.0 bad and removes
/// `next`.
#[test]
fn the_old_daemon_execs_the_new_binary_with_the_count_raised_under_the_lock() {
    let s = handed_over("ho-exec", 0);
    let exec = FakeExec::new(&s);
    let handed = at_daemon_start(&s.rig.state, OLD, &exec);
    let binary = s.layout().version_path(NEW);
    assert_eq!(
        handed,
        Handed::ExecFailed {
            binary: binary.clone(),
            error: "the fake exec runs nothing".to_owned(),
        }
    );
    assert_eq!(
        exec.calls(),
        [Call {
            binary,
            starts: Some(1),
            locked: true,
        }]
    );
    assert_fell_back(&s);
    assert!(lock(&s.rig.state).is_some(), "the lock is let go");
}

/// Catches an old daemon that hands over past the count: with the count at 3 its start execs
/// nothing and settles the update `Failed`; the daemon then starts, finds nothing to settle,
/// and answers a resend of the order with the failure from the list of bad versions.
#[test]
fn with_the_count_spent_the_old_daemon_settles_failed_and_starts() {
    let s = handed_over("ho-spent", 3);
    let exec = FakeExec::new(&s);
    assert_eq!(
        hand_over(&s.rig.state, OLD, &exec).expect("handed"),
        Handed::FellBack
    );
    assert_eq!(exec.calls(), []);
    assert_fell_back(&s);
    let started = start::startup(&s.rig.settings(), s.rig.ports(), own(OLD)).expect("started");
    assert_eq!(
        started.boot_report().self_update,
        Some(Ok(Settled::NotPending))
    );
    let (verdict, reason) = fell_back();
    assert_eq!(
        started.daemon().order(&s.order(NEW, 1)),
        Reply::Finished {
            verdict,
            reason,
            resent: true,
        }
    );
}

/// Catches the new binary execing again or writing before its lock, and a start that does not
/// confirm the hand-over: the start of 0.2.0 takes no lock and changes nothing here; the daemon's
/// start then finishes the switch (`previous` the old `current`, `current` 0.2.0), confirms the
/// update `Succeeded`, removes `next`, and answers a resend of the order `Succeeded`.
#[test]
fn the_new_daemon_execs_nothing_and_its_start_confirms_the_hand_over() {
    let s = handed_over("ho-new", 1);
    let exec = FakeExec::new(&s);
    let before = snapshot(&s.rig.state);
    assert_eq!(
        hand_over(&s.rig.state, NEW, &exec).expect("handed"),
        Handed::Settles
    );
    assert_eq!(exec.calls(), []);
    assert_eq!(snapshot(&s.rig.state), before);

    let started = start::startup(&s.rig.settings(), s.rig.ports(), own(NEW)).expect("started");
    assert_eq!(
        started.boot_report().self_update,
        Some(Ok(Settled::Confirmed))
    );
    assert_eq!(links(&s), [some(NEW), some(OLD), None]);
    assert!(matches!(
        started.daemon().order(&s.order(NEW, 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            resent: true,
            ..
        }
    ));
}

/// Catches a start that writes, locks or execs with no hand-over waiting: with no state
/// directory none is made; with a wait written the old way (`current` already switched) nothing
/// changes, and the start of the new binary settles it as it always did.
#[test]
fn with_no_hand_over_the_start_writes_nothing_and_takes_no_lock() {
    let s = SelfRig::new("ho-none");
    let exec = FakeExec::new(&s);
    let missing = s.rig.scratch.path().join("no-state");
    assert_eq!(
        hand_over(&missing, OLD, &exec).expect("handed"),
        Handed::NotPending
    );
    assert!(!missing.exists());

    // The old updater switched `current` first and wrote a wait with no `next`; a link `next`
    // stands beside it.
    assert_eq!(s.daemon().order(&s.order(NEW, 1)), awaiting(false));
    s.layout().switch(NEW).expect("the old updater switched");
    let store = hostkeeper_steps::RecordStore::new(&s.hk_state());
    let mut rec = s.rig.record(HK).expect("the record");
    let wait = rec.progress.awaiting_restart.as_mut().expect("the wait");
    (wait.next, wait.starts) = (None, None);
    store.save(&rec).expect("the old-way record");
    let before = snapshot(&s.rig.state);
    assert_eq!(
        at_daemon_start(&s.rig.state, OLD, &exec),
        Handed::NotPending
    );
    assert_eq!(exec.calls(), []);
    assert_eq!(snapshot(&s.rig.state), before);
    assert!(!s.rig.state.join(LOCK_FILE).exists());
    assert_eq!(
        s.daemon_at(NEW).settle_self_update().expect("settled"),
        Settled::Confirmed
    );
}

/// Catches a start that writes under a daemon that runs: while another holds the state
/// directory's lock, the hand-over is left as it is and nothing is exec'd.
#[test]
fn a_daemon_holding_the_state_directory_leaves_the_hand_over_alone() {
    let s = handed_over("ho-busy", 0);
    let exec = FakeExec::new(&s);
    let _held = lock(&s.rig.state).expect("the lock");
    let before = snapshot(&s.rig.state);
    assert_eq!(
        hand_over(&s.rig.state, OLD, &exec).expect("handed"),
        Handed::Busy
    );
    assert_eq!(exec.calls(), []);
    assert_eq!(snapshot(&s.rig.state), before);
}

/// Catches a start stopped by a hand-over it cannot read or lock: a lock file that is a link
/// (refused, never followed) and a record that does not parse are each an error of
/// [`hand_over`], and [`at_daemon_start`] goes on as with nothing to hand over; neither writes.
#[test]
fn a_hand_over_that_cannot_be_locked_or_read_is_left_to_the_start() {
    let s = handed_over("ho-no-lock", 0);
    let exec = FakeExec::new(&s);
    symlink(s.rig.scratch.path(), s.rig.state.join(LOCK_FILE)).expect("a linked lock");
    let before = snapshot(&s.rig.state);
    let error = hand_over(&s.rig.state, OLD, &exec).expect_err("the lock is a link");
    assert!(
        matches!(error, HandOverError::Lock(StartError::Io { .. })),
        "{error:?}"
    );
    assert_eq!(
        at_daemon_start(&s.rig.state, OLD, &exec),
        Handed::NotPending
    );
    assert_eq!(exec.calls(), []);
    assert_eq!(snapshot(&s.rig.state), before);

    let s = handed_over("ho-garbage", 0);
    let exec = FakeExec::new(&s);
    fs::write(s.hk_state().join(RECORD_FILE), "garbage").expect("garbage");
    let error = hand_over(&s.rig.state, OLD, &exec).expect_err("the record is garbage");
    assert!(
        matches!(error, HandOverError::SelfUpdate(SelfUpdateError::Record(_))),
        "{error:?}"
    );
    assert_eq!(
        at_daemon_start(&s.rig.state, OLD, &exec),
        Handed::NotPending
    );
    assert_eq!(exec.calls(), []);
    assert_eq!(
        fs::read_to_string(s.hk_state().join(RECORD_FILE)).expect("the record"),
        "garbage"
    );
}

/// Catches the host's exec reporting success it did not have: a binary that is not there comes
/// back as the error `execve` gave, `NotFound`.
#[test]
fn the_process_exec_returns_the_error_of_a_binary_it_cannot_run() {
    let scratch = hostkeeper_testkit::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "ho")
        .expect("a scratch dir");
    let exec = ProcessExec::new(vec!["daemon".into()]);
    let error = exec.exec(&scratch.path().join("missing"));
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
}
