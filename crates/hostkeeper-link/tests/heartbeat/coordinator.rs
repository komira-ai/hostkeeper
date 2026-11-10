//! The link against the real coordinator: `hostkeeper-coordinator`'s heartbeat handler answers
//! in process, behind the TLS server of `tls`, with the certificate the link presented as the
//! caller. Its desired state and its halt clear generation are files written before it starts,
//! so no CI token is needed.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::http::{Peer, Surface, handle};
use hostkeeper_coordinator::identity::identity_of;
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::service::{Clock, Coordinator};
use hostkeeper_coordinator::services::Services;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_link::{
    BeatOutcome, CertState, CoordinatorLink, DEFAULT_HEARTBEAT_SECS, Entry, Feature, Heartbeater,
    LoopConfig, Order, OrderId, Platform, Ports, Reply, Target, Verb,
};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, Repository, StepBounds, Target as Release, TargetList,
};
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::wire;
use http_body_util::{BodyExt, Full};
use hyper::Version as Http;
use prost::Message;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::grpc::{DuplexDial, Server as GrpcServer, grpc_link};
use crate::tls::{Close, Farm, PipeTransport, Pki, Request, http, link, serve};
use crate::{Rig, digest, recorded, service, version};
use hostkeeper_link::RecordedService;

/// The operator token of the coordinator.
pub(crate) const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// How long the handler waits for a body: the whole body is there when it is called.
pub(crate) const PATIENCE: Duration = Duration::from_secs(30);

