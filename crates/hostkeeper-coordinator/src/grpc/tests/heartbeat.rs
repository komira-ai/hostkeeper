//! `Coordinator.Heartbeat` against the frozen JSON route: the same facts sent both ways get the
//! same decision, the host is the one the certificate names, and a caller that is no updater or
//! is denied is refused before its request is read. The helpers at the top (a farm, a call
//! through [`crate::grpc::route`], a reading of the reply) are shared with `limits` and `agent`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_proto::hostkeeper::agent::v1 as proto;
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::wire::HEARTBEAT_PATH;
use http_body_util::{BodyExt, Full};
use hyper::body::Body;
use hyper::{Request, Version};
use prost::Message;
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, SanType};
use serde_json::{Value, json};
use tonic::{Code, Status};

use crate::enrollment::Enrollment;
use crate::enrollment::tests::support::{HostKey, ONE_STORAGE, Rig as EnrollmentRig};
use crate::enrollment::tests::support::{enrolled, host_id, watched};
use crate::grpc::{RouteBody, route};
use crate::http::{BoxError, Peer, Surface};
use crate::service::Coordinator;
use crate::testutil::{OPERATOR, Rig, scratch, with_kbf};

/// How long a body has in these tests.
pub(super) const PATIENCE: Duration = Duration::from_secs(30);

/// The path of `Coordinator.Heartbeat`.
pub(super) const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";

/// A package digest CI registers for the versions the farm wants.
const DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";

/// The second service of the farm: a daemon service beside `kbf`, the coordinator's legacy one.
const SERVICES_TEXT: &str = "[[service]]\nname = \"other\"\nroles = [\"daemon\"]\n";

/// The features the agents of this file announce; the coordinator answers the ones it
/// advertises, in its own order.
const ANNOUNCED: [&str; 4] = ["order-v1", "halt-v1", "deferred-v1", "target-v1"];

/// What the coordinator answers an agent that announced [`ANNOUNCED`].
const AGREED: [&str; 4] = ["deferred-v1", "halt-v1", "order-v1", "target-v1"];

/// The agent of the heartbeats of this file.
const AGENT_NAME: &str = "hostkeeper-daemon";
const AGENT_VERSION: &str = "1.0";

// ---- the certificates ---------------------------------------------------------------------

fn san(sans: &[&str]) -> Vec<SanType> {
    sans.iter()
        .map(|s| match s.strip_prefix("dns:") {
            Some(name) => SanType::DnsName(name.try_into().expect("a name")),
            None => SanType::URI((*s).try_into().expect("a uri")),
        })
        .collect()
}

/// The leaf of a self-signed client certificate with `sans`, as a listener hands it over.
pub(super) fn peer(sans: &[&str]) -> Peer {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = san(sans);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("key");
    Some(Arc::from(
        params.self_signed(&key).expect("sign").der().as_ref(),
    ))
}

/// The certificate of the farm's updater `name` whose host role is `host_role`.
pub(super) fn updater(name: &str, host_role: &str) -> Peer {
    peer(&[
        "urn:kbf:role:updater",
        &format!("urn:kbf:host-role:{host_role}"),
        &format!("dns:{name}"),
    ])
}

/// The peer that holds the certificate `der`.
pub(super) fn peer_of(der: &[u8]) -> Peer {
    Some(Arc::from(der))
}

// ---- the farm -----------------------------------------------------------------------------

/// A coordinator past its quiet start on a clock the test holds still, with a scratch directory
/// of its own.
pub(super) struct Farm {
    pub coordinator: Arc<Coordinator>,
    _dir: Scratch,
}

/// A farm of `kbf` (the legacy service, wanting 2.0) and `other` (a daemon service wanting 3.0),
/// CI having registered a package digest for both versions, so that an agent that announced
/// `order-v1` is ordered them.
pub(super) async fn farm(name: &str) -> Farm {
    let dir = scratch(name);
    let clock = Arc::new(AtomicU64::new(1_000));
    let list = format!(
        "{}services = [\"kbf\", \"other\"]\n",
        config_text(STATIC_ISSUER)
    );
    let coordinator =
        Rig::start_with_allow_list(&dir, &clock, &list).with_services(with_kbf(SERVICES_TEXT));
    let digest = DIGEST;
    for body in [
        format!(
            r#"{{"version":"2.0","min_up":{{"storage":0,"frontend":0}},"release_digest":"{digest}"}}"#
        ),
        format!(
            r#"{{"service":"other","version":"3.0","min_up":{{"storage":0,"frontend":0}},"release_digest":"{digest}"}}"#
        ),
    ] {
        crate::testutil::set_native(&coordinator, &body).await;
    }
    clock.store(2_000, Ordering::SeqCst);
    Farm {
        coordinator: Arc::new(coordinator),
        _dir: dir,
    }
}

