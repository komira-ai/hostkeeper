//! The real coordinator in process, reached the way a daemon reaches it: its heartbeat over
//! gRPC (`Coordinator.Heartbeat` through the listeners' router, as the host `host-1` of the role
//! `daemon`), whose request carries the daemon's installers and each service's installer and
//! whose answer carries the typed order with the entry the coordinator picked from the release
//! CI kept ([`set_release`], over `Releases.SetDesired`). A version set with no release is
//! ordered to no host, so a scenario that has the coordinator order a version sets it with the
//! entry of a package its host's repository holds.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_coordinator::grpc::route;
use hostkeeper_coordinator::http::{Peer, Surface};
use hostkeeper_coordinator::service::Coordinator;
use hostkeeper_hook::{Class, Phase};
use hostkeeper_link::{
    Acknowledged, ClientIdentity, CoordinatorLink, Entry, Feature, Health, Heartbeat, LinkError,
    Order, OrderId, OrderRef, OrderResult, Outcome, PackageDigest, Reason, Reply, Target, Verb,
};
use hostkeeper_oidc::testkit::{STATIC_ISSUER, good_claims, keys, mint};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::hostkeeper::release::v1::{Target as Release, TargetList};
use hostkeeper_wire::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, UPDATER_ROLE,
};
use http_body_util::{BodyExt, Full};
use hyper::{Request, Version as Http};
use prost::Message;
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, SanType};

/// The path of `Coordinator.Heartbeat`.
const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";
/// The path of `Releases.SetDesired`.
const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";
const PUT_REPOSITORY: &str = "/hostkeeper.operator.v1.Repositories/Put";

/// How long the coordinator's router waits for a body: the whole body is there.
const PATIENCE: Duration = Duration::from_secs(30);

/// The platforms a release may list.
const PLATFORMS: [&str; 3] = ["linux-x86_64", "linux-arm64", "macos-arm64"];

/// The certificate of the updater `host` of the role `daemon`, as the coordinator reads the
/// peer of a heartbeat.
pub fn peer(host: &str) -> Peer {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    let uri = |text: String| SanType::URI(text.try_into().expect("a URI"));
    params.subject_alt_names = vec![
        uri(format!("{DEFAULT_ROLE_URI_PREFIX}{UPDATER_ROLE}")),
        uri(format!("{DEFAULT_HOST_ROLE_URI_PREFIX}daemon")),
        SanType::DnsName(host.to_owned().try_into().expect("a DNS name")),
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("a key");
    let der = params.self_signed(&key).expect("signed").der().to_vec();
    Some(Arc::from(der.as_slice()))
}

/// One gRPC call through the router: the status code, its message and the response message.
fn call(
    coordinator: &Arc<Coordinator>,
    surface: Surface,
    peer: &Peer,
    path: &str,
    bearer: Option<&str>,
    message: &[u8],
) -> (i32, String, Vec<u8>) {
    let mut framed = vec![0u8];
    framed.extend(u32::try_from(message.len()).expect("fits").to_be_bytes());
    framed.extend(message);
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .version(Http::HTTP_2)
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder
        .body(Full::new(Bytes::from(framed)))
        .expect("a request");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let response = route(coordinator, surface, peer, request, PATIENCE).await;
        // A refusal made before the body is read has its status in the headers.
        let headers = response.headers().clone();
        let collected = response.into_body().collect().await.expect("a body");
        let trailers = collected.trailers().cloned().unwrap_or(headers);
        let data = collected.to_bytes();
        let code = trailers
            .get("grpc-status")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1);
        let text = trailers
            .get("grpc-message")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = if code == 0 && data.len() >= 5 {
            data[5..].to_vec()
        } else {
            Vec::new()
        };
        (code, text, body)
    })
}

