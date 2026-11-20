//! `Maintenance` on the operator API end to end, through `crate::grpc::route` as the private
//! listener serves it, once over gRPC and once over the JSON gateway: `Schedule`, `List`, `Get`
//! and `Cancel` over hosts that heartbeat a maintenance report, the status of each refusal, the
//! operator's credential alone, and `RetryHost` and `PutBack`, which answer `UNIMPLEMENTED`. No
//! socket.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::maintenance_server::Maintenance as Api;
use hostkeeper_proto::hostkeeper::operator::v1::{
    MaintenanceHostStep as HostStep, MaintenanceSkip, MaintenanceState,
};
use prost::Message;
use tonic::Code;

use super::heartbeat::{beat, proto_message, svc, updater};
use super::rollouts::{Refused, WIRES, Wire, rpc};
use super::support::{call, grpc_request};
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::grpc::operator_maintenance::MaintenanceService;
use crate::host_groups_support::{SELF, UNIX, body, coordinator_on, enroll_host, move_of, put_of};
use crate::http::Surface;
use crate::maintenance::FEATURE;
use crate::marks_support::operator;
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::{OPERATOR, Rig, ci_token};

const SCHEDULE: &str = "/hostkeeper.operator.v1.Maintenance/Schedule";
const LIST: &str = "/hostkeeper.operator.v1.Maintenance/List";
const GET: &str = "/hostkeeper.operator.v1.Maintenance/Get";
const CANCEL: &str = "/hostkeeper.operator.v1.Maintenance/Cancel";
const RETRY_HOST: &str = "/hostkeeper.operator.v1.Maintenance/RetryHost";
const PUT_BACK: &str = "/hostkeeper.operator.v1.Maintenance/PutBack";

/// The id of the first run a world stores: its clock reads [`UNIX`].
const FIRST: &str = "run-1900000000-1";

/// The id of the second run a world stores.
const SECOND: &str = "run-1900000000-2";

/// A coordinator the tests call through the router. The group `alpha` holds the enrolled hosts
/// `a1`, `a2` and `a3`; `b1` is enrolled and in no group.
struct Wired {
    coordinator: Arc<Coordinator>,
    _enrollment: EnrollmentRig,
    _rig: Rig,
}

impl Wired {
    /// A world named `name`, none of whose hosts has been heard.
    fn new(name: &str) -> Self {
        let enrollment = EnrollmentRig::over_records(
            &format!("{name}-enrollment"),
            Arc::new(MemoryRecords::new()),
        );
        let fake = Fake::new();
        let rig = Rig::new(name);
        let coordinator =
            coordinator_on(&fake, &rig.clock, Some(Arc::clone(&enrollment.enrollment)));
        rig.at(2_000);
        coordinator
            .host_groups_put(
                &operator(),
                put_of(body("alpha", &[SELF, "web"], None), None),
            )
            .expect("a group");
        for host in ["a1", "a2", "a3", "b1"] {
            enroll_host(&enrollment.enrollment, host);
        }
        for host in ["a1", "a2", "a3"] {
            coordinator
                .host_groups_move(&operator(), move_of(host, "alpha"))
                .expect("moved");
        }
        Self {
            coordinator: Arc::new(coordinator),
            _enrollment: enrollment,
            _rig: rig,
        }
    }

    /// A heartbeat of `host` over gRPC that announces `maintenance-v1` and reports `due` as due.
    async fn beat(&self, host: &str, due: &[agent::MaintenanceKind]) {
        let mut message = proto_message(&[svc("web", "1.0")]);
        message.features.push(FEATURE.to_owned());
        message.maintenance = Some(agent::MaintenanceReport {
            due: due
                .iter()
                .map(|kind| agent::DueItem {
                    kind: *kind as i32,
                    from: "1.0".to_owned(),
                    to: "1.1".to_owned(),
                    needs_reboot: false,
                    label: "update".to_owned(),
                    security: false,
                })
                .collect(),
            looked_unix: UNIX,
            ..agent::MaintenanceReport::default()
        });
        self.send(host, &message).await;
    }

