//! What a binary's start reads of a hand-over (docs/design/two-programs.md, section 4.3): the
//! binary `current` names execs `next` while the start count lasts and settles the update failed
//! once it cannot, the binary the hand-over waits for finishes the switch and confirms, and a
//! wait written the old way is settled as before. Every case runs on links and records in a
//! scratch directory; nothing is exec'd.

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_steps::handover::{AtStart, HAND_OVER_TRIES, NEXT, at_start, exec_failed, pending};
use hostkeeper_steps::record::{AwaitingRestart, REASON_SELF_UPDATE_FALLBACK, RECORD_FILE, State};
use hostkeeper_steps::{
    BAD_VERSIONS_FILE, BadVersions, CrashPoint, Layout, Loaded, Record, RecordError, RecordStore,
    SelfUpdateError, Settled, settle, settle_with,
};
use support::Scratch;

/// The version before the one that runs.
const BEFORE: &str = "1.3.0";
/// The version `current` names: the old binary.
const OLD: &str = "1.4.0";
/// The version the hand-over waits for: the new binary, `next`.
const NEW: &str = "1.5.0";

fn fixture(dir: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/{dir}/record.json",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(path).unwrap()
}

/// A host whose binary directory has [`BEFORE`] as `previous`, [`OLD`] as `current` and [`NEW`]
/// staged with `next` naming it, and whose record waits for a hand-over into [`NEW`] as a release
/// that hands over writes it (`fixtures/hand-over/record.json`, a start count of 0).
struct Host {
    scratch: Scratch,
}

impl Host {
    fn new(name: &str) -> Host {
        let host = Host {
            scratch: Scratch::new(name),
        };
        let layout = host.layout();
        fs::create_dir_all(layout.dir()).unwrap();
        for version in [BEFORE, OLD, NEW] {
            let file = host.scratch.path().join(format!("binary-{version}"));
            fs::write(&file, format!("#!hostkeeper {version}\n")).unwrap();
            layout.stage(version, &file).unwrap();
        }
        layout.switch(BEFORE).unwrap();
        layout.switch(OLD).unwrap();
        symlink(Path::new("versions").join(NEW), layout.next_path()).unwrap();
        fs::write(host.dir().join(RECORD_FILE), fixture("hand-over")).unwrap();
        host
    }

    /// The state directory of the service: the record and the list of bad versions.
    fn dir(&self) -> &Path {
        self.scratch.path()
    }

    fn layout(&self) -> Layout {
        Layout::new(&self.dir().join("bin"))
    }

    fn store(&self) -> RecordStore {
        RecordStore::new(self.dir())
    }

    fn record(&self) -> Record {
        match self.store().load().unwrap() {
            Loaded::Current(rec) => *rec,
            other => panic!("no record: {other:?}"),
        }
    }

    fn record_bytes(&self) -> Vec<u8> {
        fs::read(self.dir().join(RECORD_FILE)).unwrap()
    }

    /// Rewrites the wait with `edit` applied.
    fn edit_wait(&self, edit: impl FnOnce(&mut AwaitingRestart)) {
        let mut rec = self.record();
        edit(rec.progress.awaiting_restart.as_mut().unwrap());
        self.store().save(&rec).unwrap();
    }

    fn with_starts(&self, starts: Option<u32>) {
        self.edit_wait(|wait| wait.starts = starts);
    }

    fn links(&self) -> [Option<String>; 3] {
        let layout = self.layout();
        [
            layout.current().unwrap(),
            layout.previous().unwrap(),
            layout.next().unwrap(),
        ]
    }

    fn bad(&self) -> Vec<String> {
        BadVersions::load(self.dir()).unwrap().versions().to_vec()
    }

    fn new_binary(&self) -> PathBuf {
        self.layout().version_path(NEW)
    }
}

fn some(version: &str) -> Option<String> {
    Some(version.to_owned())
}

