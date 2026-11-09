//! The update of hostkeeper itself through its native package (docs/design/packages-native.md,
//! section 10.4, step 5): the new binary's confirmation leaves the `activate` of its own package
//! pending in the step record, the `activate` ends it with its verdict, and the field stays
//! additive.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::record::{
    Activated, AwaitingRestart, HookSel, PendingActivate, REASON_SELF_UPDATE_FALLBACK, RECORD_FILE,
    SCHEMA, State,
};
use hostkeeper_steps::{
    BadVersions, CrashPoint, ForceRecord, Layout, Loaded, Record, RecordStore, ResumeAction,
    Settled, resume_action, settle_activating,
};
use support::Scratch;

/// The step record of hostkeeper's own update to `version` whose steps ran: `done` at `cleanup`.
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
        progress: hostkeeper_steps::record::Progress {
            committed: true,
            ..Default::default()
        },
        order_try: Some(3),
        forced: false,
        force: None,
    }
}

/// `rec` waiting for the restart into `version`, by a hand-over when `next` is set, for the
/// order `order-7` with the promotion `promotion`.
fn waiting(mut rec: Record, version: &str, next: bool, promotion: bool) -> Record {
    rec.progress.awaiting_restart = Some(AwaitingRestart {
        version: version.to_owned(),
        verdict_promotion_pending: promotion,
        order_id: Some("order-7".to_owned()),
        next: next.then(|| version.to_owned()),
        starts: next.then_some(1),
    });
    rec
}

/// The `activate` the record waits for after `version` confirmed.
fn pending(version: &str, promotion: bool) -> PendingActivate {
    PendingActivate {
        version: version.to_owned(),
        verdict_promotion_pending: promotion,
        order_id: Some("order-7".to_owned()),
    }
}

/// A host whose binary directory holds `v1` (`current`) and `v2`, and a state directory.
struct Host {
    s: Scratch,
    layout: Layout,
    state: PathBuf,
}

