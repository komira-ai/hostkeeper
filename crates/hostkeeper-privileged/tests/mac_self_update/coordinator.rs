//! The coordinator of a Mac of `device_mac_selfupdate.rs` that is driven by the real one: the
//! production coordinator (`hostkeeper_coordinator::service::Coordinator`, its heartbeat with
//! typed orders, its verdicts file, its bake and its convergence) in the test's process, on state
//! files of the test's tree, answering each heartbeat the daemon wrote to `link/beats.jsonl` as
//! its routes answer the host `host-1` of the role `daemon`: the order it gives is written to
//! `link/order.json` (removed when it gives none), and the features and acknowledged verdicts of
//! its answer to `link/answer.json`, which the fixture's link reads at the daemon's next
//! heartbeat (`tests/bin/selfupdate_daemon.rs`).
//!
//! Each line of `beats.jsonl` is the loop's own JSON of the heartbeat (`Heartbeat::to_json`):
//! [`wire_beat`] reads it into the coordinator's heartbeat, field by field as the link's mapping
//! to the wire does (the agent's version and health at the top level, each service's facts in
//! its entry, a `deferred` verdict sent as `failed`). What this leaves out is the transport
//! (TLS, the certificate of an enrolled host) and the moment of the answer: the daemon reads
//! each answer one heartbeat later than a real link would. The
//! coordinator's clock stays at one second, so no host goes quiet between two heartbeats.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::operator::Caller;
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::service::{Clock, Coordinator};
use hostkeeper_coordinator::services::Services;
use hostkeeper_coordinator::verdict_file::{VerdictFile, verdicts_path};
use hostkeeper_hook::{Class, Phase};
use hostkeeper_oidc::testkit::{
    STATIC_ISSUER, StaticKeys, clock, config_text, jwks_of, keys, spent,
};
use hostkeeper_oidc::{OidcConfig, Verifier};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1::{GetHostRequest, HostService};
use hostkeeper_wire::identity::Identity;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{self, ServiceBeat, UpdateResult};
use serde_json::{Value, json};

/// The operator token of the coordinator.
const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";
/// The host the daemon is, to the coordinator.
pub const HOST: &str = "host-1";

/// `bytes` written at `path` whole: a temporary file beside it renamed over it, so the fixture's
/// link reads it whole or not at all.
fn place(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).unwrap_or_else(|e| panic!("write {}: {e}", tmp.display()));
    fs::rename(&tmp, path).unwrap_or_else(|e| panic!("place {}: {e}", path.display()));
}

/// The coordinator of one test's Mac, and how far it has read the daemon's heartbeats.
pub struct Relay {
    coordinator: Arc<Coordinator>,
    link: PathBuf,
    read: usize,
    ordered: Vec<pb::Order>,
}