    /// A heartbeat of `host` that does not announce `maintenance-v1`.
    async fn beat_plain(&self, host: &str) {
        self.send(host, &proto_message(&[svc("web", "1.0")])).await;
    }

    async fn send(&self, host: &str, message: &agent::HeartbeatRequest) {
        let reply = beat(&self.coordinator, &updater(host, "storage"), message).await;
        assert_eq!(reply.token(), (Code::Ok, ""), "the heartbeat of {host}");
    }

    /// The call `path` with `request` and the operator's token over `wire`.
    async fn ask<R: Message + Default>(
        &self,
        wire: Wire,
        path: &str,
        request: &impl Message,
    ) -> Result<R, Refused> {
        rpc(&self.coordinator, wire, path, Some(OPERATOR), request).await
    }

    /// The refusal of `Schedule` of `request` over `wire`.
    async fn refused(&self, wire: Wire, request: &proto::ScheduleMaintenanceRequest) -> Refused {
        self.ask::<proto::ScheduleMaintenanceResponse>(wire, SCHEDULE, request)
            .await
            .expect_err("refused")
    }

    /// How many entries the audit ring holds.
    fn audited(&self) -> usize {
        self.coordinator.ops().state().audit.len()
    }

    /// How many runs the ops store holds.
    fn stored(&self) -> usize {
        self.coordinator.ops().state().maintenance_runs.len()
    }
}

/// A `Schedule` of the group `alpha`.
fn group_run(confirm: bool, reason: &str) -> proto::ScheduleMaintenanceRequest {
    proto::ScheduleMaintenanceRequest {
        host_group: "alpha".to_owned(),
        reason: reason.to_owned(),
        confirm,
        ..proto::ScheduleMaintenanceRequest::default()
    }
}

/// A `Schedule` of `hosts`, confirmed with the reason `because`.
fn hosts_run(hosts: &[&str]) -> proto::ScheduleMaintenanceRequest {
    proto::ScheduleMaintenanceRequest {
        host_ids: hosts.iter().map(|host| (*host).to_owned()).collect(),
        reason: "because".to_owned(),
        confirm: true,
        ..proto::ScheduleMaintenanceRequest::default()
    }
}

fn get_of(id: &str) -> proto::GetMaintenanceRunRequest {
    proto::GetMaintenanceRunRequest {
        run_id: id.to_owned(),
    }
}

fn cancel_of(id: &str, reason: &str) -> proto::CancelMaintenanceRunRequest {
    proto::CancelMaintenanceRunRequest {
        run_id: id.to_owned(),
        reason: reason.to_owned(),
    }
}

/// `hosts` as (host, step, kinds) rows.
fn rows(hosts: &[proto::MaintenanceHost]) -> Vec<(String, HostStep, Vec<i32>)> {
    hosts
        .iter()
        .map(|host| (host.host_id.clone(), host.step(), host.kinds.clone()))
        .collect()
}

fn row(
    host: &str,
    step: HostStep,
    kinds: &[agent::MaintenanceKind],
) -> (String, HostStep, Vec<i32>) {
    (
        host.to_owned(),
        step,
        kinds.iter().map(|kind| *kind as i32).collect(),
    )
}

fn refused(code: Code, token: &str) -> Refused {
    (code, token.to_owned())
}

