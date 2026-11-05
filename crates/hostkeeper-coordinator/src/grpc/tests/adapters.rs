//! The adapters of the operator API over the coordinator: each method answers what the
//! coordinator's handler answers, each refusal is the status of the value the cause names, a
//! method with no handler is `UNIMPLEMENTED`, and a call the router admitted no caller for is
//! refused as an anonymous one.

use std::collections::HashMap;
use std::fmt::Debug;

use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::audit_server::Audit as _;
use hostkeeper_proto::hostkeeper::operator::v1::hold_scope::Scope;
use hostkeeper_proto::hostkeeper::operator::v1::hosts_server::Hosts as _;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use hostkeeper_proto::hostkeeper::operator::v1::services_server::Services as _;
use tonic::{Code, Request, Response, Status};

use super::operator::{World, as_ci, as_operator, disk_full, state};
use crate::grpc::Admitted;
use crate::grpc::operator::{AuditService, HostsService, ReleasesService, ServicesService};
use crate::operator::Caller;
use crate::ops::{OpsState, OpsStore};
use crate::testutil::{Rig, with_kbf};

/// Two more services: `other`, and `floor`, which refuses a version below 1.0. `search` is a role
/// of `other` alone.
const EXTRA: &str = "[[service]]\nname = \"other\"\nroles = [\"daemon\", \"search\"]\n\n\
                     [[service]]\nname = \"floor\"\nroles = [\"daemon\"]\nmin_version = \"1.0\"\n";

/// Asserts that `result` is a refusal of `code` whose message is exactly `token`.
fn assert_refused<T: Debug>(result: Result<Response<T>, Status>, code: Code, token: &str) {
    let status = result.expect_err("the call is refused");
    assert_eq!((status.code(), status.message()), (code, token));
}

/// Asserts that `result` is the answer of a method no handler serves yet.
fn assert_pending<T: Debug>(result: Result<Response<T>, Status>) {
    assert_refused(result, Code::Unimplemented, "");
}

/// A call that carries the operator's token, but whose caller the router found to be CI.
fn as_ci_caller<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.extensions_mut().insert(Admitted::Operator {
        caller: Caller::Ci,
        bearer: String::new(),
    });
    request
}

/// A store whose every write fails.
fn unwritable() -> OpsStore {
    let writer = Box::new(|_: &OpsState| Err(disk_full()));
    OpsStore::with_writer(OpsState::default(), writer)
}

/// A world that manages kbf, `other` and `floor`; CI's token may set the services `allowed`
/// names, or only the legacy one when `allowed` is none.
fn world_of(name: &str, allowed: Option<&[&str]>) -> World {
    let rig = Rig::new(name);
    let started = match allowed {
        Some(names) => {
            let quoted: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
            let list = format!(
                "{}services = [{}]\n",
                config_text(STATIC_ISSUER),
                quoted.join(", ")
            );
            Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
        }
        None => Rig::start(&rig.dir, &rig.clock),
    };
    World::over(rig, started.with_services(with_kbf(EXTRA)))
}

/// The set of `version` for `service` as CI's gRPC call makes it.
fn set_request(service: &str, version: &str) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        service: service.to_owned(),
        desired: Some(state(version, &[])),
        ..Default::default()
    }
}

/// Sets `version` of `service` through the gRPC service as CI.
async fn set(
    world: &World,
    service: &str,
    version: &str,
) -> Result<Response<proto::SetDesiredResponse>, Status> {
    ReleasesService::new(world.coordinator.clone())
        .set_desired(as_ci(&world.coordinator, set_request(service, version)).await)
        .await
}

fn hosts(world: &World) -> HostsService {
    HostsService::new(world.coordinator.clone())
}

fn releases(world: &World) -> ReleasesService {
    ReleasesService::new(world.coordinator.clone())
}

fn services(world: &World) -> ServicesService {
    ServicesService::new(world.coordinator.clone())
}

