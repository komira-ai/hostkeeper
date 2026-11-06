//! The gRPC methods that have a frozen JSON twin leave the same files as the JSON route: the
//! desired state `Releases.SetDesired` writes is the one `POST /v1/desired` writes, and the ops
//! state of `Hosts.Exclude`, `Hosts.Include`, `Hosts.SetLabels` and `Releases.ClearHalt` is the
//! one `POST /v1/operator` leaves. Each pair runs on two coordinators that start from the same
//! state, with the same clock for the audit file, and the files are compared byte for byte.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::hosts_server::Hosts as _;
use hostkeeper_proto::hostkeeper::operator::v1::releases_server::Releases as _;
use http_body_util::{BodyExt, Full};
use hyper::StatusCode;
use serde_json::{Value, json};
use tonic::Request;

use crate::audit_file::UnixClock;
use crate::channel::ChannelRefusal;
use crate::desired::{DesiredError, StoreError};
use crate::grpc::Admitted;
use crate::grpc::operator::{
    HostsService, ReleasesService, bearer_of, caller_of, converge_refusal, desired_body,
    desired_proto, hosts_refusal, releases_action_refusal, set_refusal,
};
use crate::http::{DESIRED_PATH, OPERATOR_PATH, Surface, handle};
use crate::operate::OperateError;
use crate::operator::Caller;
use crate::ops::OpsError;
use crate::registry::Digest;
use crate::service::{CiRefused, ConvergenceError, Coordinator, SetError, Unauthorized};
use crate::testutil::{OPERATOR, Rig, ci_token, desired, ops_of};

/// The time the audit file of every coordinator of these tests reads.
const AUDIT_UNIX: u64 = 1_800_000_000;

/// The desired state the tests set: version 2.0, one storage and one frontend host up.
const STATE: &str = r#"{"version":"2.0","min_up":{"storage":1,"frontend":1}}"#;

/// A coordinator over a state directory of its own, shared as the gRPC services share it.
pub(super) struct World {
    /// The coordinator.
    pub(super) coordinator: Arc<Coordinator>,
    /// The rig whose directory and clock the coordinator runs on.
    pub(super) rig: Rig,
}

impl World {
    /// A coordinator that manages the recorded kbf service, which is also the legacy one.
    pub(super) fn new(name: &str) -> Self {
        let rig = Rig::new(name);
        let started = Rig::start(&rig.dir, &rig.clock);
        Self::over(rig, started)
    }

    /// A world of `coordinator`, started on the directory of `rig`, whose audit file reads a
    /// fixed time so that two worlds that make the same changes write the same bytes.
    pub(super) fn over(rig: Rig, coordinator: Coordinator) -> Self {
        let unix: UnixClock = Arc::new(|| AUDIT_UNIX);
        let ops = ops_of(&rig.dir).with_unix(unix);
        Self::raw(rig, coordinator.with_ops(ops))
    }

    /// A world of `coordinator` exactly as it is.
    pub(super) fn raw(rig: Rig, coordinator: Coordinator) -> Self {
        Self {
            coordinator: Arc::new(coordinator),
            rig,
        }
    }

    /// The bytes of the file `name` of the state directory; none if there is none.
    pub(super) fn file(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.rig.dir.path().join(name)).ok()
    }

    /// Every file of the ops store, by name: the ops file, its audit file and its halts file.
    fn ops_files(&self) -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(self.rig.dir.path()).expect("the state directory") {
            let name = entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned();
            if name.starts_with("desired.json.ops") {
                let bytes = self.file(&name).expect("a file of the ops store");
                files.insert(name, bytes);
            }
        }
        files
    }
}

/// `message` as a call that the router admitted for the operator's token.
pub(super) async fn as_operator<T>(coordinator: &Coordinator, message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.extensions_mut().insert(Admitted::Operator {
        caller: coordinator.caller(Some(OPERATOR)).await,
        bearer: OPERATOR.to_owned(),
    });
    request
}

/// `message` as a call that the router admitted for a CI token nobody used.
pub(super) async fn as_ci<T>(coordinator: &Coordinator, message: T) -> Request<T> {
    let verified = coordinator
        .ci(Some(&ci_token()))
        .await
        .expect("CI's token is accepted");
    let mut request = Request::new(message);
    request.extensions_mut().insert(Admitted::Ci(verified));
    request
}

