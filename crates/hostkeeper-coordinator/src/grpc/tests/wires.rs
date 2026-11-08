//! `Services.Assign` and `Services.Unassign` by role, and a refused `Releases.Force`, on a real
//! coordinator through the router as the private listener serves it, once over gRPC and once over
//! the JSON gateway: the role reaches the coordinator by both, and a refusal's token comes back
//! by both with nothing changed.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tonic::Code;

use super::rollouts::{WIRES, rpc};
use crate::assignment::Entry;
use crate::marks_support::rig;
use crate::testutil::OPERATOR;

const ASSIGN: &str = "/hostkeeper.operator.v1.Services/Assign";
const UNASSIGN: &str = "/hostkeeper.operator.v1.Services/Unassign";
const FORCE: &str = "/hostkeeper.operator.v1.Releases/Force";

/// The bytes of the desired-state file and of its ops file, when they exist.
fn state_files(dir: &Path) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    (
        fs::read(dir.join("desired.json")).ok(),
        fs::read(dir.join("desired.json.ops")).ok(),
    )
}

/// Proves: `Services.Unassign` and then `Services.Assign` of `web` by the role `storage`, over
/// gRPC and over the gateway, each answer the audit entry of their scope (`role storage`); the
/// unassign leaves the role's exclusion of `web` in the ops state, and the assign takes it off.
///
/// Catches: the role field of either request lost on a wire (the call refused for want of a
/// scope, or applied to another scope than the role).
#[tokio::test]
async fn assign_and_unassign_by_role_reach_the_coordinator_over_both_wires() {
    for wire in WIRES {
        let farm = rig(&format!("wires-role-{wire:?}"), "");
        let coordinator = Arc::new(farm.coordinator);
        let unassign = proto::UnassignRequest {
            service: "web".to_owned(),
            role: "storage".to_owned(),
            ..proto::UnassignRequest::default()
        };
        let answer: proto::UnassignResponse =
            rpc(&coordinator, wire, UNASSIGN, Some(OPERATOR), &unassign)
                .await
                .unwrap_or_else(|refused| panic!("{wire:?}: {refused:?}"));
        let entry = answer.entry.expect("an audit entry");
        assert_eq!(
            (entry.action.as_str(), entry.reason.as_deref()),
            ("unassign", Some("role storage")),
            "{wire:?}"
        );
        let excluded = coordinator.ops().state().assignments;
        assert_eq!(
            excluded["web"].roles.get("storage"),
            Some(&Entry::Exclude),
            "{wire:?}"
        );

        let assign = proto::AssignRequest {
            service: "web".to_owned(),
            role: "storage".to_owned(),
            ..proto::AssignRequest::default()
        };
        let answer: proto::AssignResponse =
            rpc(&coordinator, wire, ASSIGN, Some(OPERATOR), &assign)
                .await
                .unwrap_or_else(|refused| panic!("{wire:?}: {refused:?}"));
        let entry = answer.entry.expect("an audit entry");
        assert_eq!(
            (entry.action.as_str(), entry.reason.as_deref()),
            ("assign", Some("role storage")),
            "{wire:?}"
        );
        assert!(
            !coordinator.ops().state().assignments.contains_key("web"),
            "{wire:?}: the rollout file's role again"
        );
    }
}

/// Proves: a force below the service's `min_version` is refused `FAILED_PRECONDITION`
/// `below_min_version` over both wires, and a token no one holds `UNAUTHENTICATED`
/// `unauthorized`, each changing neither the desired-state file nor its ops file.
///
/// Catches: a wire that turns a refusal into another code (so a client reports a failure for a
/// refusal), drops its token, or applies the force anyway.
#[tokio::test]
async fn a_refused_force_keeps_its_code_and_token_over_both_wires() {
    let farm = rig("wires-refusal", "min_version = \"1.5\"");
    let dir = farm.dir.path().to_owned();
    let coordinator = Arc::new(farm.coordinator);
    let before = state_files(&dir);
    let force = proto::ForceRequest {
        service: "web".to_owned(),
        version: "1.4".to_owned(),
        reason: "fix the farm".to_owned(),
        targets: Some(proto::force_request::Targets::Fleet(proto::FleetTarget {
            confirm_service: "web".to_owned(),
        })),
        ..proto::ForceRequest::default()
    };
    for (bearer, code, token) in [
        (
            OPERATOR,
            Code::FailedPrecondition,
            "below_min_version".to_owned(),
        ),
        (
            "not-an-operator-token-0123456789",
            Code::Unauthenticated,
            "unauthorized".to_owned(),
        ),
    ] {
        for wire in WIRES {
            let refused =
                rpc::<proto::ForceResponse>(&coordinator, wire, FORCE, Some(bearer), &force)
                    .await
                    .expect_err("refused");
            assert_eq!(refused, (code, token.clone()), "{wire:?}");
        }
    }
    assert_eq!(state_files(&dir), before);
}
