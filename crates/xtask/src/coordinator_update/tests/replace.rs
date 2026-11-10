//! The tests of the replacement of the container: the swap, and the way back.

use super::*;

/// The marker text and error of a roll back to the old container with the usual three checks.
fn rolled_back_text(extra: &str) -> String {
    format!(
        "v0.1.219 did not become ready within 3 checks: the old container runs again\nthe last log lines of hostkeeper-coordinator-next:\nthe new coordinator's last words{extra}"
    )
}

#[test]
fn a_new_container_that_is_not_ready_is_removed_and_the_old_one_runs_again() {
    // Proves: when the coordinator never answers at_least, the new container is stopped and
    // deleted, the old one is started and checked, its last 50 log lines are in the error, the
    // version is marked failed with the reason, and the pauses are one fewer than the checks.
    let mut w = world("cu-notready");
    w.probe = FakeProbe::new(Probed::Running(v("0.1.218")), vec![Probed::Older]);
    let err = w.update().expect_err("not ready");
    assert_eq!(err.to_string(), rolled_back_text(""));
    assert_eq!(
        w.engine.calls()[5..],
        [
            format!("create {NEXT}"),
            format!("stop {OLD_ID} 30"),
            format!("start {NEW_ID}"),
            format!("inspect {NEW_ID}"),
            format!("inspect {NEW_ID}"),
            format!("inspect {NEW_ID}"),
            format!("logs {NEW_ID} 50"),
            format!("stop {NEW_ID} 30"),
            format!("remove {NEW_ID}"),
            format!("start {OLD_ID}"),
            format!("inspect {OLD_ID}"),
        ]
    );
    assert_eq!(*w.pause.waits.borrow(), vec![5, 5]);
    assert!(w.engine.is_running(NAME) && !w.engine.exists(NEXT));
    assert_eq!(
        w.marker().as_deref(),
        Some("v0.1.219 did not become ready within 3 checks: the old container runs again\n")
    );
    // The failed version is refused on the next run, and not tried again.
    assert!(w.refused().contains("remove it to try v0.1.219 again"));
}

#[test]
fn a_new_container_that_crashes_or_loops_is_never_ready() {
    // Proves: a container that exited, that restarts, or that has restarted is not ready even
    // when the coordinator answers.
    for behaviour in [
        NewBehaviour {
            exits: true,
            ..NewBehaviour::default()
        },
        NewBehaviour {
            restarting: true,
            ..NewBehaviour::default()
        },
        NewBehaviour {
            restart_count: 1,
            ..NewBehaviour::default()
        },
    ] {
        let mut w = world("cu-crash");
        w.probe = FakeProbe::new(
            Probed::Running(v("0.1.218")),
            vec![Probed::Running(v("0.1.219"))],
        );
        w.engine.new_behaviour.set(behaviour);
        assert_eq!(w.refused(), rolled_back_text(""));
        assert!(w.engine.is_running(NAME));
    }
    // A container that vanished is not ready either.
    let w = world("cu-vanished");
    w.engine.vanish.borrow_mut().insert(NEW_ID.to_owned());
    assert_eq!(w.refused(), rolled_back_text(""));
}

#[test]
fn a_new_container_that_does_not_start_rolls_back_with_the_reason() {
    let w = world("cu-start-fails");
    w.engine.failing(&format!("start {NEW_ID}"));
    assert_eq!(
        w.refused(),
        "v0.1.219: starting hostkeeper-coordinator-next failed: docker start newid0000000000: HTTP 500: it failed: the old container runs again\nthe last log lines of hostkeeper-coordinator-next:\nthe new coordinator's last words"
    );
    assert!(w.engine.is_running(NAME));
    assert!(
        w.marker()
            .is_some_and(|m| m.starts_with("v0.1.219: starting"))
    );
}

#[test]
fn a_old_container_that_will_not_start_again_is_reported_as_down() {
    // Proves: when the old container cannot be started again, the error says the coordinator is
    // down and what to do, and the marker is still written, so the broken version is not retried.
    let mut w = world("cu-down");
    w.probe = FakeProbe::new(Probed::Running(v("0.1.218")), vec![Probed::Older]);
    w.engine.failing(&format!("start {OLD_ID}"));
    let down = "v0.1.219 did not become ready within 3 checks: the old container oldid0000000000 could not be started again (docker start oldid0000000000: HTTP 500: it failed): hostkeeper-coordinator IS DOWN; start it (docker start oldid0000000000), or deploy the coordinator by hand";
    assert_eq!(
        w.refused(),
        format!(
            "{down}\nthe last log lines of hostkeeper-coordinator-next:\nthe new coordinator's last words"
        )
    );
    assert_eq!(w.marker().as_deref(), Some(&*format!("{down}\n")));

    let mut w = world("cu-down-quiet");
    w.probe = FakeProbe::new(Probed::Running(v("0.1.218")), vec![Probed::Older]);
    w.engine.stay_down.borrow_mut().insert(OLD_ID.to_owned());
    let msg = w.refused();
    assert!(msg.contains(
        "could not be started again (docker start oldid0000000000: it started and is not running)"
    ), "{msg}");
}