impl Relay {
    /// The coordinator on the state files of `dir` (made here), wanting `service` at `version`
    /// whose package digest CI registered as `digest`, its rollout of `service` (on the role
    /// `daemon`) given the keys `rollout`; it answers the heartbeats of the fixture's link
    /// directory `link`, and orders from its start.
    pub fn new(
        dir: &Path,
        link: &Path,
        service: &str,
        version: &str,
        digest: &str,
        rollout: &str,
    ) -> Relay {
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("make {}: {e}", dir.display()));
        let state = dir.join("desired.json");
        let desired = json!({"services": {service: {
            "version": version,
            "min_up": {},
            "release_digest": digest,
            "digests": {version: digest},
        }}});
        fs::write(&state, desired.to_string()).expect("the desired state");
        let verdicts = VerdictFile::open(&verdicts_path(&state)).expect("the verdicts file");
        let desired = DesiredStore::open(&state, None).expect("the desired state");
        let ops = OpsStore::open(&dir.join("desired.json.ops")).expect("the ops store");
        let allow = format!("{}services = [\"{service}\"]\n", config_text(STATIC_ISSUER));
        let (oidc_now, _) = clock();
        let verifier = Verifier::new(
            OidcConfig::parse(&allow).expect("the allow-list"),
            Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
            Duration::from_secs(30),
            oidc_now,
            spent(),
        );
        let at: Clock = Arc::new(|| Duration::from_secs(1));
        let rollout =
            format!("[[service]]\nname = \"{service}\"\nroles = [\"daemon\"]\n{rollout}\n");
        let coordinator = Coordinator::new(desired, Arc::new(verifier), OPERATOR.to_owned(), at)
            .with_services(Services::parse(&rollout).expect("the rollout file"))
            .with_quiet(Duration::ZERO)
            .with_ops(ops)
            .with_verdicts(verdicts);
        Relay {
            coordinator: Arc::new(coordinator),
            link: link.to_path_buf(),
            read: 0,
            ordered: Vec::new(),
        }
    }

    /// Answers every heartbeat written since the last call, oldest first; a last line still being
    /// written is left for the next call. The order and the answer of the newest one are placed
    /// for the daemon's next heartbeat.
    pub fn relay(&mut self) {
        let text = fs::read_to_string(self.link.join("beats.jsonl")).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let whole = match text.ends_with('\n') {
            true => lines.len(),
            false => lines.len().saturating_sub(1),
        };
        for line in &lines[self.read.min(whole)..whole] {
            let json: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("a heartbeat line that is not JSON: {e}: {line}"));
            let beat = wire_beat(&json)
                .unwrap_or_else(|| panic!("a heartbeat the coordinator does not read: {line}"));
            let who = Identity {
                host_id: HOST.parse().expect("a host id"),
                role: "daemon".parse().expect("a role"),
            };
            let (answer, order) = self
                .coordinator
                .heartbeat_with_orders(who, beat)
                .unwrap_or_else(|e| panic!("the coordinator refused the heartbeat: {e:?}"));
            let acks: Vec<Value> = answer
                .acks
                .iter()
                .map(|ack| {
                    json!({
                        "service": ack.service,
                        "order_id": ack.order_id.as_str(),
                        "try": ack.order_try,
                    })
                })
                .collect();
            let features = answer.features.clone().unwrap_or_default();
            let answered = json!({"features": features, "acks": acks});
            place(
                &self.link.join("answer.json"),
                answered.to_string().as_bytes(),
            );
            match order.map(|order| order.to_proto()) {
                Some(order) => {
                    place(&self.link.join("order.json"), order_json(&order).as_bytes());
                    if self.ordered.last() != Some(&order) {
                        self.ordered.push(order);
                    }
                }
                None => match fs::remove_file(self.link.join("order.json")) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        panic!("remove the order: {e}")
                    }
                    _ => {}
                },
            }
        }
        self.read = whole.max(self.read);
    }

    /// Every order the coordinator gave, each once, in order.
    pub fn ordered(&self) -> &[pb::Order] {
        &self.ordered
    }

    /// The convergence the coordinator's status answers for `service`, as JSON.
    pub fn convergence(&self, service: &str) -> Value {
        let status = self.coordinator.status(Some(OPERATOR)).expect("the status");
        serde_json::to_value(&status.per_service[service].convergence).expect("JSON")
    }

    /// What `Hosts.Get` says of `service` on the host.
    pub fn on_host(&self, service: &str) -> Option<HostService> {
        let operator = Caller::Operator("operator".parse().expect("a token name"));
        let request = GetHostRequest {
            node_id: HOST.to_owned(),
        };
        let host = self.coordinator.get_host(&operator, &request).ok()?.host?;
        host.services.into_iter().find(|one| one.service == service)
    }
}

/// `order` as the fixture's `order.json` spells it: an `update` or an `install`.
fn order_json(order: &pb::Order) -> String {
    let (verb, target) = match order.verb.as_ref() {
        Some(pb::order::Verb::Update(a)) => ("update", (&a.service, &a.version, &a.package_digest)),
        Some(pb::order::Verb::Install(a)) => {
            ("install", (&a.service, &a.version, &a.package_digest))
        }
        other => panic!("an order the fixture's link does not carry: {other:?}"),
    };
    json!({
        "id": order.order_id,
        "try": order.r#try,
        "verb": verb,
        "service": target.0,
        "version": target.1,
        "digest": target.2,
    })
    .to_string()
}

