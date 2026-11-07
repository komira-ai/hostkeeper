//! What a halt does to the coordinator's state (design sections 5.4, 8.2 and 11 item 10): a
//! halt whose write fails stands in memory, holds every order, is written again at every
//! heartbeat until it lands, and leaves a marker that stops a start; who raised a halt says
//! whether CI's next version lifts it; the generation goes only to an agent that announced
//! `halt-v1`. Every time is explicit; nothing sleeps.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use hostkeeper_wire::wire::{Heartbeat, ServiceReply, UpdateResult};
use serde_json::{Value, json};

use crate::config::{StartError, refuse_unfinished_halt, settle_unfinished};
use crate::halt::{Lifted, UPDATER_HALT_REASONS, lift_by_ci, origin_of};
use crate::ops::{HaltOrigin, OpsError, OpsState, OpsStore, ServiceOps};
use crate::ops_file::unfinished_path;
use crate::rollback_tests::{TWO, rig, services_of, status, wanted, who};
use crate::service::{FEATURES, GATED_FIELDS, SetError, sends};
use crate::testutil::{LEGACY, Log, OPERATOR, Rig, beat, ci_token, scratch, version};

/// The update to 2.0 that halted for `reason`.
fn halted_update(reason: &str) -> UpdateResult {
    serde_json::from_value(json!({
        "target": "2.0", "outcome": "failed", "phase": "halt", "reason": reason
    }))
    .expect("a report")
}

/// A daemon at 1.0 whose update to 2.0 halted for `reason`.
fn halting(reason: &str) -> Heartbeat {
    let mut b = beat("1.0");
    b.last_update = Some(halted_update(reason));
    b
}

/// The reply to `host`'s heartbeat `b` at `at`.
fn ask(rig: &Rig, host: &str, b: Heartbeat, at: u64) -> ServiceReply {
    rig.at(at);
    rig.coordinator
        .heartbeat(who(host), b)
        .expect("a heartbeat")
        .reply
}

/// An ops store over the rig's ops file whose writes fail while `failing` is set, counting
/// them in `writes`.
fn flaky(path: &Path, failing: &Arc<AtomicBool>, writes: &Arc<AtomicUsize>) -> OpsStore {
    let (failing, writes, file) = (Arc::clone(failing), Arc::clone(writes), path.to_owned());
    OpsStore::open(path)
        .expect("an ops store")
        .with_write(Box::new(move |state| {
            writes.fetch_add(1, Ordering::SeqCst);
            if failing.load(Ordering::SeqCst) {
                return Err(OpsError::Invalid {
                    path: file.clone(),
                    why: "the disk is full".to_owned(),
                });
            }
            crate::ops_file::write(&file, state)
        }))
}

/// A rig of six daemons with 2.0 desired whose ops store is `ops`.
async fn rig_with(test: &str, ops: impl FnOnce(&Path) -> OpsStore) -> (Rig, PathBuf) {
    let mut rig = rig(test, "", TWO).await;
    let path = rig.dir.path().join("desired.json.ops");
    rig.coordinator = Rig::start(&rig.dir, &rig.clock)
        .with_services(services_of(""))
        .with_ops(ops(&path));
    (rig, path)
}