impl Host {
    fn new(name: &str) -> Host {
        let s = Scratch::new(name);
        let layout = Layout::new(&s.path().join("bin"));
        for version in ["v1", "v2"] {
            let file = s.path().join(format!("binary-{version}"));
            fs::write(&file, version).unwrap();
            layout.stage(version, &file).unwrap();
        }
        layout.switch("v1").unwrap();
        let state = s.path().join("state");
        fs::create_dir_all(&state).unwrap();
        Host { s, layout, state }
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

    fn record_bytes(&self) -> Vec<u8> {
        fs::read(self.state.join(RECORD_FILE)).unwrap()
    }

    fn settle(&self, store: &RecordStore, running: &str, activate: bool) -> Settled {
        settle_activating(&self.layout, store, running, &mut || Ok(()), activate).unwrap()
    }
}

/// The new binary of a hand-over and of a wait written the old way (`current` already naming it)
/// confirms itself with its package still to install: in one write, the wait is replaced by the
/// pending `activate` (version, promotion and order id carried over), the record stays `done` at
/// `cleanup` with no verdict and nothing to report yet, the hand-over's switch is finished and
/// `next` removed; a second start writes nothing more.
#[test]
fn a_confirmation_with_a_package_to_install_leaves_the_activate_pending_and_no_verdict() {
    for (next, promotion) in [(true, false), (true, true), (false, false)] {
        let host = Host::new(&format!("act-confirm-{next}-{promotion}"));
        host.store()
            .save(&waiting(steps_ran("v2"), "v2", next, promotion))
            .unwrap();
        if !next {
            host.layout.switch("v2").unwrap();
        } else {
            host.layout.set_next("v2").unwrap();
        }
        let writes = Arc::new(AtomicU64::new(0));
        let store = host.counted(&writes);
        assert_eq!(host.settle(&store, "v2", true), Settled::Confirmed);
        let mut want = steps_ran("v2");
        want.progress.activate = Some(pending("v2", promotion));
        assert_eq!(host.record(), want, "next={next}");
        assert_eq!(host.layout.current().unwrap().as_deref(), Some("v2"));
        assert_eq!(host.layout.next().unwrap(), None);
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert_eq!(host.settle(&store, "v2", true), Settled::NotPending);
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert!(
            BadVersions::load(&host.state)
                .unwrap()
                .versions()
                .is_empty()
        );
    }
}

/// A start of another binary than the one waited for falls back whether or not the new one would
/// have had a package to install: `Failed` after the commit, `self_update_fallback`, the
/// version bad, and no `activate` pending.
#[test]
fn a_fallback_has_no_activate_to_do() {
    let host = Host::new("act-fallback");
    host.store()
        .save(&waiting(steps_ran("v2"), "v2", true, false))
        .unwrap();
    host.layout.set_next("v2").unwrap();
    assert_eq!(host.settle(&host.store(), "v1", true), Settled::FellBack);
    let rec = host.record();
    assert_eq!(rec.progress.activate, None);
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
    assert_eq!(rec.progress.report_order_id.as_deref(), Some("order-7"));
    assert_eq!(
        BadVersions::load(&host.state).unwrap().versions(),
        ["v2".to_owned()]
    );
}

/// The `activate` ends the record in one write: `Succeeded` with the promotion it kept and its
/// reason, or `Failed` after the commit with the installer's class and reason; either way the
/// order id becomes the one to report the verdict under, and the field is gone, so a second end
/// writes nothing.
#[test]
fn the_activate_ends_the_record_with_its_verdict_under_the_order_id() {
    let cases = [
        (
            Activated::Succeeded { reason: None },
            true,
            Verdict::Succeeded {
                promotion_pending: true,
            },
            None,
        ),
        (
            Activated::Succeeded {
                reason: Some("drift_repaired".to_owned()),
            },
            false,
            Verdict::Succeeded {
                promotion_pending: false,
            },
            Some("drift_repaired"),
        ),
        (
            Activated::Failed {
                class: Class::Release,
                reason: "dpkg_error".to_owned(),
            },
            false,
            Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Release,
            },
            Some("dpkg_error"),
        ),
        (
            Activated::Failed {
                class: Class::Environment,
                reason: "dpkg_interrupted".to_owned(),
            },
            false,
            Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Environment,
            },
            Some("dpkg_interrupted"),
        ),
    ];
    for (n, (ended, promotion, verdict, reason)) in cases.into_iter().enumerate() {
        let host = Host::new(&format!("act-end-{n}"));
        let mut rec = steps_ran("v2");
        rec.progress.activate = Some(pending("v2", promotion));
        host.store().save(&rec).unwrap();
        let writes = Arc::new(AtomicU64::new(0));
        let store = host.counted(&writes);
        assert!(store.settle_activate(ended.clone()).unwrap());
        rec.progress.activate = None;
        rec.progress.report_order_id = Some("order-7".to_owned());
        rec.state = State::Finished {
            verdict,
            reason: reason.map(str::to_owned),
        };
        assert_eq!(host.record(), rec, "{ended:?}");
        assert!(!store.settle_activate(ended).unwrap());
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }
}

/// Nothing is written by an end of an `activate` no record waits for: no record, a record that
/// waits for the restart, a finished one, and one a newer binary wrote.
#[test]
fn an_activate_end_without_a_pending_activate_writes_nothing() {
    let host = Host::new("act-none");
    let ended = || Activated::Succeeded { reason: None };
    assert!(!host.store().settle_activate(ended()).unwrap());
    assert!(!host.state.join(RECORD_FILE).exists());

    let waits = waiting(steps_ran("v2"), "v2", true, false);
    let mut finished = steps_ran("v2");
    finished.state = State::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
    };
    for rec in [waits, finished] {
        host.store().save(&rec).unwrap();
        let before = host.record_bytes();
        assert!(!host.store().settle_activate(ended()).unwrap());
        assert_eq!(host.record_bytes(), before);
    }

    let newer = r#"{"schema": 99, "anything": true}"#;
    fs::write(host.state.join(RECORD_FILE), newer).unwrap();
    assert!(!host.store().settle_activate(ended()).unwrap());
    assert!(!host.store().pend_activate("v2", false, None).unwrap());
    assert_eq!(host.record_bytes(), newer.as_bytes());
}

