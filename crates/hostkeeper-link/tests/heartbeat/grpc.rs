//! The gRPC link, [`GrpcMtlsLink`], against the real coordinator and against the fakes of its
//! ports, and the link that chooses between it and the JSON link by the features advertised.
//!
//! The coordinator's side of an exchange is `hostkeeper_coordinator::listen::connection`, run on
//! a runtime the test owns and handed the server end of an in-memory stream; the link reaches it
//! through a dial whose connections are the client ends of such streams. Nothing here opens a
//! socket but the one test of `TcpDial`, which listens on the loopback address with a port the
//! system chose.

use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_coordinator::http::Surface;
use hostkeeper_coordinator::listen::connection;
use hostkeeper_coordinator::service::Coordinator;
use hostkeeper_link::testing::{FakeLink, RecordingLog, identity};
use hostkeeper_link::{
    ANNOUNCED, Acknowledged, AsyncConn, CoordinatorLink, EndpointError, Feature, GrpcDial,
    GrpcLinkError, GrpcMtlsLink, Health, Heartbeat, LinkError, MAX_HEARTBEAT_BYTES, NegotiatedLink,
    Order, OrderRef, OrderResult, Outcome, Platform, Reason, Reply, ServiceFacts, Target, TcpDial,
    TrustFault, Unsendable, Verb,
};
use hostkeeper_testkit::Scratch;
use hostkeeper_tls::{Alpn, TlsSource};
use http_body_util::Full;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio_rustls::TlsAcceptor;

use crate::coordinator::{OPERATOR, PATIENCE, call, coordinator, serve_coordinator};
use crate::tls::{COORDINATOR, Ca, Farm, Leaf, PipeTransport, Pki, config, config_text, link};
use crate::verdict;
use crate::{digest, order_id, reply, service, version};

/// The connections the link is handed: the client ends of in-memory streams the test queued,
/// in order. With none queued, the dial is refused.
#[derive(Default)]
struct DuplexDial {
    ends: Mutex<VecDeque<DuplexStream>>,
    dialed: Mutex<Vec<(String, u16)>>,
}

impl DuplexDial {
    fn queue(&self, end: DuplexStream) {
        self.ends.lock().expect("the ends").push_back(end);
    }

    /// The hosts and ports dialed, in order.
    fn dialed(&self) -> Vec<(String, u16)> {
        self.dialed.lock().expect("the dialed").clone()
    }
}

impl GrpcDial for DuplexDial {
    fn dial(
        &self,
        host: &str,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<Box<dyn AsyncConn>>> + Send + '_>> {
        self.dialed
            .lock()
            .expect("the dialed")
            .push((host.to_owned(), port));
        let end = self.ends.lock().expect("the ends").pop_front();
        let dialed = end
            .map(|end| Box::new(end) as Box<dyn AsyncConn>)
            .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionRefused));
        Box::pin(std::future::ready(dialed))
    }
}

/// A dial that never completes.
struct PendingDial;

impl GrpcDial for PendingDial {
    fn dial(
        &self,
        _host: &str,
        _port: u16,
    ) -> Pin<Box<dyn Future<Output = io::Result<Box<dyn AsyncConn>>> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

/// The coordinator's side: a runtime of the test's own that serves each connection queued on a
/// dial with the coordinator's listener code, over the TLS of `leaf`, which asks for a client
/// certificate of `client_ca`.
struct Server {
    runtime: tokio::runtime::Runtime,
    tls: TlsSource,
    coordinator: Arc<Coordinator>,
}

impl Server {
    fn new(leaf: &Leaf, client_ca: &Ca, coordinator: &Arc<Coordinator>) -> Server {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let tls = TlsSource::fixed_alpn(
            "heartbeat",
            leaf.cert_pem.as_bytes(),
            leaf.key_pem.as_bytes(),
            Some(client_ca.pem.as_bytes()),
            Alpn::H2AndHttp1,
        )
        .expect("a TLS source");
        Server {
            runtime,
            tls,
            coordinator: Arc::clone(coordinator),
        }
    }