/// Catches: a halt whose failed write is dropped from memory or not retried, a retry that logs
/// on every beat, or a marker that is ignored: the halt stands in memory and holds every order;
/// each heartbeat writes it again, a failure logged at most once a minute; while it is not
/// written its marker, listing the halt's audit number, stops a start against the file that does
/// not hold it; once a write lands the marker is gone, the file holds the halt and a start goes
/// on.
#[tokio::test]
async fn an_unwritten_halt_is_retried_and_blocks_start() {
    let failing = Arc::new(AtomicBool::new(true));
    let writes = Arc::new(AtomicUsize::new(0));
    let (rig, path) = rig_with("halt-unwritten", |path| flaky(path, &failing, &writes)).await;
    let marker = unfinished_path(&path);
    let log = Log::default();
    let installed = log.install();

    assert_eq!(
        ask(&rig, "d1", halting("disk_tainted"), 2_000),
        ServiceReply::Nothing
    );
    assert!(rig.coordinator.ops().halt_unwritten());
    assert!(
        marker.exists(),
        "the marker is made before the write is tried"
    );
    assert!(
        OpsStore::open(&path)
            .expect("the file")
            .marks(LEGACY)
            .halt
            .is_none(),
        "not in the file"
    );
    let halt_seq = rig
        .coordinator
        .ops()
        .state()
        .audit
        .last()
        .expect("the halt")
        .seq;
    let listed = refuse_unfinished_halt(&path).expect("a marker this build reads");
    assert_eq!(listed, Some(vec![halt_seq]));
    let on_disk = OpsStore::open(&path).expect("the file").state();
    assert!(matches!(
        settle_unfinished(&path, &[halt_seq], &on_disk),
        Err(StartError::ChangesUnwritten { path: p, numbers }) if p == marker && numbers == [halt_seq]
    ));
    let tried = writes.load(Ordering::SeqCst);

    for at in [2_015, 2_030, 2_045, 2_075] {
        assert_eq!(
            ask(&rig, "d2", beat("1.0"), at),
            ServiceReply::Nothing,
            "at {at}"
        );
    }
    assert_eq!(
        writes.load(Ordering::SeqCst),
        tried + 4,
        "one retry per heartbeat"
    );
    let shown = serde_json::to_value(status(&rig)).expect("json");
    assert_eq!(shown["per_service"][LEGACY]["hosts"][1]["hold"], "halted");
    drop(installed);
    let still = "a change is still not written";
    assert_eq!(log.text().matches(still).count(), 2, "{}", log.text());

    failing.store(false, Ordering::SeqCst);
    assert_eq!(ask(&rig, "d2", beat("1.0"), 2_090), ServiceReply::Nothing);
    assert!(!rig.coordinator.ops().halt_unwritten());
    assert!(!marker.exists());
    let on_disk = OpsStore::open(&path).expect("the file").marks(LEGACY);
    assert_eq!(on_disk.halt.map(|h| h.node_id), Some("d1".to_owned()));
    assert_eq!(
        refuse_unfinished_halt(&path).expect("a start goes on"),
        None
    );
}

/// Catches: a halt that holds no order once its write failed, and a store without a file (no
/// marker) that forgets the halt is pending: the other hosts are told nothing while the write
/// fails, and the next heartbeat after the store recovers writes it.
#[tokio::test]
async fn a_halt_that_cannot_be_written_still_blocks_orders() {
    let failing = Arc::new(AtomicBool::new(true));
    let gate = Arc::clone(&failing);
    let (rig, _) = rig_with("halt-unwritable", move |_| {
        OpsStore::refusing(
            OpsState::default(),
            Box::new(move |_| {
                if gate.load(Ordering::SeqCst) {
                    Err(OpsError::Invalid {
                        path: "ops".into(),
                        why: "the disk is full".to_owned(),
                    })
                } else {
                    Ok(())
                }
            }),
        )
    })
    .await;
    assert_eq!(
        ask(&rig, "d1", halting("disk_tainted"), 2_000),
        ServiceReply::Nothing
    );
    for host in ["d2", "d3", "d4", "d5", "d6"] {
        assert_eq!(
            ask(&rig, host, beat("1.0"), 2_015),
            ServiceReply::Nothing,
            "{host}"
        );
    }
    assert!(rig.coordinator.ops().halt_unwritten());
    let convergence =
        serde_json::to_value(&status(&rig).per_service[LEGACY].convergence).expect("json");
    assert_eq!(convergence["state"], "halted");

    failing.store(false, Ordering::SeqCst);
    assert_eq!(ask(&rig, "d2", beat("1.0"), 2_030), ServiceReply::Nothing);
    assert!(!rig.coordinator.ops().halt_unwritten());
    assert!(
        rig.coordinator.ops().marks(LEGACY).halt.is_some(),
        "still halted"
    );
}

