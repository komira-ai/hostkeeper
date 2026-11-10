//! hello's failing build through the coordinator, over gRPC and its operator calls: the farm
//! proof of docs/hello.md section 4 on hosts that install hello from the apt repository. The
//! release is set with the entry of the repository `komira-testing`; the failing build
//! (`1.2.3-fail-ready`) with the entry of the repository `komira-testing-fail`, package version
//! `1.2.3~fail.ready`. A host that fails the failing build is rolled back by the coordinator
//! under hello's rollout (`examples/hello/rollout.toml`), and another service keeps rolling.
//! The registry's repository `komira` lists the suites `testing`, `stable` and `testing-fail`.

mod support;

use std::collections::BTreeMap;

use bytes::Bytes;
use hostkeeper_coordinator::grpc::route;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::release::v1::Target;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Version};
use prost::Message;
use support::beats::{Daemon, HEARTBEAT, STATUS, grpc_beat, runs, said};
use support::rig::{PATIENCE, Rig, Spec};
use support::targets::{
    Answered, SET_DESIRED, framed, grpc, komira, list, put_repository, repository_entry,
};
use support::{OPERATOR, ci_token};

/// hello's rollout entry as the example file has it (role `farm`), and a second service on
/// hosts of the role `edge`.
const SERVICES: &str = concat!(
    include_str!("../../../examples/hello/rollout.toml"),
    "\n[[service]]\nname = \"other\"\nroles = [\"edge\"]\n"
);

const X86: agent::Platform = agent::Platform::LinuxX8664;
const APT: &str = "apt";
const NORMAL: &str = "komira-testing";
const FAIL: &str = "komira-testing-fail";

/// The release, the failing build's service version and its Debian version.
const RELEASE: &str = "1.2.3";
const FAIL_READY: &str = "1.2.3-fail-ready";
const FAIL_DEB: &str = "1.2.3~fail.ready";

/// A time past the coordinator's quiet start, and the gap between two beats of a host.
const LATER: u64 = 2_000;
const BEAT: u64 = 15;

fn rig(name: &str) -> Rig {
    Rig::new(
        name,
        Spec::recorded()
            .with_services(SERVICES)
            .with_oidc_services(&["hello", "other"])
            .without_legacy(),
    )
    .routed()
}

/// hello's entry of the release: the repository `komira-testing`, package version 1.2.3.
fn release_entry() -> Target {
    repository_entry("hello", RELEASE, 'a')
}

/// hello's entry of the failing build: the repository `komira-testing-fail`, package version
/// 1.2.3~fail.ready.
fn fail_entry() -> Target {
    Target {
        repository: Some(FAIL.to_owned()),
        ..repository_entry("hello", FAIL_DEB, 'f')
    }
}

/// A set of `service` at `version` with the one entry `entry` for `linux-x86_64`, with room for
/// every host to update at once.
fn request(service: &str, version: &str, entry: Target) -> proto::SetDesiredRequest {
    let mut request = support::targets::set(
        service,
        version,
        BTreeMap::from([("linux-x86_64".to_owned(), list(vec![entry]))]),
    );
    request
        .desired
        .as_mut()
        .expect("a desired state")
        .max_updating = [("farm".to_owned(), 16), ("edge".to_owned(), 16)]
        .into_iter()
        .collect();
    request
}

/// Sets `request` as CI does.
async fn set(rig: &Rig, request: &proto::SetDesiredRequest) -> Answered {
    grpc(rig, SET_DESIRED, Some(&ci_token()), request).await
}

/// `service` as `Releases.Status` shows it.
async fn status(rig: &Rig, service: &str) -> proto::ServiceStatus {
    let answered = grpc(
        rig,
        STATUS,
        Some(OPERATOR),
        &proto::StatusRequest {
            service: service.to_owned(),
        },
    )
    .await;
    answered
        .decoded::<proto::StatusResponse>()
        .services
        .into_iter()
        .find(|one| one.service == service)
        .expect("the service is in the status")
}

/// The desired version of `service`, and the versions it marks bad.
async fn desired_and_bad(rig: &Rig, service: &str) -> (Option<String>, Vec<String>) {
    let status = status(rig, service).await;
    (
        status.desired.map(|desired| desired.version),
        status.marks.map(|marks| marks.bad).unwrap_or_default(),
    )
}