    /// The coordinator of `farm`, over `coordinator`.
    fn of(farm: &Farm, coordinator: &Arc<Coordinator>) -> Server {
        Server::new(&farm.coordinator, &farm.ca, coordinator)
    }

    /// Serves the next call of `dial`: a new connection queued on it, ended by the client.
    fn accept(&self, dial: &DuplexDial) {
        let (client, server) = tokio::io::duplex(1 << 20);
        dial.queue(client);
        self.runtime.spawn(connection(
            server,
            self.tls.clone(),
            Surface::Heartbeat,
            Arc::clone(&self.coordinator),
            PATIENCE,
        ));
    }
}

/// A scratch directory and the coordinator over it (see [`coordinator`]).
fn fresh(name: &str) -> (Scratch, Arc<Coordinator>) {
    let dir =
        Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory");
    let coordinator = coordinator(dir.path());
    (dir, coordinator)
}

fn grpc_link(
    farm: &Farm,
    pki: &Pki,
    dial: &Arc<DuplexDial>,
    log: &Arc<RecordingLog>,
) -> GrpcMtlsLink {
    GrpcMtlsLink::new(&config(&farm.ca), &pki.root(), dial.clone(), log.clone()).expect("a link")
}

/// A service at `at`, healthy, with nothing else to say.
fn facts(name: &str, at: &str) -> ServiceFacts {
    ServiceFacts {
        name: service(name),
        version: version(at),
        health: Health::Healthy,
        updating: None,
        last_update: None,
        deferred: None,
        health_detail: None,
        observed: None,
    }
}

fn beat(services: Vec<ServiceFacts>) -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: version("0.9.0"),
        features: ANNOUNCED.to_vec(),
        services,
        sent_unix: 0,
        renewal: hostkeeper_link::Renewal::default(),
    }
}

/// The heartbeat of a host running `web` at 1.0.
fn web_beat() -> Heartbeat {
    beat(vec![facts("web", "1.0")])
}

/// What the coordinator of [`coordinator`] answers a host running `web` at 1.0: the update to
/// 2.0 with its terms, the features both sides know, and the halt generation of `web`.
fn expected_reply() -> Reply {
    let hash: String = Sha256::digest(b"web\n2.0")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Reply {
        order: Some(Order {
            id: order_id(&format!("update-{}", &hash[..32])),
            order_try: 1,
            verb: Verb::Update(Target {
                service: service("web"),
                version: version("2.0"),
                digest: digest(),
                index_digest: None,
            }),
        }),
        features: vec![
            Feature::DeferredV1,
            Feature::HaltV1,
            Feature::HeartbeatV2,
            Feature::OrderV1,
            Feature::VerdictAcksV1,
            Feature::GrpcV1,
        ],
        halt_generations: vec![(service("web"), 1)],
        acknowledged: Vec::new(),
    }
}

/// The ids of the hosts the coordinator lists under `web`.
fn hosts_of_web(coordinator: &Coordinator) -> BTreeSet<String> {
    let request = hyper::Request::builder()
        .method("GET")
        .uri("/v1/status")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(bytes::Bytes::new()))
        .expect("a request");
    let (code, body) = call(coordinator, Surface::Control, &None, request);
    assert_eq!(code, 200);
    let status: Value = serde_json::from_slice(&body).expect("JSON");
    status["per_service"]["web"]["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|host| host["node_id"].as_str().expect("a node id").to_owned())
        .collect()
}

