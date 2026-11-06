//! The runner's windows on a clock whose boot id cannot be read: a stamp of an unknown boot
//! measures no elapsed time ([`hostkeeper_steps::Stamp::elapsed_secs`]), so the runner begins
//! such a window again once, when it first meets it, and measures it from then on the monotonic
//! clock of its own process; the verify window and the promotion ceiling still close.

mod support;

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::Finished;
use hostkeeper_steps::runner::WATCH_POLL_SECS;
use support::{DEFER, PENDING, Rig};

fn failed(phase: Phase, class: Class) -> Finished {
    Finished {
        verdict: Verdict::Failed { phase, class },
        reason: None,
    }
}

/// The bounds of each run of `verify` the fake launcher saw.
fn verify_bounds(rig: &Rig) -> Vec<u64> {
    rig.fake
        .runs()
        .iter()
        .filter_map(|e| match e {
            support::Event::Run {
                step: Step::Verify,
                bound,
                ..
            } => Some(support::secs_of(*bound)),
            _ => None,
        })
        .collect()
}

/// Catches a verify window that never closes while the boot id cannot be read (every read of it
/// would begin the window again): a `verify` that keeps deferring past `verify_deadline_secs` on
/// such a clock is followed by `rollback`, each run bounded by what the window has left, as on a
/// clock that names its boot.
#[test]
fn a_verify_window_closes_while_the_boot_id_cannot_be_read() {
    let rig = Rig::new("run-window-unknown-boot");
    rig.clock.reboot("");
    let mut order = rig.order();
    order.bounds.verify_deadline_secs = 250;
    rig.fake.say(Step::Verify, &[DEFER; 8]);
    let got = rig.runner().run(&order).expect("a verdict");
    assert_eq!(got, failed(Phase::GateRelease, Class::Release));
    assert_eq!(verify_bounds(&rig), [250, 150, 50]);
    assert_eq!(rig.fake.steps().last(), Some(&Step::Rollback));
    assert_eq!(rig.clock.slept(), [100, 100]);
}

/// Catches a post-success watcher that runs forever while the boot id cannot be read: the
/// promotion ceiling still ends it as `Failed(after_commit)`, with no `rollback`, after the same
/// polls as on a clock that names its boot.
#[test]
fn the_promotion_ceiling_ends_the_watch_while_the_boot_id_cannot_be_read() {
    let rig = Rig::new("run-ceiling-unknown-boot");
    rig.clock.reboot("");
    let mut order = rig.order();
    order.bounds.promotion_ceiling_secs = 100;
    rig.fake.say(Step::Verify, &[PENDING; 10]);
    let got = rig.runner().run(&order).expect("a verdict");
    assert_eq!(got, failed(Phase::AfterCommit, Class::Release));
    assert!(!rig.fake.steps().contains(&Step::Rollback));
    assert_eq!(rig.clock.slept(), [WATCH_POLL_SECS; 4]);
}