/// The record of a hand-over step 3 settled: `Failed` after the commit, of class release,
/// `self_update_fallback`, no wait left, and the order id kept for the verdict.
fn assert_fell_back(host: &Host) {
    let rec = host.record();
    assert_eq!(
        rec.state,
        State::Finished {
            verdict: Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Release,
            },
            reason: Some(REASON_SELF_UPDATE_FALLBACK.to_owned()),
        }
    );
    assert_eq!(rec.progress.awaiting_restart, None);
    assert_eq!(
        rec.progress.report_order_id.as_deref(),
        Some("order-hostkeeper-1.5.0")
    );
    assert_eq!(host.bad(), [NEW]);
    assert_eq!(host.links(), [some(OLD), some(BEFORE), None]);
}

/// The record of a hand-over the new binary confirmed: `Succeeded` with the promotion the wait
/// kept, no wait left, the order id kept for the verdict.
fn assert_confirmed(host: &Host) {
    let rec = host.record();
    assert_eq!(
        rec.state,
        State::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: true,
            },
            reason: None,
        }
    );
    assert_eq!(rec.progress.awaiting_restart, None);
    assert_eq!(
        rec.progress.report_order_id.as_deref(),
        Some("order-hostkeeper-1.5.0")
    );
    assert!(host.bad().is_empty());
}

/// Catches an old binary that does not hand over, hands over past its count, or execs before the
/// count is durable: with a hand-over waiting and a count of none, 0, 1 or 2, the start of the
/// old binary raises the count by one in the record and tells the caller to exec
/// `versions/1.5.0`; nothing else of the record, no link and no list changes.
#[test]
fn the_old_binary_with_a_hand_over_waiting_and_a_count_of_0_1_or_2_execs_next_and_raises_the_count()
{
    for (starts, raised) in [(None, 1), (Some(0), 1), (Some(1), 2), (Some(2), 3)] {
        let host = Host::new("ho-raise");
        host.with_starts(starts);
        let before = host.record();
        let at = at_start(&host.layout(), &host.store(), OLD).unwrap();
        assert_eq!(
            at,
            AtStart::Exec {
                binary: host.new_binary(),
                starts: raised,
            },
            "a count of {starts:?}"
        );
        let mut expected = before.clone();
        expected.progress.awaiting_restart.as_mut().unwrap().starts = Some(raised);
        assert_eq!(host.record(), expected, "a count of {starts:?}");
        assert_eq!(host.links(), [some(OLD), some(BEFORE), some(NEW)]);
        assert!(!host.dir().join(BAD_VERSIONS_FILE).exists());
    }
}

/// Catches a hand-over that tries a fourth start or leaves the wait standing: with the count at
/// 3 the old binary execs nothing, puts 1.5.0 on the list of bad versions, settles the update
/// `Failed` after the commit, `self_update_fallback`, keeping its order id, and removes `next`;
/// `current` and `previous` stay where they were. A second start finds nothing waiting.
#[test]
fn with_a_count_of_3_the_old_binary_settles_the_update_failed_and_runs() {
    let host = Host::new("ho-spent");
    host.with_starts(Some(HAND_OVER_TRIES));
    let at = at_start(&host.layout(), &host.store(), OLD).unwrap();
    assert_eq!(at, AtStart::FellBack);
    assert_fell_back(&host);
    let written = host.record_bytes();
    let again = at_start(&host.layout(), &host.store(), OLD).unwrap();
    assert_eq!(again, AtStart::NotPending);
    assert_eq!(host.record_bytes(), written);
}