/// Catches a gRPC link whose request or reading of the response differs from the JSON route's:
/// two coordinators built identically are sent the same facts, one over the JSON link and one
/// over the gRPC link (HTTP/2 over mutual TLS, through the coordinator's listener code), and
/// both answer with the same reply, which is the update of `web` to 2.0 with the order id, try
/// and digest, the features both sides know (`grpc-v1` among them) and the halt generation.
#[test]
fn the_grpc_link_returns_the_same_reply_as_the_json_link_for_the_same_coordinator_state() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("grpc-same-reply", &farm.ca.pem);
    let beat = web_beat();

    let (_json_dir, json_coordinator) = fresh("grpc-same-reply-json");
    let transport = Arc::new(PipeTransport::default());
    let log = Arc::new(RecordingLog::default());
    let json = link(&farm, &pki, &transport, &log);
    let json_server = serve_coordinator(&transport, &farm, &json_coordinator);
    let from_json = json.heartbeat(&farm.host.identity(), &beat);
    json_server.join().expect("the server").expect("a request");

    let (_grpc_dir, grpc_coordinator) = fresh("grpc-same-reply-grpc");
    let server = Server::of(&farm, &grpc_coordinator);
    let dial = Arc::new(DuplexDial::default());
    let grpc = grpc_link(&farm, &pki, &dial, &log);
    server.accept(&dial);
    let from_grpc = grpc.heartbeat(&farm.host.identity(), &beat);

    assert_eq!(from_json, Ok(expected_reply()));
    assert_eq!(from_grpc, from_json);
    assert_eq!(dial.dialed(), [(COORDINATOR.to_owned(), 8443)]);
    assert_eq!(log.lines(), Vec::<String>::new());
    assert_eq!(
        hosts_of_web(&grpc_coordinator),
        BTreeSet::from(["d1".to_owned()])
    );
}

/// Catches a gRPC link that reads no acknowledgements, or reads them differently from the JSON
/// link: the same host, reporting a failed verdict of order `o-1` try 1 for `web`, is
/// acknowledged that verdict by the coordinator, and the reply of both links names it.
#[test]
fn a_verdict_is_acknowledged_in_the_same_way_on_the_json_and_the_grpc_link() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("grpc-acks", &farm.ca.pem);
    let mut web = facts("web", "1.0");
    web.last_update = Some(OrderResult {
        order: OrderRef {
            id: order_id("o-1"),
            order_try: 1,
        },
        target: version("2.0"),
        outcome: Outcome::Failed,
        phase: None,
        class: None,
        reason: None,
        forced: false,
        detail: None,
    });
    let beat = beat(vec![web]);
    let acknowledged = vec![Acknowledged {
        service: service("web"),
        order: OrderRef {
            id: order_id("o-1"),
            order_try: 1,
        },
    }];

    let (_json_dir, json_coordinator) = fresh("grpc-acks-json");
    let transport = Arc::new(PipeTransport::default());
    let log = Arc::new(RecordingLog::default());
    let json = link(&farm, &pki, &transport, &log);
    let json_server = serve_coordinator(&transport, &farm, &json_coordinator);
    let from_json = json.heartbeat(&farm.host.identity(), &beat);
    json_server.join().expect("the server").expect("a request");

    let (_grpc_dir, grpc_coordinator) = fresh("grpc-acks-grpc");
    let server = Server::of(&farm, &grpc_coordinator);
    let dial = Arc::new(DuplexDial::default());
    let grpc = grpc_link(&farm, &pki, &dial, &log);
    server.accept(&dial);
    let from_grpc = grpc.heartbeat(&farm.host.identity(), &beat);

    assert_eq!(
        from_json.map(|reply| reply.acknowledged),
        Ok(acknowledged.clone())
    );
    assert_eq!(from_grpc.map(|reply| reply.acknowledged), Ok(acknowledged));
}

/// Catches a link that talks to a server that is not the coordinator: a server whose certificate
/// is from another CA than the pinned one is refused in the handshake, as what answered not being
/// the coordinator, and no request reaches the coordinator behind it.
#[test]
fn the_grpc_link_refuses_a_server_that_does_not_chain_to_the_pinned_ca() {
    let farm = Farm::new();
    let other = Farm::new();
    let pki = Pki::with_bundle("grpc-untrusted", &farm.ca.pem);
    let (_dir, behind) = fresh("grpc-untrusted-coordinator");
    let server = Server::of(&other, &behind);
    let dial = Arc::new(DuplexDial::default());
    let log = Arc::new(RecordingLog::default());
    let grpc = grpc_link(&farm, &pki, &dial, &log);
    server.accept(&dial);

    assert_eq!(
        grpc.heartbeat(&farm.host.identity(), &web_beat()),
        Err(LinkError::Untrusted(TrustFault::ServerCertificate))
    );
    assert_eq!(
        hosts_of_web(&behind),
        BTreeSet::<String>::new(),
        "the coordinator behind it heard nothing"
    );
}

