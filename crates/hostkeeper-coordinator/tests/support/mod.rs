//! What the integration tests share: a certificate authority and the certificates it issues, an
//! in-memory network carrying real TLS, a client for the two listeners, and a running coordinator
//! behind them (`Farm`).
//!
//! Nothing here opens a socket. A "connection" is a `tokio::io::duplex` pair whose server end is
//! handed to the listener through a channel, so the TLS handshake and the peer identity are the
//! real ones: the certificate the in-memory client presents.

#![allow(
    dead_code,
    reason = "each test binary uses its own part of these shared helpers"
)]

pub mod database;
pub mod readers;
pub mod replay;
pub mod rig;
pub mod serve;

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use hostkeeper_coordinator::config::{Bound, Parts, run};
use hostkeeper_coordinator::desired::DesiredStore;
use hostkeeper_coordinator::ops::OpsStore;
use hostkeeper_coordinator::service::{Clock, Coordinator};
use hostkeeper_coordinator::services::Services;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, good_claims, keys, mint, static_verifier};
use hostkeeper_testkit::Scratch;
use hostkeeper_tls::{TlsFiles, TlsSource};
use hostkeeper_wire::identity::{SanPrefixes, UPDATER_ROLE};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;

/// The operator token of the tests.
pub const OPERATOR: &str = "operator-token-0123456789abcdef0123456789";

/// A heartbeat of a healthy host running 1.0.
pub const BEAT: &str = r#"{"platform":"linux-x86_64","version":"1.0","health":"healthy"}"#;

/// A desired state that moves the daemons one at a time.
pub const STATE: &str = r#"{"version":"2.0","max_updating":{"daemon":1},
    "min_up":{"storage":1,"frontend":1}}"#;

/// The directory of the recorded fixtures (data transcribed from kbf v0.0.64).
pub const RECORDED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/recorded");

/// The text of the recorded fixture `name`.
pub fn recorded(name: &str) -> String {
    std::fs::read_to_string(Path::new(RECORDED).join(name))
        .unwrap_or_else(|why| panic!("the fixture {name}: {why}"))
}

/// The bytes of the recorded fixture `name`.
pub fn recorded_bytes(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(RECORDED).join(name))
        .unwrap_or_else(|why| panic!("the fixture {name}: {why}"))
}

/// A new scratch directory for the test `name`, under the target directory.
pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// The clock of a coordinator that reads `seconds`.
pub fn clock_of(seconds: &Arc<AtomicU64>) -> Clock {
    let seconds = Arc::clone(seconds);
    Arc::new(move || Duration::from_secs(seconds.load(Ordering::SeqCst)))
}

/// A token CI would send: valid for the allow-list of the testkit, signed by its first key,
/// with a `jti` no other token has.
pub fn ci_token() -> String {
    mint(&keys()[0], &good_claims(STATIC_ISSUER))
}

/// The flags kbf's coordinator was run with, as recorded in `flags.json`.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flags {
    pub legacy_service: Option<String>,
    pub role_uri_prefix: String,
    pub host_role_uri_prefix: String,
}

impl Flags {
    /// The recorded flags.
    pub fn recorded() -> Self {
        serde_json::from_str(&recorded("flags.json")).expect("flags.json")
    }

    /// The certificate namespaces the flags name.
    pub fn prefixes(&self) -> SanPrefixes {
        SanPrefixes::new(&self.role_uri_prefix, &self.host_role_uri_prefix).expect("prefixes")
    }
}

/// The namespaces kbf's certificates carry.
pub fn kbf_prefixes() -> SanPrefixes {
    Flags::recorded().prefixes()
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn uri(text: String) -> SanType {
    SanType::URI(text.try_into().expect("a URI"))
}

fn dns(text: &str) -> SanType {
    SanType::DnsName(text.to_owned().try_into().expect("a DNS name"))
}

/// The DER of a self-signed client certificate with these names (`dns:` marks a DNS name, any
/// other is a URI).
pub fn self_signed(names: &[&str]) -> Vec<u8> {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params.subject_alt_names = names
        .iter()
        .map(|name| match name.strip_prefix("dns:") {
            Some(host) => dns(host),
            None => uri((*name).to_owned()),
        })
        .collect();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("a key");
    params.self_signed(&key).expect("signed").der().to_vec()
}

/// The DER of a self-signed certificate of the updater `host` whose role is `role`, in the
/// namespaces `prefixes`.
pub fn updater_der(host: &str, role: &str, prefixes: &SanPrefixes) -> Vec<u8> {
    self_signed(&[
        &format!("{}{UPDATER_ROLE}", prefixes.role()),
        &format!("{}{role}", prefixes.host_role()),
        &format!("dns:{host}"),
    ])
}

/// A certificate and the key it was issued for.
#[derive(Clone)]
pub struct Leaf {
    pub der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub cert_pem: String,
    pub key_pem: String,
}

/// A certificate authority.
pub struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
    pub der: Vec<u8>,
    pub pem: String,
}

