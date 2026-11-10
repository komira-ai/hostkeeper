//! `Releases.SetDesired` naming a host group on a coordinator that holds one, once over gRPC and
//! once over the gateway, and the frozen `POST /v1/desired` as the form that names none: the group
//! is the deployment's, the refusals that name it are the statuses of their values, and the frozen
//! route answers the same keys as before.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hyper::StatusCode;
use serde_json::json;
use tonic::Code;

use super::operator::post;
use super::rollouts::{Refused, WIRES, rpc};
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::host_groups_support::{body, coordinator_listed, put_of};
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::{OPERATOR, Rig, ci_token};

const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";
const CONVERGE: &str = "/hostkeeper.operator.v1.Releases/Converge";
const WAIT_CONVERGED: &str = "/hostkeeper.operator.v1.Releases/WaitConverged";
const GET_RELEASE: &str = "/hostkeeper.operator.v1.Releases/GetRelease";

struct Wired {
    coordinator: Arc<Coordinator>,
    fake: Fake,
    _enrollment: EnrollmentRig,
    _rig: Rig,
}

impl Wired {
    fn new(name: &str) -> Self {
        Self::limited(name, "")
    }

    /// A world whose allow-list entry also holds the lines `entry_extra`.
    fn limited(name: &str, entry_extra: &str) -> Self {
        let enrollment = EnrollmentRig::over_records(
            &format!("{name}-enrollment"),
            Arc::new(MemoryRecords::new()),
        );
        let fake = Fake::new();
        let rig = Rig::new(name);
        let coordinator = coordinator_listed(
            &fake,
            &rig.clock,
            Some(Arc::clone(&enrollment.enrollment)),
            entry_extra,
        );
        rig.at(2_000);
        for (group, packages) in [
            ("alpha", ["hostkeeper", "web"]),
            ("beta", ["hostkeeper", "web"]),
        ] {
            coordinator
                .host_groups_put(
                    &crate::marks_support::operator(),
                    put_of(body(group, &packages, None), None),
                )
                .expect("a group");
        }
        Self {
            coordinator: Arc::new(coordinator),
            fake,
            _enrollment: enrollment,
            _rig: rig,
        }
    }
}

fn set(group: &str, version: &str) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        service: "web".to_owned(),
        desired: Some(proto::DesiredState {
            version: version.to_owned(),
            ..proto::DesiredState::default()
        }),
        host_group: group.to_owned(),
        ..proto::SetDesiredRequest::default()
    }
}

fn token(code: Code, token: &str) -> Refused {
    (code, token.to_owned())
}

/// Proves: a set naming a group sets it over both wires, answers the group's state and its write
/// record, and the group's desired version is the one set; and a set naming a group that does not
/// list the package, a precondition no one group can answer, and limits are refused with the codes
/// and tokens of their values, changing nothing.
///
/// Catches: a refusal of a host group that reaches the caller as another status, or a set that
/// goes on after one.
#[tokio::test]
async fn a_set_names_its_group_over_both_wires_and_its_refusals_are_their_statuses() {
    for wire in WIRES {
        let world = Wired::new(&format!("grpc-group-set-{wire:?}").to_lowercase());
        let answered: proto::SetDesiredResponse = rpc(
            &world.coordinator,
            wire,
            SET_DESIRED,
            Some(OPERATOR),
            &set("alpha", "2.0"),
        )
        .await
        .expect("set");
        assert_eq!(
            answered.desired.map(|d| d.version),
            Some("2.0".to_owned()),
            "{wire:?}"
        );
        assert_eq!(answered.record.map(|r| r.generation), Some(1), "{wire:?}");
        let before = world.fake.desired();

        let mut limits = set("alpha", "3.0");
        if let Some(desired) = limits.desired.as_mut() {
            desired.max_updating.insert("alpha".to_owned(), 2);
        }
        let mut ambiguous = set("", "3.0");
        ambiguous.if_generation = Some(1);
        for (request, want) in [
            (
                set("missing", "3.0"),
                token(Code::FailedPrecondition, "not_in_host_group"),
            ),
            (
                ambiguous,
                token(Code::FailedPrecondition, "ambiguous_host_group"),
            ),
            (limits, token(Code::InvalidArgument, "limits_in_host_group")),
        ] {
            let refused = rpc::<proto::SetDesiredResponse>(
                &world.coordinator,
                wire,
                SET_DESIRED,
                Some(OPERATOR),
                &request,
            )
            .await
            .expect_err("refused");
            assert_eq!(refused, want, "{wire:?}");
        }
        assert_eq!(world.fake.desired(), before, "{wire:?}");
    }
}

