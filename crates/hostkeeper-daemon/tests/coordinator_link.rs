//! The shipped daemon's heartbeat link (`hostkeeper_daemon::links::HostLink`, which
//! `cli::host_links` gives the heartbeat loop) against the real coordinator: its heartbeat
//! listener (`hostkeeper_coordinator::listen::serve`) on a loopback socket of port 0, behind the
//! TLS of a test CA, asking for a client certificate of that CA. The daemon's pki directory holds
//! a host certificate of the same CA, and its config names the listener's address and pins the
//! CA's SHA-256, as enrollment and the operator leave them on a host.

mod support;

use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::http::{Surface, handle};
use hostkeeper_coordinator::listen::{Listener, MAX_CONNECTIONS, serve};
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::service::{Clock, Coordinator};
use hostkeeper_coordinator::services::Services;
use hostkeeper_daemon::Daemon;
use hostkeeper_daemon::cli::{HostFiles, host_links};
use hostkeeper_daemon::links::{HostLink, coordinator_link};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, ClientIdentity, CoordinatorLink, Dispatch, Feature, Heartbeat, Job,
    LinkError, Platform, Renewal, TrustFault,
};
use hostkeeper_testkit::Scratch;
use hostkeeper_tls::TlsSource;
use http_body_util::{BodyExt, Full};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use support::{OWN, Rig, alerting, lines};

/// The operator token of the coordinator, for its status.
const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// How long the coordinator waits for a client, as its own default.
const PATIENCE: Duration = Duration::from_secs(10);

/// The node id of the host's certificate.
const HOST: &str = "d1";

/// The heartbeat period the tests give `host_links`; nothing waits for it.
const PERIOD: NonZeroU64 = NonZeroU64::new(60).expect("not zero");

/// A test certificate authority.
struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
    der: Vec<u8>,
    pem: String,
}

/// A certificate of a [`Ca`] and its key: the PEM of both, and the key as PKCS#8 DER, as
/// enrollment writes it into a generation of the pki directory.
struct Leaf {
    cert_pem: String,
    key_pem: String,
    key_der: Vec<u8>,
}

impl Ca {
    fn new(common_name: &str) -> Ca {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name = named(common_name);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let issuer = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("a CA key"))
            .expect("a CA certificate");
        Ca {
            der: issuer.der().to_vec(),
            pem: issuer.pem(),
            issuer,
        }
    }

    /// Its SHA-256 in hex, as the daemon config pins it.
    fn sha256_hex(&self) -> String {
        Sha256::digest(&self.der)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn leaf(&self, common_name: &str, names: Vec<SanType>, usage: ExtendedKeyUsagePurpose) -> Leaf {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.distinguished_name = named(common_name);
        params.subject_alt_names = names;
        params.extended_key_usages = vec![usage];
        // rcgen's default: a P-256 key, the only kind the pki directory holds.
        let key = KeyPair::generate().expect("a key");
        let cert = params.signed_by(&key, &self.issuer).expect("signed");
        Leaf {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            key_der: key.serialize_der(),
        }
    }

    /// The coordinator's certificate: for the loopback address, a server's only.
    fn coordinator(&self) -> Leaf {
        self.leaf(
            "coordinator",
            vec![SanType::IpAddress([127, 0, 0, 1].into())],
            ExtendedKeyUsagePurpose::ServerAuth,
        )
    }

    /// The certificate of the updater [`HOST`] on a host of role `daemon`, as the coordinator
    /// reads one by default.
    fn host(&self) -> Leaf {
        let uri = |text: &str| SanType::URI(text.to_owned().try_into().expect("a URI"));
        self.leaf(
            HOST,
            vec![
                uri("urn:hostkeeper:role:updater"),
                uri("urn:hostkeeper:host-role:daemon"),
                SanType::DnsName(HOST.to_owned().try_into().expect("a DNS name")),
            ],
            ExtendedKeyUsagePurpose::ClientAuth,
        )
    }
}