/// Posts `body` to the private listener's JSON `path` with `bearer`; the status and the JSON
/// answer.
pub(super) async fn post(
    coordinator: &Coordinator,
    path: &str,
    bearer: &str,
    body: &str,
) -> (StatusCode, Value) {
    let request = hyper::Request::builder()
        .method("POST")
        .uri(path)
        .header("authorization", format!("Bearer {bearer}"))
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request");
    let response = handle(
        coordinator,
        Surface::Control,
        &None,
        request,
        Duration::from_secs(30),
    )
    .await;
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (status, serde_json::from_slice(&bytes).expect("JSON"))
}

/// A desired state of `version` with the `min_up` limits `min_up`.
pub(super) fn state(version: &str, min_up: &[(&str, u32)]) -> proto::DesiredState {
    proto::DesiredState {
        version: version.to_owned(),
        max_updating: HashMap::new(),
        min_up: min_up
            .iter()
            .map(|(role, n)| ((*role).to_owned(), *n))
            .collect(),
        package_digest: None,
    }
}

/// Does the operator action `body` through the JSON route as the operator.
async fn json_operate(coordinator: &Coordinator, body: &str) -> Value {
    let (code, answer) = post(coordinator, OPERATOR_PATH, OPERATOR, body).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    answer
}

/// Asserts that the ops store of `grpc` holds exactly the bytes the ops store of `json` holds,
/// and that it holds something.
fn assert_same_ops(grpc: &World, json: &World) {
    let written = grpc.ops_files();
    assert!(
        written.contains_key("desired.json.ops"),
        "the change wrote the ops file: {:?}",
        written.keys().collect::<Vec<_>>()
    );
    assert!(
        written.contains_key("desired.json.ops.audit"),
        "the change wrote the audit file"
    );
    assert_eq!(written, json.ops_files());
}

/// The audit entry an operator's change `action` of number `seq` makes.
fn audit(
    seq: u64,
    action: &str,
    service: Option<&str>,
    node_id: Option<&str>,
    reason: Option<&str>,
) -> Option<proto::AuditEntry> {
    Some(proto::AuditEntry {
        seq,
        by: i32::from(proto::Actor::Operator),
        action: action.to_owned(),
        service: service.map(str::to_owned),
        node_id: node_id.map(str::to_owned),
        reason: reason.map(str::to_owned),
        at_unix: None,
        by_name: Some("operator".to_owned()),
    })
}

/// Proves that a desired state set over gRPC is stored as the JSON route stores it: a set of
/// the legacy service (no `service` in either call) by two coordinators leaves the same bytes
/// in `desired.json`, and the answer is the state as stored.
#[tokio::test]
async fn grpc_set_desired_and_json_set_desired_write_the_same_desired_file() {
    let grpc = World::new("grpc-set-desired-grpc");
    let json = World::new("grpc-set-desired-json");

    let (code, _) = post(&json.coordinator, DESIRED_PATH, &ci_token(), STATE).await;
    assert_eq!(code, StatusCode::OK);
    let request = proto::SetDesiredRequest {
        desired: Some(state("2.0", &[("storage", 1), ("frontend", 1)])),
        ..Default::default()
    };
    let reply = ReleasesService::new(Arc::clone(&grpc.coordinator))
        .set_desired(as_ci(&grpc.coordinator, request).await)
        .await
        .expect("the set")
        .into_inner();

    let written = grpc.file("desired.json").expect("the set wrote the file");
    assert!(String::from_utf8_lossy(&written).contains("\"2.0\""));
    assert_eq!(Some(written), json.file("desired.json"));
    let desired = reply.desired.expect("the state as stored");
    assert_eq!(desired.version, "2.0");
    assert_eq!(desired.min_up.get("storage"), Some(&1));
    assert_eq!(desired.min_up.get("frontend"), Some(&1));
    assert_eq!(desired.package_digest, None);
    assert!(reply.dropped_roles.is_empty());
}

