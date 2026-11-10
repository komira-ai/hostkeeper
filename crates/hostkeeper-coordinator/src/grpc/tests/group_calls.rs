//! The operator's calls by host group over the wires, once over gRPC and once over the JSON
//! gateway, on a coordinator that holds groups (`docs/design/hostgroups.md`, sections 4 and 7.2):
//! `Releases` (Force, Halt, ClearHalt), `Rollouts` (Pause, Resume, Advance), `Services`
//! (SetMinVersion, Hold, Assign, Unassign, List), `Hosts` (List) and `Audit` (List). Each
//! refusal is the status and closed token of its value, and changes nothing.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::records::MemoryRecords;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::force_request::Targets;
use hostkeeper_proto::hostkeeper::operator::v1::min_version::Scope;
use tonic::Code;

use super::rollouts::{Refused, WIRES, Wire, rpc};
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::host_groups_support::{body, coordinator_on, enroll_host, move_of, put_of};
use crate::marks_support::{digest, operator};
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::{OPERATOR, Rig};

const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";
const FORCE: &str = "/hostkeeper.operator.v1.Releases/Force";
const HALT: &str = "/hostkeeper.operator.v1.Releases/Halt";
const CLEAR_HALT: &str = "/hostkeeper.operator.v1.Releases/ClearHalt";
const PAUSE: &str = "/hostkeeper.operator.v1.Rollouts/Pause";
const RESUME: &str = "/hostkeeper.operator.v1.Rollouts/Resume";
const ADVANCE: &str = "/hostkeeper.operator.v1.Rollouts/Advance";
const SET_MIN_VERSION: &str = "/hostkeeper.operator.v1.Services/SetMinVersion";
const HOLD: &str = "/hostkeeper.operator.v1.Services/Hold";
const ASSIGN: &str = "/hostkeeper.operator.v1.Services/Assign";
const UNASSIGN: &str = "/hostkeeper.operator.v1.Services/Unassign";
const LIST_SERVICES: &str = "/hostkeeper.operator.v1.Services/List";
const LIST_HOSTS: &str = "/hostkeeper.operator.v1.Hosts/List";
const LIST_AUDIT: &str = "/hostkeeper.operator.v1.Audit/List";

struct Wired {
    coordinator: Arc<Coordinator>,
    fake: Fake,
    _enrollment: EnrollmentRig,
    _rig: Rig,
}

impl Wired {
    /// Alpha and beta list `web`, and `a1` is enrolled in alpha; 1.0 and 2.0 of `web` are
    /// registered by the caller's sets.
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
        for group in ["alpha", "beta"] {
            coordinator
                .host_groups_put(
                    &operator(),
                    put_of(body(group, &["hostkeeper", "web"], None), None),
                )
                .expect("a group");
        }
        enroll_host(&enrollment.enrollment, "a1");
        coordinator
            .host_groups_move(&operator(), move_of("a1", "alpha"))
            .expect("moved");
        Self {
            coordinator: Arc::new(coordinator),
            fake,
            _enrollment: enrollment,
            _rig: rig,
        }
    }
}

fn token(code: Code, token: &str) -> Refused {
    (code, token.to_owned())
}

async fn set(world: &Wired, wire: Wire, version: &str, n: u8) {
    let request = proto::SetDesiredRequest {
        service: "web".to_owned(),
        desired: Some(proto::DesiredState {
            version: version.to_owned(),
            ..proto::DesiredState::default()
        }),
        package_digest: Some(digest(n)),
        ..proto::SetDesiredRequest::default()
    };
    let _: proto::SetDesiredResponse = rpc(
        &world.coordinator,
        wire,
        SET_DESIRED,
        Some(OPERATOR),
        &request,
    )
    .await
    .expect("a set");
}

async fn ask<R: prost::Message + Default>(
    world: &Wired,
    wire: Wire,
    path: &str,
    request: &impl prost::Message,
) -> Result<R, Refused> {
    rpc(&world.coordinator, wire, path, Some(OPERATOR), request).await
}

