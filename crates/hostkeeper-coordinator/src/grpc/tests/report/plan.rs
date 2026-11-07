//! The update plan in the operator view of a service (`crate::grpc::report`): the plan carried to
//! `ServiceStatus.plan` and the stage of each host. The helpers are those of the parent module.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::{mapped, plain_host, svc, svc_host};
use crate::ops::OpsState;
use crate::testutil::beat;

/// A plan of two stages: stage 1 takes `d0`, stage 2 takes `d1` and `d2`.
fn two_stage_plan() -> proto::RolloutPlan {
    let member = |node: &str, state: proto::PlanHostState| proto::PlanHost {
        node_id: node.to_owned(),
        role: "storage".to_owned(),
        state: state.into(),
        deferring: false,
    };
    proto::RolloutPlan {
        version: "2.0".to_owned(),
        stages: vec![
            proto::PlanStage {
                number: 1,
                percent: Some(34),
                hosts: 1,
                advance: proto::StageAdvance::Auto.into(),
                members: vec![member("d0", proto::PlanHostState::Done)],
            },
            proto::PlanStage {
                number: 2,
                percent: Some(100),
                hosts: 3,
                advance: proto::StageAdvance::Manual.into(),
                members: vec![
                    member("d1", proto::PlanHostState::Waiting),
                    member("d2", proto::PlanHostState::Waiting),
                ],
            },
        ],
        current: 1,
        eligible: 3,
        waiting: proto::PlanWait::ManualAdvance.into(),
        ..proto::RolloutPlan::default()
    }
}

/// The service's plan passes through to `ServiceStatus.plan` as the coordinator made it, and a
/// service with no plan has none.
#[test]
fn the_service_plan_is_carried_to_the_status() {
    let mut one = svc(vec![svc_host("d1", "storage", beat("2.0"))]);
    one.plan = Some(two_stage_plan());

    let out = mapped(one, OpsState::default());

    assert_eq!(out.plan, Some(two_stage_plan()));

    let bare = mapped(
        svc(vec![svc_host("d1", "storage", beat("2.0"))]),
        OpsState::default(),
    );
    assert_eq!(bare.plan, None);
}

/// A host's `HostService.stage` is the number of the stage of the service's plan that lists it;
/// a host the plan lists in no stage, and a service with no plan, give none.
#[test]
fn a_host_view_carries_the_stage_of_the_plan_that_takes_it() {
    let mut one = svc(vec![
        svc_host("d0", "storage", beat("2.0")),
        svc_host("d1", "storage", beat("1.0")),
        svc_host("d9", "storage", beat("1.0")),
    ]);
    one.plan = Some(two_stage_plan());

    let out = mapped(one, OpsState::default());

    let stage_of = |node: &str| {
        out.hosts
            .iter()
            .find(|h| h.node_id == node)
            .and_then(|h| h.view.as_ref())
            .map(|view| view.stage)
    };
    assert_eq!(stage_of("d0"), Some(Some(1)));
    assert_eq!(stage_of("d1"), Some(Some(2)));
    assert_eq!(stage_of("d9"), Some(None), "d9 is in no stage");

    let planless = plain_host(beat("2.0"));
    assert_eq!(planless.view.expect("view").stage, None);
}