/// A farm of `kbf` alone, wanting 2.0 with one storage host at a time, over `enrollment`: the
/// one the enrollment tests use, at the time they leave it.
pub(super) async fn farm_enrolled(name: &str, enrollment: &Arc<Enrollment>) -> Farm {
    let dir = scratch(name);
    let clock = Arc::new(AtomicU64::new(1_000));
    let coordinator = Rig::start(&dir, &clock).with_enrollment(Arc::clone(enrollment));
    clock.store(2_000, Ordering::SeqCst);
    crate::testutil::set_native(&coordinator, ONE_STORAGE).await;
    clock.store(2_100, Ordering::SeqCst);
    Farm {
        coordinator: Arc::new(coordinator),
        _dir: dir,
    }
}

impl Farm {
    /// The operator clears the halt of `service`.
    fn clear_halt(&self, service: &str) {
        let body =
            format!(r#"{{"action":"clear_halt","service":"{service}","reason":"disk replaced"}}"#);
        self.coordinator
            .operate(Some(OPERATOR), body.as_bytes())
            .expect("the halt is cleared");
    }

    /// The hosts the operator's status shows, as (node id, role), and the count of heartbeats
    /// whose `services` list was dropped.
    pub(super) fn heard_from(&self) -> (Vec<(String, String)>, u64) {
        let status = self
            .coordinator
            .status(Some(OPERATOR))
            .expect("the operator reads the status");
        let hosts = status
            .hosts
            .iter()
            .map(|host| (host.node_id.clone(), host.role.clone()))
            .collect();
        (hosts, status.services_dropped)
    }
}

// ---- the calls ----------------------------------------------------------------------------

/// `message` as one gRPC frame: no compression, its length, the message.
pub(super) fn frame(message: &[u8]) -> Bytes {
    let mut framed = Vec::with_capacity(5 + message.len());
    framed.push(0);
    framed.extend_from_slice(
        &u32::try_from(message.len())
            .expect("a message under 4 GiB")
            .to_be_bytes(),
    );
    framed.extend_from_slice(message);
    Bytes::from(framed)
}

/// What a gRPC call came back with: the status, and the message of an `OK` one.
pub(super) struct Reply {
    pub status: Status,
    pub message: Vec<u8>,
}

impl Reply {
    /// The code and the message (the token of a refusal) of the status.
    pub(super) fn token(&self) -> (Code, &str) {
        (self.status.code(), self.status.message())
    }