/// Proves, over gRPC and over the gateway, on a group whose hosts `a1` and `a2` report something
/// due and `a3` has not been heard: `Schedule` with `confirm` answers `OK` with the stored run
/// (its id, `RUNNING`, the group, the batch and drain deadline as asked, the reason, its time,
/// two hosts), both hosts waiting with the kind due, and the `maintenance_schedule` entry, whose
/// host group is the group and whose reason is the operator's followed by `run <id>`; `List`
/// answers that run; `Get` answers its hosts; `Cancel` answers the run `CANCELLED` with the
/// `maintenance_cancel` entry, each waiting host ends `NOT_REACHED` at that time, and a second
/// `Cancel` is `FAILED_PRECONDITION` `run_ended`.
///
/// Catches: an adapter that answers another handler's message, drops a field of the request,
/// leaves out an entry, or lets an ended run be cancelled again.
#[tokio::test]
async fn schedule_list_get_and_cancel_a_group_run_over_both_wires() {
    for (n, wire) in WIRES.into_iter().enumerate() {
        let world = Wired::new(&format!("grpc-maintenance-runs-flow-{n}"));
        world.beat("a1", &[agent::MaintenanceKind::Linux]).await;
        world.beat("a2", &[agent::MaintenanceKind::Linux]).await;
        let base = world.audited();
        let linux = [agent::MaintenanceKind::Linux];

        let mut request = group_run(true, "monthly patching");
        request.batch = Some(1);
        request.drain_deadline_secs = Some(600);
        let scheduled: proto::ScheduleMaintenanceResponse = world
            .ask(wire, SCHEDULE, &request)
            .await
            .expect("scheduled");
        let run = scheduled.run.expect("the run");
        assert_eq!(run.id, FIRST, "{wire:?}");
        assert_eq!(run.state(), MaintenanceState::Running, "{wire:?}");
        assert_eq!(
            (
                run.host_group.as_str(),
                run.host_ids.len(),
                run.batch,
                run.drain_deadline_secs,
                run.reason.as_str(),
                run.scheduled_unix,
                run.host_count
            ),
            ("alpha", 0, Some(1), Some(600), "monthly patching", UNIX, 2),
            "{wire:?}"
        );
        assert_eq!(
            rows(&scheduled.hosts),
            vec![
                row("a1", HostStep::Waiting, &linux),
                row("a2", HostStep::Waiting, &linux)
            ],
            "{wire:?}"
        );
        let entry = scheduled.entry.expect("the entry");
        assert_eq!(
            (
                entry.action.as_str(),
                entry.host_group.as_deref(),
                entry.reason.as_deref()
            ),
            (
                "maintenance_schedule",
                Some("alpha"),
                Some("monthly patching run run-1900000000-1")
            ),
            "{wire:?}"
        );
        assert_eq!((world.stored(), world.audited()), (1, base + 1), "{wire:?}");

        let listed: proto::ListMaintenanceRunsResponse = world
            .ask(wire, LIST, &proto::ListMaintenanceRunsRequest::default())
            .await
            .expect("listed");
        assert_eq!(listed.runs, vec![run.clone()], "{wire:?}");

        let got: proto::GetMaintenanceRunResponse =
            world.ask(wire, GET, &get_of(FIRST)).await.expect("got");
        assert_eq!(got.run, Some(run.clone()), "{wire:?}");
        assert_eq!(got.hosts, scheduled.hosts, "{wire:?}");

        let cancelled: proto::CancelMaintenanceRunResponse = world
            .ask(wire, CANCEL, &cancel_of(FIRST, "freeze"))
            .await
            .expect("cancelled");
        let ended = cancelled.run.expect("the run");
        assert_eq!(
            (ended.id.as_str(), ended.state(), ended.ended_unix),
            (FIRST, MaintenanceState::Cancelled, UNIX),
            "{wire:?}"
        );
        let entry = cancelled.entry.expect("the entry");
        assert_eq!(
            (
                entry.action.as_str(),
                entry.host_group.as_deref(),
                entry.reason.as_deref()
            ),
            (
                "maintenance_cancel",
                None,
                Some("freeze run run-1900000000-1")
            ),
            "{wire:?}"
        );
        assert_eq!(world.audited(), base + 2, "{wire:?}");

        let got: proto::GetMaintenanceRunResponse =
            world.ask(wire, GET, &get_of(FIRST)).await.expect("got");
        assert_eq!(
            got.hosts
                .iter()
                .map(|host| (host.host_id.as_str(), host.step(), host.since_unix))
                .collect::<Vec<_>>(),
            vec![
                ("a1", HostStep::NotReached, UNIX),
                ("a2", HostStep::NotReached, UNIX)
            ],
            "{wire:?}"
        );
        let again = world
            .ask::<proto::CancelMaintenanceRunResponse>(wire, CANCEL, &cancel_of(FIRST, "again"))
            .await
            .expect_err("ended");
        assert_eq!(again, refused(Code::FailedPrecondition, "run_ended"));
        assert_eq!(world.audited(), base + 2, "{wire:?}");
    }
}

