//! `RetryHost` and `PutBack` of a halted host end to end, once over gRPC and once over the JSON
//! gateway, in the world of the parent module.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceHostStep as HostStep;
use tonic::Code;

use super::{
    FIRST, PUT_BACK, RETRY_HOST, SCHEDULE, Wired, hosts_run, maintain_order, refused, run_state,
};
use crate::grpc::tests::rollouts::{WIRES, Wire, rpc};
use crate::testutil::ci_token;

fn retry_of(run: &str, host: &str, reason: &str) -> proto::RetryMaintenanceHostRequest {
    proto::RetryMaintenanceHostRequest {
        run_id: run.to_owned(),
        host_id: host.to_owned(),
        reason: reason.to_owned(),
    }
}

fn put_back_of(run: &str, host: &str, reason: &str) -> proto::PutBackMaintenanceHostRequest {
    proto::PutBackMaintenanceHostRequest {
        run_id: run.to_owned(),
        host_id: host.to_owned(),
        reason: reason.to_owned(),
    }
}

/// `RetryHost` and `PutBack` of `host` in `run` over `wire`, each refused with `code` and `token`.
async fn refused_both(world: &Wired, wire: Wire, run: &str, host: &str, code: Code, token: &str) {
    let retried = world
        .ask::<proto::RetryMaintenanceHostResponse>(wire, RETRY_HOST, &retry_of(run, host, "why"))
        .await
        .expect_err("refused");
    let put = world
        .ask::<proto::PutBackMaintenanceHostResponse>(
            wire,
            PUT_BACK,
            &put_back_of(run, host, "why"),
        )
        .await
        .expect_err("refused");
    let expected = refused(code, token);
    assert_eq!(
        (&retried, &put),
        (&expected, &expected),
        "{run} {host} {wire:?}"
    );
}

/// Proves, over gRPC and over the gateway, with a run of `a1` whose daemon reports its turn
/// halted: `RetryHost` answers `OK` with the host as stored (`HALTED`, its reason) and the
/// `maintenance_retry_host` entry (the host as `node_id`, the group, the reason followed by
/// `run <id>`), and `a1`'s next gRPC heartbeat is answered with the `maintain` of its turn with
/// the action `RETRY` under try 2; `PutBack` the same with `maintenance_put_back` and `PUT_BACK`
/// under try 3; a host the run does not hold is `NOT_FOUND` `not_in_run`, a bad host id
/// `INVALID_ARGUMENT` `invalid_request`, and once `a1` reports its turn applying each call is
/// `FAILED_PRECONDITION` `not_halted` and `a1` is sent no order, none of them changing anything;
/// CI's token is `PERMISSION_DENIED` `forbidden` and none `UNAUTHENTICATED` `unauthorized` at both.
///
/// Catches: an adapter that answers one call with the other's handler, drops a field of the
/// request, or leaves out an entry, and an act that the heartbeat handler does not carry to the
/// wire.
#[tokio::test]
async fn retry_host_and_put_back_queue_an_order_for_a_halted_host_over_both_wires() {
    use agent::MaintainAction as Action;

    for (n, wire) in WIRES.into_iter().enumerate() {
        let world = Wired::new(&format!("grpc-maintenance-runs-acts-{n}"));
        world.beat("a1", &[agent::MaintenanceKind::Linux]).await;
        // A second host of `alpha` that serves, so `a1` may be taken (`min_serving` 1).
        world.beat("a2", &[]).await;
        let _: proto::ScheduleMaintenanceResponse = world
            .ask(wire, SCHEDULE, &hosts_run(&["a1"]))
            .await
            .expect("scheduled");
        assert_eq!(
            world.told("a1", None).await,
            Some(maintain_order(Action::Run, 1)),
            "{wire:?}"
        );
        let halted = || run_state(agent::MaintenanceStep::Halted, "apply linux: exit 1");
        assert_eq!(world.told("a1", halted()).await, None, "{wire:?}");
        let base = world.audited();

        let retried: proto::RetryMaintenanceHostResponse = world
            .ask(wire, RETRY_HOST, &retry_of(FIRST, "a1", "try again"))
            .await
            .expect("queued");
        let host = retried.host.expect("the host");
        assert_eq!(
            (host.host_id.as_str(), host.step(), host.reason.as_str()),
            ("a1", HostStep::Halted, "apply linux: exit 1"),
            "{wire:?}"
        );
        let entry = retried.entry.expect("the entry");
        assert_eq!(
            (
                entry.action.as_str(),
                entry.node_id.as_deref(),
                entry.host_group.as_deref(),
                entry.reason.as_deref()
            ),
            (
                "maintenance_retry_host",
                Some("a1"),
                Some("alpha"),
                Some("try again run run-1900000000-1")
            ),
            "{wire:?}"
        );
        assert_eq!(
            world.told("a1", halted()).await,
            Some(maintain_order(Action::Retry, 2)),
            "{wire:?}"
        );

        let put: proto::PutBackMaintenanceHostResponse = world
            .ask(wire, PUT_BACK, &put_back_of(FIRST, "a1", "give up"))
            .await
            .expect("queued");
        assert_eq!(
            put.host.map(|host| host.step()),
            Some(HostStep::Halted),
            "{wire:?}"
        );
        let entry = put.entry.expect("the entry");
        assert_eq!(
            (
                entry.action.as_str(),
                entry.node_id.as_deref(),
                entry.reason.as_deref()
            ),
            (
                "maintenance_put_back",
                Some("a1"),
                Some("give up run run-1900000000-1")
            ),
            "{wire:?}"
        );
        assert_eq!(
            world.told("a1", halted()).await,
            Some(maintain_order(Action::PutBack, 3)),
            "{wire:?}"
        );
        assert_eq!(world.audited(), base + 2, "{wire:?}");

        refused_both(&world, wire, FIRST, "b1", Code::NotFound, "not_in_run").await;
        refused_both(
            &world,
            wire,
            FIRST,
            "A1",
            Code::InvalidArgument,
            "invalid_request",
        )
        .await;
        refused_both(&world, wire, "run-1-1", "a1", Code::NotFound, "unknown_run").await;
        assert_eq!(world.audited(), base + 2, "{wire:?}");

        let applying = run_state(agent::MaintenanceStep::Applying, "");
        assert_eq!(world.told("a1", applying).await, None, "{wire:?}");
        let base = world.audited();
        refused_both(
            &world,
            wire,
            FIRST,
            "a1",
            Code::FailedPrecondition,
            "not_halted",
        )
        .await;

        for (ci, code, token) in [
            (true, Code::PermissionDenied, "forbidden"),
            (false, Code::Unauthenticated, "unauthorized"),
        ] {
            let coordinator = &world.coordinator;
            let retried = rpc::<proto::RetryMaintenanceHostResponse>(
                coordinator,
                wire,
                RETRY_HOST,
                ci.then(ci_token).as_deref(),
                &retry_of(FIRST, "a1", "why"),
            )
            .await
            .expect_err("refused");
            let put = rpc::<proto::PutBackMaintenanceHostResponse>(
                coordinator,
                wire,
                PUT_BACK,
                ci.then(ci_token).as_deref(),
                &put_back_of(FIRST, "a1", "why"),
            )
            .await
            .expect_err("refused");
            assert_eq!(retried, refused(code, token), "RetryHost ci {ci} {wire:?}");
            assert_eq!(put, refused(code, token), "PutBack ci {ci} {wire:?}");
        }
        assert_eq!(world.audited(), base, "{wire:?}");
    }
}