/// An update or an install of the version that runs, which needs no restart, waits for its
/// package's `activate` in one write (the order id kept, a stale wait for the restart cleared),
/// and a second call writes nothing; a record of another version, a finished one, or none is
/// left as it is.
#[test]
fn the_version_that_runs_pends_its_activate_once() {
    let host = Host::new("act-pend");
    assert!(!host.store().pend_activate("v2", false, Some("o")).unwrap());

    let mut rec = waiting(steps_ran("v2"), "v2", false, false);
    host.store().save(&rec).unwrap();
    let writes = Arc::new(AtomicU64::new(0));
    let store = host.counted(&writes);
    assert!(store.pend_activate("v2", true, Some("order-7")).unwrap());
    assert!(store.pend_activate("v2", true, Some("order-7")).unwrap());
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    rec.progress.awaiting_restart = None;
    rec.progress.activate = Some(pending("v2", true));
    assert_eq!(host.record(), rec);

    let other = steps_ran("v3");
    host.store().save(&other).unwrap();
    assert!(!host.store().pend_activate("v2", false, None).unwrap());
    assert_eq!(host.record(), other);

    let mut finished = steps_ran("v2");
    finished.state = State::Finished {
        verdict: Verdict::Halt,
        reason: None,
    };
    host.store().save(&finished).unwrap();
    assert!(!host.store().pend_activate("v2", false, None).unwrap());
    assert_eq!(host.record(), finished);

    let order_less = steps_ran("v2");
    host.store().save(&order_less).unwrap();
    assert!(host.store().pend_activate("v2", false, None).unwrap());
    assert_eq!(
        host.record().progress.activate,
        Some(PendingActivate {
            version: "v2".to_owned(),
            verdict_promotion_pending: false,
            order_id: None,
        })
    );
    drop(host.s);
}

/// The boot pass never resumes an update whose `activate` waits: it has run every step, and the
/// start settles it, whatever the boot.
#[test]
fn the_boot_pass_resumes_no_update_whose_activate_waits() {
    let mut rec = steps_ran("v2");
    rec.progress.activate = Some(pending("v2", false));
    for rebooted in [false, true] {
        assert_eq!(resume_action(&rec, rebooted), ResumeAction::NothingFinished);
    }
}

/// The step record as the release before `activate` had it: what a binary of that release reads.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct PreviousRecord {
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
    progress: PreviousProgress,
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

/// `Progress` as the release before `activate` had it.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct PreviousProgress {
    committed: bool,
    promotion_pending: bool,
    then: Option<Verdict>,
    retry: u32,
    verify_started: Option<hostkeeper_steps::Stamp>,
    watching: Option<hostkeeper_steps::Stamp>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    awaiting_restart: Option<AwaitingRestart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report_order_id: Option<String>,
}

fn fixture() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/activate-pending/record.json");
    fs::read_to_string(path).unwrap()
}

/// The B8 test of the field in both directions: the committed record of an update whose
/// `activate` waits loads in the shape of the release before the field, unfinished (`done`), with
/// no wait for a restart and no verdict to report, so that binary reports nothing of it; it loads
/// in the current shape with the field and is written back byte for byte as JSON; a record of the
/// previous shape loads in the current one without the field and is written back without it.
#[test]
fn a_record_whose_activate_waits_loads_in_the_previous_release_shape() {
    let text = fixture();
    let previous: PreviousRecord = serde_json::from_str(&text).unwrap();
    assert!(
        matches!(previous.state, State::Done { code: Some(0), .. }),
        "{previous:?}"
    );
    assert_eq!(previous.progress.awaiting_restart, None);
    assert_eq!(previous.progress.report_order_id, None);

    let s = Scratch::new("act-previous-shape");
    fs::write(s.path().join(RECORD_FILE), &text).unwrap();
    let Loaded::Current(rec) = RecordStore::new(s.path()).load().unwrap() else {
        panic!("the fixture did not load");
    };
    assert_eq!(rec.progress.activate, Some(pending("1.6.0", false)));
    let written = serde_json::to_value(&*rec).unwrap();
    let committed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(written, committed);

    let older = serde_json::to_string(&previous).unwrap();
    assert!(!older.contains("activate"));
    let rec: Record = serde_json::from_str(&older).unwrap();
    assert_eq!(rec.progress.activate, None);
    assert_eq!(
        serde_json::to_value(&rec).unwrap(),
        serde_json::from_str::<serde_json::Value>(&older).unwrap()
    );
}