fn named(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

/// A coordinator that runs `web` on the role `daemon` and wants it at 2.0, over the files of
/// `dir`; its clock is past its quiet start, so it orders at once.
fn coordinator(dir: &Path) -> Arc<Coordinator> {
    std::fs::write(
        dir.join("desired.json"),
        format!(
            r#"{{"services":{{"web":{{"version":"2.0","min_up":{{}},"digests":{{"2.0":"{}"}}}}}}}}"#,
            "0a".repeat(32)
        ),
    )
    .expect("the desired state");
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

/// The coordinator's heartbeat listener on a loopback socket of port 0, served on a runtime of
/// the test's own for as long as it lives; `accepted` holds, for each connection it took, the
/// protocols its client offered ([`offered`]).
struct Listening {
    coordinator: Arc<Coordinator>,
    addr: SocketAddr,
    accepted: Arc<Mutex<Vec<Vec<String>>>>,
    runtime: tokio::runtime::Runtime,
    _dir: Scratch,
}

impl Listening {
    /// The coordinator of [`coordinator`], serving with a certificate of `ca` and asking for a
    /// client certificate of `ca`.
    fn new(name: &str, ca: &Ca) -> Listening {
        let dir = scratch(name);
        let coordinator = coordinator(dir.path());
        let leaf = ca.coordinator();
        let tls = TlsSource::fixed(
            "heartbeat",
            leaf.cert_pem.as_bytes(),
            leaf.key_pem.as_bytes(),
            Some(ca.pem.as_bytes()),
        )
        .expect("the listener's TLS");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        let tcp = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback socket");
        let addr = tcp.local_addr().expect("its address");
        let accepted = Arc::new(Mutex::new(Vec::new()));
        let counted = Arc::clone(&accepted);
        let tcp = Arc::new(tcp);
        let accept = move || {
            let tcp = Arc::clone(&tcp);
            let counted = Arc::clone(&counted);
            async move {
                let (stream, _) = tcp.accept().await?;
                let protocols = offered(&stream).await?;
                counted.lock().expect("the connections").push(protocols);
                Ok::<_, std::io::Error>(stream)
            }
        };
        let listener = Listener {
            accept,
            tls,
            surface: Surface::Heartbeat,
        };
        runtime.spawn(serve(
            listener,
            Arc::clone(&coordinator),
            PATIENCE,
            MAX_CONNECTIONS,
        ));
        Listening {
            coordinator,
            addr,
            accepted,
            runtime,
            _dir: dir,
        }
    }

    /// The connections the listener took so far: for each, the protocols its client offered.
    fn accepted(&self) -> Vec<Vec<String>> {
        self.accepted.lock().expect("the connections").clone()
    }

    /// The hosts the coordinator's status lists under `web`, as JSON.
    fn hosts_of_web(&self) -> Vec<Value> {
        let request = hyper::Request::builder()
            .method("GET")
            .uri("/v1/status")
            .header("authorization", format!("Bearer {OPERATOR}"))
            .body(Full::new(Bytes::new()))
            .expect("a request");
        let body = self.runtime.block_on(async {
            let response = handle(
                &self.coordinator,
                Surface::Control,
                &None,
                request,
                PATIENCE,
            )
            .await;
            assert_eq!(response.status().as_u16(), 200);
            response
                .into_body()
                .collect()
                .await
                .expect("a body")
                .to_bytes()
        });
        let status: Value = serde_json::from_slice(&body).expect("JSON");
        status["per_service"]["web"]["hosts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The daemon config of a host of this coordinator: its address, and `pinned`'s SHA-256.
    fn config(&self, pinned: &Ca) -> DaemonConfig {
        config(
            &format!("https://127.0.0.1:{}", self.addr.port()),
            &pinned.sha256_hex(),
        )
    }
}

/// The application protocols (ALPN) the client of `stream` offers in its TLS ClientHello, read
/// without taking the bytes off the stream, so the coordinator's handshake still reads them:
/// `["http/1.1"]` from the JSON link, `["h2"]` from the gRPC link. The hello is parsed by its
/// structure, so no byte of its random or key shares is mistaken for the extension.
async fn offered(stream: &tokio::net::TcpStream) -> std::io::Result<Vec<String>> {
    let mut buf = vec![0_u8; 16 * 1024];
    let hello = loop {
        stream.readable().await?;
        let n = stream.peek(&mut buf).await?;
        let whole = (n >= 5).then(|| 5 + usize::from(u16::from_be_bytes([buf[3], buf[4]])));
        match whole {
            Some(len) if n >= len => break &buf[..len],
            _ if n == 0 => return Ok(Vec::new()),
            _ => tokio::task::yield_now().await,
        }
    };
    Ok(alpn_of(hello).unwrap_or_default())
}

/// The ALPN protocol names of a TLS record holding a ClientHello, or `None` when it is not one
/// or has no ALPN extension.
fn alpn_of(record: &[u8]) -> Option<Vec<String>> {
    let take = |at: &mut usize, n: usize| -> Option<&[u8]> {
        let bytes = record.get(*at..*at + n)?;
        *at += n;
        Some(bytes)
    };
    let len16 = |bytes: &[u8]| usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
    // Record header (5), handshake header (4), legacy version (2), random (32).
    let mut at = 5 + 4 + 2 + 32;
    let session = usize::from(take(&mut at, 1)?[0]);
    take(&mut at, session)?;
    let suites = len16(take(&mut at, 2)?);
    take(&mut at, suites)?;
    let compression = usize::from(take(&mut at, 1)?[0]);
    take(&mut at, compression)?;
    let extensions = len16(take(&mut at, 2)?);
    let end = at + extensions;
    while at < end {
        let kind = len16(take(&mut at, 2)?);
        let len = len16(take(&mut at, 2)?);
        let data = take(&mut at, len)?;
        if kind == 0x0010 {
            let mut names = Vec::new();
            let mut i = 2;
            while i < data.len() {
                let n = usize::from(data[i]);
                names.push(String::from_utf8_lossy(data.get(i + 1..i + 1 + n)?).into_owned());
                i += 1 + n;
            }
            return Some(names);
        }
    }
    None
}

/// The protocols one connection of each link offers.
fn json() -> Vec<String> {
    vec!["http/1.1".to_owned()]
}

fn grpc() -> Vec<String> {
    vec!["h2".to_owned()]
}

/// A daemon config whose coordinator is `coordinator` and whose CA pin is `pin`.
fn config(coordinator: &str, pin: &str) -> DaemonConfig {
    let text = format!(
        "coordinator = \"{coordinator}\"\nca_sha256 = \"{pin}\"\n\
         release_channel = \"https://releases.example\"\n\n[trusted_signers]\n\
         organisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
         refs = [\"refs/tags/*\"]\nhosted_runner = true\n"
    );
    DaemonConfig::parse(&text).expect("a daemon config")
}

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// The host's files under `root`, its pki directory `root/pki`.
fn files_under(root: &Path) -> HostFiles {
    HostFiles {
        ca_bundle: root.join("ca.pem"),
        gh: root.join("gh"),
        trusted_root: root.join("trusted_root.json"),
        stores: root.join("stores.toml"),
        macos_pkg_signers: None,
        pki: root.join("pki"),
        console: None,
        managed_config: None,
    }
}

/// Writes a complete generation of the pki directory `pki`, as enrollment leaves it: `leaf`'s
/// certificate and key and `ca`'s bundle in `gen-1`, and `current` naming it.
fn enroll(pki: &Path, leaf: &Leaf, ca: &Ca) {
    let generation = pki.join("gen-1");
    std::fs::create_dir_all(&generation).expect("a generation");
    std::fs::write(generation.join("cert.pem"), &leaf.cert_pem).expect("the certificate");
    std::fs::write(generation.join("key"), &leaf.key_der).expect("the key");
    std::fs::write(generation.join("ca.pem"), &ca.pem).expect("the CA bundle");
    std::os::unix::fs::symlink("gen-1", pki.join("current")).expect("current");
}

/// A dispatcher that keeps each job for the test, which never runs it: the order a reply
/// carries is seen to reach the daemon, and nothing of it runs.
struct Kept(Mutex<Sender<Job>>);

impl Dispatch for Kept {
    fn start(&self, job: Job) {
        let sent = self.0.lock().expect("the sender").send(job);
        assert!(sent.is_ok(), "the test keeps every job");
    }
}

fn kept() -> (Arc<Kept>, Receiver<Job>) {
    let (sender, jobs) = mpsc::channel();
    (Arc::new(Kept(Mutex::new(sender))), jobs)
}

/// A heartbeat that names no service, as the loop sends one.
fn bare_beat() -> Heartbeat {
    Heartbeat {
        platform: Platform::LinuxX86_64,
        agent_version: hostkeeper_config::names::Version::from(OWN),
        features: ANNOUNCED.to_vec(),
        services: Vec::new(),
        sent_unix: 0,
        renewal: Renewal::default(),
        trusted_root: None,
        installers: Vec::new(),
    }
}

fn identity_of(leaf: &Leaf) -> ClientIdentity {
    ClientIdentity::new(
        leaf.cert_pem.as_bytes().to_vec(),
        leaf.key_pem.as_bytes().to_vec(),
    )
}

/// Catches a shipped daemon whose heartbeat never reaches its coordinator: the heartbeat loop of
/// `host_links`, on a host enrolled with a certificate of the pinned CA and running `web`, sends
/// its heartbeat to the coordinator the config names; the coordinator then lists the host
/// under `web` by the node id of its certificate, at the version the daemon has installed, with
/// the daemon's platform and its own version as the agent's; and the coordinator's update of
/// `web` to 2.0, which it sends only to a daemon that announced `order-v1`, comes back to the
/// daemon as an order. The first heartbeat goes over JSON (its connection offers `http/1.1`);
/// the reply advertised gRPC, so the next goes over gRPC (its connection offers `h2`) and is
/// answered there: a gRPC call that failed would have been sent again over JSON on a third
/// connection.
#[test]
fn the_shipped_heartbeat_reaches_the_coordinator_and_its_order_reaches_the_daemon() {
    let ca = Ca::new("hostkeeper test ca");
    let listening = Listening::new("shipped-link-coordinator", &ca);
    let rig = Rig::new("shipped-link-daemon");
    rig.add("web", "1.0");
    let daemon = Arc::new(Daemon::new(
        listening.config(&ca),
        rig.dirs(),
        rig.ports(),
        OWN,
    ));
    let files = files_under(rig.scratch.path());
    enroll(&files.pki, &ca.host(), &ca);
    let (dispatch, jobs) = kept();
    let links = hostkeeper_daemon::links::Links {
        dispatch,
        ..host_links(
            &daemon,
            &files,
            PERIOD,
            PlatformKind::Linux,
            Box::new(hostkeeper_daemon::NoWatchdog),
        )
    };
    let mut heartbeater = links.heartbeater(&daemon);

    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(
        listening.accepted(),
        [json()],
        "the first heartbeat goes over JSON"
    );
    let hosts = listening.hosts_of_web();
    assert_eq!(hosts.len(), 1, "{hosts:?}");
    let host = &hosts[0];
    assert_eq!(host["node_id"], HOST);
    assert_eq!(host["version"], support::BASE);
    // The daemon announces the platform it was built for (`links::PLATFORM`).
    let platform = if cfg!(target_os = "macos") {
        "darwin-arm64"
    } else {
        "linux-x86_64"
    };
    assert_eq!(host["platform"], platform);
    assert_eq!(host["agent"]["name"], "hostkeeper");
    assert_eq!(host["agent"]["version"], OWN.to_string());
    assert_eq!(host["holds_slot"], true, "the update of web is ordered");
    assert!(
        jobs.try_recv().is_ok(),
        "the coordinator's order, which it sends with its terms only to a daemon that \
         announced order-v1, is handed to the executor"
    );

    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(
        listening.accepted(),
        [json(), grpc()],
        "the next one over gRPC, answered there"
    );
    assert_eq!(listening.hosts_of_web().len(), 1, "still the one host");
}

/// Catches a link of the shipped daemon that answers without asking the coordinator (as the
/// link of a build without one did, `Transport(Unsupported)`), or that speaks gRPC before the
/// coordinator advertised it: the link of `host_links` answers the reply of the coordinator,
/// whose features are those it serves of the ones the heartbeat announced, `grpc-v1` among
/// them; and
/// `coordinator_link` starts on the JSON route and goes over to gRPC once that reply came.
#[test]
fn the_link_of_host_links_answers_the_coordinators_reply() {
    let ca = Ca::new("hostkeeper test ca");
    let listening = Listening::new("host-links-reply-coordinator", &ca);
    let rig = Rig::new("host-links-reply-daemon");
    let daemon = Arc::new(Daemon::new(
        listening.config(&ca),
        rig.dirs(),
        rig.ports(),
        OWN,
    ));
    let files = files_under(rig.scratch.path());
    enroll(&files.pki, &ca.host(), &ca);
    let links = host_links(
        &daemon,
        &files,
        PERIOD,
        PlatformKind::Linux,
        Box::new(hostkeeper_daemon::NoWatchdog),
    );

    let reply = links
        .link
        .heartbeat(&identity_of(&ca.host()), &bare_beat())
        .expect("the coordinator's reply");
    assert_eq!(
        reply.features,
        [
            Feature::DeferredV1,
            Feature::HaltV1,
            Feature::HeartbeatV2,
            Feature::ForcedV1,
            Feature::OrderV1,
            Feature::VerdictAcksV1,
            Feature::GrpcV1,
            Feature::TargetV1
        ],
        "the announced features the coordinator echoes: all but install-v1 and abort-v1"
    );

    let log = Arc::new(RecordingLog::default());
    let negotiated = coordinator_link(
        &listening.config(&ca),
        &files.pki,
        rig.clock.clone(),
        log.clone(),
    )
    .expect("a link");
    assert!(
        !negotiated.uses_grpc(),
        "nothing is known of the coordinator"
    );
    let identity = identity_of(&ca.host());
    assert!(negotiated.heartbeat(&identity, &bare_beat()).is_ok());
    assert!(negotiated.uses_grpc(), "the reply advertised grpc-v1");
    assert!(negotiated.heartbeat(&identity, &bare_beat()).is_ok());
    assert_eq!(
        listening.accepted(),
        [json(), json(), grpc()],
        "the link of host_links, then this one over JSON, then over gRPC"
    );
    assert_eq!(log.lines(), Vec::<String>::new());
}

/// Catches a link that trusts a coordinator its config does not pin: with the config pinning
/// another CA than the one the pki directory's bundle and the coordinator's certificate are
/// of, the heartbeat is not sent, as untrusted, and the coordinator lists no host.
#[test]
fn a_coordinator_whose_ca_is_not_the_pinned_one_hears_no_heartbeat() {
    let ca = Ca::new("hostkeeper test ca");
    let other = Ca::new("another ca");
    let listening = Listening::new("unpinned-coordinator", &ca);
    let rig = Rig::new("unpinned-daemon");
    rig.add("web", "1.0");
    let daemon = Arc::new(Daemon::new(
        listening.config(&other),
        rig.dirs(),
        rig.ports(),
        OWN,
    ));
    let files = files_under(rig.scratch.path());
    enroll(&files.pki, &ca.host(), &ca);
    let links = host_links(
        &daemon,
        &files,
        PERIOD,
        PlatformKind::Linux,
        Box::new(hostkeeper_daemon::NoWatchdog),
    );

    assert_eq!(
        links.link.heartbeat(&identity_of(&ca.host()), &bare_beat()),
        Err(LinkError::Untrusted(TrustFault::NoPinnedCa))
    );
    assert_eq!(
        listening.accepted(),
        Vec::<Vec<String>>::new(),
        "nothing is dialed"
    );
    assert_eq!(listening.hosts_of_web(), Vec::<Value>::new());
}

/// Catches a boot that fails, or a link that dials, on a host that has not enrolled: the
/// heartbeat loop of `host_links` over a pki directory with no generation says what it waits
/// for and sends nothing, and the coordinator takes no connection.
#[test]
fn a_host_not_enrolled_makes_no_link_and_dials_nothing() {
    let ca = Ca::new("hostkeeper test ca");
    let listening = Listening::new("not-enrolled-coordinator", &ca);
    let rig = Rig::new("not-enrolled-daemon");
    let (daemon, alerts) = alerting(Daemon::new(
        listening.config(&ca),
        rig.dirs(),
        rig.ports(),
        OWN,
    ));
    let daemon = Arc::new(daemon);
    let files = files_under(rig.scratch.path());
    let links = host_links(
        &daemon,
        &files,
        PERIOD,
        PlatformKind::Linux,
        Box::new(hostkeeper_daemon::NoWatchdog),
    );
    let mut heartbeater = links.heartbeater(&daemon);

    assert_eq!(heartbeater.beat_once(), BeatOutcome::NotEnrolled);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::NotEnrolled);
    assert_eq!(listening.accepted(), Vec::<Vec<String>>::new());
    assert_eq!(
        lines(&alerts),
        [format!(
            "hostkeeper daemon: not enrolled: waiting for {}",
            files.pki.join("current").display()
        )]
    );
}

/// Catches a link that cannot be made taken down with the daemon, or logged at every
/// heartbeat: a config whose coordinator address has a path, which the links do not dial,
/// leaves each heartbeat unsent as a transport error, and why is logged once.
#[test]
fn a_link_that_cannot_be_made_leaves_the_heartbeat_unsent_and_says_why_once() {
    let ca = Ca::new("hostkeeper test ca");
    let log = Arc::new(RecordingLog::default());
    let rig = Rig::new("unmade-link");
    let link = HostLink::new(
        config("https://coordinator.example/heartbeat", &ca.sha256_hex()),
        rig.scratch.path().join("pki"),
        rig.clock.clone(),
        log.clone(),
    );
    let identity = identity_of(&ca.host());
    for _ in 0..2 {
        assert_eq!(
            link.heartbeat(&identity, &bare_beat()),
            Err(LinkError::Transport(std::io::ErrorKind::Other))
        );
    }
    assert_eq!(
        log.lines(),
        [
            "error: no link to the coordinator: the coordinator address \
             \"https://coordinator.example/heartbeat\" has a path: the heartbeat goes to \
             /v1/heartbeat at its root"
        ]
    );
}