/// Proves: the frozen `POST /v1/desired` is the form that names no group: CI's body sets every
/// group that lists the package, answers the state's own keys, and a body that carries a limit or
/// names a package no group lists, or one the token may not set, is refused with the status and the closed token of its refusal.
///
/// Catches: the frozen route answering a group's refusal as a server error, or its body changed.
#[tokio::test]
async fn the_frozen_route_deploys_to_each_group_that_lists_the_package() {
    let world = Wired::new("grpc-group-frozen");
    let ci = ci_token();

    let (status, answer) = post(
        &world.coordinator,
        "/v1/desired",
        &ci,
        &json!({"service": "web", "version": "2.0"}).to_string(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["version"], "2.0");
    assert!(answer["max_updating"].is_object() && answer["min_up"].is_object());
    for group in ["alpha", "beta"] {
        let name = group.parse().expect("a group");
        assert_eq!(
            world
                .coordinator
                .desired
                .group_desired(&name, "web")
                .map(|desired| desired.version.to_string()),
            Some("2.0".to_owned())
        );
    }
    for (body, want, reason) in [
        (
            json!({"service": "web", "version": "3.0", "max_updating": {"alpha": 1}}),
            StatusCode::BAD_REQUEST,
            "limits_in_host_group",
        ),
        (
            json!({"service": "db", "version": "3.0"}),
            StatusCode::CONFLICT,
            "not_in_host_group",
        ),
        (
            json!({"service": "queue", "version": "3.0"}),
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
    ] {
        let (status, answer) = post(
            &world.coordinator,
            "/v1/desired",
            &ci_token(),
            &body.to_string(),
        )
        .await;
        assert_eq!(status, want, "{body}");
        assert_eq!(answer["error"], reason, "{body}");
    }
}

/// Proves: `Converge`, `WaitConverged` and `GetRelease` take a group over both wires: a group that
/// lists the package answers its own convergence (an empty group is converged) and release, with
/// no group they answer for every group that has a desired version, `WaitConverged` answers
/// `no_desired` at once while none has, and a group that does not exist or does not list the
/// package is `not_in_host_group` for each of the three.
///
/// Catches: a refusal of a group that reaches the caller as another status, a named group read as
/// none, or a wait on a package no group has a version of held to its deadline.
#[tokio::test]
async fn converge_wait_and_get_release_name_their_group_over_both_wires() {
    for wire in WIRES {
        let world = Wired::new(&format!("grpc-group-converge-{wire:?}").to_lowercase());
        let wait = |group: &str| proto::WaitConvergedRequest {
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            wait_secs: 1,
            host_group: group.to_owned(),
        };
        let waited = |group: &'static str| {
            let world = &world;
            let request = wait(group);
            async move {
                rpc::<proto::WaitConvergedResponse>(
                    &world.coordinator,
                    wire,
                    WAIT_CONVERGED,
                    Some(OPERATOR),
                    &request,
                )
                .await
                .map(|answer| answer.outcome)
            }
        };
        assert_eq!(
            waited("").await,
            Ok(proto::WaitOutcome::NoDesired.into()),
            "{wire:?}"
        );

        let _: proto::SetDesiredResponse = rpc(
            &world.coordinator,
            wire,
            SET_DESIRED,
            Some(OPERATOR),
            &set("alpha", "2.0"),
        )
        .await
        .expect("set");
        let converged = |group: &str| proto::ConvergeRequest {
            service: "web".to_owned(),
            host_group: group.to_owned(),
        };
        for group in ["alpha", ""] {
            let answer: proto::ConvergeResponse = rpc(
                &world.coordinator,
                wire,
                CONVERGE,
                Some(OPERATOR),
                &converged(group),
            )
            .await
            .expect("a convergence");
            let convergence = answer.convergence.expect("a convergence");
            assert_eq!(
                convergence.desired.as_deref(),
                Some("2.0"),
                "{wire:?} {group}"
            );
            assert_eq!(
                convergence.state,
                proto::ConvergenceState::Converged as i32,
                "{wire:?} {group}"
            );
            assert_eq!(
                convergence.record.map(|record| record.generation),
                Some(1),
                "{wire:?} {group}"
            );
        }
        assert_eq!(
            waited("alpha").await,
            Ok(proto::WaitOutcome::Converged.into()),
            "{wire:?}"
        );
        assert_eq!(
            waited("").await,
            Ok(proto::WaitOutcome::Converged.into()),
            "{wire:?}"
        );
        let release = |group: &str| proto::GetReleaseRequest {
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            host_group: group.to_owned(),
        };
        let unlisted = token(Code::FailedPrecondition, "not_in_host_group");
        for group in ["missing", "gamma"] {
            let refused = rpc::<proto::ConvergeResponse>(
                &world.coordinator,
                wire,
                CONVERGE,
                Some(OPERATOR),
                &converged(group),
            )
            .await
            .expect_err("refused");
            assert_eq!(refused, unlisted, "{wire:?} {group}");
            assert_eq!(
                waited_refused(&world, wire, group).await,
                unlisted,
                "{wire:?} {group}"
            );
            let refused = rpc::<proto::GetReleaseResponse>(
                &world.coordinator,
                wire,
                GET_RELEASE,
                Some(OPERATOR),
                &release(group),
            )
            .await
            .expect_err("refused");
            assert_eq!(refused, unlisted, "{wire:?} {group}");
        }
    }
}

/// The refusal `WaitConverged` makes of a wait that names `group`.
async fn waited_refused(world: &Wired, wire: super::rollouts::Wire, group: &str) -> Refused {
    rpc::<proto::WaitConvergedResponse>(
        &world.coordinator,
        wire,
        WAIT_CONVERGED,
        Some(OPERATOR),
        &proto::WaitConvergedRequest {
            service: "web".to_owned(),
            version: "2.0".to_owned(),
            wait_secs: 1,
            host_group: group.to_owned(),
        },
    )
    .await
    .expect_err("refused")
}

/// Proves: a CI token whose entry names `host_groups = ["alpha"]` sets alpha over both wires; a
/// set naming beta (which exists and lists the package) or a group that does not exist is
/// `PermissionDenied` with the token `forbidden`, changing nothing; a set naming no group is one
/// deployment to alpha alone.
#[tokio::test]
async fn a_limited_ci_token_is_permission_denied_outside_its_groups_over_both_wires() {
    for wire in WIRES {
        let world = Wired::limited(
            &format!("grpc-group-limited-{wire:?}").to_lowercase(),
            "host_groups = [\"alpha\"]\n",
        );
        let answered: proto::SetDesiredResponse = rpc(
            &world.coordinator,
            wire,
            SET_DESIRED,
            Some(&ci_token()),
            &set("alpha", "2.0"),
        )
        .await
        .expect("set");
        assert_eq!(
            answered.desired.map(|d| d.version),
            Some("2.0".to_owned()),
            "{wire:?}"
        );
        let before = world.fake.desired();

        for group in ["beta", "missing"] {
            let refused = rpc::<proto::SetDesiredResponse>(
                &world.coordinator,
                wire,
                SET_DESIRED,
                Some(&ci_token()),
                &set(group, "3.0"),
            )
            .await
            .expect_err("refused");
            assert_eq!(
                refused,
                token(Code::PermissionDenied, "forbidden"),
                "{wire:?} {group}"
            );
        }
        assert_eq!(world.fake.desired(), before, "{wire:?}");

        let _: proto::SetDesiredResponse = rpc(
            &world.coordinator,
            wire,
            SET_DESIRED,
            Some(&ci_token()),
            &set("", "4.0"),
        )
        .await
        .expect("no group");
        let held = |group: &str| {
            world
                .coordinator
                .desired
                .group_desired(&group.parse().expect("a group"), "web")
                .map(|desired| desired.version.to_string())
        };
        assert_eq!(held("alpha"), Some("4.0".to_owned()), "{wire:?}");
        assert_eq!(held("beta"), None, "{wire:?}");
    }
}

/// Proves: the same token reads a group it may not deploy to over both wires: `Converge` and
/// `WaitConverged` naming beta are answered.
#[tokio::test]
async fn a_limited_ci_token_reads_other_groups_over_both_wires() {
    for wire in WIRES {
        let world = Wired::limited(
            &format!("grpc-group-limited-read-{wire:?}").to_lowercase(),
            "host_groups = [\"alpha\"]\n",
        );
        let _: proto::SetDesiredResponse = rpc(
            &world.coordinator,
            wire,
            SET_DESIRED,
            Some(OPERATOR),
            &set("beta", "2.0"),
        )
        .await
        .expect("the operator sets beta");

        let answer: proto::ConvergeResponse = rpc(
            &world.coordinator,
            wire,
            CONVERGE,
            Some(&ci_token()),
            &proto::ConvergeRequest {
                service: "web".to_owned(),
                host_group: "beta".to_owned(),
            },
        )
        .await
        .expect("a convergence");
        assert_eq!(
            answer.convergence.and_then(|c| c.desired),
            Some("2.0".to_owned()),
            "{wire:?}"
        );
        let waited: proto::WaitConvergedResponse = rpc(
            &world.coordinator,
            wire,
            WAIT_CONVERGED,
            Some(&ci_token()),
            &proto::WaitConvergedRequest {
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                wait_secs: 1,
                host_group: "beta".to_owned(),
            },
        )
        .await
        .expect("a wait");
        assert_eq!(
            waited.outcome,
            i32::from(proto::WaitOutcome::Converged),
            "{wire:?}"
        );
    }
}

/// Proves: the frozen `POST /v1/desired` with a limited token is the form that names no group
/// over the groups the token may deploy to: alpha is set and beta is not, and a limited token
/// whose list holds no group that lists the package is HTTP 403 with `{"error":"forbidden"}`.
#[tokio::test]
async fn the_frozen_route_deploys_a_limited_token_to_its_groups_alone() {
    let world = Wired::limited("grpc-group-frozen-limited", "host_groups = [\"alpha\"]\n");
    let (status, answer) = post(
        &world.coordinator,
        "/v1/desired",
        &ci_token(),
        &json!({"service": "web", "version": "2.0"}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["version"], "2.0");
    let held = |group: &str| {
        world
            .coordinator
            .desired
            .group_desired(&group.parse().expect("a group"), "web")
            .map(|desired| desired.version.to_string())
    };
    assert_eq!(held("alpha"), Some("2.0".to_owned()));
    assert_eq!(held("beta"), None);

    let none = Wired::limited("grpc-group-frozen-none", "host_groups = [\"gamma\"]\n");
    let (status, answer) = post(
        &none.coordinator,
        "/v1/desired",
        &ci_token(),
        &json!({"service": "web", "version": "2.0"}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(answer["error"], "forbidden");
}