    /// The message of an `OK` reply, decoded.
    pub(super) fn decode<M: Message + Default>(&self) -> M {
        assert_eq!(
            self.status.code(),
            Code::Ok,
            "not OK: {}",
            self.status.message()
        );
        M::decode(self.message.as_slice()).expect("the reply is the method's message")
    }
}

/// The status of a gRPC response, from its headers (a refusal that sent no message) or its
/// trailers, and the message of an `OK` one.
pub(super) async fn read_reply(response: hyper::Response<RouteBody>) -> Reply {
    let (parts, body) = response.into_parts();
    let collected = body.collect().await.expect("a body");
    let status = Status::from_header_map(&parts.headers)
        .or_else(|| collected.trailers().and_then(Status::from_header_map))
        .expect("the response carries a grpc-status");
    let bytes = collected.to_bytes();
    let message = if status.code() == Code::Ok {
        assert!(bytes.len() >= 5, "an OK reply carries a message frame");
        assert_eq!(bytes[0], 0, "the reply is not compressed");
        let length = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        assert_eq!(bytes.len(), 5 + usize::try_from(length).expect("a length"));
        bytes[5..].to_vec()
    } else {
        assert!(bytes.is_empty(), "a refusal carries no message");
        Vec::new()
    };
    Reply { status, message }
}

/// The gRPC call of `path` with the body `body`, over HTTP/2, as a listener hands it to
/// [`route`].
pub(super) async fn send<B>(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    peer: &Peer,
    path: &str,
    bearer: Option<&str>,
    body: B,
) -> Reply
where
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<BoxError> + Send + 'static,
{
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .version(Version::HTTP_2)
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder.body(body).expect("a request");
    read_reply(route(coordinator, surface, peer, request, PATIENCE).await).await
}

/// The gRPC call of `path` whose request message is the encoded bytes `message`.
pub(super) async fn grpc_call(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    peer: &Peer,
    path: &str,
    bearer: Option<&str>,
    message: &[u8],
) -> Reply {
    send(
        coordinator,
        surface,
        peer,
        path,
        bearer,
        Full::new(frame(message)),
    )
    .await
}

/// `Coordinator.Heartbeat` of `request` from `peer`.
pub(super) async fn beat(
    coordinator: &Arc<Coordinator>,
    peer: &Peer,
    request: &proto::HeartbeatRequest,
) -> Reply {
    grpc_call(
        coordinator,
        Surface::Heartbeat,
        peer,
        HEARTBEAT,
        None,
        &request.encode_to_vec(),
    )
    .await
}

/// `POST /v1/heartbeat` of the JSON `body` from `peer`, over HTTP/1.1: the status and the JSON
/// answer.
pub(super) async fn json_beat(
    coordinator: &Arc<Coordinator>,
    peer: &Peer,
    body: &str,
) -> (u16, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(HEARTBEAT_PATH)
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("a request");
    let response = route(coordinator, Surface::Heartbeat, peer, request, PATIENCE).await;
    let code = response.status().as_u16();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    (code, serde_json::from_slice(&bytes).expect("JSON"))
}

/// The heartbeat of a host that runs kbf `running` (and is updating to `updating`) and sends
/// no agent: its `kbf` entry is the JSON top level, as the coordinator's legacy service.
pub(super) fn legacy_beat(running: &str, updating: Option<&str>) -> proto::HeartbeatRequest {
    proto::HeartbeatRequest {
        platform: proto::Platform::LinuxX8664 as i32,
        services: vec![proto::ServiceBeat {
            name: "kbf".to_owned(),
            version: running.to_owned(),
            health: proto::Health::Healthy as i32,
            updating: updating.map(str::to_owned),
            ..proto::ServiceBeat::default()
        }],
        ..proto::HeartbeatRequest::default()
    }
}

// ---- the same facts, sent both ways -------------------------------------------------------

/// What a service entry reports besides its version.
#[derive(Clone, Copy)]
pub(super) enum Report {
    /// The update to 2.0 halted.
    Halt,
    /// The update to 2.0 was declined for the host's moment.
    Deferred,
}

/// One service of a heartbeat, healthy.
#[derive(Clone)]
pub(super) struct Svc {
    pub name: String,
    pub version: &'static str,
    pub report: Option<Report>,
}

pub(super) fn svc(name: &str, version: &'static str) -> Svc {
    Svc {
        name: name.to_owned(),
        version,
        report: None,
    }
}

impl Svc {
    pub(super) fn reporting(mut self, report: Report) -> Svc {
        self.report = Some(report);
        self
    }
}

fn json_service(service: &Svc) -> Value {
    let mut entry = json!({"name": service.name, "version": service.version, "health": "healthy"});
    match service.report {
        Some(Report::Halt) => {
            entry["last_update"] = json!({
                "target": "2.0", "outcome": "failed", "phase": "halt", "reason": "disk_tainted"
            });
        }
        Some(Report::Deferred) => {
            entry["deferred"] =
                json!({"target": "2.0", "reason": "drain_bound", "retry_after_secs": 300});
        }
        None => {}
    }
    entry
}

/// The heartbeat of an agent that announced [`ANNOUNCED`] and lists `services`, as the
/// daemon's JSON link sends it: the agent is the top level.
pub(super) fn json_text(services: &[Svc]) -> String {
    json!({
        "platform": "linux-x86_64",
        "version": AGENT_VERSION,
        "health": "healthy",
        "services": services.iter().map(json_service).collect::<Vec<_>>(),
        "agent_features": ANNOUNCED,
        "agent": {"name": AGENT_NAME, "version": AGENT_VERSION},
    })
    .to_string()
}

fn proto_service(service: &Svc) -> proto::ServiceBeat {
    let mut entry = proto::ServiceBeat {
        name: service.name.clone(),
        version: service.version.to_owned(),
        health: proto::Health::Healthy as i32,
        installer: Some("apt".to_owned()),
        ..proto::ServiceBeat::default()
    };
    match service.report {
        Some(Report::Halt) => {
            entry.last_update = Some(proto::UpdateResult {
                target: "2.0".to_owned(),
                outcome: proto::Outcome::Failed as i32,
                phase: proto::FailurePhase::Halt as i32,
                reason: "disk_tainted".to_owned(),
                ..proto::UpdateResult::default()
            });
        }
        Some(Report::Deferred) => {
            entry.deferred = Some(proto::DeferredReport {
                target: "2.0".to_owned(),
                reason: "drain_bound".to_owned(),
                retry_after_secs: 300,
            });
        }
        None => {}
    }
    entry
}

/// The same heartbeat as [`json_text`], as `Coordinator.Heartbeat` carries it.
pub(super) fn proto_message(services: &[Svc]) -> proto::HeartbeatRequest {
    proto::HeartbeatRequest {
        installers: vec!["apt".to_owned()],
        platform: proto::Platform::LinuxX8664 as i32,
        agent: Some(proto::AgentInfo {
            name: AGENT_NAME.to_owned(),
            version: AGENT_VERSION.to_owned(),
        }),
        features: ANNOUNCED.iter().map(|name| (*name).to_owned()).collect(),
        services: services.iter().map(proto_service).collect(),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root_age_secs: None,
        trusted_root_reason: 0,
    }
}

/// An update order as either transport carries it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Update {
    service: String,
    version: String,
    order_id: String,
    order_try: u64,
    package_digest: String,
}