/// A coordinator that runs `web` on the role `daemon`, with one halt of `web` cleared, over the
/// files of `dir`; nothing is desired until CI sets a version ([`set_web_release`]). Started at
/// second 1000 of its clock, which then reads 1100, past its quiet start.
pub(crate) fn coordinator(dir: &Path) -> Arc<Coordinator> {
    std::fs::write(dir.join("desired.json"), r#"{"services":{}}"#).expect("the desired state");
    std::fs::write(
        dir.join("desired.json.ops.halts"),
        r#"{"services":{"web":{"halt_generation":1}}}"#,
    )
    .expect("the halts file");
    let desired = DesiredStore::open(&dir.join("desired.json"), None).expect("the desired state");
    let ops = OpsStore::open(&dir.join("desired.json.ops")).expect("the ops store");
    let seconds = Arc::new(AtomicU64::new(1_000));
    let reading = Arc::clone(&seconds);
    let clock: Clock = Arc::new(move || Duration::from_secs(reading.load(Ordering::SeqCst)));
    // CI's allow-list admits `web`, so a test can set its release.
    let allow = format!(
        "{}services = [\"web\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    let (oidc_now, _) = hostkeeper_oidc::testkit::clock();
    let verifier = Arc::new(hostkeeper_oidc::Verifier::new(
        hostkeeper_oidc::OidcConfig::parse(&allow).expect("the allow-list"),
        Arc::new(hostkeeper_oidc::testkit::StaticKeys(
            hostkeeper_oidc::testkit::jwks_of(&[&hostkeeper_oidc::testkit::keys()[0]]),
        )),
        Duration::from_secs(30),
        oidc_now,
        hostkeeper_oidc::testkit::spent(),
    ));
    let rollout = "[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n";
    let coordinator = Coordinator::new(desired, verifier, OPERATOR.to_owned(), clock)
        .with_services(Services::parse(rollout).expect("the rollout file"))
        .with_ops(ops);
    seconds.store(1_100, Ordering::SeqCst);
    Arc::new(coordinator)
}

/// The path of `Releases.SetDesired`.
const SET_DESIRED: &str = "/hostkeeper.operator.v1.Releases/SetDesired";
const PUT_REPOSITORY: &str = "/hostkeeper.operator.v1.Repositories/Put";

/// The entry of `web` a release lists for the installer `apt`: the repository entry (no
/// location, no size, no attestation), pinned by the package digest [`digest`].
pub(crate) fn apt_entry() -> Release {
    Release {
        installer: "apt".to_owned(),
        digest: digest().as_str().to_owned(),
        manifest: Some(Manifest {
            package: Some("web".to_owned()),
            hook: Some(Hook {
                timeout_secs: Some(StepBounds {
                    prepare: 0,
                    cutover: 10,
                    verify: 20,
                    rollback: 30,
                    cleanup: 40,
                    uninstall: None,
                }),
                verify_deadline_secs: 50,
                ..Hook::default()
            }),
            run: vec!["web.service".to_owned()],
            min_hostkeeper: "0.4.0".to_owned(),
            installer_bounds: Some(InstallerBounds {
                stage_secs: 60,
                activate_secs: 120,
                remove_secs: 300,
            }),
            ..Manifest::default()
        }),
        repository: Some("komira-testing".to_owned()),
        package_version: Some("2.0-1".to_owned()),
        ..Release::default()
    }
}

/// `komira`, the repository every service names by default, with the suite `testing` the entry of
/// [`apt_entry`] names the channel of; its keyring is one hand-built public key packet,
/// and the fingerprint is the SHA-1 of that key's body, which the registry checks.
fn komira() -> Repository {
    let mut keyring = vec![0xc0 | 6, 14, 4, 0, 0, 0, 1, 22];
    keyring.extend([1_u8; 8]);
    Repository {
        name: "komira".to_owned(),
        kind: "apt".to_owned(),
        uri: "https://packages.example.com/apt".to_owned(),
        suites: vec!["testing".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec!["D282F759B44189DAE07134C869E048AEDB2EEC39".to_owned()],
        keyring,
        ..Default::default()
    }
}

/// One gRPC call through the coordinator's router on its private listener, as `token`: the
/// status and its message.
fn grpc_call(
    coordinator: &Arc<Coordinator>,
    path: &str,
    token: &str,
    message: &[u8],
) -> (String, String) {
    let mut framed = vec![0u8];
    framed.extend(u32::try_from(message.len()).expect("fits").to_be_bytes());
    framed.extend(message);
    let posted = hyper::Request::builder()
        .method("POST")
        .uri(path)
        .version(Http::HTTP_2)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {token}"))
        .body(Full::new(Bytes::from(framed)))
        .expect("a request");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let response = hostkeeper_coordinator::grpc::route(
            coordinator,
            Surface::Control,
            &None,
            posted,
            PATIENCE,
        )
        .await;
        let headers = response.headers().clone();
        let collected = response.into_body().collect().await.expect("a body");
        let trailers = collected.trailers().cloned().unwrap_or(headers);
        let field = |name: &str| {
            trailers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        (
            field("grpc-status").unwrap_or_default(),
            field("grpc-message").unwrap_or_default(),
        )
    })
}

/// The operator puts [`komira`] in the registry (`Repositories.Put`), then CI sets `web` to 2.0
/// with the release that lists [`apt_entry`] for every platform, through the coordinator's gRPC
/// router (`Releases.SetDesired`: a set with a repository entry is refused until the registry
/// holds the repository); the calls panic if one is refused. A version set by digest alone is
/// ordered to no host: only a kept release is.
pub(crate) fn set_web_release(coordinator: &Arc<Coordinator>) {
    let put = operator::PutRepositoryRequest {
        repository: Some(komira()),
        reason: "a test".to_owned(),
    };
    assert_eq!(
        grpc_call(coordinator, PUT_REPOSITORY, OPERATOR, &put.encode_to_vec()),
        ("0".to_owned(), String::new()),
        "the put of komira"
    );
    let list = TargetList {
        entries: vec![apt_entry()],
    };
    let request = operator::SetDesiredRequest {
        service: "web".to_owned(),
        desired: Some(operator::DesiredState {
            version: "2.0".to_owned(),
            ..operator::DesiredState::default()
        }),
        targets: ["linux-x86_64", "linux-arm64", "macos-arm64"]
            .into_iter()
            .map(|platform| (platform.to_owned(), list.clone()))
            .collect(),
        ..operator::SetDesiredRequest::default()
    };
    let token = hostkeeper_oidc::testkit::mint(
        &hostkeeper_oidc::testkit::keys()[0],
        &hostkeeper_oidc::testkit::good_claims(hostkeeper_oidc::testkit::STATIC_ISSUER),
    );
    let (code, text) = grpc_call(coordinator, SET_DESIRED, &token, &request.encode_to_vec());
    assert_eq!(
        (code.as_str(), text.as_str()),
        ("0", ""),
        "the set of web 2.0 with its release"
    );
}

/// The coordinator's status, as the operator reads it.
fn status_of(coordinator: &Coordinator) -> Value {
    let status = hyper::Request::builder()
        .method("GET")
        .uri("/v1/status")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let (code, body) = call(coordinator, Surface::Control, &None, status);
    assert_eq!(code, 200);
    serde_json::from_slice(&body).expect("JSON")
}

/// The coordinator's answer to one request, on `surface`, from `peer`: the status and the body.
pub(crate) fn call(
    coordinator: &Coordinator,
    surface: Surface,
    peer: &Peer,
    request: hyper::Request<Full<Bytes>>,
) -> (u16, Vec<u8>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let response = handle(coordinator, surface, peer, request, PATIENCE).await;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("a body")
            .to_bytes()
            .to_vec();
        (status, body)
    })
}