#[test]
fn trouble_while_rolling_back_is_added_to_the_error() {
    // Proves: a log that cannot be read, a new container that cannot be deleted and a marker
    // that cannot be written are each said, and the rollback goes on.
    let mut w = world("cu-rollback-trouble");
    w.probe = FakeProbe::new(Probed::Running(v("0.1.218")), vec![Probed::Older]);
    w.engine.failing(&format!("logs {NEW_ID} 50"));
    w.engine.failing(&format!("remove {NEW_ID}"));
    std::fs::create_dir(w.download.join("failed-v0.1.219"))
        .expect("a directory where the marker goes");
    let msg = w.refused();
    assert_eq!(
        msg,
        format!(
            "v0.1.219 did not become ready within 3 checks: the old container runs again\nthe last log lines of hostkeeper-coordinator-next:\n(the log could not be read: docker logs newid0000000000 50: HTTP 500: it failed)\nhostkeeper-coordinator-next could not be deleted: docker remove newid0000000000: HTTP 500: it failed; delete it (docker rm -f hostkeeper-coordinator-next) before the next run\n{} could not be written: Is a directory (os error 21)",
            w.download.join("failed-v0.1.219").display()
        )
    );
    assert!(w.engine.is_running(NAME));
}

#[test]
fn a_container_that_cannot_be_created_or_the_old_one_stopped_changes_nothing() {
    // Proves: a failed create leaves the old container as it was; a failed stop deletes the new
    // container and says so (or says it could not).
    let w = world("cu-create-fails");
    w.engine.failing(&format!("create {NEXT}"));
    assert_eq!(
        w.refused(),
        "docker create hostkeeper-coordinator-next: HTTP 500: it failed"
    );
    assert!(w.engine.is_running(NAME));
    assert!(!w.engine.calls().iter().any(|c| c.starts_with("stop ")));
    assert!(w.marker().is_none());

    let w = world("cu-stop-fails");
    w.engine.failing(&format!("stop {OLD_ID} 30"));
    assert_eq!(
        w.refused(),
        "hostkeeper-coordinator could not be stopped: docker stop oldid0000000000 30: HTTP 500: it failed; hostkeeper-coordinator-next was deleted and hostkeeper-coordinator was left as it was"
    );
    assert!(w.engine.is_running(NAME) && !w.engine.exists(NEXT));
    assert!(w.marker().is_none());

    let w = world("cu-stop-remove-fails");
    w.engine.failing(&format!("stop {OLD_ID} 30"));
    w.engine.failing(&format!("remove {NEW_ID}"));
    assert_eq!(
        w.refused(),
        "hostkeeper-coordinator could not be stopped: docker stop oldid0000000000 30: HTTP 500: it failed; hostkeeper-coordinator-next could not be deleted either (docker remove newid0000000000: HTTP 500: it failed): delete it by hand"
    );
}

#[test]
fn a_ready_container_whose_swap_does_not_finish_says_what_to_do() {
    let w = world("cu-remove-fails");
    w.engine.failing(&format!("remove {OLD_ID}"));
    assert_eq!(
        w.refused(),
        "hostkeeper-coordinator-next runs v0.1.219 and is ready, but the old container oldid0000000000 could not be deleted: docker remove oldid0000000000: HTTP 500: it failed; delete it (docker rm oldid0000000000) and rename hostkeeper-coordinator-next to hostkeeper-coordinator"
    );
    let w = world("cu-rename-fails");
    w.engine.failing(&format!("rename {NEW_ID} {NAME}"));
    assert_eq!(
        w.refused(),
        "hostkeeper-coordinator-next runs v0.1.219 (was v0.1.218) and the old container is deleted, but hostkeeper-coordinator-next could not be renamed to hostkeeper-coordinator: docker rename newid0000000000 hostkeeper-coordinator: HTTP 500: it failed; rename it (docker rename hostkeeper-coordinator-next hostkeeper-coordinator)"
    );
}

#[test]
fn a_docker_error_while_waiting_rolls_back_instead_of_ending_the_run() {
    // Proves: an inspect of the new container that fails during the checks is a check that is
    // not ready, never an early end with the old container stopped: every check is made, then
    // the new container goes, the old one runs again and the version is marked failed.
    let w = world("cu-inspect-fails");
    w.engine.failing(&format!("inspect {NEW_ID}"));
    let err = w.update().expect_err("not ready");
    assert_eq!(err.to_string(), rolled_back_text(""));
    assert_eq!(
        w.engine
            .calls()
            .iter()
            .filter(|c| **c == format!("inspect {NEW_ID}"))
            .count(),
        3
    );
    assert!(w.engine.is_running(NAME) && !w.engine.exists(NEXT));
    assert_eq!(
        w.marker().as_deref(),
        Some("v0.1.219 did not become ready within 3 checks: the old container runs again\n")
    );
}