/// Proves that a service named in the call, limits for a role, and a package digest sent beside
/// the state are stored as the JSON route stores a body with `service` and `release_digest`:
/// the two desired files are equal and the answer names the digest.
#[tokio::test]
async fn grpc_set_desired_with_a_service_limits_and_a_digest_writes_the_json_routes_file() {
    let digest = "ab".repeat(32);
    let grpc = World::new("grpc-set-digest-grpc");
    let json = World::new("grpc-set-digest-json");

    let body = json!({
        "service": "kbf",
        "version": "2.1",
        "max_updating": {"frontend": 3},
        "min_up": {"storage": 2, "frontend": 1},
        "release_digest": digest,
    });
    let (code, _) = post(
        &json.coordinator,
        DESIRED_PATH,
        &ci_token(),
        &body.to_string(),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let mut desired = state("2.1", &[("storage", 2), ("frontend", 1)]);
    desired.max_updating.insert("frontend".to_owned(), 3);
    let request = proto::SetDesiredRequest {
        service: "kbf".to_owned(),
        desired: Some(desired),
        package_digest: Some(digest.clone()),
        ..Default::default()
    };
    let reply = ReleasesService::new(Arc::clone(&grpc.coordinator))
        .set_desired(as_ci(&grpc.coordinator, request).await)
        .await
        .expect("the set")
        .into_inner();

    let written = grpc.file("desired.json").expect("the set wrote the file");
    assert!(String::from_utf8_lossy(&written).contains(&digest));
    assert_eq!(Some(written), json.file("desired.json"));
    let stored = reply.desired.expect("the state as stored");
    assert_eq!(stored.package_digest, Some(digest));
    assert_eq!(stored.max_updating.get("frontend"), Some(&3));
}

/// Proves that `Hosts.Exclude` leaves the ops state `POST /v1/operator` leaves for the same
/// exclusion: equal ops, audit and halts files, and the same audit entry in the answer.
#[tokio::test]
async fn grpc_exclude_and_json_operator_exclude_leave_the_same_ops_state() {
    let grpc = World::new("grpc-exclude-grpc");
    let json = World::new("grpc-exclude-json");

    let entry = json_operate(
        &json.coordinator,
        r#"{"action":"exclude","service":"kbf","node_id":"d1","reason":"disk swap"}"#,
    )
    .await;
    let request = proto::ExcludeRequest {
        service: "kbf".to_owned(),
        node_id: "d1".to_owned(),
        reason: "disk swap".to_owned(),
        abandon: false,
    };
    let reply = HostsService::new(Arc::clone(&grpc.coordinator))
        .exclude(as_operator(&grpc.coordinator, request).await)
        .await
        .expect("the exclusion")
        .into_inner();

    assert_same_ops(&grpc, &json);
    assert_eq!(
        entry,
        json!({"seq":1,"by":"operator","action":"exclude","service":"kbf","node_id":"d1","reason":"disk swap"})
    );
    assert_eq!(
        reply.entry,
        audit(1, "exclude", Some("kbf"), Some("d1"), Some("disk swap"))
    );
}

/// Proves that `Hosts.Include` leaves the ops state `POST /v1/operator` leaves for the same
/// inclusion of an excluded host: equal files, and the entry the JSON route answers.
#[tokio::test]
async fn grpc_include_and_json_operator_include_leave_the_same_ops_state() {
    let grpc = World::new("grpc-include-grpc");
    let json = World::new("grpc-include-json");
    let exclude = r#"{"action":"exclude","service":"kbf","node_id":"d1","reason":"disk swap"}"#;
    json_operate(&grpc.coordinator, exclude).await;
    json_operate(&json.coordinator, exclude).await;

    let entry = json_operate(
        &json.coordinator,
        r#"{"action":"include","service":"kbf","node_id":"d1"}"#,
    )
    .await;
    let request = proto::IncludeRequest {
        service: "kbf".to_owned(),
        node_id: "d1".to_owned(),
    };
    let reply = HostsService::new(Arc::clone(&grpc.coordinator))
        .include(as_operator(&grpc.coordinator, request).await)
        .await
        .expect("the inclusion")
        .into_inner();

    assert_same_ops(&grpc, &json);
    assert_eq!(
        entry,
        json!({"seq":2,"by":"operator","action":"include","service":"kbf","node_id":"d1"})
    );
    assert_eq!(
        reply.entry,
        audit(2, "include", Some("kbf"), Some("d1"), None)
    );
}

/// Proves that `Hosts.SetLabels` leaves the ops state `POST /v1/operator` leaves for the same
/// labels: equal files, and the entry the JSON route answers.
#[tokio::test]
async fn grpc_set_labels_and_json_operator_set_labels_leave_the_same_ops_state() {
    let grpc = World::new("grpc-labels-grpc");
    let json = World::new("grpc-labels-json");

    let entry = json_operate(
        &json.coordinator,
        r#"{"action":"set_labels","node_id":"d1","labels":{"zone":"a","tier":"gold"}}"#,
    )
    .await;
    let request = proto::SetLabelsRequest {
        node_id: "d1".to_owned(),
        labels: HashMap::from([
            ("zone".to_owned(), "a".to_owned()),
            ("tier".to_owned(), "gold".to_owned()),
        ]),
    };
    let reply = HostsService::new(Arc::clone(&grpc.coordinator))
        .set_labels(as_operator(&grpc.coordinator, request).await)
        .await
        .expect("the labels")
        .into_inner();

    assert_same_ops(&grpc, &json);
    assert_eq!(
        entry,
        json!({"seq":1,"by":"operator","action":"set_labels","node_id":"d1"})
    );
    assert_eq!(reply.entry, audit(1, "set_labels", None, Some("d1"), None));
}

/// Sets the desired state on `world` and halts kbf by the operator, so there is a halt to
/// clear.
async fn halted(world: &World) {
    world
        .coordinator
        .set_desired(Some(&ci_token()), STATE.as_bytes())
        .await
        .expect("the set");
    let caller = world.coordinator.caller(Some(OPERATOR)).await;
    world
        .coordinator
        .halt(
            &caller,
            proto::HaltRequest {
                service: "kbf".to_owned(),
                reason: "stop the line".to_owned(),
            },
        )
        .expect("the halt");
}

/// Proves that `Releases.ClearHalt` leaves the ops state `POST /v1/operator` leaves for the
/// same clear of the same halt: equal files (the acknowledgement and the clear generation
/// among them), and the entry the JSON route answers.
#[tokio::test]
async fn grpc_clear_halt_and_json_operator_clear_halt_leave_the_same_ops_state() {
    let grpc = World::new("grpc-clear-halt-grpc");
    let json = World::new("grpc-clear-halt-json");
    halted(&grpc).await;
    halted(&json).await;
    assert_same_ops(&grpc, &json);

    let entry = json_operate(
        &json.coordinator,
        r#"{"action":"clear_halt","service":"kbf","reason":"fixed"}"#,
    )
    .await;
    let request = proto::ClearHaltRequest {
        service: "kbf".to_owned(),
        reason: "fixed".to_owned(),
    };
    let reply = ReleasesService::new(Arc::clone(&grpc.coordinator))
        .clear_halt(as_operator(&grpc.coordinator, request).await)
        .await
        .expect("the clear")
        .into_inner();

    assert_same_ops(&grpc, &json);
    assert_eq!(
        entry,
        json!({"seq":2,"by":"operator","action":"clear_halt","service":"kbf","reason":"fixed"})
    );
    assert_eq!(
        reply.entry,
        audit(2, "clear_halt", Some("kbf"), None, Some("fixed"))
    );
}

/// What `operate` says of a service the coordinator does not manage.
const UNKNOWN_SERVICE: &str = "service is not one this coordinator manages";

/// What `operate` says of a clear of a service that is not halted.
const NOT_HALTED: &str = "the service is not halted";

/// The error of a write that failed.
pub(super) fn disk_full() -> OpsError {
    OpsError::Io(StoreError::Io {
        path: "ops.json".into(),
        source: std::io::Error::other("disk full"),
    })
}

fn invalid() -> OperateError {
    OperateError::Invalid("something else")
}

fn capped() -> OperateError {
    OperateError::Capped {
        what: "exclusions (MAX_EXCLUDED)",
        max: 4096,
    }
}

/// Proves the refusal of `Hosts` for each cause of a failed operator action: the token
/// refused, the unknown service the action names, any other invalid action, a full ops file,
/// and a failed write.
#[test]
fn a_failed_operator_action_is_refused_by_hosts_with_the_value_its_cause_names() {
    use proto::HostsRefusal as R;
    assert_eq!(
        hosts_refusal(OperateError::Unauthorized(Unauthorized)),
        R::Unauthorized
    );
    assert_eq!(
        hosts_refusal(OperateError::Invalid(UNKNOWN_SERVICE)),
        R::UnknownService
    );
    assert_eq!(hosts_refusal(invalid()), R::InvalidRequest);
    assert_eq!(hosts_refusal(capped()), R::Capacity);
    assert_eq!(
        hosts_refusal(OperateError::Store(disk_full())),
        R::StoreFailed
    );
}

/// Proves the refusal of `Releases` for each cause of a failed operator action: a service
/// that is not halted is `not_halted`, a full ops file is only an invalid request (there is no
/// capacity refusal), and the other causes are as for `Hosts`.
#[test]
fn a_failed_operator_action_is_refused_by_releases_with_the_value_its_cause_names() {
    use proto::ReleasesRefusal as R;
    assert_eq!(
        releases_action_refusal(OperateError::Unauthorized(Unauthorized)),
        R::Unauthorized
    );
    assert_eq!(
        releases_action_refusal(OperateError::Invalid(UNKNOWN_SERVICE)),
        R::UnknownService
    );
    assert_eq!(
        releases_action_refusal(OperateError::Invalid(NOT_HALTED)),
        R::NotHalted
    );
    assert_eq!(releases_action_refusal(invalid()), R::InvalidRequest);
    assert_eq!(releases_action_refusal(capped()), R::InvalidRequest);
    assert_eq!(
        releases_action_refusal(OperateError::Store(disk_full())),
        R::StoreFailed
    );
}

/// Proves the refusal of `Releases` for every way CI's set of a desired state is refused.
#[test]
fn every_way_a_set_of_the_desired_state_fails_is_refused_with_its_value() {
    use proto::ReleasesRefusal as R;
    let malformed = || DesiredError::Malformed("a field".to_owned());
    let cases = [
        (SetError::Unauthorized("missing"), R::Unauthorized),
        (SetError::Unavailable, R::KeysUnavailable),
        (SetError::Forbidden, R::Forbidden),
        (
            SetError::Invalid(DesiredError::UnknownService),
            R::UnknownService,
        ),
        (SetError::Invalid(DesiredError::Count), R::InvalidRequest),
        (SetError::Invalid(malformed()), R::InvalidRequest),
        (SetError::UnparseableVersion, R::InvalidRequest),
        (SetError::DigestConflict, R::InvalidRequest),
        (SetError::BelowMinVersion, R::BelowMinVersion),
        (SetError::BadVersion, R::HeldBad),
        (
            SetError::Channel(ChannelRefusal::NotInChannel),
            R::NotInChannel,
        ),
        (
            SetError::Channel(ChannelRefusal::DigestMismatch),
            R::ChannelDigestMismatch,
        ),
        (
            SetError::Store(StoreError::Io {
                path: "desired.json".into(),
                source: std::io::Error::other("disk full"),
            }),
            R::StoreFailed,
        ),
        (SetError::Ops(disk_full()), R::StoreFailed),
    ];
    for (why, refusal) in cases {
        let text = why.to_string();
        assert_eq!(set_refusal(why), refusal, "{text}");
    }
}

/// Proves the refusal of `Releases` for every way a convergence is not read: CI's token not
/// accepted, its issuer's keys not available, and a service the coordinator does not manage.
#[test]
fn every_way_a_convergence_is_not_read_is_refused_with_its_value() {
    use proto::ReleasesRefusal as R;
    assert_eq!(
        converge_refusal(ConvergenceError::Refused(CiRefused::Unauthorized("bad"))),
        R::Unauthorized
    );
    assert_eq!(
        converge_refusal(ConvergenceError::Refused(CiRefused::Unavailable)),
        R::KeysUnavailable
    );
    assert_eq!(
        converge_refusal(ConvergenceError::UnknownService),
        R::UnknownService
    );
}

/// The JSON `desired_body` makes of `request`.
fn body_of(request: proto::SetDesiredRequest) -> Value {
    let bytes = desired_body(request).expect("a body");
    serde_json::from_slice(&bytes).expect("JSON")
}

/// A request that sets `version` 2.0 of `service` with a limit per role.
fn limits_request(service: &str) -> proto::SetDesiredRequest {
    let mut desired = state("2.0", &[("storage", 2)]);
    desired.max_updating.insert("frontend".to_owned(), 1);
    proto::SetDesiredRequest {
        service: service.to_owned(),
        desired: Some(desired),
        ..Default::default()
    }
}

/// Proves the body the JSON route reads is made of the request: the service, the version, both
/// limits, and no `release_digest` when the request names none.
#[test]
fn the_json_body_of_a_set_holds_the_service_the_version_and_the_limits() {
    assert_eq!(
        body_of(limits_request("kbf")),
        json!({
            "service": "kbf",
            "version": "2.0",
            "max_updating": {"frontend": 1},
            "min_up": {"storage": 2},
        })
    );
}

/// Proves that a request that names no service makes a body that names none, which the JSON
/// route reads as the legacy service.
#[test]
fn a_set_that_names_no_service_makes_a_body_that_names_none() {
    let body = body_of(limits_request(""));
    assert_eq!(body.get("service"), None);
    assert_eq!(body["version"], "2.0");
}

/// Proves where the package digest of a body comes from: the state, the request beside it, or
/// both when they are the same text.
#[test]
fn the_digest_of_a_body_is_the_one_in_the_state_or_beside_it_or_both_when_equal() {
    let digest = "ab".repeat(32);

    let mut inside = limits_request("kbf");
    inside.desired.as_mut().expect("a state").package_digest = Some(digest.clone());
    assert_eq!(body_of(inside)["release_digest"], json!(digest));

    let mut beside = limits_request("kbf");
    beside.package_digest = Some(digest.clone());
    assert_eq!(body_of(beside)["release_digest"], json!(digest));

    let mut both = limits_request("kbf");
    both.desired.as_mut().expect("a state").package_digest = Some(digest.clone());
    both.package_digest = Some(digest.clone());
    assert_eq!(body_of(both)["release_digest"], json!(digest));

    assert_eq!(body_of(limits_request("kbf")).get("release_digest"), None);
}

/// Proves that a request that cannot be honoured is refused `invalid_request`, never partly
/// done: two different digests, a precondition on the generation, a write id (the store keeps
/// neither), and no desired state at all.
#[test]
fn a_set_that_cannot_be_honoured_makes_no_body() {
    let invalid = Err(proto::ReleasesRefusal::InvalidRequest);

    let mut differing = limits_request("kbf");
    differing.desired.as_mut().expect("a state").package_digest = Some("ab".repeat(32));
    differing.package_digest = Some("cd".repeat(32));
    assert_eq!(desired_body(differing), invalid);

    let mut generation = limits_request("kbf");
    generation.if_generation = Some(3);
    assert_eq!(desired_body(generation), invalid);

    let mut write_id = limits_request("kbf");
    write_id.write_id = Some("write-1".to_owned());
    assert_eq!(desired_body(write_id), invalid);

    let mut missing = limits_request("kbf");
    missing.desired = None;
    assert_eq!(desired_body(missing), invalid);
}

/// Proves the desired state of the answer: the version as text, both limits per role as the
/// state is written (the four roles of the format filled in), and the digest when there is
/// one.
#[test]
fn the_desired_state_of_an_answer_holds_the_version_the_limits_and_the_digest() {
    let mut one = desired("2.0", 1, 2);
    let answered = desired_proto(&one);
    assert_eq!(answered.version, "2.0");
    assert_eq!(
        answered.max_updating,
        HashMap::from([
            ("frontend".to_owned(), 1),
            ("storage".to_owned(), 1),
            ("daemon".to_owned(), 2),
            ("mac-daemon".to_owned(), 2),
        ])
    );
    assert_eq!(
        answered.min_up,
        HashMap::from([("storage".to_owned(), 1), ("frontend".to_owned(), 2)])
    );
    assert_eq!(answered.package_digest, None);

    let digest = "cd".repeat(32);
    one.release_digest = Some(Digest::parse(&digest).expect("a digest"));
    assert_eq!(desired_proto(&one).package_digest, Some(digest));
}

/// Proves who a call is from: the operator the router admitted, and nobody for a call with no
/// admission and for one admitted as another kind of caller, whose bearer is not the operator's
/// either.
#[tokio::test]
async fn a_call_is_from_the_operator_the_router_admitted_and_from_no_one_otherwise() {
    let world = World::new("operator-caller");

    let operator = as_operator(&world.coordinator, ()).await;
    assert!(matches!(
        caller_of(&operator),
        Caller::Operator(name) if name.as_str() == "operator"
    ));
    assert_eq!(bearer_of(&operator), Some(OPERATOR));

    let unadmitted = Request::new(());
    assert!(matches!(caller_of(&unadmitted), Caller::Anonymous));
    assert_eq!(bearer_of(&unadmitted), None);

    let ci = as_ci(&world.coordinator, ()).await;
    assert!(matches!(caller_of(&ci), Caller::Anonymous));
    assert_eq!(bearer_of(&ci), None);
}