/// Catches an old binary that execs what it cannot start or what the hand-over did not name:
/// each of these settles the update `Failed` at once, with the count below its limit and no
/// exec: `next` missing; `versions/1.5.0` not executable; `versions/1.5.0` gone, `next`
/// dangling; `next` a file and not a link; `next` naming another version than the wait; the
/// wait's own `next` naming another version than it waits for.
#[test]
fn next_missing_not_executable_or_not_the_awaited_version_settles_failed_at_once() {
    type Break = fn(&Host);
    let cases: [(&str, Break); 6] = [
        ("next missing", |host| {
            fs::remove_file(host.layout().next_path()).unwrap();
        }),
        ("next not executable", |host| {
            fs::set_permissions(host.new_binary(), fs::Permissions::from_mode(0o644)).unwrap();
        }),
        ("next dangling", |host| {
            fs::remove_file(host.new_binary()).unwrap();
        }),
        ("next a file", |host| {
            let next = host.layout().next_path();
            fs::remove_file(&next).unwrap();
            fs::write(&next, "not a link").unwrap();
        }),
        ("next naming another version", |host| {
            let next = host.layout().next_path();
            fs::remove_file(&next).unwrap();
            symlink(Path::new("versions").join(BEFORE), &next).unwrap();
        }),
        ("the wait naming another next", |host| {
            host.edit_wait(|wait| wait.next = some(BEFORE));
        }),
    ];
    for (name, break_it) in cases {
        let host = Host::new("ho-cannot");
        break_it(&host);
        let at = at_start(&host.layout(), &host.store(), OLD).unwrap();
        assert_eq!(at, AtStart::FellBack, "{name}");
        let rec = host.record();
        assert_eq!(rec.progress.awaiting_restart, None, "{name}");
        assert_eq!(host.bad(), [NEW], "{name}");
        assert!(
            fs::symlink_metadata(host.layout().next_path()).is_err(),
            "{name}: next is removed"
        );
        assert_eq!(host.layout().current().unwrap(), some(OLD), "{name}");
    }
}

/// Catches an exec that returns being retried or left waiting: after step 1 raised the count, an
/// exec that came back settles the update `Failed` at once, as step 3 does; called again with
/// nothing waiting it writes nothing.
#[test]
fn an_exec_that_returns_settles_the_update_failed_at_once() {
    let host = Host::new("ho-exec");
    let at = at_start(&host.layout(), &host.store(), OLD).unwrap();
    assert!(matches!(at, AtStart::Exec { starts: 1, .. }), "{at:?}");
    assert_eq!(
        exec_failed(&host.layout(), &host.store()).unwrap(),
        Settled::FellBack
    );
    assert_fell_back(&host);
    let written = host.record_bytes();
    assert_eq!(
        exec_failed(&host.layout(), &host.store()).unwrap(),
        Settled::NotPending
    );
    assert_eq!(host.record_bytes(), written);
}

/// Catches the new binary execing again, confirming before `current` names it, or removing
/// `next` before the confirmation: started with `current` still the old binary, the new one's
/// start writes nothing, and its settlement points `previous` at the old `current` and `current`
/// at itself, runs the work before the confirmation with the switch done and the wait and `next`
/// still there, confirms `Succeeded`, and only then removes `next`.
#[test]
fn the_new_binary_started_with_current_still_old_finishes_the_switch_and_confirms() {
    let host = Host::new("ho-new");
    let written = host.record_bytes();
    let at = at_start(&host.layout(), &host.store(), NEW).unwrap();
    assert_eq!(at, AtStart::Settles);
    assert_eq!(host.record_bytes(), written);

    let mut seen = Vec::new();
    let layout = host.layout();
    let store = host.store();
    let settled = settle_with(&layout, &store, NEW, &mut || {
        let waits = pending(&host.store())?.is_some();
        seen.push((host.links(), waits));
        Ok(())
    })
    .unwrap();
    assert_eq!(settled, Settled::Confirmed);
    assert_eq!(seen, [([some(NEW), some(OLD), some(NEW)], true)]);
    assert_confirmed(&host);
    assert_eq!(host.links(), [some(NEW), some(OLD), None]);
    assert_eq!(settle(&layout, &store, NEW).unwrap(), Settled::NotPending);
}