/// Proves that a method no handler serves yet answers `UNIMPLEMENTED` with an empty message,
/// whoever calls.
#[tokio::test]
async fn a_method_no_handler_serves_yet_is_unimplemented_with_no_message() {
    let world = world_of("adapters-pending", None);
    let c = &world.coordinator;
    assert_pending(
        hosts(&world)
            .list(as_operator(c, proto::ListHostsRequest {}).await)
            .await,
    );
    assert_pending(
        hosts(&world)
            .get(as_operator(c, proto::GetHostRequest::default()).await)
            .await,
    );
    assert_pending(
        releases(&world)
            .unmark_bad(as_operator(c, proto::UnmarkBadRequest::default()).await)
            .await,
    );
    assert_pending(
        services(&world)
            .assign(as_operator(c, proto::AssignRequest::default()).await)
            .await,
    );
    assert_pending(
        services(&world)
            .unassign(as_operator(c, proto::UnassignRequest::default()).await)
            .await,
    );
    assert_pending(
        services(&world)
            .set_min_version(as_operator(c, proto::SetMinVersionRequest::default()).await)
            .await,
    );
}

/// Proves that `Hosts` refuses a call from no one `unauthorized`, and one from CI `forbidden`,
/// in each method that takes the caller from the router (`Exclude`, `Abort`) and each that
/// takes the bearer (`Include`, `SetLabels`, which the JSON route's check refuses).
#[tokio::test]
async fn hosts_refuses_a_caller_that_is_not_the_operator() {
    let world = world_of("adapters-hosts-callers", None);
    let exclude = || proto::ExcludeRequest {
        service: "kbf".to_owned(),
        node_id: "d1".to_owned(),
        reason: "disk swap".to_owned(),
        abandon: false,
    };
    let abort = || proto::AbortRequest {
        node_id: "d1".to_owned(),
        service: "kbf".to_owned(),
        reason: "stuck".to_owned(),
    };
    let include = || proto::IncludeRequest {
        service: "kbf".to_owned(),
        node_id: "d1".to_owned(),
    };
    let labels = || proto::SetLabelsRequest {
        node_id: "d1".to_owned(),
        labels: HashMap::new(),
    };
    assert_refused(
        hosts(&world).exclude(Request::new(exclude())).await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        hosts(&world).abort(Request::new(abort())).await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        hosts(&world).include(Request::new(include())).await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        hosts(&world).set_labels(Request::new(labels())).await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        hosts(&world).exclude(as_ci_caller(exclude())).await,
        Code::PermissionDenied,
        "forbidden",
    );
    assert_refused(
        hosts(&world).abort(as_ci_caller(abort())).await,
        Code::PermissionDenied,
        "forbidden",
    );
    assert!(world.file("desired.json.ops").is_none(), "nothing changed");
}

/// Proves the refusals of `Hosts` that come from the request: a service the coordinator does
/// not manage is `unknown_service` and a bad node id or label is `invalid_request` (as the
/// JSON route's `bad_request`), a host never heard from cannot be aborted (`unknown_host`),
/// and nothing is written.
#[tokio::test]
async fn hosts_refuses_a_request_the_coordinator_cannot_honour_with_the_value_for_it() {
    let world = world_of("adapters-hosts-requests", None);
    let c = &world.coordinator;
    assert_refused(
        hosts(&world)
            .include(
                as_operator(
                    c,
                    proto::IncludeRequest {
                        service: "nope".to_owned(),
                        node_id: "d1".to_owned(),
                    },
                )
                .await,
            )
            .await,
        Code::NotFound,
        "unknown_service",
    );
    assert_refused(
        hosts(&world)
            .include(
                as_operator(
                    c,
                    proto::IncludeRequest {
                        service: "kbf".to_owned(),
                        node_id: "Not A Node!".to_owned(),
                    },
                )
                .await,
            )
            .await,
        Code::InvalidArgument,
        "invalid_request",
    );
    assert_refused(
        hosts(&world)
            .set_labels(
                as_operator(
                    c,
                    proto::SetLabelsRequest {
                        node_id: "d1".to_owned(),
                        labels: HashMap::from([("Zone".to_owned(), "a".to_owned())]),
                    },
                )
                .await,
            )
            .await,
        Code::InvalidArgument,
        "invalid_request",
    );
    assert_refused(
        hosts(&world)
            .exclude(
                as_operator(
                    c,
                    proto::ExcludeRequest {
                        service: "nope".to_owned(),
                        node_id: "d1".to_owned(),
                        reason: "disk swap".to_owned(),
                        abandon: false,
                    },
                )
                .await,
            )
            .await,
        Code::NotFound,
        "unknown_service",
    );
    assert_refused(
        hosts(&world)
            .abort(
                as_operator(
                    c,
                    proto::AbortRequest {
                        node_id: "ghost".to_owned(),
                        service: "kbf".to_owned(),
                        reason: "stuck".to_owned(),
                    },
                )
                .await,
            )
            .await,
        Code::NotFound,
        "unknown_host",
    );
    assert!(world.file("desired.json.ops").is_none(), "nothing changed");
}