/// Serves one heartbeat on `transport` with the coordinator's handler, the client's
/// certificate as the caller.
pub(crate) fn serve_coordinator(
    transport: &PipeTransport,
    farm: &Farm,
    coordinator: &Arc<Coordinator>,
) -> std::thread::JoinHandle<Option<Request>> {
    let coordinator = Arc::clone(coordinator);
    serve(transport, farm.server(), Close::Notify, move |request| {
        let path = request
            .head
            .lines()
            .next()
            .and_then(|line| line.split(' ').nth(1))
            .expect("a request line")
            .to_owned();
        let posted = hyper::Request::builder()
            .method("POST")
            .uri(path)
            .body(Full::new(Bytes::from(request.body.clone())))
            .expect("a request");
        let peer: Peer = Some(Arc::from(request.peer.as_slice()));
        let (status, body) = call(&coordinator, Surface::Heartbeat, &peer, posted);
        http(status, &String::from_utf8(body).expect("text"))
    })
}

/// Catches a mapping that drops a field of the reply or a heartbeat the real coordinator does
/// not read: the coordinator, which keeps the release of `web` 2.0 (one `apt` entry), answers
/// the link's JSON heartbeat with the features both sides know and the halt generation of `web`
/// and no order (the JSON route carries none); the loop over it raises the held generation; the
/// loop of a daemon that announces `apt`, over gRPC, is given the update order with the id, try,
/// digest and entry of the typed order the coordinator gives for the same host (the other API),
/// and runs it; its verdict goes back under the order's id and try, and the coordinator's status
/// shows the verdict and the host under the agent and version the heartbeat named.
#[test]
fn a_reply_from_the_real_coordinator_maps_to_the_loops_reply() {
    let dir = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "coordinator-real")
        .expect("a scratch directory");
    let coordinator = coordinator(dir.path());
    set_web_release(&coordinator);
    let farm = Farm::new();
    let pki = Pki::with_bundle("coordinator-real-pki", &farm.ca.pem);
    let transport = Arc::new(PipeTransport::default());
    let rig = Rig::new("coordinator-real-loop");
    // `web` runs from the package manager `apt`, which the release lists an entry of.
    rig.install(vec![RecordedService {
        installer: Some("apt".to_owned()),
        ..recorded("web", "1.0")
    }]);
    rig.world
        .certs
        .set(CertState::Enrolled(farm.host.identity()));

    // The link alone.
    let log = Arc::new(RecordingLog::default());
    let json = link(&farm, &pki, &transport, &log);
    let server = serve_coordinator(&transport, &farm, &coordinator);
    let beat = hostkeeper_link::Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: version("0.9.0"),
        features: hostkeeper_link::ANNOUNCED.to_vec(),
        services: Vec::new(),
        sent_unix: 0,
        renewal: hostkeeper_link::Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
        repositories: None,
        facts_generation: None,
    };
    assert_eq!(
        json.heartbeat(&farm.host.identity(), &beat),
        Ok(Reply {
            order: None,
            features: vec![
                Feature::DeferredV1,
                Feature::HaltV1,
                Feature::HeartbeatV2,
                Feature::ForcedV1,
                Feature::OrderV1,
                Feature::VerdictAcksV1,
                Feature::GrpcV1,
                Feature::TargetV1
            ],
            halt_generations: vec![(service("web"), 1)],
            acknowledged: Vec::new(),
            repositories: None,
            facts: None,
        })
    );
    server.join().expect("the server").expect("a request");
    assert_eq!(
        log.lines(),
        Vec::<String>::new(),
        "no update for a host listing no web"
    );

    // The loop over it.
    let server = serve_coordinator(&transport, &farm, &coordinator);
    let ports = Ports {
        link: Arc::new(link(&farm, &pki, &transport, &rig.world.log)),
        ..rig.world.ports()
    };
    let mut heartbeater = Heartbeater::new(
        LoopConfig {
            platform: Platform::LinuxX86_64,
            agent_version: version("0.9.0"),
            state_dir: rig.state().to_path_buf(),
            period_secs: DEFAULT_HEARTBEAT_SECS,
            installers: Vec::new(),
            private_repository: false,
        },
        ports,
    );
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let request = server.join().expect("the server").expect("a request");
    let sent: Value = serde_json::from_slice(&request.body).expect("JSON");
    assert_eq!(sent["services"][0]["name"], "web");
    assert_eq!(sent["services"][0]["version"], "1.0");
    assert_eq!(rig.world.halts.raises(), [("web".to_owned(), 1)]);
    assert_eq!(rig.world.log.lines(), Vec::<String>::new());
    // The JSON route carries no order: the update of web is ordered over gRPC alone.
    assert_eq!(rig.world.dispatch.pending(), 0);

    // The loop of a daemon that announces apt, over gRPC: the typed order the coordinator gives
    // it, with the entry of its release, is the loop's update order.
    let hash: String = Sha256::digest(b"web\n2.0")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let entry = apt_entry();
    let ordered = Order {
        id: OrderId::new(&format!("update-{}", &hash[..32])).expect("an order id"),
        order_try: 1,
        verb: Verb::Update(Target {
            service: service("web"),
            version: version("2.0"),
            digest: digest(),
            index_digest: None,
            entry: Some(Entry::new(entry.clone())),
        }),
    };
    let grpc_server = GrpcServer::of(&farm, &coordinator);
    let dial = Arc::new(DuplexDial::default());
    let mut grpc_beater = Heartbeater::new(
        LoopConfig {
            platform: Platform::LinuxX86_64,
            agent_version: version("0.9.0"),
            state_dir: rig.state().to_path_buf(),
            period_secs: DEFAULT_HEARTBEAT_SECS,
            installers: vec!["apt".to_owned()],
            private_repository: false,
        },
        Ports {
            link: Arc::new(grpc_link(&farm, &pki, &dial, &rig.world.log)),
            ..rig.world.ports()
        },
    );
    grpc_server.accept(&dial);
    assert_eq!(grpc_beater.beat_once(), BeatOutcome::Sent);
    // The same typed order, from the coordinator's own API for the same heartbeat of the same
    // host (which lists apt): the loop's order, field for field.
    let who = identity_of(&request.peer, coordinator.prefixes()).expect("an updater");
    let again: wire::Heartbeat = serde_json::from_slice(&request.body).expect("a heartbeat");
    let (_, typed) = coordinator
        .heartbeat_with_orders(
            who,
            again,
            hostkeeper_coordinator::trusted_root::TrustedRootSaid::default(),
        )
        .expect("an answer");
    assert_eq!(
        typed.expect("a typed order").to_proto(),
        agent::Order {
            order_id: ordered.id.as_str().to_owned(),
            r#try: ordered.order_try,
            verb: Some(agent::order::Verb::Update(agent::Update {
                target: Some(entry),
                service: "web".to_owned(),
                version: "2.0".to_owned(),
                package_digest: digest().as_str().to_owned(),
                index_digest: String::new(),
            })),
        }
    );
    assert_eq!(rig.world.dispatch.pending(), 1);
    assert!(rig.world.dispatch.run_next());
    assert_eq!(rig.world.orders.orders(), std::slice::from_ref(&ordered));

    // Its verdict goes back under the order's id and try, and the coordinator takes it.
    grpc_server.accept(&dial);
    assert_eq!(grpc_beater.beat_once(), BeatOutcome::Sent);
    let last = &status_of(&coordinator)["per_service"]["web"]["hosts"][0]["last_update"];
    assert_eq!(last["order_id"], ordered.id.as_str());
    assert_eq!(last["order_try"], 1);
    assert_eq!(last["target"], "2.0");

    let status = status_of(&coordinator);
    // With no legacy service the hosts are listed under their service.
    let hosts = status["per_service"]["web"]["hosts"]
        .as_array()
        .expect("hosts");
    assert_eq!(hosts.len(), 1, "{status}");
    assert_eq!(
        hosts[0]["agent"],
        serde_json::json!({ "name": "hostkeeper", "version": "0.9.0" }),
        "{status}"
    );
}
