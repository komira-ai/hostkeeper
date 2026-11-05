//! The `forced` flag and the `force` journal are additive fields of step record schema 1
//! (AGENTS.md B8): a record without them still loads, a record with them round-trips, a reader
//! that knows only the earlier fields reads it, and the on-disk strings of the journal are
//! fixed by a golden file.

mod support;

use std::fs;
use std::path::PathBuf;

use hostkeeper_hook::Step;
use hostkeeper_steps::record::{HookSel, Loaded, Progress, RECORD_FILE, Record, State};
use hostkeeper_steps::{ForceFailure, ForcePhase, ForceRecord, ForceVerdict};
use serde::Deserialize;
use support::Scratch;

const BEFORE: &str = include_str!("fixtures/before-order-try/finished.json");
const STRINGS: &str = include_str!("fixtures/force/strings.json");

/// A step record as the fields of schema 1 stood before `forced` and `force`, read with the
/// same rules: what a daemon of the previous release declares.
#[derive(Debug, Deserialize)]
#[allow(
    dead_code,
    reason = "the fields are read to prove that they read, and not used"
)]
struct EarlierRecord {
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
    progress: Progress,
    #[serde(default)]
    order_try: Option<u64>,
}

fn load(scratch: &Scratch) -> Record {
    match hostkeeper_steps::RecordStore::new(scratch.path())
        .load()
        .expect("the record loads")
    {
        Loaded::Current(rec) => *rec,
        other => panic!("not a current record: {other:?}"),
    }
}

fn journal(verdict: Option<ForceVerdict>) -> ForceRecord {
    ForceRecord {
        target_version: "2.0.0".to_owned(),
        target_digest: "ab".repeat(32),
        from_version: Some("1.0.0".to_owned()),
        phase: ForcePhase::Verdicted,
        order_try: 4,
        halt_generation: 3,
        order_id: Some("force-1".to_owned()),
        verdict,
    }
}

/// Catches `forced` or `force` made required, written when unset, or renamed on disk, and a
/// reader of the earlier fields that refuses a record with them: a record written before they
/// existed loads with neither, and is written back without them byte for byte; a record with
/// both round-trips; the earlier reader reads it; and every phase and verdict is spelt as the
/// golden file has it.
#[test]
fn a_force_record_is_additive_to_schema_1() {
    let scratch = Scratch::new("force-record");
    fs::write(scratch.path().join(RECORD_FILE), BEFORE).expect("the earlier record");
    let earlier = load(&scratch);
    assert!(!earlier.forced);
    assert_eq!(earlier.force, None);
    let written = serde_json::to_value(&earlier).expect("JSON");
    assert!(written.get("forced").is_none() & written.get("force").is_none());

    let mut rec = earlier.clone();
    rec.forced = true;
    rec.force = Some(journal(Some(ForceVerdict::Failed {
        reason: ForceFailure::StartFailed,
    })));
    let store = hostkeeper_steps::RecordStore::new(scratch.path());
    store.save(&rec).expect("write the record");
    assert_eq!(load(&scratch), rec);
    let text = fs::read_to_string(scratch.path().join(RECORD_FILE)).expect("the record");
    let read: EarlierRecord = serde_json::from_str(&text).expect("the earlier reader reads it");
    assert_eq!(read.target_version, earlier.target_version);

    let strings: serde_json::Value = serde_json::from_str(STRINGS).expect("the golden file");
    assert_eq!(
        serde_json::to_value(ForcePhase::ALL).expect("JSON"),
        strings["phases"]
    );
    for phase in ForcePhase::ALL {
        let text = serde_json::to_string(&phase).expect("JSON");
        assert_eq!(
            serde_json::from_str::<ForcePhase>(&text).expect("back"),
            phase
        );
    }
    let verdicts = [
        ForceVerdict::Succeeded,
        ForceVerdict::Failed {
            reason: ForceFailure::LinkFailed,
        },
        ForceVerdict::Failed {
            reason: ForceFailure::DaemonReloadFailed,
        },
        ForceVerdict::Failed {
            reason: ForceFailure::StartFailed,
        },
    ];
    assert_eq!(
        serde_json::to_value(verdicts).expect("JSON"),
        strings["verdicts"]
    );
    for (verdict, golden) in verdicts
        .iter()
        .zip(strings["verdicts"].as_array().expect("a list"))
    {
        let back: ForceVerdict = serde_json::from_value(golden.clone()).expect("back");
        assert_eq!(back, *verdict);
        if let ForceVerdict::Failed { reason } = verdict {
            assert_eq!(golden["reason"], reason.token());
        }
    }
}

/// Catches a prune after a force that did not move `current`, or none after one that did: a
/// force moves it from another version or from none, and not from its own target.
#[test]
fn a_force_moves_current_unless_it_began_on_its_target() {
    let mut force = journal(None);
    assert!(force.moves_current());
    force.from_version = None;
    assert!(force.moves_current());
    force.from_version = Some("2.0.0".to_owned());
    assert!(!force.moves_current());
}
