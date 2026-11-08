//! A release index through the gRPC services: `Releases.SetDesired` takes the index and its
//! digest, refuses what it cannot take with a token and stores nothing, and answers the digest of
//! the index; `Coordinator.Heartbeat` answers each host the package of its platform and the
//! index's digest; the status shows a host whose platform the index does not list as
//! `NO_PACKAGE_FOR_PLATFORM`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use hostkeeper_testkit::Scratch;
use tonic::{Code, Response, Status};

use super::heartbeat::{beat, updater};
use super::operator::{World, as_ci, as_operator, post, state};
use crate::grpc::operator::ReleasesService;
use crate::grpc::report;
use crate::marks_support::{WEB, digest, rig, runs};
use crate::release_index_support::{ALL, beat_on, index_body, index_text, set_index, sha};
use crate::service::Coordinator;
use crate::testutil::{LEGACY, OPERATOR, Rig, ci_token, scratch};

/// The set of `version` of kbf over the index `text` whose digest is `digest_of_text`.
fn set_over(version: &str, text: &str, digest_of_text: &str) -> proto::SetDesiredRequest {
    proto::SetDesiredRequest {
        service: LEGACY.to_owned(),
        desired: Some(state(version, &[])),
        release_index: Some(text.to_owned()),
        release_index_digest: Some(digest_of_text.to_owned()),
        ..Default::default()
    }
}

/// Asserts that `result` is a refusal of `code` whose message is exactly `token`.
fn refused<T: std::fmt::Debug>(result: Result<Response<T>, Status>, token: &str) {
    let status = result.expect_err("the call is refused");
    assert_eq!(
        (status.code(), status.message()),
        (Code::InvalidArgument, token)
    );
}

/// A set whose index was changed after its digest was taken is refused `index_digest_mismatch`,
/// and every set that names an index it cannot take is `index_invalid`, each as an
/// `INVALID_ARGUMENT` whose message is the token, and the state files are not written. The same
/// sets are refused with the same tokens by the JSON route.
///
/// Catches: a tampered index taken, a refusal told as another code or token, an index beside a
/// package digest (inside the desired state or beside it) taken, and a refused set that writes.
#[tokio::test]
async fn a_tampered_index_is_refused_through_grpc_and_nothing_is_stored() {
    let world = World::new("grpc-index-refusals");
    let releases = ReleasesService::new(world.coordinator.clone());
    let text = index_text(LEGACY, "2.0", &ALL);
    let good = sha(&text);
    let tampered = text.replace(&digest(1), &digest(9));

    let call = |request: proto::SetDesiredRequest| {
        let world = &world;
        let releases = &releases;
        async move {
            releases
                .set_desired(as_ci(&world.coordinator, request).await)
                .await
        }
    };
    refused(
        call(set_over("2.0", &tampered, &good)).await,
        "index_digest_mismatch",
    );
    refused(
        call(set_over("2.0", &text[..text.len() - 1], &good)).await,
        "index_digest_mismatch",
    );

    let mut no_digest = set_over("2.0", &text, &good);
    no_digest.release_index_digest = None;
    refused(call(no_digest).await, "index_invalid");
    let mut no_index = set_over("2.0", &text, &good);
    no_index.release_index = None;
    refused(call(no_index).await, "index_invalid");
    let mut beside = set_over("2.0", &text, &good);
    beside.package_digest = Some(digest(1));
    refused(call(beside).await, "index_invalid");
    let mut inside = set_over("2.0", &text, &good);
    inside
        .desired
        .as_mut()
        .expect("a desired state")
        .package_digest = Some(digest(1));
    refused(call(inside).await, "index_invalid");
    let mut short = set_over("2.0", &text, &good);
    short.release_index_digest = Some(good[..63].to_owned());
    refused(call(short).await, "index_invalid");
    let other = index_text(LEGACY, "3.0", &ALL);
    refused(
        call(set_over("2.0", &other, &sha(&other))).await,
        "index_invalid",
    );
    let not_an_index = "not an index";
    refused(
        call(set_over("2.0", not_an_index, &sha(not_an_index))).await,
        "index_invalid",
    );

    for file in ["desired.json", "desired.json.ops", "desired.json.ops.audit"] {
        assert_eq!(
            world.file(file),
            None,
            "{file} was written by a refused set"
        );
    }

    let body = index_body(LEGACY, "2.0", &tampered, &good).to_string();
    let (status, answer) = post(&world.coordinator, "/v1/desired", &ci_token(), &body).await;
    assert_eq!(
        (status.as_u16(), answer),
        (400, serde_json::json!({"error": "index_digest_mismatch"}))
    );
    assert_eq!(world.file("desired.json"), None);
}

