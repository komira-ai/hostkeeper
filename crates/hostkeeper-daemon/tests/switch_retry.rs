//! The switch around a hooked step is tried again when it fails: the installer's `activate` of
//! the target before `cutover`, and of the release the update replaced before `rollback`, can
//! meet a service manager that refuses one call for a moment. The update keeps its one run, and
//! a resend of the order while it runs is answered `Running`, not run again.
//!
//! The host is an apt host: the repository installer's fake of apt and dpkg answers the tool
//! runs of the daemon's step launcher, and a wrapper of that answer makes `systemctl
//! daemon-reload`, the last tool run of an `activate`, refuse its next calls.

mod support;
mod support_apt;
mod support_hooked;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use hostkeeper_daemon::testing::spawn;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::{ToolCall, ToolExit};
use support::alerting;
use support::lines;
use support_apt::{bridge, seen};
use support_hooked::Farm;

const WEB: &str = "web";

/// The refusal of a `systemctl daemon-reload` that the service manager is not ready for.
const REFUSED: &str = "Failed to reload daemon: Connection timed out";

/// What `daemon-reload` says when it is refused, as the installer reports it.
const WHY: &str =
    "io_error: systemctl daemon-reload failed: Failed to reload daemon: Connection timed out";

/// Makes each of the next `refusals` runs of `systemctl daemon-reload` exit 5 with [`REFUSED`];
/// every other run, and every reload after those, is answered by the fake host.
fn refuse_reloads(farm: &Farm, refusals: u32) {
    let left = AtomicU32::new(refusals);
    let host = bridge(Arc::clone(&farm.apt.host));
    farm.rig()
        .launcher
        .answer_tools(Arc::new(move |call: &ToolCall<'_>| {
            let reload = call.program.ends_with("systemctl")
                && call.args.first().is_some_and(|arg| arg == "daemon-reload");
            let refused = reload
                && left
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok();
            match refused {
                true => Ok(ToolExit {
                    code: Some(5),
                    stdout: Vec::new(),
                    stderr: REFUSED.as_bytes().to_vec(),
                    cut: false,
                }),
                false => host(call),
            }
        }));
}

fn failed_in_verify() -> Reply {
    Reply::Finished {
        verdict: Verdict::Failed {
            phase: Phase::GateRelease,
            class: Class::Release,
        },
        reason: None,
        resent: false,
    }
}

/// Catches a switch back before `rollback` that one refusal of the service manager ends: an
/// update of `web` from 1.0.0 to 2.0.0 whose `verify` fails meets, in the switch back, a
/// `systemctl daemon-reload` that exits 5. The switch is tried again, the installer's `activate`
/// of 1.0.0 runs again from its start and works, `rollback` runs, and the update ends with
/// `verify`'s failure, not `link_failed`. A resend of the same order (same try) while `rollback`
/// runs is answered `Running`: every step ran once. The retry is logged with what failed, and
/// nothing else is.
#[test]
fn a_refused_reload_in_the_switch_back_is_tried_again_and_the_rollback_runs_once() {
    let farm = Farm::new("switch-retry-reload");
    let rig = farm.rig();
    for version in ["1.0.0", "2.0.0"] {
        farm.publish(WEB, version);
    }
    let (daemon, alerts) = alerting(farm.daemon());
    let daemon = Arc::new(daemon);
    assert_eq!(
        daemon.order(&farm.install(WEB, "1.0.0", 1)),
        seen::succeeded()
    );

    rig.launcher.exits(WEB, Step::Verify, &[1]);
    let verify = rig.launcher.hold(WEB, Step::Verify);
    let rollback = rig.launcher.hold(WEB, Step::Rollback);
    let update = farm.update(WEB, "2.0.0", 2);
    let first = {
        let (daemon, update) = (Arc::clone(&daemon), update.clone());
        spawn("the update", move || daemon.order(&update))
    };
    verify.entered(&first).expect("verify runs");
    assert_eq!(seen::linked(&farm.apt, "current").as_deref(), Some("2.0.0"));
    let before_back = farm.apt.host.calls().len();
    refuse_reloads(&farm, 1);
    verify.release();

    rollback
        .entered(&first)
        .expect("rollback runs after the switch back");
    assert_eq!(seen::linked(&farm.apt, "current").as_deref(), Some("1.0.0"));
    assert_eq!(daemon.order(&update), Reply::Running);
    rollback.release();
    assert_eq!(first.join().expect("the update"), failed_in_verify());

    assert_eq!(
        rig.launcher.steps(WEB),
        [
            // The install of 1.0.0.
            Step::Prepare,
            Step::Cutover,
            Step::Verify,
            Step::Cleanup,
            // The update to 2.0.0, once.
            Step::Prepare,
            Step::Cutover,
            Step::Verify,
            Step::Rollback,
        ]
    );
    // The `activate` of 1.0.0 ends with its reload: the first try's is refused (the fake host
    // never sees it), so the second try runs the whole of it again, and its reload works.
    let activate = seen::labels_since(&farm.apt, before_back);
    let reloads = activate
        .iter()
        .filter(|label| *label == "systemctl daemon-reload")
        .count();
    assert_eq!(reloads, 1, "{activate:?}");
    let installs = activate
        .iter()
        .filter(|label| *label == "apt-get install")
        .count();
    assert_eq!(installs, 2, "{activate:?}");
    assert_eq!(farm.apt.host.installed(WEB), seen::held("1.0.0-1"));
    let lines = lines(&alerts);
    assert_eq!(
        lines,
        [format!(
            "hostkeeper daemon: level=warn alert=switch_retried service=web: the switch Back \
             failed (try 1 of 5), tried again in 2s: {WHY}"
        )],
        "{lines:#?}"
    );
}

