//! A host's daemon as the tests of the pick drive it (docs/design/packages.md, section 3.3): one
//! description of what it says, sent as a heartbeat over gRPC through the listeners' router, or
//! as the same heartbeat over the JSON route; and the operator calls that show what the
//! coordinator made of it.

use bytes::Bytes;
use hostkeeper_coordinator::grpc::route;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Version};
use prost::Message;
use serde_json::{Value, json};

use super::OPERATOR;
use super::rig::{PATIENCE, Rig};
use super::targets::{Answered, framed, grpc};

/// The path of `Coordinator.Heartbeat`.
pub const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";
/// The path of `Hosts.Get`.
pub const HOSTS_GET: &str = "/hostkeeper.operator.v1.Hosts/Get";
/// The path of `Releases.Converge`.
pub const CONVERGE: &str = "/hostkeeper.operator.v1.Releases/Converge";
/// The path of `Releases.Status`.
pub const STATUS: &str = "/hostkeeper.operator.v1.Releases/Status";
/// The path of `Releases.Force`.
pub const FORCE: &str = "/hostkeeper.operator.v1.Releases/Force";

/// The features of a daemon before `target-v1`: it lists every service it runs and takes the
/// order terms, installs and forces.
pub const OLD: [&str; 4] = ["heartbeat-v2", "install-v1", "order-v1", "forced-v1"];

/// The feature that has a daemon's installers read and the picked entry sent in its orders.
pub const TARGET_V1: &str = "target-v1";

/// What a daemon says of one service: its name, the version it runs, its health, and the
/// installer it installed it with (`None` for `hostkeeper-archive`).
#[derive(Clone, Debug)]
pub struct Runs {
    pub name: String,
    pub version: String,
    pub health: agent::Health,
    pub installer: Option<String>,
    pub deferred: Option<agent::DeferredReport>,
    /// The drift its installer found at this heartbeat, over gRPC only.
    pub drift: Option<agent::DriftReport>,
}

impl Runs {
    /// The same service with drift: `found` is what the package manager shows, `recorded` what
    /// the record of what was placed names.
    pub fn drifted(mut self, found: Option<&str>, recorded: Option<&str>) -> Self {
        self.drift = Some(agent::DriftReport {
            found: found.map(str::to_owned),
            recorded: recorded.map(str::to_owned),
        });
        self
    }
}

/// `name` at `version`, healthy, installed with `installer`.
pub fn runs(name: &str, version: &str, installer: Option<&str>) -> Runs {
    Runs {
        name: name.to_owned(),
        version: version.to_owned(),
        health: agent::Health::Healthy,
        installer: installer.map(str::to_owned),
        deferred: None,
        drift: None,
    }
}

/// What a host's daemon says in a heartbeat.
#[derive(Clone, Debug)]
pub struct Daemon {
    /// The host's node id.
    pub host: String,
    /// The role its certificate gives it: `daemon` unless a test says otherwise.
    pub role: String,
    pub platform: agent::Platform,
    pub features: Vec<String>,
    pub installers: Vec<String>,
    pub services: Vec<Runs>,
}

impl Daemon {
    /// A daemon of `target-v1` on `host`, of `platform`, listing `installers` and running nothing.
    pub fn new(host: &str, platform: agent::Platform, installers: &[&str]) -> Self {
        let mut features: Vec<String> = OLD.iter().map(|f| (*f).to_owned()).collect();
        features.push(TARGET_V1.to_owned());
        Self {
            host: host.to_owned(),
            role: "daemon".to_owned(),
            platform,
            features,
            installers: installers.iter().map(|i| (*i).to_owned()).collect(),
            services: Vec::new(),
        }
    }

    /// A daemon before `target-v1` on `host`, of `platform`: it lists no installers.
    pub fn old(host: &str, platform: agent::Platform) -> Self {
        Self {
            features: OLD.iter().map(|f| (*f).to_owned()).collect(),
            ..Self::new(host, platform, &[])
        }
    }

    /// The same daemon running `one` too.
    pub fn running(mut self, one: Runs) -> Self {
        self.services.push(one);
        self
    }

    /// The heartbeat as `Coordinator.Heartbeat` carries it.
    pub fn request(&self) -> agent::HeartbeatRequest {
        agent::HeartbeatRequest {
            platform: self.platform as i32,
            agent: Some(agent::AgentInfo {
                name: "hostkeeper".to_owned(),
                version: "0.5.0".to_owned(),
            }),
            features: self.features.clone(),
            installers: self.installers.clone(),
            services: self
                .services
                .iter()
                .map(|one| agent::ServiceBeat {
                    name: one.name.clone(),
                    version: one.version.clone(),
                    health: one.health as i32,
                    installer: one.installer.clone(),
                    deferred: one.deferred.clone(),
                    drift: one.drift.clone(),
                    ..agent::ServiceBeat::default()
                })
                .collect(),
            ..agent::HeartbeatRequest::default()
        }
    }