/// Proves, over gRPC and over the gateway: `Schedule` without `confirm` answers the run as it
/// would be (no id, `RUNNING`, its hosts) with no entry, and stores no run and writes no audit
/// entry; `List` then answers no run.
///
/// Catches: a preview that stores the run or audits it, or one that carries an id.
#[tokio::test]
async fn a_schedule_without_confirm_answers_the_run_and_stores_nothing() {
    for (n, wire) in WIRES.into_iter().enumerate() {
        let world = Wired::new(&format!("grpc-maintenance-runs-preview-{n}"));
        world.beat("a1", &[agent::MaintenanceKind::Linux]).await;
        world.beat("a2", &[agent::MaintenanceKind::Reboot]).await;
        let base = world.audited();

        let preview: proto::ScheduleMaintenanceResponse = world
            .ask(wire, SCHEDULE, &group_run(false, ""))
            .await
            .expect("a preview");
        let run = preview.run.expect("the run");
        assert_eq!(
            (run.id.as_str(), run.state(), run.host_count, preview.entry),
            ("", MaintenanceState::Running, 2, None),
            "{wire:?}"
        );
        assert_eq!(
            rows(&preview.hosts),
            vec![
                row("a1", HostStep::Waiting, &[agent::MaintenanceKind::Linux]),
                row("a2", HostStep::Waiting, &[agent::MaintenanceKind::Reboot])
            ],
            "{wire:?}"
        );
        assert_eq!((world.stored(), world.audited()), (0, base), "{wire:?}");
        let listed: proto::ListMaintenanceRunsResponse = world
            .ask(wire, LIST, &proto::ListMaintenanceRunsRequest::default())
            .await
            .expect("listed");
        assert_eq!(listed.runs, Vec::<proto::MaintenanceRun>::new(), "{wire:?}");
    }
}

