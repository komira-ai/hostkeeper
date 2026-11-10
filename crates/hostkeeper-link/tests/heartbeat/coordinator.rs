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
    BeatOutcome, CertState, CoordinatorLink, DEFAULT_HEARTBEAT_SECS, Feature, Heartbeater,
    LoopConfig, Order, OrderId, Platform, Ports, Reply, Target, Verb,
};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::wire;
use http_body_util::{BodyExt, Full};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::tls::{Close, Farm, PipeTransport, Pki, Request, http, link, serve};
use crate::{Rig, digest, recorded, service, version};

/// The operator token of the coordinator.
pub(crate) const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// How long the handler waits for a body: the whole body is there when it is called.
pub(crate) const PATIENCE: Duration = Duration::from_secs(30);

/// A coordinator that runs `web` on the role `daemon` and wants it at 2.0 (whose package digest
/// CI registered as [`digest`]), with one halt of
/// `web` cleared, over the files of `dir`; started at second 1000 of its clock, which then
/// reads 1100, past its quiet start.
pub(crate) fn coordinator(dir: &Path) -> Arc<Coordinator> {
    std::fs::write(
        dir.join("desired.json"),
        format!(
            r#"{{"services":{{"web":{{"version":"2.0","min_up":{{}},"digests":{{"2.0":"{}"}}}}}}}}"#,
            digest().as_str()
        ),
    )
    .expect("the desired state");
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
    let (verifier, _) = hostkeeper_oidc::testkit::static_verifier();
    let rollout = "[[service]]\nname = \"web\"\nroles = [\"daemon\"]\n";
    let coordinator = Coordinator::new(desired, verifier, OPERATOR.to_owned(), clock)
        .with_services(Services::parse(rollout).expect("the rollout file"))
        .with_ops(ops);
    seconds.store(1_100, Ordering::SeqCst);
    Arc::new(coordinator)
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
/// not read: the coordinator answers the link's heartbeat with an update of `web` to 2.0 and
/// its terms (`order-v1`: order id, try, the registered digest), the features both sides know
/// and the halt generation of `web`; the link gives the loop the features, the generation and
/// the update order with that id, try and digest, which is also the coordinator's typed order
/// for the same heartbeat (the other API); the loop raises the held generation and runs the
/// order; its verdict goes back under the order's id and try; and the coordinator's status
/// shows the host under the agent and version the heartbeat named.
#[test]
fn a_reply_from_the_real_coordinator_maps_to_the_loops_reply() {
    let dir = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "coordinator-real")
        .expect("a scratch directory");
    let coordinator = coordinator(dir.path());
    let farm = Farm::new();
    let pki = Pki::with_bundle("coordinator-real-pki", &farm.ca.pem);
    let transport = Arc::new(PipeTransport::default());
    let rig = Rig::new("coordinator-real-loop");
    rig.install(vec![recorded("web", "1.0")]);
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
    // The coordinator's order, with its id, try and digest, is the loop's update order.
    let hash: String = Sha256::digest(b"web\n2.0")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let ordered = Order {
        id: OrderId::new(&format!("update-{}", &hash[..32])).expect("an order id"),
        order_try: 1,
        verb: Verb::Update(Target {
            service: service("web"),
            version: version("2.0"),
            digest: digest(),
            index_digest: None,
            entry: None,
        }),
    };
    // The other API carries the same order: the typed order the coordinator gives for the same
    // heartbeat of the same host is the loop's order, field for field.
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
                target: None,
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

    // Its verdict goes back under the order's id and try.
    let server = serve_coordinator(&transport, &farm, &coordinator);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let request = server.join().expect("the server").expect("a request");
    let sent: Value = serde_json::from_slice(&request.body).expect("JSON");
    let last = &sent["services"][0]["last_update"];
    assert_eq!(last["order_id"], ordered.id.as_str());
    assert_eq!(last["order_try"], 1);
    assert_eq!(last["target"], "2.0");

    let status = hyper::Request::builder()
        .method("GET")
        .uri("/v1/status")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(Bytes::new()))
        .expect("a request");
    let (code, body) = call(&coordinator, Surface::Control, &None, status);
    assert_eq!(code, 200);
    let status: Value = serde_json::from_slice(&body).expect("JSON");
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