/// Proves: over both wires a force names its group in `host_group`: it sets the group's desired
/// version and answers its audit entry naming the group; a group that does not list the package is
/// `not_in_host_group` (FAILED_PRECONDITION), a role that names another group `unknown_target`
/// (INVALID_ARGUMENT), and a refusal changes nothing; a halt answers an entry naming the first
/// group, a second is `already_halted`, and a clear answers the entry of the first group.
///
/// Catches: a refusal of a group that reaches the caller as another status, or a force that
/// writes the package alone.
#[tokio::test]
async fn a_force_and_a_halt_by_group_are_answered_over_both_wires() {
    for wire in WIRES {
        let world = Wired::new(&format!("grpc-gc-force-{wire:?}").to_lowercase());
        set(&world, wire, "1.0", 1).await;
        set(&world, wire, "2.0", 2).await;
        let force = |host_group: &str, role: Option<&str>| proto::ForceRequest {
            service: "web".to_owned(),
            version: "1.0".to_owned(),
            reason: "fix the farm".to_owned(),
            targets: Some(role.map_or_else(
                || {
                    Targets::Fleet(proto::FleetTarget {
                        confirm_service: "web".to_owned(),
                    })
                },
                |role| Targets::Role(role.to_owned()),
            )),
            include_held: false,
            host_group: host_group.to_owned(),
        };
        let before = world.fake.desired();
        for (request, want) in [
            (
                force("missing", None),
                token(Code::FailedPrecondition, "not_in_host_group"),
            ),
            (
                force("alpha", Some("beta")),
                token(Code::InvalidArgument, "unknown_target"),
            ),
        ] {
            let refused = ask::<proto::ForceResponse>(&world, wire, FORCE, &request)
                .await
                .expect_err("refused");
            assert_eq!(refused, want, "{wire:?}");
        }
        assert_eq!(world.fake.desired(), before, "{wire:?}");

        let forced: proto::ForceResponse = ask(&world, wire, FORCE, &force("alpha", None))
            .await
            .expect("forced");
        assert_eq!(
            forced
                .entries
                .iter()
                .map(|entry| (entry.action.as_str(), entry.host_group.as_deref()))
                .collect::<Vec<_>>(),
            [("force", Some("alpha"))],
            "{wire:?}"
        );
        let alpha = "alpha".parse().expect("a group");
        let beta = "beta".parse().expect("a group");
        let wanted = |group| {
            world
                .coordinator
                .desired
                .group_desired(group, "web")
                .map(|desired| desired.version.to_string())
        };
        assert_eq!(wanted(&alpha).as_deref(), Some("1.0"), "{wire:?}");
        assert_eq!(wanted(&beta).as_deref(), Some("2.0"), "{wire:?}");

        let halt = proto::HaltRequest {
            service: "web".to_owned(),
            reason: "a bad disk".to_owned(),
        };
        let halted: proto::HaltResponse = ask(&world, wire, HALT, &halt).await.expect("halted");
        assert_eq!(
            halted.entry.and_then(|entry| entry.host_group).as_deref(),
            Some("alpha"),
            "{wire:?}"
        );
        assert_eq!(
            ask::<proto::HaltResponse>(&world, wire, HALT, &halt)
                .await
                .expect_err("refused"),
            token(Code::FailedPrecondition, "already_halted"),
            "{wire:?}"
        );
        let cleared: proto::ClearHaltResponse = ask(
            &world,
            wire,
            CLEAR_HALT,
            &proto::ClearHaltRequest {
                service: "web".to_owned(),
                reason: "fixed".to_owned(),
            },
        )
        .await
        .expect("cleared");
        assert_eq!(
            cleared.entry.and_then(|entry| entry.host_group).as_deref(),
            Some("alpha"),
            "{wire:?}"
        );
    }
}

/// Proves: over both wires a pause without a group pauses each group that lists the package and
/// answers the first group's entry, a resume without one is `ambiguous_host_group`
/// (FAILED_PRECONDITION), a resume naming a group resumes it, and an advance naming a group that
/// does not list the package is `not_in_host_group`.
///
/// Catches: a resume that acts on one of two groups, or a refusal answered as a server error.
#[tokio::test]
async fn a_pause_and_a_resume_by_group_are_answered_over_both_wires() {
    for wire in WIRES {
        let world = Wired::new(&format!("grpc-gc-pause-{wire:?}").to_lowercase());
        let pause = |group: &str| proto::PauseRequest {
            service: "web".to_owned(),
            reason: "night".to_owned(),
            host_group: group.to_owned(),
        };
        let resume = |group: &str| proto::ResumeRequest {
            service: "web".to_owned(),
            reason: "morning".to_owned(),
            host_group: group.to_owned(),
        };

        let paused: proto::PauseResponse =
            ask(&world, wire, PAUSE, &pause("")).await.expect("paused");
        assert_eq!(
            paused.entry.and_then(|entry| entry.host_group).as_deref(),
            Some("alpha"),
            "{wire:?}"
        );
        assert_eq!(
            ask::<proto::ResumeResponse>(&world, wire, RESUME, &resume(""))
                .await
                .expect_err("refused"),
            token(Code::FailedPrecondition, "ambiguous_host_group"),
            "{wire:?}"
        );
        let resumed: proto::ResumeResponse = ask(&world, wire, RESUME, &resume("beta"))
            .await
            .expect("resumed");
        assert_eq!(
            resumed.entry.and_then(|entry| entry.host_group).as_deref(),
            Some("beta"),
            "{wire:?}"
        );
        assert_eq!(
            ask::<proto::AdvanceResponse>(
                &world,
                wire,
                ADVANCE,
                &proto::AdvanceRequest {
                    service: "web".to_owned(),
                    reason: "go".to_owned(),
                    host_group: "missing".to_owned(),
                },
            )
            .await
            .expect_err("refused"),
            token(Code::FailedPrecondition, "not_in_host_group"),
            "{wire:?}"
        );
    }
}