/// Proves that an ops file that cannot be written is `INTERNAL` `store_failed`, from each
/// method of `Hosts` that writes it, and that the exclusion is not made.
#[tokio::test]
async fn hosts_answers_a_failed_write_of_the_ops_file_store_failed() {
    let rig = Rig::new("adapters-hosts-store");
    let started = Rig::start(&rig.dir, &rig.clock).with_ops(unwritable());
    let world = World::raw(rig, started);
    let c = &world.coordinator;
    assert_refused(
        hosts(&world)
            .include(
                as_operator(
                    c,
                    proto::IncludeRequest {
                        service: "kbf".to_owned(),
                        node_id: "d1".to_owned(),
                    },
                )
                .await,
            )
            .await,
        Code::Internal,
        "store_failed",
    );
    assert_refused(
        hosts(&world)
            .set_labels(
                as_operator(
                    c,
                    proto::SetLabelsRequest {
                        node_id: "d1".to_owned(),
                        labels: HashMap::new(),
                    },
                )
                .await,
            )
            .await,
        Code::Internal,
        "store_failed",
    );
    assert_refused(
        hosts(&world)
            .exclude(
                as_operator(
                    c,
                    proto::ExcludeRequest {
                        service: "kbf".to_owned(),
                        node_id: "d1".to_owned(),
                        reason: "disk swap".to_owned(),
                        abandon: false,
                    },
                )
                .await,
            )
            .await,
        Code::Internal,
        "store_failed",
    );
}