/// Catches a link that keeps the certificate of its first call: two calls of one link with the
/// certificates of two hosts of the CA reach the coordinator as two hosts, each under the name
/// in its own certificate, over a connection of its own.
#[test]
fn the_grpc_link_uses_the_replaced_certificate_at_the_next_call() {
    let farm = Farm::new();
    let second = farm.ca.host("d2", "daemon");
    let pki = Pki::with_bundle("grpc-renewed", &farm.ca.pem);
    let (_dir, behind) = fresh("grpc-renewed-coordinator");
    let server = Server::of(&farm, &behind);
    let dial = Arc::new(DuplexDial::default());
    let log = Arc::new(RecordingLog::default());
    let grpc = grpc_link(&farm, &pki, &dial, &log);

    server.accept(&dial);
    assert!(
        grpc.heartbeat(&farm.host.identity(), &web_beat()).is_ok(),
        "the first host is heard"
    );
    assert_eq!(
        hosts_of_web(&behind),
        BTreeSet::from(["d1".to_owned()]),
        "after the first call"
    );
    server.accept(&dial);
    assert!(
        grpc.heartbeat(&second.identity(), &web_beat()).is_ok(),
        "the second host is heard"
    );
    assert_eq!(
        hosts_of_web(&behind),
        BTreeSet::from(["d1".to_owned(), "d2".to_owned()]),
        "after the second call"
    );
    assert_eq!(dial.dialed().len(), 2);
}

/// A link over `fake`, which the test keeps a handle on.
struct Shared(Arc<FakeLink>);

impl CoordinatorLink for Shared {
    fn heartbeat(
        &self,
        identity: &hostkeeper_link::ClientIdentity,
        beat: &Heartbeat,
    ) -> Result<Reply, LinkError> {
        self.0.heartbeat(identity, beat)
    }
}