/// Catches a confirmation that switches twice and loses the old binary: the new binary started
/// with `current` already its own and `previous` the old one (power lost after the switch and
/// before the confirmation) execs nothing and confirms, leaving `previous` at the old binary.
#[test]
fn the_new_binary_started_after_the_switch_and_before_the_confirm_confirms_with_no_exec() {
    let host = Host::new("ho-switched");
    host.layout().switch(NEW).unwrap();
    assert_eq!(host.links(), [some(NEW), some(OLD), some(NEW)]);
    let at = at_start(&host.layout(), &host.store(), NEW).unwrap();
    assert_eq!(at, AtStart::Settles);
    let settled = settle(&host.layout(), &host.store(), NEW).unwrap();
    assert_eq!(settled, Settled::Confirmed);
    assert_confirmed(&host);
    assert_eq!(host.links(), [some(NEW), some(OLD), None]);
}

/// Catches a settlement that leaves a hand-over standing when the start's own step did not run:
/// a settlement by a binary that is not the one waited for settles it as step 3 does, `Failed`,
/// the version bad and `next` removed, with `current` unchanged.
#[test]
fn a_settlement_by_another_binary_settles_the_hand_over_failed() {
    let host = Host::new("ho-settle-old");
    let settled = settle(&host.layout(), &host.store(), OLD).unwrap();
    assert_eq!(settled, Settled::FellBack);
    assert_fell_back(&host);
}

/// Catches a `next` left by a hand-over that crashed between the link and the wait, or a
/// confirmation that crashed before the removal, being kept, and a `next` beside a record of a
/// newer binary being touched: a settlement with no wait (a finished record, or none) removes
/// `next` and writes nothing else; with a newer record it removes nothing; the start execs
/// nothing in any of them.
#[test]
fn a_link_next_with_no_wait_is_removed_and_one_beside_a_newer_record_is_kept() {
    let host = Host::new("ho-stale");
    let mut rec = host.record();
    rec.progress.awaiting_restart = None;
    host.store().save(&rec).unwrap();
    let written = host.record_bytes();
    assert_eq!(
        at_start(&host.layout(), &host.store(), OLD).unwrap(),
        AtStart::NotPending
    );
    assert_eq!(
        settle(&host.layout(), &host.store(), OLD).unwrap(),
        Settled::NotPending
    );
    assert_eq!(host.links(), [some(OLD), some(BEFORE), None]);
    assert_eq!(host.record_bytes(), written);

    let host = Host::new("ho-stale-absent");
    fs::remove_file(host.dir().join(RECORD_FILE)).unwrap();
    assert_eq!(
        at_start(&host.layout(), &host.store(), OLD).unwrap(),
        AtStart::NotPending
    );
    assert_eq!(
        settle(&host.layout(), &host.store(), OLD).unwrap(),
        Settled::NotPending
    );
    assert_eq!(host.links(), [some(OLD), some(BEFORE), None]);

    let host = Host::new("ho-stale-newer");
    let newer = r#"{"schema": 2, "anything": true}"#;
    fs::write(host.dir().join(RECORD_FILE), newer).unwrap();
    assert_eq!(
        at_start(&host.layout(), &host.store(), OLD).unwrap(),
        AtStart::NotPending
    );
    assert_eq!(pending(&host.store()).unwrap(), None);
    assert_eq!(
        settle(&host.layout(), &host.store(), OLD).unwrap(),
        Settled::NotPending
    );
    assert_eq!(host.links(), [some(OLD), some(BEFORE), some(NEW)]);
    assert_eq!(host.record_bytes(), newer.as_bytes());
}