/// What the coordinator decided for a heartbeat, in the terms both transports share.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Heard {
    /// The heartbeat was not answered: it is no heartbeat the coordinator reads.
    Refused,
    /// The answer: the update ordered, the features, the clear generations.
    Told {
        update: Option<Update>,
        features: Vec<String>,
        halts: BTreeMap<String, u64>,
    },
}

fn text(value: &Value) -> String {
    value.as_str().expect("a string").to_owned()
}

/// The decision a JSON answer says.
pub(super) fn heard_json(code: u16, body: &Value) -> Heard {
    if code == 400 {
        assert_eq!(*body, json!({"error": "bad_request"}));
        return Heard::Refused;
    }
    assert_eq!(code, 200, "{body}");
    let update = (body["reply"] == "update").then(|| Update {
        service: text(&body["service"]),
        version: text(&body["version"]),
        order_id: text(&body["order_id"]),
        order_try: body["order_try"].as_u64().expect("a try"),
        package_digest: text(&body["package_digest"]),
    });
    if update.is_none() {
        assert_eq!(body["reply"], "nothing");
    }
    Heard::Told {
        update,
        features: body["features"]
            .as_array()
            .map_or_else(Vec::new, |list| list.iter().map(text).collect()),
        halts: body["halts"].as_object().map_or_else(BTreeMap::new, |map| {
            map.iter()
                .map(|(name, generation)| (name.clone(), generation.as_u64().expect("a number")))
                .collect()
        }),
    }
}

/// The decision a gRPC reply says.
pub(super) fn heard_proto(reply: &Reply) -> Heard {
    if reply.status.code() == Code::InvalidArgument {
        assert_eq!(reply.status.message(), "invalid_heartbeat");
        return Heard::Refused;
    }
    let response: proto::HeartbeatResponse = reply.decode();
    let update = response.order.map(|order| match order.verb {
        Some(proto::order::Verb::Update(update)) => Update {
            service: update.service,
            version: update.version,
            order_id: order.order_id,
            order_try: order.r#try,
            package_digest: update.package_digest,
        },
        other => panic!("the order is not an update: {other:?}"),
    });
    Heard::Told {
        update,
        features: response.features,
        halts: response.halt_generations.into_iter().collect(),
    }
}