/// Catches a negotiated link that speaks gRPC before the coordinator said it serves it, or that
/// stays on it after it stopped: the first call goes to the JSON link; after a reply that
/// advertises `grpc-v1` calls go to the gRPC link and stay there across replies that advertise
/// it again, a refusal and a bad reply (the coordinator answered); a transport error of the gRPC
/// link sends the same heartbeat over JSON at once, whose answer is the loop's; after a reply
/// without it the next call is the JSON link's again. Every answer is the answering link's,
/// unchanged.
#[test]
fn grpc_is_used_only_after_the_reply_advertised_grpc_v1() {
    let json = Arc::new(FakeLink::default());
    let grpc = Arc::new(FakeLink::default());
    let negotiated = NegotiatedLink::new(Shared(json.clone()), Shared(grpc.clone()));
    let who = identity("n");
    let beat = web_beat();
    let advertising = reply(None, &[Feature::HaltV1, Feature::GrpcV1]);
    let silent = reply(None, &[Feature::HaltV1]);
    let refused = LinkError::Refused(Reason::new("forbidden").expect("a reason"));
    let reset = LinkError::Transport(io::ErrorKind::ConnectionReset);
    let malformed = LinkError::BadReply(hostkeeper_link::ReplyFault::Malformed);
    let over = LinkError::BadReply(hostkeeper_link::ReplyFault::OverBound);

    // Each step: what gRPC answers if the call reaches it, what JSON answers if the call reaches
    // it, the answer the loop gets, and whether the next call goes over gRPC.
    type Answer = Result<Reply, LinkError>;
    let steps: Vec<(Option<Answer>, Option<Answer>, Answer, bool)> = vec![
        // JSON first, whatever the coordinator is; it advertises.
        (
            None,
            Some(Ok(advertising.clone())),
            Ok(advertising.clone()),
            true,
        ),
        // gRPC, advertising again.
        (
            Some(Ok(advertising.clone())),
            None,
            Ok(advertising.clone()),
            true,
        ),
        // A refusal and a bad reply over gRPC are the answer, and change nothing.
        (Some(Err(refused.clone())), None, Err(refused.clone()), true),
        (
            Some(Err(malformed.clone())),
            None,
            Err(malformed.clone()),
            true,
        ),
        (Some(Err(over.clone())), None, Err(over.clone()), true),
        // A transport error over gRPC: the same heartbeat over JSON at once, whose reply
        // without the feature keeps the next one on JSON.
        (
            Some(Err(reset.clone())),
            Some(Ok(silent.clone())),
            Ok(silent.clone()),
            false,
        ),
        // JSON errors that are not transport ones change nothing.
        (None, Some(Err(refused.clone())), Err(refused), false),
        (None, Some(Err(malformed.clone())), Err(malformed), false),
        // JSON advertises, gRPC answers without it: JSON again.
        (
            None,
            Some(Ok(advertising.clone())),
            Ok(advertising.clone()),
            true,
        ),
        (Some(Ok(silent.clone())), None, Ok(silent), false),
        // JSON advertises; gRPC cannot be reached: JSON carries the heartbeat and advertises
        // again, so the next one tries gRPC; then neither can be reached: JSON next.
        (
            None,
            Some(Ok(advertising.clone())),
            Ok(advertising.clone()),
            true,
        ),
        (
            Some(Err(reset.clone())),
            Some(Ok(advertising.clone())),
            Ok(advertising),
            true,
        ),
        (
            Some(Err(reset.clone())),
            Some(Err(reset.clone())),
            Err(reset.clone()),
            false,
        ),
        (None, Some(Err(reset.clone())), Err(reset), false),
    ];
    assert!(
        !negotiated.uses_grpc(),
        "nothing is known of the coordinator"
    );
    let (mut json_calls, mut grpc_calls) = (0, 0);
    for (n, (over_grpc, over_json, answer, next)) in steps.into_iter().enumerate() {
        assert_eq!(
            negotiated.uses_grpc(),
            over_grpc.is_some(),
            "step {n}: the link chosen"
        );
        if let Some(scripted) = over_grpc {
            grpc.answer(scripted);
            grpc_calls += 1;
        }
        if let Some(scripted) = over_json {
            json.answer(scripted);
            json_calls += 1;
        }
        assert_eq!(
            negotiated.heartbeat(&who, &beat),
            answer,
            "step {n}: the answer"
        );
        assert_eq!(
            (json.calls().len(), grpc.calls().len()),
            (json_calls, grpc_calls),
            "step {n}: which links were called"
        );
        assert_eq!(negotiated.uses_grpc(), next, "step {n}: the link next");
    }
    assert_eq!(json.calls()[0], (who, beat));
}

/// Catches a link that dials with no usable trust or identity: a pki directory with no CA
/// bundle, a bundle without the pinned CA, and a client certificate and key that are no TLS
/// identity (not PEM, a certificate that does not parse, a certificate without its key) each end
/// the call with the TLS fault that names it, and nothing is dialed.
#[test]
fn the_grpc_link_dials_nothing_without_a_pinned_ca_and_a_client_identity() {
    let farm = Farm::new();
    let strange = Farm::new();
    let dial = Arc::new(DuplexDial::default());
    let log = Arc::new(RecordingLog::default());
    let beat = web_beat();

    let empty = Pki::empty("grpc-no-bundle");
    assert_eq!(
        grpc_link(&farm, &empty, &dial, &log).heartbeat(&farm.host.identity(), &beat),
        Err(LinkError::Untrusted(TrustFault::NoPinnedCa))
    );
    let unpinned = Pki::with_bundle("grpc-other-bundle", &strange.ca.pem);
    assert_eq!(
        grpc_link(&farm, &unpinned, &dial, &log).heartbeat(&farm.host.identity(), &beat),
        Err(LinkError::Untrusted(TrustFault::NoPinnedCa))
    );

    let pki = Pki::with_bundle("grpc-bad-identity", &farm.ca.pem);
    let link = grpc_link(&farm, &pki, &dial, &log);
    let broken_pem = "-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n";
    let good = farm.host.identity();
    let bad_identities = [
        hostkeeper_link::ClientIdentity::new(b"not PEM".to_vec(), b"not PEM".to_vec()),
        hostkeeper_link::ClientIdentity::new(broken_pem.as_bytes().to_vec(), good.key().to_vec()),
        hostkeeper_link::ClientIdentity::new(good.certificate().to_vec(), b"not PEM".to_vec()),
    ];
    for bad in bad_identities {
        assert_eq!(
            link.heartbeat(&bad, &beat),
            Err(LinkError::Untrusted(TrustFault::ClientIdentity))
        );
    }
    assert_eq!(dial.dialed(), Vec::<(String, u16)>::new());
}