/// A host of the role `farm` (hello's) or `edge` (the other service's).
fn host(name: &str, role: &str) -> Daemon {
    Daemon {
        role: role.to_owned(),
        ..Daemon::new(name, X86, &["hostkeeper-archive", APT])
    }
}

/// The heartbeat of `daemon` over gRPC, whose service `service` reports `last_update`.
async fn beat_with_result(
    rig: &Rig,
    daemon: &Daemon,
    service: &str,
    last_update: agent::UpdateResult,
) -> agent::HeartbeatResponse {
    let mut body = daemon.request();
    body.services
        .iter_mut()
        .find(|one| one.name == service)
        .expect("the daemon runs the service")
        .last_update = Some(last_update);
    let request = Request::builder()
        .method("POST")
        .uri(HEARTBEAT)
        .version(Version::HTTP_2)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(Full::new(framed(&body.encode_to_vec())))
        .expect("a request");
    let peer = rig.updater(&daemon.host, &daemon.role);
    let response = route(
        &rig.coordinator,
        Surface::Heartbeat,
        &peer,
        request,
        PATIENCE,
    )
    .await;
    let collected = response.into_body().collect().await.expect("a body");
    let data: Bytes = collected.to_bytes();
    agent::HeartbeatResponse::decode(&data[5..]).expect("a heartbeat response")
}

/// The pieces of an order that matter here: its verb, version, and the target's repository and
/// package version.
fn picked(response: &agent::HeartbeatResponse) -> Option<(&'static str, String, String, String)> {
    said(response).map(|(verb, _, version, package, _, target)| {
        let target = target.expect("a target");
        assert_eq!(target.digest, package, "the package digest is the entry's");
        (
            verb,
            version,
            target.repository.expect("a repository"),
            target.package_version.expect("a package version"),
        )
    })
}

fn order(
    verb: &'static str,
    version: &str,
    repository: &str,
    package_version: &str,
) -> Option<(&'static str, String, String, String)> {
    Some((
        verb,
        version.to_owned(),
        repository.to_owned(),
        package_version.to_owned(),
    ))
}