/// What a step of a case expects of the decision.
#[derive(Debug)]
enum Expect {
    Refused,
    Told {
        update: Option<(&'static str, &'static str)>,
        halts: Vec<(&'static str, u64)>,
    },
}

impl Heard {
    /// The same decision with no update: what the JSON route, which carries no order, says of it.
    pub(super) fn without_update(&self) -> Heard {
        match self {
            Heard::Told {
                features, halts, ..
            } => Heard::Told {
                update: None,
                features: features.clone(),
                halts: halts.clone(),
            },
            Heard::Refused => Heard::Refused,
        }
    }
}

fn nothing() -> Expect {
    Expect::Told {
        update: None,
        halts: Vec::new(),
    }
}

fn update(service: &'static str, version: &'static str) -> Expect {
    Expect::Told {
        update: Some((service, version)),
        halts: Vec::new(),
    }
}

impl Expect {
    fn with_halts(self, halts: Vec<(&'static str, u64)>) -> Expect {
        match self {
            Expect::Told { update, .. } => Expect::Told { update, halts },
            Expect::Refused => Expect::Refused,
        }
    }

    fn holds(&self, heard: &Heard, label: &str) {
        match (self, heard) {
            (Expect::Refused, Heard::Refused) => {}
            (
                Expect::Told { update, halts },
                Heard::Told {
                    update: got,
                    features,
                    halts: got_halts,
                },
            ) => {
                let got_update = got
                    .as_ref()
                    .map(|one| (one.service.as_str(), one.version.as_str()));
                assert_eq!(got_update, *update, "{label}: the update");
                if let Some(one) = got {
                    assert_eq!(one.package_digest, DIGEST, "{label}: the digest");
                    assert_eq!(one.order_try, 1, "{label}: the try");
                    assert!(one.order_id.starts_with("update-"), "{label}");
                }
                let want: BTreeMap<String, u64> = halts
                    .iter()
                    .map(|(name, generation)| ((*name).to_owned(), *generation))
                    .collect();
                assert_eq!(*got_halts, want, "{label}: the clear generations");
                assert_eq!(*features, AGREED, "{label}: the features");
            }
            _ => panic!("{label}: expected {self:?}, heard {heard:?}"),
        }
    }
}

/// One step of a case.
enum Step {
    /// The host `.0`, whose host role is `.1`, sends a heartbeat that lists `.2`; the decision
    /// is `.3`.
    Beat(&'static str, &'static str, Vec<Svc>, Expect),
    /// The operator clears the halt of `kbf`.
    Clear,
}

/// Runs the steps of a case on two farms in the same state, one told over `POST /v1/heartbeat`
/// and one over `Coordinator.Heartbeat`: the decisions are equal but for the update, which the
/// JSON route never carries, and the gRPC reply is what the case expects.
async fn run(name: &str, steps: Vec<Step>) {
    let over_json = farm(&format!("grpc-same-json-{name}")).await;
    let over_grpc = farm(&format!("grpc-same-grpc-{name}")).await;
    for (n, step) in steps.iter().enumerate() {
        let label = format!("{name}, step {n}");
        match step {
            Step::Clear => {
                over_json.clear_halt("kbf");
                over_grpc.clear_halt("kbf");
            }
            Step::Beat(host, role, services, expect) => {
                let who = updater(host, role);
                let (code, body) =
                    json_beat(&over_json.coordinator, &who, &json_text(services)).await;
                let told_json = heard_json(code, &body);
                let reply = beat(&over_grpc.coordinator, &who, &proto_message(services)).await;
                let told_grpc = heard_proto(&reply);
                assert_eq!(
                    told_json,
                    told_grpc.without_update(),
                    "{label}: JSON and gRPC disagree beyond the update"
                );
                expect.holds(&told_grpc, &label);
            }
        }
    }
}

/// Proves that the facts of a heartbeat decide the same whichever way they are sent: for each
/// state (up to date, behind, deferral, halted, halt cleared, a role the service does not run
/// on, a service list over the limit) the JSON answer and the gRPC reply, mapped to one shape,
/// carry the same features and the same clear generations, and the gRPC reply the update the
/// state calls for (service, version, order id, try, package digest) where the JSON answer, which
/// carries no order, says `nothing`.
#[tokio::test]
async fn grpc_heartbeat_and_json_heartbeat_give_the_same_decision() {
    let too_many: Vec<Svc> = std::iter::once(svc("kbf", "1.0"))
        .chain((1..=16).map(|n| svc(&format!("s{n:02}"), "1.0")))
        .collect();
    assert_eq!(too_many.len(), 17);
    let cases: Vec<(&str, Vec<Step>)> = vec![
        (
            "up-to-date",
            vec![Step::Beat(
                "d1",
                "daemon",
                vec![svc("kbf", "2.0")],
                nothing(),
            )],
        ),
        (
            "behind",
            vec![Step::Beat(
                "d1",
                "daemon",
                vec![svc("kbf", "1.0")],
                update("kbf", "2.0"),
            )],
        ),
        (
            "deferral",
            vec![Step::Beat(
                "d1",
                "daemon",
                vec![svc("kbf", "1.0").reporting(Report::Deferred)],
                nothing(),
            )],
        ),
        (
            "halted",
            vec![
                Step::Beat(
                    "d1",
                    "daemon",
                    vec![svc("kbf", "1.0").reporting(Report::Halt)],
                    nothing(),
                ),
                Step::Beat("d2", "daemon", vec![svc("kbf", "1.0")], nothing()),
            ],
        ),
        (
            "halt-cleared",
            vec![
                Step::Beat(
                    "d1",
                    "daemon",
                    vec![svc("kbf", "1.0").reporting(Report::Halt)],
                    nothing(),
                ),
                Step::Clear,
                Step::Beat(
                    "d2",
                    "daemon",
                    vec![svc("kbf", "1.0")],
                    update("kbf", "2.0").with_halts(vec![("kbf", 1)]),
                ),
            ],
        ),
        (
            "role-matches",
            vec![Step::Beat(
                "d1",
                "daemon",
                vec![svc("other", "1.0")],
                update("other", "3.0"),
            )],
        ),
        (
            "role-does-not-match",
            vec![Step::Beat(
                "s1",
                "storage",
                vec![svc("other", "1.0")],
                nothing(),
            )],
        ),
        (
            "service-list-over-the-limit",
            vec![Step::Beat("d1", "daemon", too_many, Expect::Refused)],
        ),
    ];
    for (name, steps) in cases {
        run(name, steps).await;
    }
}

/// Proves that who a heartbeat comes from is the certificate's: the message has no host field,
/// and a message that names other hosts (an agent, a service and unknown fields numbered where
/// a host id could be) is recorded under the host and the role the certificate names, once per
/// certificate.
#[tokio::test]
async fn heartbeat_identity_comes_from_the_certificate_not_the_message() {
    let farm = farm("grpc-identity").await;
    let mut message = legacy_beat("1.0", None);
    message.agent = Some(proto::AgentInfo {
        name: "frontend-1".to_owned(),
        version: "1.0".to_owned(),
    });
    message.services.push(proto::ServiceBeat {
        name: "frontend-1".to_owned(),
        version: "1.0".to_owned(),
        health: proto::Health::Healthy as i32,
        ..proto::ServiceBeat::default()
    });
    let mut bytes = message.encode_to_vec();
    // Strings in fields 20 and 21, which the message does not define.
    bytes.extend_from_slice(&[0xA2, 0x01, 10]);
    bytes.extend_from_slice(b"frontend-1");
    bytes.extend_from_slice(&[0xAA, 0x01, 8]);
    bytes.extend_from_slice(b"frontend");
    for (name, role) in [("real-host", "storage"), ("other-host", "daemon")] {
        let reply = grpc_call(
            &farm.coordinator,
            Surface::Heartbeat,
            &updater(name, role),
            HEARTBEAT,
            None,
            &bytes,
        )
        .await;
        assert_eq!(reply.token(), (Code::Ok, ""));
    }
    let (mut hosts, dropped) = farm.heard_from();
    hosts.sort();
    assert_eq!(
        hosts,
        [
            ("other-host".to_owned(), "daemon".to_owned()),
            ("real-host".to_owned(), "storage".to_owned()),
        ]
    );
    assert_eq!(dropped, 0);
}

/// Proves that a certificate that is no updater's is refused `PERMISSION_DENIED` `forbidden`
/// before the request is read, and the host is not noted: no certificate, an updater of the
/// wrong kind (a storage node's), bytes that are no certificate.
#[tokio::test]
async fn a_heartbeat_without_an_updater_certificate_is_refused_unread() {
    let farm = farm("grpc-forbidden").await;
    let strangers: [Peer; 3] = [
        None,
        peer(&[
            "urn:kbf:role:storage",
            "urn:kbf:host-role:storage",
            "dns:h1",
        ]),
        Some(Arc::from(&b"not DER"[..])),
    ];
    for who in &strangers {
        let (body, read) = watched();
        let reply = send(
            &farm.coordinator,
            Surface::Heartbeat,
            who,
            HEARTBEAT,
            None,
            body,
        )
        .await;
        assert_eq!(reply.token(), (Code::PermissionDenied, "forbidden"));
        assert!(
            !read.load(Ordering::SeqCst),
            "the request was read before the caller was known"
        );
    }
    let (hosts, dropped) = farm.heard_from();
    assert!(hosts.is_empty(), "{hosts:?}");
    assert_eq!(dropped, 0);
}

/// Proves the refusals of a host an operator denied: a host that retires (denied while it
/// updates) is heard for its final report and refused `PERMISSION_DENIED` `denied`, and stops
/// retiring when the report is final; a denied host's later heartbeat is refused unread.
#[tokio::test]
async fn a_denied_host_is_refused_denied_and_a_retiring_one_is_heard_for_its_final_report() {
    let enrollment = EnrollmentRig::new("grpc-hb-enrollment");
    let key = HostKey::new();
    let certificate = enrolled(&enrollment.enrollment, &key, "machine-1", "s1");
    let farm = farm_enrolled("grpc-hb-denied", &enrollment.enrollment).await;
    let c = &farm.coordinator;
    let who = peer_of(&certificate);
    let store = enrollment.enrollment.store();
    let s1 = host_id("s1");

    let ordered = beat(c, &who, &legacy_beat("1.0", None)).await;
    assert_eq!(ordered.token(), (Code::Ok, ""));
    let updating = legacy_beat("1.0", Some("2.0"));
    assert_eq!(beat(c, &who, &updating).await.token(), (Code::Ok, ""));

    store.deny(&s1).expect("denied");
    let retiring = beat(c, &who, &updating).await;
    assert_eq!(retiring.token(), (Code::PermissionDenied, "denied"));
    assert!(
        store.is_retiring(&s1).expect("read"),
        "its update is not final"
    );

    let done = beat(c, &who, &legacy_beat("2.0", None)).await;
    assert_eq!(done.token(), (Code::PermissionDenied, "denied"));
    assert!(
        !store.is_retiring(&s1).expect("read"),
        "the final report ends the retirement"
    );
    assert!(store.is_denied(&s1).expect("read"));

    let (body, read) = watched();
    let later = send(c, Surface::Heartbeat, &who, HEARTBEAT, None, body).await;
    assert_eq!(later.token(), (Code::PermissionDenied, "denied"));
    assert!(
        !read.load(Ordering::SeqCst),
        "a denied host's request was read"
    );
}

/// Proves that a bad optional fact is dropped and counted by name whichever way it is sent:
/// a step with no name, over JSON and over gRPC, is answered and counts one `step` on each
/// farm; and a service entry with no health drops the `services` list, not the heartbeat, on
/// each.
#[tokio::test]
async fn a_bad_optional_fact_is_dropped_alike_on_grpc_and_json() {
    let over_json = farm("grpc-extras-json").await;
    let over_grpc = farm("grpc-extras-grpc").await;
    let who = updater("d1", "daemon");

    let json_body = json!({
        "platform": "linux-x86_64", "version": "1.0", "health": "healthy",
        "step": {"name": "", "attempt": 1},
        "services": [{"name": "other", "version": "1.0"}],
    })
    .to_string();
    let (code, _) = json_beat(&over_json.coordinator, &who, &json_body).await;
    assert_eq!(code, 200);

    let mut message = legacy_beat("1.0", None);
    message.services[0].step = Some(proto::StepRun {
        name: String::new(),
        attempt: 1,
    });
    message.services.push(proto::ServiceBeat {
        name: "other".to_owned(),
        version: "1.0".to_owned(),
        health: proto::Health::Unspecified as i32,
        ..proto::ServiceBeat::default()
    });
    assert_eq!(
        beat(&over_grpc.coordinator, &who, &message).await.token(),
        (Code::Ok, "")
    );

    let counted = |farm: &Farm| {
        let status = farm.coordinator.status(Some(OPERATOR)).expect("status");
        (
            status.services_dropped,
            serde_json::to_value(&status.extras_dropped).expect("JSON"),
        )
    };
    assert_eq!(counted(&over_json), (1, json!({"step": 1})));
    assert_eq!(counted(&over_grpc), (1, json!({"step": 1})));
}