/// Proves, over gRPC and over the gateway, that `Schedule` is refused with the status and token
/// of its value and stores nothing: a request naming nothing, a kind unspecified, a kind twice
/// and a confirm with no reason `INVALID_ARGUMENT` `invalid_request`; an unknown group
/// `NOT_FOUND` `unknown_group`; an unknown host `NOT_FOUND` `unknown_host`; a window that has
/// ended, a host never heard, a kind nothing is due of, all the hosts of a group without
/// `take_last_host` (and with it, taken) and a host already in a run `FAILED_PRECONDITION` with
/// `window_ended`, `nothing_due`, `last_host` and `host_in_maintenance`; and a host heard that
/// does not announce `maintenance-v1` is in the run, skipped with `NO_MAINTENANCE_V1`.
///
/// Catches: a refusal with another code than its value names, a run stored by a refused call, a
/// kind filter that does not filter, and a host without the feature dropped from the run.
#[tokio::test]
async fn schedule_refusals_have_the_status_of_their_value_and_store_nothing() {
    use agent::MaintenanceKind::{Linux, Reboot};

    for (n, wire) in WIRES.into_iter().enumerate() {
        let world = Wired::new(&format!("grpc-maintenance-runs-refusals-{n}"));
        world.beat("a1", &[Linux]).await;
        world.beat("a2", &[Linux]).await;
        world.beat_plain("b1").await;
        let base = world.audited();

        let invalid = refused(Code::InvalidArgument, "invalid_request");
        let mut unspecified = group_run(true, "r");
        unspecified.kinds = vec![0];
        let mut twice = group_run(true, "r");
        twice.kinds = vec![Linux as i32, Linux as i32];
        for request in [
            proto::ScheduleMaintenanceRequest::default(),
            unspecified,
            twice,
            group_run(true, ""),
        ] {
            assert_eq!(world.refused(wire, &request).await, invalid, "{wire:?}");
        }

        let mut request = group_run(true, "r");
        request.host_group = "nosuch".to_owned();
        assert_eq!(
            world.refused(wire, &request).await,
            refused(Code::NotFound, "unknown_group")
        );
        assert_eq!(
            world.refused(wire, &hosts_run(&["zz9"])).await,
            refused(Code::NotFound, "unknown_host")
        );
        let mut request = group_run(true, "r");
        request.window_start_unix = 1_000;
        request.window_end_unix = UNIX - 1;
        assert_eq!(
            world.refused(wire, &request).await,
            refused(Code::FailedPrecondition, "window_ended")
        );
        let nothing_due = refused(Code::FailedPrecondition, "nothing_due");
        assert_eq!(world.refused(wire, &hosts_run(&["a3"])).await, nothing_due);
        let mut request = group_run(true, "r");
        request.kinds = vec![Reboot as i32];
        assert_eq!(world.refused(wire, &request).await, nothing_due, "{wire:?}");
        assert_eq!((world.stored(), world.audited()), (0, base), "{wire:?}");

        world.beat("a3", &[Linux]).await;
        assert_eq!(
            world.refused(wire, &group_run(true, "r")).await,
            refused(Code::FailedPrecondition, "last_host"),
            "{wire:?}"
        );
        let mut request = group_run(false, "");
        request.take_last_host = true;
        let taken: proto::ScheduleMaintenanceResponse = world
            .ask(wire, SCHEDULE, &request)
            .await
            .expect("every host taken");
        assert_eq!(taken.hosts.len(), 3, "{wire:?}");
        assert_eq!(world.stored(), 0, "{wire:?}");

        let mixed: proto::ScheduleMaintenanceResponse = world
            .ask(wire, SCHEDULE, &hosts_run(&["a1", "b1"]))
            .await
            .expect("scheduled");
        assert_eq!(
            rows(&mixed.hosts),
            vec![
                row("a1", HostStep::Waiting, &[Linux]),
                row("b1", HostStep::Skipped, &[])
            ],
            "{wire:?}"
        );
        assert_eq!(
            mixed.hosts[1].skipped_because(),
            MaintenanceSkip::NoMaintenanceV1,
            "{wire:?}"
        );
        assert_eq!(
            (mixed.hosts[0].skipped_because(), mixed.entry.is_some()),
            (MaintenanceSkip::Unspecified, true),
            "{wire:?}"
        );
        let entry = mixed.entry.expect("the entry");
        assert_eq!(entry.host_group, None, "{wire:?}");
        assert_eq!(world.stored(), 1, "{wire:?}");
        assert_eq!(
            world.refused(wire, &hosts_run(&["a1"])).await,
            refused(Code::FailedPrecondition, "host_in_maintenance"),
            "{wire:?}"
        );
        assert_eq!(world.stored(), 1, "{wire:?}");
    }
}