/// A set over an index that passes is answered with the digest of the index as the desired
/// state's `release_index_digest` and no `package_digest`; the status shows the same; and the
/// operator's set is audited with the digest.
///
/// Catches: the digest of the index missing from the answer, a package digest invented for an
/// index, and the status that does not show which release a version has.
#[tokio::test]
async fn a_set_over_an_index_is_answered_with_the_index_digest() {
    let world = World::new("grpc-index-set");
    let releases = ReleasesService::new(world.coordinator.clone());
    let text = index_text(LEGACY, "2.0", &ALL);
    let good = sha(&text);
    let answered = releases
        .set_desired(as_operator(&world.coordinator, set_over("2.0", &text, &good)).await)
        .await
        .expect("the set is accepted")
        .into_inner();
    let desired = answered.desired.expect("a desired state");
    assert_eq!(
        (
            desired.package_digest,
            desired.release_index_digest.as_deref()
        ),
        (None, Some(good.as_str()))
    );

    let status = world.coordinator.status(Some(OPERATOR)).expect("status");
    let shown = report::status(&status, LEGACY);
    let in_status = shown.services[0].desired.clone().expect("a desired state");
    assert_eq!(
        in_status.release_index_digest.as_deref(),
        Some(good.as_str())
    );
    assert_eq!(in_status.package_digest, None);

    let reason = world
        .coordinator
        .ops()
        .state()
        .audit
        .last()
        .and_then(|entry| entry.reason.clone());
    assert_eq!(reason, Some(format!("2.0 with release index {good}")));
}

/// A coordinator past its quiet start whose kbf at 2.0 is set over an index of all three
/// platforms; the directory keeps it alive.
async fn index_coordinator(name: &str) -> (Arc<Coordinator>, Scratch, String) {
    let dir = scratch(name);
    let clock = Arc::new(AtomicU64::new(1_000));
    let list = format!("{}services = [\"kbf\"]\n", config_text(STATIC_ISSUER));
    let coordinator = Rig::start_with_allow_list(&dir, &clock, &list);
    let text = index_text(LEGACY, "2.0", &ALL);
    let index = sha(&text);
    let body = index_body(LEGACY, "2.0", &text, &index);
    coordinator
        .set_desired(Some(&ci_token()), body.to_string().as_bytes())
        .await
        .expect("CI sets the version over its index");
    clock.store(2_000, Ordering::SeqCst);
    (Arc::new(coordinator), dir, index)
}

/// A heartbeat of an `order-v1` agent on `platform` that runs kbf at `running`.
fn kbf_beat(platform: agent::Platform, running: &str) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        platform: platform as i32,
        agent: Some(agent::AgentInfo {
            name: "hostkeeper-daemon".to_owned(),
            version: "1.0".to_owned(),
        }),
        features: vec!["order-v1".to_owned(), "heartbeat-v2".to_owned()],
        services: vec![agent::ServiceBeat {
            name: LEGACY.to_owned(),
            version: running.to_owned(),
            health: agent::Health::Healthy as i32,
            ..agent::ServiceBeat::default()
        }],
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root_age_secs: None,
        trusted_root_reason: 0,
    }
}

/// The `update` a heartbeat is answered with.
async fn updated(
    coordinator: &Arc<Coordinator>,
    host: &str,
    request: &agent::HeartbeatRequest,
) -> Option<agent::Update> {
    let reply = beat(coordinator, &updater(host, "daemon"), request).await;
    let response: agent::HeartbeatResponse = reply.decode();
    match response.order?.verb {
        Some(agent::order::Verb::Update(update)) => Some(update),
        other => panic!("an update was expected, found {other:?}"),
    }
}

/// The gRPC heartbeat answers a linux-x86_64 host and a macos-arm64 host (`darwin-arm64` on the
/// wire) each an `update` with the package digest of its own platform and the index's SHA-256,
/// and a host of a platform the index lacks no order.
///
/// Catches: the digest of one platform in every order, the Mac found under no key, and the
/// index digest missing from the typed order.
#[tokio::test]
async fn the_grpc_heartbeat_answers_each_platform_its_package_and_the_index_digest() {
    let (coordinator, _dir, index) = index_coordinator("grpc-index-heartbeat").await;
    for (host, platform, running_after, n) in [
        ("d1", agent::Platform::LinuxX8664, "2.0", 1),
        ("d2", agent::Platform::DarwinArm64, "2.0", 3),
        ("d3", agent::Platform::LinuxArm64, "2.0", 2),
    ] {
        let update = updated(&coordinator, host, &kbf_beat(platform, "1.0"))
            .await
            .expect("an update order");
        assert_eq!(update.version, "2.0", "{host}");
        assert_eq!(update.package_digest, digest(n), "{host}");
        assert_eq!(update.index_digest, index, "{host}");
        // The host runs it, which frees the one live update for the next host.
        assert_eq!(
            updated(&coordinator, host, &kbf_beat(platform, running_after)).await,
            None
        );
    }
}

/// The status of a host whose platform the index does not list holds it at
/// `NO_PACKAGE_FOR_PLATFORM`, and no order is sent.
///
/// Catches: the reason reported as unspecified, and an order sent to a host with no package.
#[tokio::test]
async fn the_status_holds_an_unlisted_host_at_no_package_for_platform() {
    let rig = rig("grpc-index-hold", "");
    set_index(&rig, WEB, "2.0", &ALL[..2]).await;
    let mac = beat_on(
        hostkeeper_wire::Platform::DarwinArm64,
        vec![runs(WEB, "1.0")],
    );
    let (reply, order) = crate::marks_support::ask_typed(&rig, "d1", "daemon", mac);
    assert_eq!(
        (reply, order),
        (hostkeeper_wire::wire::ServiceReply::Nothing, None)
    );
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    let shown = report::status(&status, WEB);
    let host = shown.services[0]
        .hosts
        .iter()
        .find(|host| host.node_id == "d1")
        .expect("the host");
    let reason = host.view.as_ref().map(|view| view.gate_reason);
    assert_eq!(reason, Some(proto::GateReason::NoPackageForPlatform as i32));
}
