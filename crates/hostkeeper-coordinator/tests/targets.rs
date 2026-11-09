//! `Releases.SetDesired` with targets, and `Releases.GetRelease` (docs/design/packages.md,
//! sections 3.1 to 3.6, build item A2), over the gRPC route and the JSON gateway.

mod support;

/// The refusals of section 3.4, the gateway's null step and the message limits.
#[path = "targets/checks.rs"]
mod checks;
/// What is kept: a repeat, `release_exists`, the audit, `GetRelease`, a restart, and the
/// previous coordinator on the new state.
#[path = "targets/kept.rs"]
mod kept;
/// Repository entries (packages-repositories.md section 2, build item A16): the refusals of
/// section 2.2, what is kept and audited, and the release before's reader on what is kept.
#[path = "targets/repository.rs"]
mod repository;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use support::rig::{Rig, Spec};
use support::targets::{GET_RELEASE, SET_DESIRED, gateway, grpc, set, set_json, targets};
use support::{OPERATOR, ci_token};

const SERVICES: &str = r#"
[[service]]
name = "web"
roles = ["daemon"]

[[service]]
name = "hostkeeper"
roles = ["daemon"]
"#;

fn rig(name: &str) -> Rig {
    Rig::new(
        name,
        Spec::recorded()
            .with_services(SERVICES)
            .with_oidc_services(&["web", "hostkeeper"])
            .without_legacy(),
    )
    .routed()
}

#[tokio::test]
async fn a_set_with_targets_is_kept_and_read_back_on_both_routes() {
    let rig = rig("targets-smoke");
    let answered = grpc(
        &rig,
        SET_DESIRED,
        Some(&ci_token()),
        &set("web", "2.0", targets("web")),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let read = grpc(
        &rig,
        GET_RELEASE,
        Some(OPERATOR),
        &proto::GetReleaseRequest {
            service: "web".to_owned(),
            version: "2.0".to_owned(),
        },
    )
    .await;
    let release: proto::GetReleaseResponse = read.decoded();
    assert_eq!(release.targets, targets("web"));
    let body = set_json(&set("web", "2.1", targets("web"))).to_string();
    let (code, answer) = gateway(&rig, SET_DESIRED, Some(OPERATOR), &body).await;
    assert_eq!(code, 200, "{answer}");
}