/// Proves, over gRPC and over the gateway: `List` answers the newest run first, at most `limit`
/// of them, and only those in the states asked; a state `UNSPECIFIED` is `INVALID_ARGUMENT`
/// `invalid_request`, over gRPC so is a state number the proto does not know, and a request with
/// nothing set answers `OK`.
///
/// Catches: runs listed oldest first, a limit or a state filter ignored, and a state the proto
/// does not name taken as a filter.
#[tokio::test]
async fn list_answers_the_newest_first_within_its_limit_and_states() {
    for (n, wire) in WIRES.into_iter().enumerate() {
        let world = Wired::new(&format!("grpc-maintenance-runs-list-{n}"));
        world.beat("a1", &[agent::MaintenanceKind::Linux]).await;
        world.beat("a2", &[agent::MaintenanceKind::Linux]).await;
        for hosts in [["a1"], ["a2"]] {
            let _: proto::ScheduleMaintenanceResponse = world
                .ask(wire, SCHEDULE, &hosts_run(&hosts))
                .await
                .expect("scheduled");
        }
        let _: proto::CancelMaintenanceRunResponse = world
            .ask(wire, CANCEL, &cancel_of(FIRST, "freeze"))
            .await
            .expect("cancelled");
        let ids = |request: proto::ListMaintenanceRunsRequest| {
            let world = &world;
            async move {
                let listed: proto::ListMaintenanceRunsResponse =
                    world.ask(wire, LIST, &request).await.expect("listed");
                listed
                    .runs
                    .into_iter()
                    .map(|run| run.id)
                    .collect::<Vec<_>>()
            }
        };
        let states = |states: &[MaintenanceState]| proto::ListMaintenanceRunsRequest {
            states: states.iter().map(|state| *state as i32).collect(),
            limit: 0,
        };
        assert_eq!(
            ids(proto::ListMaintenanceRunsRequest::default()).await,
            vec![SECOND, FIRST],
            "{wire:?}"
        );
        assert_eq!(
            ids(proto::ListMaintenanceRunsRequest {
                states: vec![],
                limit: 1
            })
            .await,
            vec![SECOND],
            "{wire:?}"
        );
        assert_eq!(
            ids(states(&[MaintenanceState::Cancelled])).await,
            vec![FIRST],
            "{wire:?}"
        );
        assert_eq!(
            ids(states(&[MaintenanceState::Running])).await,
            vec![SECOND],
            "{wire:?}"
        );
        assert_eq!(
            ids(states(&[MaintenanceState::Done])).await,
            Vec::<String>::new(),
            "{wire:?}"
        );
        let unspecified = world
            .ask::<proto::ListMaintenanceRunsResponse>(
                wire,
                LIST,
                &states(&[MaintenanceState::Unspecified]),
            )
            .await
            .expect_err("refused");
        assert_eq!(
            unspecified,
            refused(Code::InvalidArgument, "invalid_request"),
            "{wire:?}"
        );
    }
    let world = Wired::new("grpc-maintenance-runs-list-unknown-state");
    let unknown = proto::ListMaintenanceRunsRequest {
        states: vec![99],
        limit: 0,
    };
    let answer = world
        .ask::<proto::ListMaintenanceRunsResponse>(Wire::Grpc, LIST, &unknown)
        .await
        .expect_err("refused");
    assert_eq!(answer, refused(Code::InvalidArgument, "invalid_request"));
}

/// Proves, over gRPC and over the gateway: `Get` and `Cancel` of a run id that is not one (none,
/// or over 64 bytes) are `INVALID_ARGUMENT` `invalid_request`, `Cancel` with no reason too, and
/// of a run no one scheduled `NOT_FOUND` `unknown_run`; none of them changes anything.
///
/// Catches: an id taken without its rule, and a missing run answered as an empty one.
#[tokio::test]
async fn get_and_cancel_refuse_a_bad_id_a_missing_reason_and_an_unknown_run() {
    for (n, wire) in WIRES.into_iter().enumerate() {
        let world = Wired::new(&format!("grpc-maintenance-runs-ids-{n}"));
        world.beat("a1", &[agent::MaintenanceKind::Linux]).await;
        let _: proto::ScheduleMaintenanceResponse = world
            .ask(wire, SCHEDULE, &hosts_run(&["a1"]))
            .await
            .expect("scheduled");
        let base = world.audited();
        let long = "r".repeat(65);
        let invalid = refused(Code::InvalidArgument, "invalid_request");
        let unknown = refused(Code::NotFound, "unknown_run");
        for (id, expected) in [
            ("", &invalid),
            (long.as_str(), &invalid),
            ("run-1-1", &unknown),
        ] {
            let got = world
                .ask::<proto::GetMaintenanceRunResponse>(wire, GET, &get_of(id))
                .await
                .expect_err("refused");
            assert_eq!(&got, expected, "Get {id:?} {wire:?}");
            let cancelled = world
                .ask::<proto::CancelMaintenanceRunResponse>(wire, CANCEL, &cancel_of(id, "why"))
                .await
                .expect_err("refused");
            assert_eq!(&cancelled, expected, "Cancel {id:?} {wire:?}");
        }
        let no_reason = world
            .ask::<proto::CancelMaintenanceRunResponse>(wire, CANCEL, &cancel_of(FIRST, ""))
            .await
            .expect_err("refused");
        assert_eq!(no_reason, invalid, "{wire:?}");
        assert_eq!(world.audited(), base, "{wire:?}");
        let got: proto::GetMaintenanceRunResponse = world
            .ask(wire, GET, &get_of(FIRST))
            .await
            .expect("still there");
        assert_eq!(
            got.run.map(|run| run.state()),
            Some(MaintenanceState::Running),
            "{wire:?}"
        );
    }
}