/// Proves that `Releases.SetDesired` is CI's: a call with no admission, and one admitted as the
/// operator, are `unauthorized` and set nothing.
#[tokio::test]
async fn set_desired_refuses_a_call_that_was_not_admitted_as_ci() {
    let world = world_of("adapters-set-callers", None);
    let before = world.file("desired.json");
    assert_refused(
        releases(&world)
            .set_desired(Request::new(set_request("kbf", "2.0")))
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        releases(&world)
            .set_desired(as_operator(&world.coordinator, set_request("kbf", "2.0")).await)
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_eq!(world.file("desired.json"), before);
}

/// Proves that a set the store cannot honour is refused whole: a precondition on the generation,
/// a write id, two different digests, and no desired state are each `invalid_request`, and the
/// desired file is as it was.
#[tokio::test]
async fn set_desired_refuses_what_the_store_cannot_honour_and_changes_nothing() {
    let world = world_of("adapters-set-unhonoured", None);
    let before = world.file("desired.json");

    let mut generation = set_request("kbf", "2.0");
    generation.if_generation = Some(1);
    let mut write_id = set_request("kbf", "2.0");
    write_id.write_id = Some("write-1".to_owned());
    let mut differing = set_request("kbf", "2.0");
    differing.desired.as_mut().expect("a state").package_digest = Some("ab".repeat(32));
    differing.package_digest = Some("cd".repeat(32));
    let mut missing = set_request("kbf", "2.0");
    missing.desired = None;
    for request in [generation, write_id, differing, missing] {
        assert_refused(
            releases(&world)
                .set_desired(as_ci(&world.coordinator, request).await)
                .await,
            Code::InvalidArgument,
            "invalid_request",
        );
    }
    assert_eq!(world.file("desired.json"), before);
}

/// Proves the refusals of a set that the desired-state store makes: a service the coordinator
/// does not manage (`unknown_service`), a version no JSON could carry (`invalid_request`), and a
/// service the token's allow-list entry does not name (`forbidden`).
#[tokio::test]
async fn set_desired_refuses_an_unknown_service_a_bad_version_and_a_service_not_allowed() {
    let world = world_of("adapters-set-refusals", None);
    assert_refused(
        set(&world, "nope", "2.0").await,
        Code::NotFound,
        "unknown_service",
    );
    assert_refused(
        set(&world, "kbf", "not a version").await,
        Code::InvalidArgument,
        "invalid_request",
    );
    assert_refused(
        set(&world, "other", "2.0").await,
        Code::PermissionDenied,
        "forbidden",
    );
}

/// Proves that a set of a service named by no one, by a coordinator with no legacy service, is
/// `invalid_request`: the body names no service and there is none to default to.
#[tokio::test]
async fn set_desired_without_a_service_is_invalid_when_there_is_no_legacy_service() {
    let rig = Rig::new("adapters-set-no-legacy");
    let started = Rig::start(&rig.dir, &rig.clock).with_legacy_service(None);
    let world = World::over(rig, started);
    assert_refused(
        set(&world, "", "2.0").await,
        Code::InvalidArgument,
        "invalid_request",
    );
}

/// Proves the refusals of a version: below the service's `min_version` is `below_min_version`,
/// a version the operator marked bad is `held_bad`, and a version set before with another
/// package digest is `invalid_request`. Each leaves the desired state as it was.
#[tokio::test]
async fn set_desired_refuses_a_version_below_the_floor_marked_bad_or_with_a_changed_digest() {
    let world = world_of("adapters-set-versions", Some(&["kbf", "floor"]));
    assert_refused(
        set(&world, "floor", "0.5").await,
        Code::FailedPrecondition,
        "below_min_version",
    );

    releases(&world)
        .mark_bad(
            as_operator(
                &world.coordinator,
                proto::MarkBadRequest {
                    service: "kbf".to_owned(),
                    version: "3.0".to_owned(),
                    reason: "crashes at start".to_owned(),
                },
            )
            .await,
        )
        .await
        .expect("the mark");
    assert_refused(
        set(&world, "kbf", "3.0").await,
        Code::FailedPrecondition,
        "held_bad",
    );

    let mut first = set_request("kbf", "2.0");
    first.package_digest = Some("ab".repeat(32));
    releases(&world)
        .set_desired(as_ci(&world.coordinator, first).await)
        .await
        .expect("the first set");
    let after_first = world.file("desired.json");
    let mut changed = set_request("kbf", "2.0");
    changed.package_digest = Some("cd".repeat(32));
    assert_refused(
        releases(&world)
            .set_desired(as_ci(&world.coordinator, changed).await)
            .await,
        Code::InvalidArgument,
        "invalid_request",
    );
    assert_eq!(world.file("desired.json"), after_first);
}

/// Proves what a set answers: the state as stored with its digest and the limits it kept, and
/// the roles whose limits were dropped because the service does not run them.
#[tokio::test]
async fn set_desired_answers_the_state_as_stored_and_the_roles_it_dropped() {
    let world = world_of("adapters-set-answer", Some(&["floor"]));
    let mut request = set_request("floor", "1.5");
    let sent = request.desired.as_mut().expect("a state");
    sent.min_up.insert("storage".to_owned(), 2);
    sent.min_up.insert("search".to_owned(), 1);
    request.package_digest = Some("ab".repeat(32));
    let reply = releases(&world)
        .set_desired(as_ci(&world.coordinator, request).await)
        .await
        .expect("the set")
        .into_inner();
    let stored = reply.desired.expect("the state as stored");
    assert_eq!(stored.version, "1.5");
    assert_eq!(stored.package_digest, Some("ab".repeat(32)));
    assert_eq!(stored.min_up.get("storage"), Some(&2));
    assert_eq!(stored.min_up.get("search"), None);
    assert!(reply.record.is_none());
    assert_eq!(reply.dropped_roles, ["search"]);
}

/// Proves that `Releases.Converge` is CI's: a call with no admission, and one admitted as the
/// operator, are `unauthorized`.
#[tokio::test]
async fn converge_refuses_a_call_that_was_not_admitted_as_ci() {
    let world = world_of("adapters-converge-callers", None);
    let request = || proto::ConvergeRequest {
        service: "kbf".to_owned(),
    };
    assert_refused(
        releases(&world).converge(Request::new(request())).await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        releases(&world)
            .converge(as_operator(&world.coordinator, request()).await)
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
}

/// Proves what `Converge` answers for a service: the desired version, whether it names the
/// service or leaves it empty (the legacy service, as the JSON route's query does); and that
/// a service the coordinator does not manage is `unknown_service`.
#[tokio::test]
async fn converge_reports_the_service_named_or_the_legacy_one_and_refuses_an_unknown_one() {
    let world = world_of("adapters-converge", None);
    set(&world, "kbf", "2.0").await.expect("the set");
    for service in ["kbf", ""] {
        let request = proto::ConvergeRequest {
            service: service.to_owned(),
        };
        let reply = releases(&world)
            .converge(as_ci(&world.coordinator, request).await)
            .await
            .expect("the convergence")
            .into_inner();
        let convergence = reply.convergence.expect("a convergence");
        assert_eq!(convergence.desired.as_deref(), Some("2.0"), "{service:?}");
    }
    let unknown = proto::ConvergeRequest {
        service: "nope".to_owned(),
    };
    assert_refused(
        releases(&world)
            .converge(as_ci(&world.coordinator, unknown).await)
            .await,
        Code::NotFound,
        "unknown_service",
    );
}

/// Proves that `Converge` with no service on a coordinator with no legacy service is
/// `invalid_request`: there is no service to default to.
#[tokio::test]
async fn converge_without_a_service_is_invalid_when_there_is_no_legacy_service() {
    let rig = Rig::new("adapters-converge-no-legacy");
    let started = Rig::start(&rig.dir, &rig.clock).with_legacy_service(None);
    let world = World::over(rig, started);
    assert_refused(
        releases(&world)
            .converge(as_ci(&world.coordinator, proto::ConvergeRequest::default()).await)
            .await,
        Code::InvalidArgument,
        "invalid_request",
    );
}

/// Proves `Releases.Status`: the operator gets every service, or only the one asked for (none
/// for a name the coordinator does not manage), with the desired state set; a call with no
/// admission, and one admitted as CI, are `unauthorized`.
#[tokio::test]
async fn status_reports_every_service_or_the_one_asked_for_to_the_operator_only() {
    let world = world_of("adapters-status", None);
    set(&world, "kbf", "2.0").await.expect("the set");
    let status = |only: &str| proto::StatusRequest {
        service: only.to_owned(),
    };

    let all = releases(&world)
        .status(as_operator(&world.coordinator, status("")).await)
        .await
        .expect("the status")
        .into_inner();
    let names: Vec<&str> = all.services.iter().map(|s| s.service.as_str()).collect();
    assert_eq!(names, ["floor", "kbf", "other"]);

    let one = releases(&world)
        .status(as_operator(&world.coordinator, status("kbf")).await)
        .await
        .expect("the status")
        .into_inner();
    assert_eq!(one.services.len(), 1);
    let desired = one.services[0].desired.as_ref().expect("the desired state");
    assert_eq!(desired.version, "2.0");

    let none = releases(&world)
        .status(as_operator(&world.coordinator, status("nope")).await)
        .await
        .expect("the status")
        .into_inner();
    assert!(none.services.is_empty());

    assert_refused(
        releases(&world).status(Request::new(status(""))).await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        releases(&world)
            .status(as_ci(&world.coordinator, status("")).await)
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
}

/// Proves `MarkBad`, `Halt` and `ClearHalt` over the adapters: each answers the audit entry of
/// its change; a halt of a halted service is `already_halted`; a clear of a service that is not
/// halted is `not_halted`, of one the coordinator does not manage `unknown_service`, with no
/// reason `invalid_request`, and from no caller `unauthorized`.
#[tokio::test]
async fn mark_bad_halt_and_clear_halt_answer_their_entries_and_their_refusals() {
    let world = world_of("adapters-releases-actions", None);
    let c = &world.coordinator;
    set(&world, "kbf", "2.0").await.expect("the set");

    let marked = releases(&world)
        .mark_bad(
            as_operator(
                c,
                proto::MarkBadRequest {
                    service: "kbf".to_owned(),
                    version: "1.5".to_owned(),
                    reason: "crashes at start".to_owned(),
                },
            )
            .await,
        )
        .await
        .expect("the mark")
        .into_inner();
    assert_eq!(marked.entry.expect("an entry").action, "mark_bad");

    let halt = || proto::HaltRequest {
        service: "kbf".to_owned(),
        reason: "stop the line".to_owned(),
    };
    let halted = releases(&world)
        .halt(as_operator(c, halt()).await)
        .await
        .expect("the halt")
        .into_inner();
    assert_eq!(halted.entry.expect("an entry").action, "halt");
    assert_refused(
        releases(&world).halt(as_operator(c, halt()).await).await,
        Code::FailedPrecondition,
        "already_halted",
    );

    let clear = |service: &str, reason: &str| proto::ClearHaltRequest {
        service: service.to_owned(),
        reason: reason.to_owned(),
    };
    assert_refused(
        releases(&world)
            .clear_halt(as_operator(c, clear("nope", "fixed")).await)
            .await,
        Code::NotFound,
        "unknown_service",
    );
    assert_refused(
        releases(&world)
            .clear_halt(as_operator(c, clear("kbf", "")).await)
            .await,
        Code::InvalidArgument,
        "invalid_request",
    );
    assert_refused(
        releases(&world)
            .clear_halt(Request::new(clear("kbf", "fixed")))
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
    let cleared = releases(&world)
        .clear_halt(as_operator(c, clear("kbf", "fixed")).await)
        .await
        .expect("the clear")
        .into_inner();
    assert_eq!(cleared.entry.expect("an entry").action, "clear_halt");
    assert_refused(
        releases(&world)
            .clear_halt(as_operator(c, clear("kbf", "fixed")).await)
            .await,
        Code::FailedPrecondition,
        "not_halted",
    );
}

/// Proves that `Force`, `MarkBad` and `Halt` take the caller the router admitted: no admission is
/// `unauthorized`, and CI as an operator's caller is `forbidden`.
#[tokio::test]
async fn releases_refuses_a_caller_that_is_not_the_operator() {
    let world = world_of("adapters-releases-callers", None);
    assert_refused(
        releases(&world)
            .force(Request::new(proto::ForceRequest::default()))
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        releases(&world)
            .force(as_ci_caller(proto::ForceRequest::default()))
            .await,
        Code::PermissionDenied,
        "forbidden",
    );
    assert_refused(
        releases(&world)
            .mark_bad(Request::new(proto::MarkBadRequest::default()))
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        releases(&world)
            .halt(as_ci_caller(proto::HaltRequest::default()))
            .await,
        Code::PermissionDenied,
        "forbidden",
    );
}

/// The scope of a hold or an unhold: the whole fleet.
fn fleet() -> Option<proto::HoldScope> {
    Some(proto::HoldScope {
        scope: Some(Scope::Fleet(proto::WholeFleet {})),
    })
}

/// Proves `Services`: a hold answers the mark it made and an entry, `List` shows it, an unhold
/// answers an entry, and an unhold of what is not held is `not_held`; a hold of a service the
/// coordinator does not manage is `unknown_service`, and one with no scope `invalid_request`.
#[tokio::test]
async fn services_hold_unhold_and_list_answer_their_marks_and_their_refusals() {
    let world = world_of("adapters-services", None);
    let c = &world.coordinator;
    let hold = |service: &str, scope| proto::HoldRequest {
        scope,
        service: service.to_owned(),
        reason: "freeze".to_owned(),
    };
    let unhold = |scope| proto::UnholdRequest {
        scope,
        service: "kbf".to_owned(),
        reason: "thaw".to_owned(),
    };

    let held = services(&world)
        .hold(as_operator(c, hold("kbf", fleet())).await)
        .await
        .expect("the hold")
        .into_inner();
    assert_eq!(held.hold.expect("the mark").service, "kbf");
    assert_eq!(held.entry.expect("an entry").action, "hold");

    let listed = services(&world)
        .list(as_operator(c, proto::ListServicesRequest {}).await)
        .await
        .expect("the list")
        .into_inner();
    let names: Vec<&str> = listed.services.iter().map(|s| s.service.as_str()).collect();
    assert_eq!(names, ["floor", "kbf", "other"]);
    assert_eq!(listed.holds.len(), 1);

    let released = services(&world)
        .unhold(as_operator(c, unhold(fleet())).await)
        .await
        .expect("the unhold")
        .into_inner();
    assert_eq!(released.entry.expect("an entry").action, "unhold");
    assert_refused(
        services(&world)
            .unhold(as_operator(c, unhold(fleet())).await)
            .await,
        Code::FailedPrecondition,
        "not_held",
    );
    assert_refused(
        services(&world)
            .hold(as_operator(c, hold("nope", fleet())).await)
            .await,
        Code::NotFound,
        "unknown_service",
    );
    assert_refused(
        services(&world)
            .hold(as_operator(c, hold("kbf", None)).await)
            .await,
        Code::InvalidArgument,
        "invalid_request",
    );
}

/// Proves that `Services` and `Audit` take the caller the router admitted: no admission is
/// `unauthorized`, and CI as an operator's caller is `forbidden`.
#[tokio::test]
async fn services_and_audit_refuse_a_caller_that_is_not_the_operator() {
    let world = world_of("adapters-services-callers", None);
    let audit = AuditService::new(world.coordinator.clone());
    assert_refused(
        services(&world)
            .list(Request::new(proto::ListServicesRequest {}))
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        services(&world)
            .hold(Request::new(proto::HoldRequest::default()))
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        services(&world)
            .unhold(as_ci_caller(proto::UnholdRequest::default()))
            .await,
        Code::PermissionDenied,
        "forbidden",
    );
    assert_refused(
        audit
            .list(Request::new(proto::ListAuditRequest::default()))
            .await,
        Code::Unauthenticated,
        "unauthorized",
    );
    assert_refused(
        audit
            .list(as_ci_caller(proto::ListAuditRequest::default()))
            .await,
        Code::PermissionDenied,
        "forbidden",
    );
}

/// Proves `Audit.List`: it answers the entries of the changes made, oldest first, and a filter
/// that is no actor token is `invalid_request`.
#[tokio::test]
async fn audit_list_answers_the_entries_made_and_refuses_a_filter_that_is_no_actor() {
    let world = world_of("adapters-audit", None);
    let c = &world.coordinator;
    hosts(&world)
        .include(
            as_operator(
                c,
                proto::IncludeRequest {
                    service: "kbf".to_owned(),
                    node_id: "d1".to_owned(),
                },
            )
            .await,
        )
        .await
        .expect("the inclusion");
    let audit = AuditService::new(world.coordinator.clone());
    let listed = audit
        .list(as_operator(c, proto::ListAuditRequest::default()).await)
        .await
        .expect("the list")
        .into_inner();
    let actions: Vec<(u64, &str)> = listed
        .entries
        .iter()
        .map(|e| (e.seq, e.action.as_str()))
        .collect();
    assert_eq!(actions, [(1, "include")]);
    assert_eq!(listed.next_seq, 1);

    let robot = proto::ListAuditRequest {
        actor: Some("robot".to_owned()),
        ..Default::default()
    };
    assert_refused(
        audit.list(as_operator(c, robot).await).await,
        Code::InvalidArgument,
        "invalid_request",
    );
}