/// Catches a wait written the old way being read as a hand-over: a record that waits with no
/// `next` is no hand-over at the start of either binary (nothing written, nothing exec'd, even
/// with a link `next` there), and its settlement is today's: the new binary that `current`
/// names confirms it and moves no link, any other binary records it fallen back and moves no
/// link either.
#[test]
fn an_old_way_record_is_left_to_the_settlement_which_settles_it_as_before() {
    for (running, current, settled) in [
        (NEW, NEW, Settled::Confirmed),
        (OLD, OLD, Settled::FellBack),
        (NEW, OLD, Settled::FellBack),
    ] {
        let host = Host::new("ho-old-way");
        fs::write(host.dir().join(RECORD_FILE), fixture("awaiting-restart")).unwrap();
        if current == NEW {
            host.layout().switch(NEW).unwrap();
        }
        let links = host.links();
        let written = host.record_bytes();
        assert_eq!(pending(&host.store()).unwrap(), None);
        assert_eq!(
            at_start(&host.layout(), &host.store(), running).unwrap(),
            AtStart::NotPending
        );
        assert_eq!(host.record_bytes(), written);
        assert_eq!(
            settle(&host.layout(), &host.store(), running).unwrap(),
            settled,
            "{running} with current {current}"
        );
        assert_eq!(host.links(), links, "the old way moves no link");
        let rec = host.record();
        assert_eq!(rec.progress.awaiting_restart, None);
        assert!(matches!(rec.state, State::Finished { .. }), "{rec:?}");
    }
}

/// Catches a reader that drops or misreads the two fields before any release writes them: the
/// record a release that hands over writes (`fixtures/hand-over/record.json`) loads with `next`
/// and `starts`, is written back with both and every other key as it was, and settles as the old
/// binary (exec at a count of 1, written as `"starts": 1`) and as the new one (confirmed); the
/// old-way record (`fixtures/awaiting-restart/record.json`) loads with neither and is written
/// back without them.
#[test]
fn a_reader_of_today_settles_a_hand_over_record_and_an_old_way_record() {
    let host = Host::new("ho-stage2");
    let wait = host.record().progress.awaiting_restart.unwrap();
    assert_eq!(
        wait,
        AwaitingRestart {
            version: NEW.to_owned(),
            verdict_promotion_pending: true,
            order_id: some("order-hostkeeper-1.5.0"),
            next: some(NEW),
            starts: Some(0),
        }
    );
    let committed: serde_json::Value = serde_json::from_str(&fixture("hand-over")).unwrap();
    assert_eq!(serde_json::to_value(host.record()).unwrap(), committed);

    let at = at_start(&host.layout(), &host.store(), OLD).unwrap();
    assert!(matches!(at, AtStart::Exec { starts: 1, .. }), "{at:?}");
    let mut raised = committed.clone();
    raised["progress"]["awaiting_restart"]["starts"] = serde_json::Value::from(1);
    let written: serde_json::Value = serde_json::from_slice(&host.record_bytes()).unwrap();
    assert_eq!(written, raised);
    assert_eq!(
        settle(&host.layout(), &host.store(), NEW).unwrap(),
        Settled::Confirmed
    );
    assert_confirmed(&host);

    let old_way = fixture("awaiting-restart");
    let host = Host::new("ho-old-fixture");
    fs::write(host.dir().join(RECORD_FILE), &old_way).unwrap();
    let wait = host.record().progress.awaiting_restart.unwrap();
    assert_eq!((wait.next, wait.starts), (None, None));
    let text = serde_json::to_string(&host.record()).unwrap();
    assert!(
        !text.contains("\"next\"") & !text.contains("\"starts\""),
        "{text}"
    );
    host.layout().switch(NEW).unwrap();
    assert_eq!(
        settle(&host.layout(), &host.store(), NEW).unwrap(),
        Settled::Confirmed
    );
}