/// Proves, over gRPC and over the gateway: each of `Schedule`, `List`, `Get` and `Cancel` refuses
/// CI's token `PERMISSION_DENIED` `forbidden` and no credential `UNAUTHENTICATED` `unauthorized`,
/// before the request is read, and changes nothing.
///
/// Catches: a method that admits CI's token, or one that reads the request before the caller.
#[tokio::test]
async fn only_the_operator_reaches_the_maintenance_calls() {
    for (n, wire) in WIRES.into_iter().enumerate() {
        let world = Wired::new(&format!("grpc-maintenance-runs-callers-{n}"));
        world.beat("a1", &[agent::MaintenanceKind::Linux]).await;
        let base = world.audited();
        for (ci, code, token) in [
            (true, Code::PermissionDenied, "forbidden"),
            (false, Code::Unauthenticated, "unauthorized"),
        ] {
            // A CI token's id is spent by its first call, so each call is given a fresh one: a
            // token used twice is a replay, refused `unauthorized` whatever the method.
            let coordinator = &world.coordinator;
            let answers = [
                (
                    SCHEDULE,
                    rpc::<proto::ScheduleMaintenanceResponse>(
                        coordinator,
                        wire,
                        SCHEDULE,
                        ci.then(ci_token).as_deref(),
                        &hosts_run(&["a1"]),
                    )
                    .await,
                ),
                (
                    LIST,
                    rpc(
                        coordinator,
                        wire,
                        LIST,
                        ci.then(ci_token).as_deref(),
                        &proto::ListMaintenanceRunsRequest::default(),
                    )
                    .await,
                ),
                (
                    GET,
                    rpc(
                        coordinator,
                        wire,
                        GET,
                        ci.then(ci_token).as_deref(),
                        &get_of(FIRST),
                    )
                    .await,
                ),
                (
                    CANCEL,
                    rpc(
                        coordinator,
                        wire,
                        CANCEL,
                        ci.then(ci_token).as_deref(),
                        &cancel_of(FIRST, "why"),
                    )
                    .await,
                ),
            ];
            for (path, answer) in answers {
                let answer = answer.expect_err("refused");
                assert_eq!(answer, refused(code, token), "{path} ci {ci} {wire:?}");
            }
        }
        assert_eq!((world.stored(), world.audited()), (0, base), "{wire:?}");
    }
}

/// Proves: `RetryHost` and `PutBack` answer `UNIMPLEMENTED` with an empty message and no response
/// message on the private listener, with the operator's token, with CI's and with none (the
/// router refuses before it reads a credential); and the service itself, called directly,
/// answers `UNIMPLEMENTED` with an empty message to each.
///
/// Catches: a method served before its handler lands, and an adapter that answers a call of
/// either with a message or a token.
#[tokio::test]
async fn retry_host_and_put_back_answer_unimplemented() {
    let world = Wired::new("grpc-maintenance-runs-unimplemented");
    let ci = ci_token();
    for path in [RETRY_HOST, PUT_BACK] {
        for bearer in [Some(OPERATOR), Some(ci.as_str()), None] {
            let request = grpc_request(path, (), bearer);
            let answer = call(&world.coordinator, Surface::Control, &None, request).await;
            assert_eq!(
                answer,
                (Code::Unimplemented, String::new(), None),
                "{path} {bearer:?}"
            );
        }
    }

    let service = MaintenanceService::new(Arc::clone(&world.coordinator));
    let retried = service
        .retry_host(tonic::Request::new(
            proto::RetryMaintenanceHostRequest::default(),
        ))
        .await
        .expect_err("not served");
    assert_eq!(
        (retried.code(), retried.message()),
        (Code::Unimplemented, "")
    );
    let put_back = service
        .put_back(tonic::Request::new(
            proto::PutBackMaintenanceHostRequest::default(),
        ))
        .await
        .expect_err("not served");
    assert_eq!(
        (put_back.code(), put_back.message()),
        (Code::Unimplemented, "")
    );
}