/// The repository `komira` of the coordinator's registry, which every service names by default:
/// the host's entry of the tests is its channel `komira-stable`. Its keyring is one hand-built
/// public key packet.
fn komira() -> hostkeeper_proto::hostkeeper::release::v1::Repository {
    let mut keyring = vec![0xc0 | 6, 14, 4, 0, 0, 0, 1, 22];
    keyring.extend([1_u8; 8]);
    let fingerprint = hostkeeper_release::registry::keys(&keyring).expect("a keyring")[0]
        .fingerprint
        .clone();
    hostkeeper_proto::hostkeeper::release::v1::Repository {
        name: "komira".to_owned(),
        kind: "apt".to_owned(),
        uri: "https://packages.example.com/apt".to_owned(),
        suites: vec!["stable".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec![fingerprint],
        keyring,
    }
}

/// The operator puts [`komira`] in the registry: a set with a repository entry is refused until
/// the registry holds the repository its service names.
fn put_komira(coordinator: &Arc<Coordinator>) {
    let request = operator::PutRepositoryRequest {
        repository: Some(komira()),
        reason: "a test".to_owned(),
    };
    let (code, message, _) = call(
        coordinator,
        Surface::Control,
        &None,
        PUT_REPOSITORY,
        Some(crate::support::coordinated::OPERATOR),
        &request.encode_to_vec(),
    );
    assert_eq!(code, 0, "the put of komira: {message}");
}

/// CI sets `service` to `version` with the release that lists `entry` for every platform, after
/// the operator has put `komira` in the registry. The set is refused (the call panics) when the
/// coordinator refuses it.
pub fn set_release(coordinator: &Arc<Coordinator>, service: &str, version: &str, entry: &Release) {
    let list = TargetList {
        entries: vec![entry.clone()],
    };
    let request = operator::SetDesiredRequest {
        service: service.to_owned(),
        desired: Some(operator::DesiredState {
            version: version.to_owned(),
            ..operator::DesiredState::default()
        }),
        targets: PLATFORMS
            .iter()
            .map(|platform| ((*platform).to_owned(), list.clone()))
            .collect(),
        ..operator::SetDesiredRequest::default()
    };
    put_komira(coordinator);
    let token = mint(&keys()[0], &good_claims(STATIC_ISSUER));
    let (code, message, _) = call(
        coordinator,
        Surface::Control,
        &None,
        SET_DESIRED,
        Some(&token),
        &request.encode_to_vec(),
    );
    assert_eq!(code, 0, "the set of {service} {version}: {message}");
}

fn wire_version(version: &Version) -> String {
    version.as_str().to_owned()
}

fn phase(phase: Phase) -> pb::FailurePhase {
    match phase {
        Phase::BeforeCommit => pb::FailurePhase::BeforeCommit,
        Phase::GateRelease => pb::FailurePhase::GateRelease,
        Phase::GateEnvironment => pb::FailurePhase::GateEnvironment,
        Phase::AfterCommit => pb::FailurePhase::AfterCommit,
        Phase::Halt => pb::FailurePhase::Halt,
    }
}

fn class(class: Class) -> pb::FailureClass {
    match class {
        Class::Release => pb::FailureClass::Release,
        Class::Environment => pb::FailureClass::Environment,
    }
}

/// A verdict as `last_update`: none for a deferral, which `deferred` says.
fn verdict(result: &OrderResult) -> Option<pb::UpdateResult> {
    let outcome = match result.outcome {
        Outcome::Succeeded => pb::Outcome::Succeeded,
        Outcome::Failed => pb::Outcome::Failed,
        Outcome::Deferred { .. } => return None,
    };
    Some(pb::UpdateResult {
        target: wire_version(&result.target),
        outcome: outcome.into(),
        phase: result
            .phase
            .map_or(pb::FailurePhase::Unspecified, phase)
            .into(),
        class: result
            .class
            .map_or(pb::FailureClass::Unspecified, class)
            .into(),
        reason: result
            .reason
            .as_ref()
            .map(|reason| reason.as_str().to_owned())
            .unwrap_or_default(),
        order_try: Some(result.order.order_try),
        order_id: Some(result.order.id.as_str().to_owned()),
        forced: result.forced,
        detail: result.detail.clone(),
        ..pb::UpdateResult::default()
    })
}

/// The loop's heartbeat as the request `Coordinator.Heartbeat` carries: the features and the
/// installers announced, and every service with its installer.
pub fn request_of(beat: &Heartbeat) -> pb::HeartbeatRequest {
    pb::HeartbeatRequest {
        installers: beat.installers.clone(),
        platform: match beat.platform.as_str() {
            "linux-arm64" => pb::Platform::LinuxArm64,
            "darwin-arm64" | "macos-arm64" => pb::Platform::DarwinArm64,
            _ => pb::Platform::LinuxX8664,
        }
        .into(),
        agent: Some(pb::AgentInfo {
            name: hostkeeper_link::beat::AGENT_NAME.to_owned(),
            version: wire_version(&beat.agent_version),
        }),
        features: beat
            .features
            .iter()
            .map(|feature| feature.as_str().to_owned())
            .collect(),
        services: beat
            .services
            .iter()
            .map(|facts| pb::ServiceBeat {
                name: facts.name.as_str().to_owned(),
                version: wire_version(&facts.version),
                health: match facts.health {
                    Health::Healthy => pb::Health::Healthy,
                    Health::Unhealthy => pb::Health::Unhealthy,
                    Health::Absent => pb::Health::Absent,
                }
                .into(),
                last_update: facts.last_update.as_ref().and_then(verdict),
                updating: facts.updating.as_ref().map(wire_version),
                deferred: facts.deferred.as_ref().map(|report| pb::DeferredReport {
                    target: wire_version(&report.target),
                    reason: report.reason.as_str().to_owned(),
                    retry_after_secs: report.retry_after_secs,
                }),
                record_try: facts.record_try,
                installer: facts.installer.clone(),
                ..pb::ServiceBeat::default()
            })
            .collect(),
        sent_unix: Some(beat.sent_unix),
        trusted_root_reason: pb::TrustedRootReason::Unspecified.into(),
        ..pb::HeartbeatRequest::default()
    }
}

/// The coordinator's typed order as the loop reads it: the verb with its terms and the entry
/// the coordinator picked, word for word.
pub fn link_order(order: pb::Order) -> Order {
    let service = |name: &str| ServiceName::new(name).expect("a service");
    let target =
        |name: &str, version: &str, digest: &str, index: &str, entry: Option<Release>| Target {
            service: service(name),
            version: Version::new(version).expect("a version"),
            digest: PackageDigest::new(digest).expect("a digest"),
            index_digest: (!index.is_empty())
                .then(|| PackageDigest::new(index).expect("an index digest")),
            entry: entry.map(Entry::new),
        };
    let verb = match order.verb.expect("a verb") {
        pb::order::Verb::Update(a) => Verb::Update(target(
            &a.service,
            &a.version,
            &a.package_digest,
            &a.index_digest,
            a.target,
        )),
        pb::order::Verb::Install(a) => Verb::Install(target(
            &a.service,
            &a.version,
            &a.package_digest,
            &a.index_digest,
            a.target,
        )),
        pb::order::Verb::Force(a) => Verb::Force(target(
            &a.service,
            &a.version,
            &a.package_digest,
            &a.index_digest,
            a.target,
        )),
        pb::order::Verb::Uninstall(a) => Verb::Uninstall(service(&a.service)),
        pb::order::Verb::Abort(a) => Verb::Abort(service(&a.service)),
        pb::order::Verb::ClearRecord(a) => Verb::ClearRecord(service(&a.service)),
        other => panic!("a verb these scenarios do not order: {other:?}"),
    };
    Order {
        id: OrderId::new(&order.order_id).expect("an order id"),
        order_try: order.r#try,
        verb,
    }
}

/// The network transport, in process: each heartbeat goes through the coordinator's gRPC router
/// as the host `host-1`, and the order of each answer is kept, in order. With `unadvertise`, the
/// reply's features leave `forced-v1` out, as a coordinator that does not advertise it answers.
pub struct GrpcInProcess {
    pub coordinator: Arc<Coordinator>,
    pub ordered: Arc<Mutex<Vec<Option<Order>>>>,
    pub unadvertise: bool,
    /// The request of each heartbeat, in order.
    pub sent: Arc<Mutex<Vec<pb::HeartbeatRequest>>>,
    peer: Peer,
}

impl GrpcInProcess {
    pub fn new(coordinator: &Arc<Coordinator>, ordered: Arc<Mutex<Vec<Option<Order>>>>) -> Self {
        GrpcInProcess {
            coordinator: Arc::clone(coordinator),
            ordered,
            unadvertise: false,
            sent: Arc::default(),
            peer: peer("host-1"),
        }
    }
}

impl CoordinatorLink for GrpcInProcess {
    fn heartbeat(&self, _identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        let request = request_of(beat);
        self.sent
            .lock()
            .expect("the log is not poisoned")
            .push(request.clone());
        let (code, _, body) = call(
            &self.coordinator,
            Surface::Heartbeat,
            &self.peer,
            HEARTBEAT,
            None,
            &request.encode_to_vec(),
        );
        if code != 0 {
            return Err(LinkError::Refused(
                Reason::new("forbidden").expect("a token"),
            ));
        }
        let response = pb::HeartbeatResponse::decode(body.as_slice()).expect("a response");
        let order = response.order.map(link_order);
        self.ordered
            .lock()
            .expect("the log is not poisoned")
            .push(order.clone());
        let mut features: Vec<Feature> = response
            .features
            .iter()
            .filter_map(|name| Feature::parse(name))
            .collect();
        if self.unadvertise {
            features.retain(|feature| *feature != Feature::ForcedV1);
        }
        let halts: BTreeMap<_, _> = response.halt_generations.into_iter().collect();
        Ok(Reply {
            order,
            features,
            halt_generations: halts
                .into_iter()
                .map(|(service, generation)| {
                    (ServiceName::new(&service).expect("a service"), generation)
                })
                .collect(),
            acknowledged: response
                .acks
                .iter()
                .map(|ack| Acknowledged {
                    service: ServiceName::new(&ack.service).expect("a service"),
                    order: OrderRef {
                        id: OrderId::new(ack.order_id.as_str()).expect("an id"),
                        order_try: ack.order_try,
                    },
                })
                .collect(),
            repositories: None,
        })
    }
}