    /// The same heartbeat as the JSON route carries it: no installers, which it has no key for.
    pub fn json(&self) -> String {
        let platform = match self.platform {
            agent::Platform::LinuxArm64 => "linux-arm64",
            agent::Platform::DarwinArm64 => "darwin-arm64",
            _ => "linux-x86_64",
        };
        let services: Vec<Value> = self
            .services
            .iter()
            .map(|one| {
                let health = match one.health {
                    agent::Health::Absent => "absent",
                    agent::Health::Unhealthy => "unhealthy",
                    _ => "healthy",
                };
                json!({"name": one.name, "version": one.version, "health": health})
            })
            .collect();
        json!({
            "platform": platform,
            "version": "0.5.0",
            "health": "healthy",
            "agent": {"name": "hostkeeper", "version": "0.5.0"},
            "agent_features": self.features,
            "services": services,
        })
        .to_string()
    }
}

/// The daemon's heartbeat over gRPC, answered.
pub async fn grpc_beat(rig: &Rig, daemon: &Daemon) -> agent::HeartbeatResponse {
    let request = Request::builder()
        .method("POST")
        .uri(HEARTBEAT)
        .version(Version::HTTP_2)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(Full::new(framed(&daemon.request().encode_to_vec())))
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
    let headers = response.headers().clone();
    let collected = response.into_body().collect().await.expect("a body");
    let trailers = collected.trailers().cloned().unwrap_or(headers);
    let status = trailers.get("grpc-status").and_then(|v| v.to_str().ok());
    assert_eq!(status, Some("0"), "{trailers:?}");
    let data: Bytes = collected.to_bytes();
    agent::HeartbeatResponse::decode(&data[5..]).expect("a heartbeat response")
}

/// The daemon's heartbeat over the JSON route, answered.
pub async fn json_beat(rig: &Rig, daemon: &Daemon) -> Value {
    let (code, body) = rig
        .heartbeat(&daemon.host, &daemon.role, &daemon.json())
        .await;
    assert_eq!(code, 200, "{body}");
    serde_json::from_str(&body).expect("a JSON answer")
}

/// The order of an answer as `(verb, service, version, package digest, index digest, target)`.
pub type Said = (
    &'static str,
    String,
    String,
    String,
    String,
    Option<hostkeeper_proto::hostkeeper::release::v1::Target>,
);

/// What the order of `response` says, if it carries one.
pub fn said(response: &agent::HeartbeatResponse) -> Option<Said> {
    use agent::order::Verb;
    match response.order.as_ref()?.verb.clone()? {
        Verb::Update(u) => Some((
            "update",
            u.service,
            u.version,
            u.package_digest,
            u.index_digest,
            u.target,
        )),
        Verb::Install(i) => Some((
            "install",
            i.service,
            i.version,
            i.package_digest,
            i.index_digest,
            i.target,
        )),
        Verb::Force(f) => Some((
            "force",
            f.service,
            f.version,
            f.package_digest,
            f.index_digest,
            f.target,
        )),
        other => panic!("an order of another verb: {other:?}"),
    }
}

/// `Hosts.Get` of `host`.
pub async fn host(rig: &Rig, host: &str) -> proto::Host {
    let answered: Answered = grpc(
        rig,
        HOSTS_GET,
        Some(OPERATOR),
        &proto::GetHostRequest {
            node_id: host.to_owned(),
        },
    )
    .await;
    answered
        .decoded::<proto::GetHostResponse>()
        .host
        .expect("a host")
}

/// `Releases.Converge` of `service`.
pub async fn converge(rig: &Rig, service: &str) -> proto::Convergence {
    let answered = grpc(
        rig,
        CONVERGE,
        Some(OPERATOR),
        &proto::ConvergeRequest {
            service: service.to_owned(),
        },
    )
    .await;
    answered
        .decoded::<proto::ConvergeResponse>()
        .convergence
        .expect("a convergence")
}

/// The gate reason of `host` for `service`: the one `Releases.Status` gives a host that reports
/// the service, else the one `Hosts.Get` shows.
pub async fn reason(rig: &Rig, service: &str, node_id: &str) -> proto::GateReason {
    let answered = grpc(
        rig,
        STATUS,
        Some(OPERATOR),
        &proto::StatusRequest {
            service: service.to_owned(),
        },
    )
    .await;
    let status: proto::StatusResponse = answered.decoded();
    let listed = status
        .services
        .iter()
        .filter(|one| one.service == service)
        .flat_map(|one| &one.hosts)
        .find(|h| h.node_id == node_id)
        .and_then(|found| found.view.as_ref())
        .map(proto::HostService::gate_reason);
    match listed {
        Some(reason) => reason,
        None => host(rig, node_id)
            .await
            .services
            .iter()
            .find(|one| one.service == service)
            .map_or(
                proto::GateReason::Unspecified,
                proto::HostService::gate_reason,
            ),
    }
}