/// Proves: over both wires a minimum version of a role scope is the group's and is audited naming
/// the group; a role that is no group is `unknown_host_group` (NOT_FOUND) for a minimum version
/// and for a hold; `Assign` and `Unassign` are `placed_by_host_group` (FAILED_PRECONDITION);
/// `Services.List` names the groups that list each package.
///
/// Catches: a role scope read as a role of the rollout file, or an assignment accepted.
#[tokio::test]
async fn the_services_calls_by_group_are_answered_over_both_wires() {
    for wire in WIRES {
        let world = Wired::new(&format!("grpc-gc-services-{wire:?}").to_lowercase());
        set(&world, wire, "2.0", 2).await;
        let min = |role: &str| proto::SetMinVersionRequest {
            service: "web".to_owned(),
            min_version: Some(proto::MinVersion {
                scope: Some(Scope::Role(role.to_owned())),
                version: "1.0".to_owned(),
            }),
        };

        let set_min: proto::SetMinVersionResponse =
            ask(&world, wire, SET_MIN_VERSION, &min("alpha"))
                .await
                .expect("set");
        assert_eq!(
            set_min.entry.and_then(|entry| entry.host_group).as_deref(),
            Some("alpha"),
            "{wire:?}"
        );
        let refused =
            ask::<proto::SetMinVersionResponse>(&world, wire, SET_MIN_VERSION, &min("missing"))
                .await
                .expect_err("refused");
        assert_eq!(
            refused,
            token(Code::NotFound, "unknown_host_group"),
            "{wire:?}"
        );
        let held = ask::<proto::HoldResponse>(
            &world,
            wire,
            HOLD,
            &proto::HoldRequest {
                scope: Some(proto::HoldScope {
                    scope: Some(proto::hold_scope::Scope::Role("missing".to_owned())),
                }),
                service: "web".to_owned(),
                reason: "look".to_owned(),
            },
        )
        .await
        .expect_err("refused");
        assert_eq!(
            held,
            token(Code::NotFound, "unknown_host_group"),
            "{wire:?}"
        );
        let before = world.fake.ops();
        assert_eq!(
            ask::<proto::AssignResponse>(
                &world,
                wire,
                ASSIGN,
                &proto::AssignRequest {
                    node_id: "a1".to_owned(),
                    service: "web".to_owned(),
                    role: String::new(),
                },
            )
            .await
            .expect_err("refused"),
            token(Code::FailedPrecondition, "placed_by_host_group"),
            "{wire:?}"
        );
        assert_eq!(
            ask::<proto::UnassignResponse>(
                &world,
                wire,
                UNASSIGN,
                &proto::UnassignRequest {
                    node_id: "a1".to_owned(),
                    service: "web".to_owned(),
                    uninstall: false,
                    role: String::new(),
                },
            )
            .await
            .expect_err("refused"),
            token(Code::FailedPrecondition, "placed_by_host_group"),
            "{wire:?}"
        );
        assert_eq!(world.fake.ops(), before, "{wire:?}");

        let listed: proto::ListServicesResponse =
            ask(&world, wire, LIST_SERVICES, &proto::ListServicesRequest {})
                .await
                .expect("listed");
        let web = listed
            .services
            .iter()
            .find(|entry| entry.service == "web")
            .expect("web");
        assert_eq!(web.host_groups, ["alpha", "beta"], "{wire:?}");
        assert_eq!(web.desired_version.as_deref(), Some("2.0"), "{wire:?}");
    }
}

/// Proves: over both wires `Hosts.List` names the group of an enrolled host in
/// `host_group` and `role`, and `Audit.List` with a `host_group` filter lists the entries of that
/// group and a filter that is no group name is `invalid_request` (INVALID_ARGUMENT).
///
/// Catches: a host shown with no group, or a filter that lists nothing as before groups were held.
#[tokio::test]
async fn the_reads_by_group_are_answered_over_both_wires() {
    for wire in WIRES {
        let world = Wired::new(&format!("grpc-gc-reads-{wire:?}").to_lowercase());

        let listed: proto::ListHostsResponse = ask(
            &world,
            wire,
            LIST_HOSTS,
            &proto::ListHostsRequest::default(),
        )
        .await
        .expect("listed");
        let [host] = listed.hosts.as_slice() else {
            panic!("one host: {listed:?}");
        };
        assert_eq!(
            (host.role.as_str(), host.host_group.as_str()),
            ("alpha", "alpha"),
            "{wire:?}"
        );
        let audit = |group: &str| proto::ListAuditRequest {
            host_group: Some(group.to_owned()),
            ..proto::ListAuditRequest::default()
        };
        let beta: proto::ListAuditResponse = ask(&world, wire, LIST_AUDIT, &audit("beta"))
            .await
            .expect("listed");
        assert!(!beta.entries.is_empty(), "{wire:?}");
        assert!(
            beta.entries
                .iter()
                .all(|entry| entry.host_group.as_deref() == Some("beta")),
            "{wire:?}"
        );
        let none: proto::ListAuditResponse = ask(&world, wire, LIST_AUDIT, &audit("missing"))
            .await
            .expect("listed");
        assert!(none.entries.is_empty(), "{wire:?}");
        assert_eq!(
            ask::<proto::ListAuditResponse>(&world, wire, LIST_AUDIT, &audit("Bad Name"))
                .await
                .expect_err("refused"),
            token(Code::InvalidArgument, "invalid_request"),
            "{wire:?}"
        );
    }
}