/// Catches: a marker that cannot be made, or one removed by hand, stopping the write or the
/// end of the pending state; one that cannot be removed taken for removed; and a marker removed
/// by hand while a change is pending not made again before the next try.
#[tokio::test]
async fn a_marker_that_cannot_be_made_or_is_gone_is_logged_and_the_write_goes_on() {
    let failing = Arc::new(AtomicBool::new(true));
    let writes = Arc::new(AtomicUsize::new(0));
    let (rig, path) = rig_with("halt-marker", |path| flaky(path, &failing, &writes)).await;
    let marker = unfinished_path(&path);
    fs::create_dir(&marker).expect("a directory where the marker goes");
    let log = Log::default();
    let installed = log.install();
    ask(&rig, "d1", halting("disk_tainted"), 2_000);
    assert!(rig.coordinator.ops().halt_unwritten());
    failing.store(false, Ordering::SeqCst);
    ask(&rig, "d2", beat("1.0"), 2_015);
    assert!(
        rig.coordinator.ops().halt_unwritten(),
        "a marker that cannot be removed keeps it pending"
    );
    drop(installed);
    let text = log.text();
    assert!(
        text.contains("the marker of changes not yet written could not be made"),
        "{text}"
    );
    assert!(
        text.contains("the marker of changes written could not be removed"),
        "{text}"
    );

    // The halt is written: once the marker is gone, nothing is pending, whatever the disk does.
    fs::remove_dir(&marker).expect("removed by hand");
    failing.store(true, Ordering::SeqCst);
    ask(&rig, "d2", beat("1.0"), 2_030);
    assert!(
        !rig.coordinator.ops().halt_unwritten(),
        "a marker gone is no error"
    );
    assert!(!marker.exists(), "no marker is made for a change written");

    // A report kept while halted, whose write fails: its marker removed by hand is made again by
    // the next try, and an operator's change lands it.
    ask(&rig, "d3", halting("disk_tainted"), 2_045);
    assert!(rig.coordinator.ops().halt_unwritten());
    fs::remove_file(&marker).expect("removed by hand");
    ask(&rig, "d2", beat("1.0"), 2_060);
    assert!(marker.is_file(), "the retry made it");
    fs::remove_file(&marker).expect("removed by hand");
    failing.store(false, Ordering::SeqCst);
    rig.coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"set_labels","node_id":"d1","labels":{"zone":"a"}}"#,
        )
        .expect("an operator's change lands, the report with it");
    assert!(!rig.coordinator.ops().halt_unwritten());
    assert!(!marker.exists());
    let on_disk = OpsStore::open(&path).expect("the file").marks(LEGACY);
    assert_eq!(
        on_disk
            .halt_seen
            .iter()
            .map(|seen| seen.node_id.as_str())
            .collect::<Vec<_>>(),
        ["d3"]
    );
}

/// The rig's halt, as the ops store holds it now.
fn halt_now(rig: &Rig) -> Option<crate::ops::Halt> {
    rig.coordinator.ops().marks(LEGACY).halt
}