fn named(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

impl Ca {
    pub fn new(common_name: &str) -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name = named(common_name);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let issuer = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("a CA key"))
            .expect("a CA certificate");
        Self {
            der: issuer.der().to_vec(),
            pem: issuer.pem(),
            issuer,
        }
    }

    fn leaf(&self, common_name: &str, names: Vec<SanType>, usage: ExtendedKeyUsagePurpose) -> Leaf {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.distinguished_name = named(common_name);
        params.subject_alt_names = names;
        params.extended_key_usages = vec![usage];
        let key = KeyPair::generate().expect("a key");
        let cert = params.signed_by(&key, &self.issuer).expect("signed");
        Leaf {
            der: cert.der().to_vec(),
            key_der: key.serialize_der(),
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        }
    }

    /// The coordinator's certificate: for `localhost`, to be used as a server's only.
    pub fn coordinator(&self) -> Leaf {
        self.leaf(
            "coordinator",
            vec![dns("localhost")],
            ExtendedKeyUsagePurpose::ServerAuth,
        )
    }

    /// The certificate of the updater `host` on a host whose role is `role`.
    pub fn updater(&self, host: &str, role: &str, prefixes: &SanPrefixes) -> Leaf {
        self.leaf(
            host,
            vec![
                uri(format!("{}{UPDATER_ROLE}", prefixes.role())),
                uri(format!("{}{role}", prefixes.host_role())),
                dns(host),
            ],
            ExtendedKeyUsagePurpose::ClientAuth,
        )
    }

    /// A client certificate of this authority that names a role other than the updater's: a
    /// storage node's own, say. It passes TLS and is no updater's.
    pub fn node(&self, host: &str, role: &str, prefixes: &SanPrefixes) -> Leaf {
        self.leaf(
            host,
            vec![
                uri(format!("{}{role}", prefixes.role())),
                uri(format!("{}{role}", prefixes.host_role())),
                dns(host),
            ],
            ExtendedKeyUsagePurpose::ClientAuth,
        )
    }
}

/// How a client presents itself: it trusts `ca`, and presents `identity` if given.
pub fn client_config(ca: &Ca, identity: Option<&Leaf>) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca.der.clone()))
        .expect("a root");
    let builder = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots);
    let config = match identity {
        Some(leaf) => builder
            .with_client_auth_cert(
                vec![CertificateDer::from(leaf.der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf.key_der.clone())),
            )
            .expect("a client certificate"),
        None => builder.with_no_client_auth(),
    };
    Arc::new(config)
}

/// The listeners of an in-memory network: connecting to an address hands the server end of a new
/// duplex pair to whoever listens there.
#[derive(Clone, Default)]
pub struct Net(Arc<Mutex<HashMap<SocketAddr, mpsc::UnboundedSender<DuplexStream>>>>);

impl Net {
    /// Starts listening at `addr`: the connections made to it arrive on the receiver.
    pub fn listen(&self, addr: SocketAddr) -> mpsc::UnboundedReceiver<DuplexStream> {
        let (send, receive) = mpsc::unbounded_channel();
        self.0.lock().expect("the registry").insert(addr, send);
        receive
    }

    /// The client end of a new connection to `addr`.
    pub fn connect(&self, addr: SocketAddr) -> io::Result<DuplexStream> {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let listener = self
            .0
            .lock()
            .expect("the registry")
            .get(&addr)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionRefused))?;
        listener
            .send(server)
            .map_err(|_| io::Error::from(io::ErrorKind::ConnectionReset))?;
        Ok(client)
    }
}

/// The accept function of a listener fed by `receive`: each call waits for the next connection.
pub fn accepting(
    receive: mpsc::UnboundedReceiver<DuplexStream>,
) -> impl FnMut() -> BoxFuture<'static, io::Result<DuplexStream>> + Send {
    let receive = Arc::new(tokio::sync::Mutex::new(receive));
    move || {
        let receive = Arc::clone(&receive);
        Box::pin(async move {
            receive
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
        })
    }
}

/// One HTTP/1.1 exchange over TLS: the status and the body, or why there was none.
pub async fn exchange(
    net: &Net,
    addr: SocketAddr,
    config: &Arc<ClientConfig>,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: &str,
) -> io::Result<(u16, String)> {
    let stream = net.connect(addr)?;
    let name = ServerName::try_from("localhost").expect("a name");
    let mut tls = TlsConnector::from(Arc::clone(config))
        .connect(name, stream)
        .await?;
    let auth = bearer.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tls.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    tls.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| io::Error::other(format!("no HTTP answer: {text:?}")))?;
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_owned();
    Ok((status, body))
}

/// Where the heartbeat listener of a `Farm` is on its network.
pub const HEARTBEAT_ADDR: &str = "192.0.2.1:1";