/// Catches a last try that is not made: the switch back meets four refused reloads in a row and
/// works on its fifth and last try, 8 seconds of the daemon's clock later, so `rollback` runs
/// and the update ends with `verify`'s failure; the four failed tries are logged, and the update
/// is not.
#[test]
fn a_switch_back_refused_on_four_tries_works_on_its_fifth() {
    let farm = Farm::new("switch-retry-last");
    let rig = farm.rig();
    for version in ["1.0.0", "2.0.0"] {
        farm.publish(WEB, version);
    }
    let (daemon, alerts) = alerting(farm.daemon());
    let daemon = Arc::new(daemon);
    assert_eq!(
        daemon.order(&farm.install(WEB, "1.0.0", 1)),
        seen::succeeded()
    );
    rig.launcher.exits(WEB, Step::Verify, &[1]);
    let verify = rig.launcher.hold(WEB, Step::Verify);
    let update = farm.update(WEB, "2.0.0", 2);
    let first = {
        let daemon = Arc::clone(&daemon);
        spawn("the update", move || daemon.order(&update))
    };
    verify.entered(&first).expect("verify runs");
    refuse_reloads(&farm, 4);
    let at = hostkeeper_steps::Clock::mono_secs(&*rig.clock);
    verify.release();

    assert_eq!(first.join().expect("the update"), failed_in_verify());
    assert_eq!(hostkeeper_steps::Clock::mono_secs(&*rig.clock) - at, 8);
    assert_eq!(seen::linked(&farm.apt, "current").as_deref(), Some("1.0.0"));
    assert_eq!(rig.launcher.steps(WEB).last(), Some(&Step::Rollback));
    let expected: Vec<String> = (1..5)
        .map(|attempt| {
            format!(
                "hostkeeper daemon: level=warn alert=switch_retried service=web: the switch Back \
                 failed (try {attempt} of 5), tried again in 2s: {WHY}"
            )
        })
        .collect();
    assert_eq!(lines(&alerts), expected);
}

/// Catches a switch tried without end, or one whose failure is not told: a `daemon-reload` that
/// keeps failing in the switch back is tried five times, two seconds of the daemon's clock apart,
/// each failure but the last logged, and then stops the update `link_failed` with the step
/// record still at `rollback`, not run, and the last failure logged as `order_interrupted`.
#[test]
fn a_switch_back_that_keeps_failing_is_tried_five_times_then_stops_the_update_link_failed() {
    let farm = Farm::new("switch-retry-exhausted");
    let rig = farm.rig();
    for version in ["1.0.0", "2.0.0"] {
        farm.publish(WEB, version);
    }
    let (daemon, alerts) = alerting(farm.daemon());
    assert_eq!(
        daemon.order(&farm.install(WEB, "1.0.0", 1)),
        seen::succeeded()
    );
    rig.launcher.exits(WEB, Step::Verify, &[1]);
    let verify = rig.launcher.hold(WEB, Step::Verify);
    let daemon = Arc::new(daemon);
    let update = farm.update(WEB, "2.0.0", 2);
    let first = {
        let daemon = Arc::clone(&daemon);
        spawn("the update", move || daemon.order(&update))
    };
    verify.entered(&first).expect("verify runs");
    refuse_reloads(&farm, u32::MAX);
    let at = hostkeeper_steps::Clock::mono_secs(&*rig.clock);
    verify.release();

    let reply = first.join().expect("the update");
    let stopped = "the switch before the step failed; nothing ran and the step is still pending";
    assert_eq!(
        reply,
        Reply::Interrupted {
            refusal: Refusal::LinkFailed,
            detail: format!("{stopped}: {WHY}"),
        }
    );
    assert_eq!(hostkeeper_steps::Clock::mono_secs(&*rig.clock) - at, 8);
    let record = rig.record(WEB).expect("the step record");
    assert_eq!(record.step, Step::Rollback);
    assert!(!rig.launcher.steps(WEB).contains(&Step::Rollback));
    let retried = (1..5).map(|attempt| {
        format!(
            "hostkeeper daemon: level=warn alert=switch_retried service=web: the switch Back \
             failed (try {attempt} of 5), tried again in 2s: {WHY}"
        )
    });
    let interrupted = format!(
        "hostkeeper daemon: level=error alert=order_interrupted service=web: link_failed: \
         {stopped}: {WHY}"
    );
    assert_eq!(
        lines(&alerts),
        retried.chain([interrupted]).collect::<Vec<_>>()
    );
}