/// The farm host `f1` ordered hello 1.2.3 as an install, running it, and baked: hello 1.2.3 is
/// the last good version. The edge host `e1` runs `other` 1.0, which is the desired one.
async fn converged_farm(name: &str) -> (Rig, Daemon, Daemon) {
    let rig = rig(name);
    // The registry's `komira` lists the suite of the failing build while its proof runs.
    let mut repository = komira();
    repository.suites.push("testing-fail".to_owned());
    put_repository(&rig, repository).await;
    let answered = set(&rig, &request("hello", RELEASE, release_entry())).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    let answered = set(
        &rig,
        &request("other", "1.0", repository_entry("other", "1.0-1", 'b')),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    rig.at(LATER);

    let fresh = host("f1", "farm");
    let answer = grpc_beat(&rig, &fresh).await;
    assert_eq!(
        picked(&answer),
        order("install", RELEASE, NORMAL, RELEASE),
        "a normal set orders the entry of the release, never the failing build's"
    );
    let f1 = host("f1", "farm").running(runs("hello", RELEASE, Some(APT)));
    for beat in 1..=3 {
        rig.at(LATER + beat * BEAT);
        assert_eq!(said(&grpc_beat(&rig, &f1).await), None);
    }
    let e1 = host("e1", "edge").running(runs("other", "1.0", Some(APT)));
    assert_eq!(said(&grpc_beat(&rig, &e1).await), None);
    let marks = status(&rig, "hello").await.marks.expect("marks");
    assert_eq!(marks.last_good.as_deref(), Some(RELEASE));
    (rig, f1, e1)
}

/// Proves the farm proof's rollback on hosts that install from the repository: with hello 1.2.3
/// converged and baked, the failing build set with its entry (repository `komira-testing-fail`,
/// package version `1.2.3~fail.ready`) orders the canary host that entry; the host reports the
/// failing build failed at the release gate, `not_ready`, and running 1.2.3 again; the
/// coordinator marks `1.2.3-fail-ready` bad, sets hello back to 1.2.3, and orders nothing more
/// to the host. CI setting the failing build again is refused (`held_bad`), and no host
/// is ordered it.
///
/// Catches: a failing build the rollout does not roll back, an order that names the release's
/// repository for the failing build (or the failing one for the release), or a version
/// marked bad that CI can set again.
#[tokio::test]
async fn a_host_that_fails_the_fail_ready_build_is_rolled_back_and_the_build_is_marked_bad() {
    let (rig, f1, _) = converged_farm("fail-ready-rollback").await;

    let answered = set(&rig, &request("hello", FAIL_READY, fail_entry())).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    rig.at(LATER + 10 * BEAT);
    let ordered = grpc_beat(&rig, &f1).await;
    assert_eq!(
        picked(&ordered),
        order("update", FAIL_READY, FAIL, FAIL_DEB),
        "the failing build is ordered the entry of its own repository"
    );
    let sent = ordered.order.expect("an order");

    // The daemon rolled the host back to 1.2.3 and reports the failing build's verdict.
    rig.at(LATER + 11 * BEAT);
    let reported = beat_with_result(
        &rig,
        &f1,
        "hello",
        agent::UpdateResult {
            target: FAIL_READY.to_owned(),
            outcome: agent::Outcome::Failed as i32,
            phase: agent::FailurePhase::GateRelease as i32,
            class: agent::FailureClass::Release as i32,
            reason: "not_ready".to_owned(),
            order_try: Some(sent.r#try),
            order_id: Some(sent.order_id.clone()),
            ..agent::UpdateResult::default()
        },
    )
    .await;
    assert_eq!(
        picked(&reported),
        None,
        "the host runs 1.2.3, which is wanted again"
    );
    assert_eq!(
        desired_and_bad(&rig, "hello").await,
        (Some(RELEASE.to_owned()), vec![FAIL_READY.to_owned()])
    );
    let marks = status(&rig, "hello").await.marks.expect("marks");
    assert_eq!(marks.last_good.as_deref(), Some(RELEASE));

    // CI cannot set the failing build again, and no host is ordered it.
    let again = set(&rig, &request("hello", FAIL_READY, fail_entry())).await;
    assert_ne!(again.code, 0, "{again:?}");
    assert_eq!(again.message, "held_bad", "{again:?}");
    rig.at(LATER + 12 * BEAT);
    let running = host("f1", "farm").running(runs("hello", RELEASE, Some(APT)));
    assert_eq!(picked(&grpc_beat(&rig, &running).await), None);
}

/// Proves another service rolls once hello's failing build has failed: a new version of `other`,
/// on a host of another role, waits while the failing build is in flight on hello's canary (that
/// update holds the farm's one live rollout), and is ordered to its host with its own entry as
/// soon as the canary reports the failure and hello is rolled back.
///
/// Catches: a failed and rolled-back service that still holds the one live rollout of the farm,
/// so no other service ever rolls again.
#[tokio::test]
async fn another_service_rolls_once_hellos_failing_build_is_rolled_back() {
    let (rig, f1, e1) = converged_farm("fail-ready-other-service").await;
    let answered = set(&rig, &request("hello", FAIL_READY, fail_entry())).await;
    assert_eq!(answered.code, 0, "{answered:?}");
    rig.at(LATER + 10 * BEAT);
    let sent = grpc_beat(&rig, &f1).await.order.expect("an order");

    // The other service's new version is set while hello's failing build is ordered.
    let answered = set(
        &rig,
        &request("other", "1.1", repository_entry("other", "1.1-1", 'c')),
    )
    .await;
    assert_eq!(answered.code, 0, "{answered:?}");
    rig.at(LATER + 11 * BEAT);
    assert_eq!(
        picked(&grpc_beat(&rig, &e1).await),
        None,
        "while hello's failing build is in flight on its canary, hello holds the farm's live rollout"
    );
    rig.at(LATER + 12 * BEAT);
    let reported = beat_with_result(
        &rig,
        &f1,
        "hello",
        agent::UpdateResult {
            target: FAIL_READY.to_owned(),
            outcome: agent::Outcome::Failed as i32,
            phase: agent::FailurePhase::GateRelease as i32,
            class: agent::FailureClass::Release as i32,
            reason: "not_ready".to_owned(),
            order_try: Some(sent.r#try),
            order_id: Some(sent.order_id),
            ..agent::UpdateResult::default()
        },
    )
    .await;
    assert_eq!(picked(&reported), None);
    assert_eq!(
        desired_and_bad(&rig, "hello").await,
        (Some(RELEASE.to_owned()), vec![FAIL_READY.to_owned()])
    );

    rig.at(LATER + 13 * BEAT);
    assert_eq!(
        picked(&grpc_beat(&rig, &e1).await),
        order("update", "1.1", NORMAL, "1.1-1"),
        "the other service rolls with hello rolled back"
    );
}
