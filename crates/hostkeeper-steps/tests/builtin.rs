//! Built-in steps (`UpdateOrder::builtin_steps`): every step ends at once as a hook that exits 0
//! and prints nothing, through the runner's own path, and nothing is launched.

mod support;

use hostkeeper_hook::{Step, Verdict};
use hostkeeper_steps::record::State;
use hostkeeper_steps::{BUILTIN_EXIT, Finished, Loaded};
use support::{FAIL, Rig};

/// Catches built-in steps that launch the hook anyway, end otherwise than a hook that exits 0
/// and prints nothing, or skip the runner's own path (the switch, the record of each step): an
/// update whose steps are built in, with a launcher that would fail every step, launches
/// nothing, is `Succeeded` with its record finished at `cleanup` and the verify window stamped,
/// as the same update with a hook that exits 0 is; with the steps not built in, the same launcher
/// runs `prepare` and the update fails.
#[test]
fn builtin_steps_launch_nothing_and_end_as_a_hook_that_exits_zero() {
    assert_eq!(BUILTIN_EXIT.code, Some(0));
    assert_eq!(BUILTIN_EXIT.last_line, None);
    let rig = Rig::new("builtin-steps");
    for step in [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup] {
        rig.fake.say(step, &[FAIL]);
    }
    let mut order = rig.order();
    order.builtin_steps = true;
    let finished = rig.runner().run(&order).expect("the update ends");
    assert_eq!(
        finished,
        Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
        }
    );
    assert!(rig.fake.events().is_empty(), "{:?}", rig.fake.events());
    let Loaded::Current(rec) = rig.store().load().expect("the record") else {
        panic!("no record");
    };
    assert_eq!(rec.step, Step::Cleanup);
    assert!(matches!(
        rec.state,
        State::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));
    assert!(rec.progress.verify_started.is_some());

    // The same update with the hook run launches it, and fails as the launcher says.
    let rig = Rig::new("builtin-steps-off");
    rig.fake.say(Step::Prepare, &[FAIL]);
    let finished = rig.runner().run(&rig.order()).expect("the update ends");
    assert!(
        matches!(finished.verdict, Verdict::Failed { .. }),
        "{finished:?}"
    );
    assert_eq!(rig.fake.steps().first(), Some(&Step::Prepare));
}
