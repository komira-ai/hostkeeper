//! A rollback and CI's set of the desired state do not overwrite each other.
//!
//! The race is made to happen, not waited for: the ops store's writer pauses the heartbeat that
//! plans a rollback at its bad mark, with the plan made from the state it read; CI's set of
//! another version runs until it says it is at the fleet's lock, and only then does the heartbeat
//! go on. Every step waits on a message; nothing sleeps.

use std::sync::Mutex;
use std::sync::mpsc;

use crate::ops::{OpsError, OpsState, OpsStore};
use crate::rollback_tests::{AUTO, ONE, THREE, TWO, failure, good, release, rig_over, round, who};
use crate::testutil::{LEGACY, ci_token, version};

/// What pauses the writer: it says it is paused, and waits to be let go.
struct Gate {
    paused: Mutex<mpsc::Sender<()>>,
    go: Mutex<mpsc::Receiver<()>>,
}

/// Proves: CI's set reaches the fleet's lock while the rollback is paused at its mark holding
/// that lock, so the set waits for it; the rollback, planned from 2.0, is written first and
/// CI's 3.0 after it, in memory and on disk.
///
/// Catches: a rollback written over a version CI stored while the rollback was being made. A
/// set that took no lock would run on past the progress event and store 3.0 while the rollback
/// is paused, and the rollback would then put 1.0 over it.
#[tokio::test]
async fn a_rollback_paused_at_its_mark_does_not_overwrite_the_version_ci_sets_meanwhile() {
    let (paused_tx, paused_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let gate = Gate {
        paused: Mutex::new(paused_tx),
        go: Mutex::new(go_rx),
    };
    let writer = Box::new(move |state: &OpsState| {
        let action = state.audit.last().map_or("", |e| e.action.as_str());
        Some(&gate)
            .filter(|_| action == "mark_bad")
            .into_iter()
            .for_each(|gate| {
                let _ = gate.paused.lock().expect("paused").send(());
                let _ = gate.go.lock().expect("go").recv();
            });
        Ok::<(), OpsError>(())
    });
    let mut state = OpsState::default();
    state.services.insert(LEGACY.to_owned(), good());
    let rig = rig_over(
        "rb-race-ci",
        AUTO,
        ONE,
        OpsStore::with_writer(state, writer),
    )
    .await;
    rig.set(TWO).await;
    round(&rig, 2_000, &[]);
    rig.at(2_015);
    let (phase, class) = release();
    let coordinator = &rig.coordinator;
    coordinator
        .heartbeat(who("d1"), failure(phase, class, "2.0"))
        .expect("a heartbeat");
    let token = ci_token();
    std::thread::scope(|scope| {
        let beat = scope.spawn(|| {
            coordinator
                .heartbeat(who("d2"), failure(phase, class, "2.0"))
                .expect("a heartbeat")
        });
        paused_rx.recv().expect("the rollback paused at its mark");
        let (at_lock_tx, at_lock_rx) = mpsc::channel();
        coordinator.tell_at_set_lock(at_lock_tx);
        let ci = scope.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a runtime")
                .block_on(coordinator.set_desired(Some(&token), THREE.as_bytes()))
        });
        at_lock_rx.recv().expect("CI's set is at the fleet's lock");
        assert!(
            coordinator.fleet_held(),
            "the paused rollback holds the lock CI's set needs"
        );
        go_tx.send(()).expect("let the rollback go");
        beat.join().expect("the heartbeat");
        ci.join().expect("the set").expect("CI's set");
    });
    let status = crate::rollback_tests::status(&rig);
    assert_eq!(status.services[LEGACY].version, version("3.0"));
    assert_eq!(status.ops.services[LEGACY].bad, vec![version("2.0")]);
    let on_disk = std::fs::read_to_string(rig.dir.path().join("desired.json")).expect("the file");
    assert!(on_disk.contains("\"3.0\""), "{on_disk}");
}
