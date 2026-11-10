//! The host group of the example service hello (`examples/hello/host-group.json`), posted through
//! the gateway as `hkctl host-groups put --request` sends it: what docs/hello.md tells an operator
//! to put is a group the coordinator accepts, with the packages and the policy the farm proof
//! relies on.

use std::sync::Arc;

use bytes::Bytes;
use hostkeeper_enrollment::enroll::records::MemoryRecords;
use http_body_util::Full;
use hyper::{Request, StatusCode};
use serde_json::{Value, json};

use super::support::raw;
use crate::enrollment::tests::support::Rig as EnrollmentRig;
use crate::host_groups_support::coordinator_on;
use crate::http::Surface;
use crate::service::Coordinator;
use crate::store::Fake;
use crate::testutil::{OPERATOR, Rig};

const FILE: &str = include_str!("../../../../../examples/hello/host-group.json");
const PUT: &str = "/hostkeeper.operator.v1.HostGroups/Put";
const GET: &str = "/hostkeeper.operator.v1.HostGroups/Get";

/// The gateway's answer to `body` posted to `path` with the operator's token: the status and the
/// JSON.
async fn gateway(coordinator: &Arc<Coordinator>, path: &str, body: String) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::from(body)))
        .expect("a request");
    let (status, _, answer) = raw(coordinator, Surface::Control, &None, request).await;
    (status, serde_json::from_slice(&answer).expect("JSON"))
}

/// Proves: the file, with the reason `hkctl` adds, is accepted by `HostGroups.Put` through the
/// gateway as the first generation of a group `farm`, and `Get` answers the group with the
/// packages `hostkeeper` then `hello` and the policy of the file: a lone canary, two baked
/// beats, automatic rollback after one failed host or 25 percent.
///
/// Catches: a file the coordinator refuses, or a policy that no longer says what docs/hello.md
/// says of it.
#[tokio::test]
async fn the_hello_group_file_is_put_and_read_back() {
    let enrollment =
        EnrollmentRig::over_records("hello-group-enrollment", Arc::new(MemoryRecords::new()));
    let rig = Rig::new("hello-group");
    let coordinator = Arc::new(coordinator_on(
        &Fake::new(),
        &rig.clock,
        Some(Arc::clone(&enrollment.enrollment)),
    ));
    rig.at(2_000);
    let mut request: Value = serde_json::from_str(FILE).expect("the file is JSON");
    request["reason"] = json!("hello's farm");
    let (status, put) = gateway(&coordinator, PUT, request.to_string()).await;
    assert_eq!(
        (status, put["generation"].as_str()),
        (StatusCode::OK, Some("1"))
    );

    let (status, got) = gateway(&coordinator, GET, r#"{"hostGroup":"farm"}"#.to_owned()).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let group = &got["hostGroup"]["group"];
    assert_eq!(group["name"], "farm", "{got}");
    assert_eq!(group["packages"], json!(["hostkeeper", "hello"]), "{got}");
    let policy = &group["policy"];
    assert_eq!(policy["canary"], json!({"count": 1}), "{got}");
    assert_eq!(policy["bakeBeats"], 2, "{got}");
    assert_eq!(policy["rollback"], "ROLLBACK_MODE_AUTOMATIC", "{got}");
    assert_eq!(
        policy["rollbackAfterFailed"],
        json!({"percent": 25, "minHosts": 1}),
        "{got}"
    );
}