/// Catches: a halt's origin ignored when CI sets a new version: the updater's halt
/// (`artifact_changed`) is lifted by CI's set of 3.0, audited as CI's with the workflow and
/// counted as a clear; a hook's halt stays; and CI's set of the updater halt's own target lifts
/// nothing.
#[tokio::test]
async fn ci_write_lifts_an_updater_halt_but_not_a_hook_halt() {
    let three = r#"{"version":"3.0","min_up":{"storage":0,"frontend":0}}"#;

    let updater = rig("halt-updater", "", TWO).await;
    ask(&updater, "d1", halting("artifact_changed"), 2_000);
    assert_eq!(
        halt_now(&updater).map(|h| h.origin),
        Some(HaltOrigin::Updater)
    );
    updater.set(TWO).await;
    assert!(halt_now(&updater).is_some(), "its own target lifts nothing");
    updater.set(three).await;
    assert_eq!(halt_now(&updater), None);
    let marks = crate::rollback_tests::persisted(&updater);
    assert_eq!(marks.halt, None, "in the file too");
    assert_eq!(marks.halt_generation, 1);
    let audit = status(&updater).ops.audit;
    let lift = audit.last().expect("audited");
    assert_eq!(
        (lift.by.as_str(), lift.action.as_str()),
        ("ci", "lift_halt")
    );
    assert_eq!(
        lift.reason.as_deref(),
        Some("3.0 set by example-org/example-repo/.github/workflows/ci-check.yml@refs/heads/main")
    );
    assert!(matches!(
        ask(&updater, "d2", beat("1.0"), 2_015),
        ServiceReply::Update { .. }
    ));

    let hook = rig("halt-hook", "", TWO).await;
    ask(&hook, "d1", halting("disk_tainted"), 2_000);
    assert_eq!(halt_now(&hook).map(|h| h.origin), Some(HaltOrigin::Hook));
    hook.set(three).await;
    assert!(halt_now(&hook).is_some(), "a hook's halt stays");
    assert_eq!(wanted(&hook), version("3.0"));
    assert_eq!(ask(&hook, "d2", beat("1.0"), 2_015), ServiceReply::Nothing);
}

/// Catches: an updater's halt lifted over a hook's report kept while it stood (that report is a
/// hook's halt of the service), and a lift that fails to be written reported as a success.
#[tokio::test]
async fn an_updater_halt_with_a_hooks_report_stays_and_a_failed_lift_is_an_error() {
    let mixed = rig("halt-mixed", "", TWO).await;
    ask(&mixed, "d1", halting("artifact_changed"), 2_000);
    ask(&mixed, "d2", halting("disk_tainted"), 2_000);
    let ops = mixed.coordinator.ops();
    assert!(matches!(
        lift_by_ci(ops, LEGACY, &version("3.0"), "ci.yml"),
        Ok(Lifted::Nothing)
    ));
    assert!(matches!(
        lift_by_ci(ops, "unknown", &version("3.0"), "ci.yml"),
        Ok(Lifted::Nothing)
    ));
    assert!(halt_now(&mixed).is_some());

    let failing = Arc::new(AtomicBool::new(true));
    let writes = Arc::new(AtomicUsize::new(0));
    let (stuck, _) = rig_with("halt-lift-fails", |path| flaky(path, &failing, &writes)).await;
    ask(&stuck, "d1", halting("artifact_changed"), 2_000);
    let three = r#"{"version":"3.0","min_up":{"storage":0,"frontend":0}}"#;
    let set = stuck
        .coordinator
        .set_desired(Some(&ci_token()), three.as_bytes())
        .await;
    assert!(matches!(set, Err(SetError::Ops(_))), "{set:?}");
    assert_eq!(wanted(&stuck), version("3.0"), "the desired state is set");
    assert!(halt_now(&stuck).is_some(), "the halt stands");
}

/// Catches: the updater's own halts and the hooks' told apart by anything but the reason.
#[test]
fn the_origin_of_a_halt_is_its_reason() {
    assert_eq!(UPDATER_HALT_REASONS, ["artifact_changed"]);
    assert_eq!(origin_of(None), HaltOrigin::Hook);
    assert_eq!(
        origin_of(Some(&halted_update("artifact_changed"))),
        HaltOrigin::Updater
    );
    assert_eq!(
        origin_of(Some(&halted_update("disk_tainted"))),
        HaltOrigin::Hook
    );
    let mut silent = halted_update("x");
    silent.reason = None;
    assert_eq!(origin_of(Some(&silent)), HaltOrigin::Hook);
    assert!(HaltOrigin::Hook.is_hook() && !HaltOrigin::Updater.is_hook());
}