/// Where the control listener of a `Farm` is on its network.
pub const CONTROL_ADDR: &str = "192.0.2.1:2";

/// A coordinator serving behind the in-memory network, on a clock the test sets.
pub struct Farm {
    pub ca: Arc<Ca>,
    pub net: Net,
    pub bound: Bound,
    pub clock: Arc<AtomicU64>,
    pub dir: Arc<Scratch>,
    pub coordinator: Arc<Coordinator>,
    pub tls: (TlsSource, TlsSource),
    pub prefixes: SanPrefixes,
    task: JoinHandle<()>,
}

impl Farm {
    pub async fn start(name: &str) -> Self {
        let ca = Arc::new(Ca::new("farm CA"));
        let dir = Arc::new(scratch(name));
        Self::start_on(ca, dir, 1_000).await
    }

    /// A coordinator that starts now (clock `at`) over the CA and the directory given: what a
    /// restart makes. It runs `config::run` over the in-memory network.
    pub async fn start_on(ca: Arc<Ca>, dir: Arc<Scratch>, at: u64) -> Self {
        let flags = Flags::recorded();
        let prefixes = flags.prefixes();
        let clock = Arc::new(AtomicU64::new(at));
        let store = DesiredStore::open(
            &dir.path().join("desired.json"),
            flags.legacy_service.as_deref(),
        )
        .expect("a store");
        let ops = OpsStore::open(&dir.path().join("desired.json.ops")).expect("an ops store");
        let (verifier, _) = static_verifier();
        let coordinator = Arc::new(
            Coordinator::new(store, verifier, OPERATOR.to_owned(), clock_of(&clock))
                .with_services(
                    Services::parse(&recorded("kbf.services.toml")).expect("kbf's definition"),
                )
                .with_legacy_service(flags.legacy_service.clone())
                .with_prefixes(prefixes.clone())
                .with_ops(ops),
        );
        let leaf = ca.coordinator();
        let write = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).expect("written");
            path
        };
        let files = TlsFiles {
            cert: write("coordinator.pem", &leaf.cert_pem),
            key: write("coordinator.key", &leaf.key_pem),
            client_ca: None,
        };
        let mut with_ca = files.clone();
        with_ca.client_ca = Some(write("ca.pem", &ca.pem));
        let reload = Duration::from_secs(3_600);
        let heartbeat_tls =
            TlsSource::files("heartbeat", with_ca, reload).expect("a heartbeat source");
        let control_tls = TlsSource::files("control", files, reload).expect("a control source");
        let parts = Parts {
            coordinator: Arc::clone(&coordinator),
            heartbeat_tls: heartbeat_tls.clone(),
            control_tls: control_tls.clone(),
            heartbeat_listen: HEARTBEAT_ADDR.parse().expect("an address"),
            control_listen: CONTROL_ADDR.parse().expect("an address"),
            lan_listen: None,
            reload,
            lock: hostkeeper_coordinator::lock::LockGuard::new(()),
        };
        let net = Net::default();
        let registry = net.clone();
        let bind = move |addr: SocketAddr| {
            let receive = registry.listen(addr);
            async move { Ok::<_, io::Error>((addr, accepting(receive))) }
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = run(parts, bind, move |bound| {
                let _ = tx.send(bound);
            })
            .await;
        });
        let bound = rx.await.expect("bound");
        Self {
            ca,
            net,
            bound,
            clock,
            dir,
            coordinator,
            tls: (heartbeat_tls, control_tls),
            prefixes,
            task,
        }
    }

    pub fn at(&self, seconds: u64) {
        self.clock.store(seconds, Ordering::SeqCst);
    }

    /// Stops serving: what a crash leaves.
    pub fn stop(self) -> (Arc<Ca>, Arc<Scratch>) {
        self.task.abort();
        (self.ca, self.dir)
    }

    /// An updater's certificate for this farm.
    pub fn updater(&self, host: &str, role: &str) -> Leaf {
        self.ca.updater(host, role, &self.prefixes)
    }

    pub async fn beat(&self, who: &Leaf, body: &str) -> io::Result<(u16, String)> {
        let config = client_config(&self.ca, Some(who));
        exchange(
            &self.net,
            self.bound.heartbeat,
            &config,
            "POST",
            "/v1/heartbeat",
            None,
            body,
        )
        .await
    }

    pub async fn control(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: &str,
    ) -> io::Result<(u16, String)> {
        let config = client_config(&self.ca, None);
        exchange(
            &self.net,
            self.bound.control,
            &config,
            method,
            path,
            bearer,
            body,
        )
        .await
    }

    pub async fn status(&self) -> serde_json::Value {
        let (code, body) = self
            .control("GET", "/v1/status", Some(OPERATOR), "")
            .await
            .expect("answered");
        assert_eq!(code, 200, "{body}");
        serde_json::from_str(&body).expect("JSON")
    }
}
