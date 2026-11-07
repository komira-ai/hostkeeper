//! A coordinator that restarts in the middle of an update plan (`docs/design/rollout-update-plans.md`
//! section 3): it resumes at the stage the plan had entered, and a host recorded done that runs the
//! version is neither ordered again nor made to bake again. Every time is the rig's clock;
//! nothing sleeps.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::ServiceReply;
use serde_json::json;

use crate::rollouts::Rollout;
use crate::rollouts_state_tests::in_second_stage;
use crate::testutil::version;

/// The position `in_second_stage` leaves on disk: 2.0, three stages entered, `d1` done.
fn second_stage_position() -> Rollout {
    Rollout {
        version: Some(version("2.0")),
        entered: 3,
        done: ["d1".to_owned()].into(),
        paused: None,
    }
}

/// Proves: a coordinator that restarts with the second stage entered resumes there. The position
/// on disk is version 2.0, three stages of the rollout file entered, `d1` done; after the restart
/// the plan is in stage 2 of 3; at its first beat `d1`, which has beaten once at 2.0 of the two
/// that bake needs, is Done (recorded, not re-baked) while `d2`, at 2.0 as well, is Baking; and
/// past the quiet start exactly one host is ordered, `d4`, the one place of the stage that is
/// left (`d1`, `d2` and `d3` run 2.0 and are not ordered again), with the position as it was and
/// no stage written again.
///
/// Catches: a restart that forgets the position and goes back to stage 1, one that makes `d1` bake
/// again from zero, or one that orders a host that is already at the desired version or none of
/// the stage's remaining hosts.
#[tokio::test]
async fn a_restart_resumes_at_the_stage_the_plan_had_entered() {
    let mut farm = in_second_stage("rollouts-restart-resume").await;
    // d2 and d3 were ordered at the beat that entered the stage; they report updating, and
    // their next beat is the first at 2.0.
    farm.round();
    assert_eq!(farm.running("2.0"), ["d1", "d2", "d3"]);
    assert_eq!(
        farm.kept(),
        Some(json!({"web": {"version": "2.0", "entered": 3, "done": ["d1"]}}))
    );
    farm.restart("bake_beats = 2");
    // Every host beats once within the quiet start, which rebuilds the fleet; nothing is ordered.
    farm.at += 5;
    assert_eq!(farm.round(), Vec::<String>::new());
    let plan = farm.plan();
    assert_eq!(plan.current, 2);
    assert_eq!(plan.stages.len(), 3);
    assert_eq!(plan.waiting(), proto::PlanWait::Quiet);
    let states = farm.states();
    assert_eq!(states["d1"], proto::PlanHostState::Done);
    assert_eq!(states["d2"], proto::PlanHostState::Baking);
    assert_eq!(states["d3"], proto::PlanHostState::Baking);

    // Past the quiet start the plan orders the one host its stage has a place for.
    farm.at += 20;
    assert_eq!(farm.round(), ["d4"]);
    assert_eq!(farm.plan().current, 2);
    assert_eq!(
        farm.rig.coordinator.ops().state().rollouts["web"],
        second_stage_position()
    );
    assert_eq!(farm.advances().len(), 1, "the restart enters nothing");
}

/// Proves: a host recorded done that runs the version is not ordered and does not bake again,
/// however many rounds follow a restart: from the first beat of the new coordinator to the end
/// of the roll `d1` is Done and its beat is answered with nothing; the hosts ordered, in all, are
/// `d4` of the second stage and then `d5` to `d8` of the third, never `d1`, `d2` or `d3`; the
/// roll ends with every host Done within 20 rounds; and the audit ring holds one more `rollout_advance` (the
/// third stage) since the restart.
///
/// Catches: a recorded host that is ordered the version it runs, one that is shown Baking after a
/// restart that its record should have spared, or a roll that stalls on the host it records.
#[tokio::test]
async fn a_host_recorded_done_that_runs_the_version_is_never_ordered_again() {
    let mut farm = in_second_stage("rollouts-restart-recorded-done").await;
    farm.round();
    farm.restart("bake_beats = 2");
    let before = farm.advances().len();
    // Every host beats within the quiet start, as real ones do: the plan counts all eight.
    farm.at += 5;
    for host in ["d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8"] {
        assert_eq!(farm.beat(host), ServiceReply::Nothing, "{host}: quiet");
    }
    assert_eq!(farm.states()["d1"], proto::PlanHostState::Done);
    farm.at += 20;

    let mut ordered = Vec::new();
    let mut rounds = 0;
    while farm.running("2.0").len() < 8
        || farm
            .states()
            .values()
            .any(|s| *s != proto::PlanHostState::Done)
    {
        rounds += 1;
        assert!(rounds <= 20, "the roll did not finish within 20 rounds");
        ordered.extend(farm.round());
        assert_eq!(farm.states()["d1"], proto::PlanHostState::Done);
    }
    assert_eq!(ordered, ["d4", "d5", "d6", "d7", "d8"]);
    assert_eq!(
        farm.advances().len() - before,
        1,
        "the third stage is the only one entered after the restart"
    );
}