/// Catches a heartbeat sent over the bound, or one at it refused: a heartbeat whose protobuf
/// encoding is one byte over [`MAX_HEARTBEAT_BYTES`] is refused with its length and nothing is
/// dialed, and the one a byte shorter goes on to the dial. (The verdict's detail is the lever:
/// each byte of it is one byte of the encoding at these lengths.)
#[test]
fn the_grpc_link_refuses_a_heartbeat_one_byte_over_the_bound_and_dials_for_one_at_it() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("grpc-bound", &farm.ca.pem);
    let dial = Arc::new(DuplexDial::default());
    let log = Arc::new(RecordingLog::default());
    let link = grpc_link(&farm, &pki, &dial, &log);
    let with_detail = |bytes: usize| {
        let order = Order {
            id: order_id("o-1"),
            order_try: 1,
            verb: Verb::Update(Target {
                service: service("web"),
                version: version("2.0"),
                digest: digest(),
                index_digest: None,
            }),
        };
        let mut done = verdict(&order, Outcome::Failed);
        done.detail = Some("x".repeat(bytes));
        let mut web = facts("web", "1.0");
        web.last_update = Some(done);
        link.heartbeat(&farm.host.identity(), &beat(vec![web]))
    };
    // The largest detail the link still sends: the dial, which has no connection, refuses it.
    let (mut fits, mut over) = (0_usize, 2 * MAX_HEARTBEAT_BYTES);
    while over - fits > 1 {
        let middle = (fits + over) / 2;
        match with_detail(middle) {
            Err(LinkError::Unsendable(Unsendable::Bytes { .. })) => over = middle,
            Err(LinkError::Transport(io::ErrorKind::ConnectionRefused)) => fits = middle,
            other => panic!("{middle}: {other:?}"),
        }
    }
    assert_eq!(
        with_detail(fits),
        Err(LinkError::Transport(io::ErrorKind::ConnectionRefused))
    );
    assert_eq!(
        with_detail(over),
        Err(LinkError::Unsendable(Unsendable::Bytes {
            len: MAX_HEARTBEAT_BYTES + 1
        }))
    );
}

/// Catches a heartbeat sent without a fact the wire has no word for: a service name outside the
/// wire's grammar is refused by name, and nothing is dialed.
#[test]
fn the_grpc_link_refuses_a_service_name_the_wire_cannot_carry() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("grpc-unsendable", &farm.ca.pem);
    let dial = Arc::new(DuplexDial::default());
    let log = Arc::new(RecordingLog::default());
    let link = grpc_link(&farm, &pki, &dial, &log);
    assert_eq!(
        link.heartbeat(&farm.host.identity(), &beat(vec![facts("Web_1", "1.0")])),
        Err(LinkError::Unsendable(Unsendable::ServiceName(
            "Web_1".to_owned()
        )))
    );
    assert_eq!(dial.dialed(), Vec::<(String, u16)>::new());
}