/// The recorded ops file and halts file (the ones the release before this one read) in a
/// directory of their own, the halts file as `edit` leaves it.
fn recorded_ops(
    name: &str,
    edit: impl FnOnce(&mut Value),
) -> (hostkeeper_testkit::Scratch, PathBuf) {
    let dir = scratch(name);
    let path = dir.path().join("desired.json.ops");
    fs::write(&path, include_str!("../tests/fixtures/recorded/ops.json")).expect("written");
    let mut halts: Value =
        serde_json::from_str(include_str!("../tests/fixtures/recorded/ops.json.halts"))
            .expect("the halts file");
    edit(&mut halts);
    fs::write(
        crate::ops_file::halts_path(&path),
        serde_json::to_vec_pretty(&halts).expect("json"),
    )
    .expect("written");
    (dir, path)
}

/// Catches: a halt read from a file that predates `origin` taken for the updater's (CI would
/// lift it), or the ops file of the release before refused: it reads, its halt is a hook's with
/// its report, an operator's halt is the operator's, and an origin this release does not know is
/// a hook's too, the record kept.
#[test]
fn an_old_halt_file_reads_as_a_hook_halt() {
    let (_dir, path) = recorded_ops("halt-old", |_| {});
    let marks: ServiceOps = OpsStore::open(&path).expect("the old files").marks(LEGACY);
    let halt = marks.halt.expect("the recorded halt");
    assert_eq!(halt.origin, HaltOrigin::Hook);
    assert!(halt.report.is_some());

    for (origin, read) in [
        ("updater", HaltOrigin::Updater),
        ("operator", HaltOrigin::Operator),
        ("someone_else", HaltOrigin::Hook),
    ] {
        let (_dir, path) = recorded_ops(&format!("halt-origin-{origin}"), |halts| {
            halts["services"][LEGACY]["halt"]["origin"] = json!(origin);
        });
        let halt = OpsStore::open(&path)
            .expect("read")
            .marks(LEGACY)
            .halt
            .expect("the halt");
        assert_eq!(halt.origin, read, "{origin}");
        assert!(halt.report.is_some(), "{origin}: the record is kept");
    }
}

/// Catches: the ops file of the release before refused at start: the recorded one, with its
/// halts file, opens, and what it holds is there.
#[test]
fn an_old_ops_file_still_reads() {
    let (_dir, path) = recorded_ops("ops-old", |_| {});
    let store = OpsStore::open(&path).expect("the old files");
    assert!(!store.state().is_empty());
    assert!(!store.halt_unwritten());
    refuse_unfinished_halt(&path).expect("no marker");
}

/// Catches: a gated reply field sent to an agent that did not announce its feature (its strict
/// reader would refuse the answer): of two agents after a clear, only the one that announced
/// `halt-v1` is told the generation; the other's answer has no `halts` at all.
#[tokio::test]
async fn a_reply_field_goes_only_to_a_daemon_that_announced_the_feature() {
    let rig = rig("halt-feature", "", TWO).await;
    ask(&rig, "d1", halting("disk_tainted"), 2_000);
    rig.coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"clear_halt","service":"kbf","reason":"disk replaced"}"#,
        )
        .expect("cleared");
    rig.at(2_015);
    let answer = |host: &str, features: &[&str]| {
        let mut b = beat("1.0");
        b.agent_features = features.iter().map(|f| (*f).to_owned()).collect();
        rig.coordinator.heartbeat(who(host), b).expect("answered")
    };
    let announced = answer("d1", &["halt-v1"]);
    assert_eq!(announced.halts, BTreeMap::from([(LEGACY.to_owned(), 1)]));
    let other = answer("d2", &["deferred-v1"]);
    assert!(other.halts.is_empty());
    let text = serde_json::to_value(&other).expect("json");
    assert_eq!(text.get("halts"), None, "{text}");
    assert_eq!(text["features"], json!(["deferred-v1"]));

    for (_, feature) in GATED_FIELDS {
        assert!(FEATURES.contains(feature), "{feature} is advertised");
    }
    assert!(sends("reply", None));
    assert!(!sends("halts", None));
    assert!(!sends("halts", Some(["deferred-v1".to_owned()].as_slice())));
    assert!(sends("halts", Some(["halt-v1".to_owned()].as_slice())));
}