/// Catches a count raised in memory only, or a crash that spends a try: power lost before the
/// count is written leaves the record as it was, and the next start makes the same try again,
/// with the count 1.
#[test]
fn a_crash_before_the_count_is_written_makes_the_same_try_again() {
    let host = Host::new("ho-crash-count");
    let written = host.record_bytes();
    let gated = host
        .store()
        .with_crash_gate(Arc::new(|point| point == CrashPoint::BeforeWrite(1)));
    let crashed = at_start(&host.layout(), &gated, OLD).unwrap_err();
    assert!(
        matches!(
            crashed,
            SelfUpdateError::Record(RecordError::Crashed(CrashPoint::BeforeWrite(1)))
        ),
        "{crashed:?}"
    );
    assert_eq!(host.record_bytes(), written);
    let at = at_start(&host.layout(), &host.store(), OLD).unwrap();
    assert!(matches!(at, AtStart::Exec { starts: 1, .. }), "{at:?}");
}

/// Catches a confirmation a crash can lose or one that moves the links twice: the work before
/// the confirmation failing leaves the switch done and the wait standing, and the next start of
/// the new binary confirms with no second switch; a crash after the confirmation is written and
/// before `next` is removed leaves `next`, which the next start removes.
#[test]
fn a_crash_in_the_settlement_is_finished_by_the_next_start() {
    let host = Host::new("ho-crash-confirm");
    let failed = settle_with(&host.layout(), &host.store(), NEW, &mut || {
        Err(SelfUpdateError::Io(std::io::Error::other("stopped")))
    });
    assert!(failed.is_err());
    assert!(pending(&host.store()).unwrap().is_some());
    assert_eq!(host.links(), [some(NEW), some(OLD), some(NEW)]);
    assert_eq!(
        settle(&host.layout(), &host.store(), NEW).unwrap(),
        Settled::Confirmed
    );
    assert_eq!(host.links(), [some(NEW), some(OLD), None]);

    let host = Host::new("ho-crash-remove");
    let gated = host
        .store()
        .with_crash_gate(Arc::new(|point| point == CrashPoint::AfterWrite(1)));
    assert!(settle(&host.layout(), &gated, NEW).is_err());
    assert_confirmed(&host);
    assert_eq!(host.links(), [some(NEW), some(OLD), some(NEW)]);
    assert_eq!(
        settle(&host.layout(), &host.store(), NEW).unwrap(),
        Settled::NotPending
    );
    assert_eq!(host.links(), [some(NEW), some(OLD), None]);
}

/// Catches a step 3 that loses the bad mark or writes it twice: power lost after the list is
/// written and before the record leaves 1.5.0 bad and the wait standing, and the next start
/// settles it with the version on the list once.
#[test]
fn a_crash_between_the_bad_mark_and_the_record_settles_at_the_next_start() {
    let host = Host::new("ho-crash-bad");
    host.with_starts(Some(HAND_OVER_TRIES));
    let gated = host
        .store()
        .with_crash_gate(Arc::new(|point| point == CrashPoint::BeforeWrite(1)));
    assert!(at_start(&host.layout(), &gated, OLD).is_err());
    assert_eq!(host.bad(), [NEW]);
    assert!(pending(&host.store()).unwrap().is_some());
    assert_eq!(
        at_start(&host.layout(), &host.store(), OLD).unwrap(),
        AtStart::FellBack
    );
    assert_fell_back(&host);
}

/// Catches a `next` that cannot be removed being passed over in silence: a settlement with no
/// wait whose `next` is a directory with something in it fails, naming the cause, and changes
/// nothing else.
#[test]
fn a_next_that_cannot_be_removed_fails_the_settlement() {
    let host = Host::new("ho-next-dir");
    fs::remove_file(host.dir().join(RECORD_FILE)).unwrap();
    let next = host.layout().next_path();
    fs::remove_file(&next).unwrap();
    fs::create_dir(&next).unwrap();
    fs::write(next.join("inside"), "x").unwrap();
    let error = settle(&host.layout(), &host.store(), OLD).unwrap_err();
    assert!(matches!(error, SelfUpdateError::Io(_)), "{error:?}");
    assert!(next.join("inside").exists());
    assert_eq!(next.file_name().and_then(|n| n.to_str()), Some(NEXT));
}