/// Catches a dial failure reported as anything but a transport error of its kind, and a call
/// that is not ended at its bound: a dial that is refused is `Transport(ConnectionRefused)`; a
/// peer that is gone before the handshake, and one that goes after it, are transport errors; a
/// dial that never completes ends at the bound (here none) as a time-out.
#[test]
fn the_grpc_link_reports_a_connection_that_fails_as_a_transport_error() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("grpc-transport", &farm.ca.pem);
    let dial = Arc::new(DuplexDial::default());
    let log = Arc::new(RecordingLog::default());
    let link = grpc_link(&farm, &pki, &dial, &log);
    let beat = web_beat();
    let who = farm.host.identity();

    assert_eq!(
        link.heartbeat(&who, &beat),
        Err(LinkError::Transport(io::ErrorKind::ConnectionRefused))
    );

    let (client, gone) = tokio::io::duplex(1 << 16);
    drop(gone);
    dial.queue(client);
    assert!(matches!(
        link.heartbeat(&who, &beat),
        Err(LinkError::Transport(_))
    ));

    // A server that completes the handshake and hangs up before HTTP/2.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("a runtime");
    let (client, server) = tokio::io::duplex(1 << 16);
    dial.queue(client);
    let acceptor = TlsAcceptor::from(farm.server());
    runtime.spawn(async move {
        let _ = acceptor.accept(server).await;
    });
    assert!(matches!(
        link.heartbeat(&who, &beat),
        Err(LinkError::Transport(_))
    ));

    let never = GrpcMtlsLink::new(
        &config(&farm.ca),
        &pki.root(),
        Arc::new(PendingDial),
        log.clone(),
    )
    .expect("a link")
    .bounded_by(Duration::ZERO);
    assert_eq!(
        never.heartbeat(&who, &beat),
        Err(LinkError::Transport(io::ErrorKind::TimedOut))
    );
}

/// Catches a refusal of the coordinator read as something else: a client certificate that is not
/// a host's (the CA made it for the coordinator's name, with no role) completes the handshake
/// and is answered with the coordinator's refusal, whose one-token message is the link's
/// `Refused` reason.
#[test]
fn the_grpc_link_gives_the_coordinators_refusal_token() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("grpc-refused", &farm.ca.pem);
    let (_dir, behind) = fresh("grpc-refused-coordinator");
    let server = Server::of(&farm, &behind);
    let dial = Arc::new(DuplexDial::default());
    let log = Arc::new(RecordingLog::default());
    let grpc = grpc_link(&farm, &pki, &dial, &log);
    server.accept(&dial);
    let impostor = farm.ca.client_for_coordinator_name().identity();
    assert_eq!(
        grpc.heartbeat(&impostor, &web_beat()),
        Err(LinkError::Refused(
            Reason::new("forbidden").expect("a reason")
        ))
    );
}

/// Catches a link made over an address it does not dial, or a runtime failure reported with the
/// wrong words: an address with a path is `Endpoint(Path)` with the endpoint's own words, and a
/// runtime that cannot start says so.
#[test]
fn a_grpc_link_is_not_made_for_an_address_it_cannot_dial() {
    let farm = Farm::new();
    let address = format!("https://{COORDINATOR}:8443/v1");
    let config = DaemonConfig::parse(&config_text(&address, &farm.ca.sha256_hex()))
        .expect("a daemon config");
    let made = GrpcMtlsLink::new(
        &config,
        Path::new("pki"),
        Arc::new(DuplexDial::default()),
        Arc::new(RecordingLog::default()),
    );
    let error = made.err().expect("an address with a path is refused");
    assert!(matches!(
        &error,
        GrpcLinkError::Endpoint(EndpointError::Path(path)) if *path == address
    ));
    assert_eq!(error.to_string(), EndpointError::Path(address).to_string());
    assert_eq!(
        GrpcLinkError::from(io::Error::other("no threads")).to_string(),
        "cannot start the runtime of the gRPC link: no threads"
    );
}

/// Catches a production dial that does not reach the address it is given: it connects to a
/// listener on the loopback address with a port the system chose and carries bytes both ways,
/// and an address no one listens on is an error.
#[test]
fn the_tcp_dial_carries_bytes_to_the_address_and_fails_where_none_listens() {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut ping = [0_u8; 4];
        stream.read_exact(&mut ping).expect("the ping");
        stream.write_all(b"pong").expect("the pong");
        ping
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    runtime.block_on(async {
        let mut conn = TcpDial.dial("127.0.0.1", port).await.expect("connected");
        conn.write_all(b"ping").await.expect("written");
        let mut pong = [0_u8; 4];
        conn.read_exact(&mut pong).await.expect("read");
        assert_eq!(&pong, b"pong");
        assert!(TcpDial.dial("127.0.0.1", 0).await.is_err());
    });
    assert_eq!(&server.join().expect("the server"), b"ping");
}