/// A version of the heartbeat's JSON.
fn version(value: &Value) -> Option<Version> {
    Version::parse(value.as_str()?).ok()
}

/// A health of the heartbeat's JSON (`healthy`, `unhealthy`, `absent`).
fn health(value: &Value) -> Option<wire::Health> {
    match value.as_str()? {
        "healthy" => Some(wire::Health::Healthy),
        "unhealthy" => Some(wire::Health::Unhealthy),
        "absent" => Some(wire::Health::Absent),
        _ => None,
    }
}

/// A verdict of the heartbeat's JSON as the wire carries it: `succeeded`, or `failed` for a
/// failure and a deferral, with its phase, class, reason, order id and try.
fn verdict(value: &Value) -> Option<UpdateResult> {
    let outcome = match value["outcome"].as_str()? {
        "succeeded" => wire::Outcome::Succeeded,
        _ => wire::Outcome::Failed,
    };
    let phase = serde_json::from_value::<Phase>(value["phase"].clone())
        .ok()
        .map(|phase| match phase {
            Phase::BeforeCommit => wire::Phase::BeforeCommit,
            Phase::GateRelease => wire::Phase::GateRelease,
            Phase::GateEnvironment => wire::Phase::GateEnvironment,
            Phase::AfterCommit => wire::Phase::AfterCommit,
            Phase::Halt => wire::Phase::Halt,
        });
    let class = serde_json::from_value::<Class>(value["class"].clone())
        .ok()
        .map(|class| match class {
            Class::Release => wire::Class::Release,
            Class::Environment => wire::Class::Environment,
        });
    Some(UpdateResult {
        phase,
        class,
        reason: value["reason"].as_str().and_then(wire::Token::parse),
        order_id: value["order_id"].as_str().and_then(wire::OrderId::parse),
        order_try: value["order_try"].as_u64(),
        ..UpdateResult::new(version(&value["target"])?, outcome)
    })
}

/// One service entry of the heartbeat's JSON.
fn service_beat(value: &Value) -> Option<ServiceBeat> {
    Some(ServiceBeat {
        name: value["name"].as_str()?.to_owned(),
        version: version(&value["version"])?,
        health: health(&value["health"])?,
        last_update: value
            .get("last_update")
            .filter(|v| !v.is_null())
            .map(verdict)
            .flatten(),
        updating: version(&value["updating"]),
        step: None,
        commit: None,
        step_secs: std::collections::BTreeMap::new(),
        promotion_pending: false,
        deferred: None,
        record_try: value["record_try"].as_u64(),
    })
}

/// The loop's JSON of a heartbeat (`Heartbeat::to_json`) as the coordinator reads it: the
/// agent's version and a healthy top level, each service entry, the features announced and the
/// agent; `None` when a field the coordinator needs is missing or malformed.
pub fn wire_beat(value: &Value) -> Option<wire::Heartbeat> {
    let agent_version = version(&value["agent"]["version"])?;
    let services = value["services"]
        .as_array()?
        .iter()
        .map(service_beat)
        .collect::<Option<Vec<_>>>()?;
    Some(wire::Heartbeat {
        platform: hostkeeper_wire::Platform::parse(value["platform"].as_str()?)?,
        version: agent_version.clone(),
        health: wire::Health::Healthy,
        last_update: None,
        updating: None,
        services: Some(services),
        agent_features: value["agent_features"]
            .as_array()?
            .iter()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect(),
        step: None,
        commit: None,
        step_secs: std::collections::BTreeMap::new(),
        promotion_pending: false,
        deferred: None,
        agent: Some(wire::Agent {
            name: wire::AgentName::parse(value["agent"]["name"].as_str()?)?,
            version: agent_version,
        }),
        sent_unix: value["sent_unix"].as_u64(),
        cert_not_after_unix: None,
        last_renew_error: None,
        record_try: None,
    })
}
