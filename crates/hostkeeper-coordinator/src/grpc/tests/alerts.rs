//! `Alerts.List` end to end, through the router as the private listener serves it: a canary of
//! kbf fails its release over real heartbeats, and the alert comes back the same by gRPC and by
//! the gateway the UI calls.
//!
//! The gateway's answer here is the reply the UI's Fleet page is tested on
//! (`crates/hostkeeper-ui-tests/tests/fixtures/alerts_list.json`): the test below fails when the
//! coordinator's answer and that file part.

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, OrderId};
use http_body_util::Full;
use hyper::{Request, StatusCode};
use serde_json::Value;
use tonic::Code;

use super::operator::World;
use super::support::{call, decode, grpc_request, raw};
use crate::http::Surface;
use crate::rollback_tests::{HOSTS, ONE, TWO, failure, release, services_of, who};
use crate::testutil::{OPERATOR, Rig, beat, ci_token};

/// The reply of `Alerts.List` the UI's tests render, as this coordinator answers it.
const ALERTS_LIST: &str =
    include_str!("../../../../hostkeeper-ui-tests/tests/fixtures/alerts_list.json");

/// The path of `Alerts.List`.
const LIST: &str = "/hostkeeper.operator.v1.Alerts/List";

/// One heartbeat of each of the six daemons at `at`, healthy at 1.0 but `d1`, which sends
/// `d1_beat` when there is one.
fn round(world: &World, at: u64, d1_beat: Option<&Heartbeat>) {
    world.rig.at(at);
    for host in HOSTS {
        let sent = match d1_beat {
            Some(b) if host == "d1" => b.clone(),
            _ => beat("1.0"),
        };
        world
            .coordinator
            .heartbeat(who(host), sent)
            .expect("a heartbeat");
    }
}

/// A coordinator of kbf with one canary, in which the canary `d1` failed the release of 2.0:
/// 2.0 is marked bad and kbf rolled back to 1.0.
async fn failed_canary(name: &str) -> World {
    let rig = Rig::new(name);
    let started = Rig::start(&rig.dir, &rig.clock).with_services(services_of(
        "rollback = \"automatic\"\ncanary = { count = 1 }",
    ));
    let world = World::over(rig, started);
    let set = |body: &'static str| {
        let coordinator = world.coordinator.clone();
        async move {
            coordinator
                .set_desired(Some(&ci_token()), body.as_bytes())
                .await
                .expect("set");
        }
    };
    set(ONE).await;
    round(&world, 2_000, None);
    set(TWO).await;
    let (phase, class) = release();
    let mut failed = failure(phase, class, "2.0");
    let last = failed.last_update.as_mut().expect("a verdict");
    last.order_id = OrderId::parse("o-1");
    last.order_try = Some(1);
    round(&world, 2_015, Some(&failed));
    world
}

/// Proves: a canary's failed release, made by real heartbeats and the reconcile they run, is
/// listed by `Alerts.List` as `failed_canary` naming the canary host, the service and the
/// version, critical, first observed at the coordinator's time; the gRPC answer and the
/// gateway's JSON carry the same alert; and that JSON is the reply the UI's Fleet page is tested
/// on.
///
/// Catches: a failed canary that never reaches the alerts (the gap the Fleet page had), one of
/// the two APIs answering another alert than the other, and a UI test that renders a reply this
/// coordinator does not give.
#[tokio::test]
async fn a_failed_canary_is_the_same_alert_over_grpc_and_the_gateway() {
    let world = failed_canary("grpc-alerts-canary").await;
    let request = grpc_request(LIST, proto::ListAlertsRequest {}, Some(OPERATOR));
    let (code, said, body) = call(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!((code, said.as_str()), (Code::Ok, ""));
    let listed: proto::ListAlertsResponse = decode(&body.expect("a reply"));
    let want = proto::Alert {
        kind: proto::AlertKind::FailedCanary.into(),
        severity: proto::AlertSeverity::Critical.into(),
        host_id: "d1".to_owned(),
        service: "kbf".to_owned(),
        version: "2.0".to_owned(),
        message: "d1, a canary of kbf, failed 2.0: it is marked bad and rolled back".to_owned(),
        first_observed_unix: 1_800_000_000,
        request_id: String::new(),
        other_host_id: String::new(),
    };
    assert_eq!(listed.alerts, vec![want]);

    let request = Request::builder()
        .method("POST")
        .uri(LIST)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::from_static(b"{}")))
        .expect("a request");
    let (status, _, bytes) = raw(&world.coordinator, Surface::Control, &None, request).await;
    assert_eq!(status, StatusCode::OK);
    let json: Value = serde_json::from_slice(&bytes).expect("JSON");
    let fixture: Value = serde_json::from_str(ALERTS_LIST).expect("the fixture is JSON");
    assert_eq!(json, fixture);
}

/// Proves: CI's token is refused `forbidden` and no token `unauthorized`, by gRPC.
///
/// Catches: the alerts read with a token the operator API does not take.
#[tokio::test]
async fn alerts_list_refuses_ci_and_no_token() {
    let world = failed_canary("grpc-alerts-refused").await;
    for (bearer, want) in [
        (Some(ci_token()), (Code::PermissionDenied, "forbidden")),
        (None, (Code::Unauthenticated, "unauthorized")),
    ] {
        let request = grpc_request(LIST, proto::ListAlertsRequest {}, bearer.as_deref());
        let (code, said, _) = call(&world.coordinator, Surface::Control, &None, request).await;
        assert_eq!((code, said.as_str()), want);
    }
}
