//! The try of an order as the runner reads a finished record: the same try of the same update is
//! answered from the record, another try of it is a new order.

mod support;

use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_steps::Finished;
use support::{FAIL, Rig};

/// Catches a new try of an update that ended being answered with the old verdict (a first
/// install that failed could then never be installed again), and a resend of the same try run
/// twice: the same try is answered `Failed` and runs nothing; the next try runs the update again,
/// in the next generation, to its own verdict.
#[test]
fn another_try_of_an_update_that_ended_runs_again() {
    let rig = Rig::new("try-again");
    rig.fake.say(Step::Prepare, &[FAIL]);
    let mut order = rig.order();
    order.order_try = Some(1);
    let failed = Finished {
        verdict: Verdict::Failed {
            phase: Phase::BeforeCommit,
            class: Class::Release,
        },
        reason: None,
    };
    assert_eq!(rig.runner().run(&order).expect("runs"), failed);
    assert_eq!(rig.runner().run(&order).expect("answers"), failed);
    assert_eq!(rig.fake.steps(), [Step::Prepare, Step::Rollback]);

    order.order_try = Some(2);
    let ok = rig.runner().run(&order).expect("runs again");
    assert_eq!(
        ok.verdict,
        Verdict::Succeeded {
            promotion_pending: false
        }
    );
    assert_eq!(
        rig.fake.steps(),
        [
            Step::Prepare,
            Step::Rollback,
            Step::Prepare,
            Step::Cutover,
            Step::Verify,
            Step::Cleanup
        ]
    );
    let units: Vec<String> = rig
        .fake
        .runs()
        .into_iter()
        .filter_map(|event| match event {
            support::Event::Run { unit, .. } => Some(unit),
            support::Event::Stop(_) => None,
        })
        .collect();
    assert_eq!(units[2], "updater-svc-prepare-2");
}
