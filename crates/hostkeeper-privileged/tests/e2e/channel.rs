//! A release channel on the loopback interface: the HTTPS server the daemon fetches packages
//! from during the privileged end-to-end tests.
//!
//! The real channel is a static file tree behind HTTPS,
//! `{channel}/{service}/{version}/index.json` beside the package and its Sigstore bundle. The
//! rig needs the same tree without leaving the machine and without a public certificate, so
//! [`ReleaseChannel`] makes its own throwaway CA and a server certificate for `127.0.0.1`
//! (the test hands the CA to the host's CA bundle, see [`ReleaseChannel::ca_pem`]), and serves
//! whatever the test published from memory over rustls, on plain `std` threads.
//!
//! What the tests use it for beyond serving:
//!
//! - a request log ([`ReleaseChannel::requests`]), including requests answered 404, so a test
//!   can assert which paths the daemon fetched and how often (for example that a refused
//!   version's archive was fetched once and nothing else was);
//! - a switch ([`ReleaseChannel::tamper`]) that makes one version's archive differ from the
//!   digest its index states, which the daemon must refuse;
//! - an outage ([`ReleaseChannel::stop`] and [`ReleaseChannel::start_again`]): the listener
//!   goes away so connects are refused, and comes back on a new port with the same content.
//!
//! The server speaks HTTP/1.1 with `Connection: close`, one request per connection, and ends
//! each connection with a TLS close notification so the client sees a clean end of body. The
//! client (`ureq` with rustls, see `hostkeeper-package`'s `net.rs`) needs no ALPN.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};

/// The most bytes of a request head the server reads before it gives up on the connection.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// How long a connection may sit idle before the server drops it. A bound for a client that
/// never speaks, not a timing the tests assert on.
const IDLE: Duration = Duration::from_secs(30);

/// The path of a version's archive on the channel: `/<service>/<version>/<file_name>`.
pub fn archive_path(service: &str, version: &str, file_name: &str) -> String {
    format!("/{service}/{version}/{file_name}")
}

/// What the channel serves and what it has been asked, shared by the accept thread, the
/// connection threads and the test.
#[derive(Default)]
struct State {
    /// Path to body.
    objects: BTreeMap<String, Vec<u8>>,
    /// `(service, version)` to the path of that version's archive, for [`ReleaseChannel::tamper`].
    archives: BTreeMap<(String, String), String>,
    /// Path to the number of requests for it, whatever the answer was.
    requests: BTreeMap<String, u64>,
}

type Shared = Arc<Mutex<State>>;

fn locked(state: &Shared) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A throwaway certificate authority and the certificate it issued for `127.0.0.1`.
struct Identity {
    ca_pem: String,
    ca_der: Vec<u8>,
    config: Arc<ServerConfig>,
}

fn named(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

impl Identity {
    /// A fresh CA, and a server certificate it signed: IP SAN `127.0.0.1`, extended key usage
    /// server authentication, on a fresh P-256 key.
    fn new() -> Identity {
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name = named("hostkeeper e2e release channel CA");
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().expect("a CA key"))
            .expect("a CA certificate");

        let mut leaf_params = CertificateParams::new(Vec::<String>::new()).expect("leaf params");
        leaf_params.distinguished_name = named("hostkeeper e2e release channel");
        leaf_params.subject_alt_names = vec![SanType::IpAddress(Ipv4Addr::LOCALHOST.into())];
        leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf_key = KeyPair::generate().expect("a server key");
        let leaf = leaf_params
            .signed_by(&leaf_key, &*ca)
            .expect("a server certificate");

        let chain = vec![CertificateDer::from(leaf.der().to_vec())];
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("protocol versions")
                .with_no_client_auth()
                .with_single_cert(chain, key)
                .expect("a server certificate that matches its key");
        Identity {
            ca_pem: ca.pem(),
            ca_der: ca.der().to_vec(),
            config: Arc::new(config),
        }
    }
}

/// One listener with its accept thread; [`ReleaseChannel::stop`] ends it.
struct Listener {
    port: u16,
    stop: Arc<AtomicBool>,
    accept: JoinHandle<()>,
}

impl Listener {
    /// Binds `127.0.0.1:0` and accepts connections on a thread, one thread per connection.
    fn start(config: Arc<ServerConfig>, state: Shared) -> Listener {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let accept = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let (config, state) = (Arc::clone(&config), Arc::clone(&state));
                handlers.push(std::thread::spawn(move || {
                    // A client that gives up mid-handshake or mid-head ends only its connection.
                    drop(serve(stream, config, &state));
                }));
            }
            // The listener drops here, so later connects are refused.
            for handler in handlers {
                drop(handler.join());
            }
        });
        Listener { port, stop, accept }
    }

    /// Stops accepting, waits for every connection thread, and closes the listening socket.
    fn finish(self) {
        self.stop.store(true, Ordering::SeqCst);
        // `accept` blocks until a connection arrives; this one is only to wake it.
        drop(TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)));
        drop(self.accept.join());
    }
}

/// The request target of a `GET` request head, without any query, or none for any other head.
fn target_of(head: &str) -> Option<&str> {
    let mut words = head.lines().next()?.split_whitespace();
    if words.next()? != "GET" {
        return None;
    }
    let target = words.next()?;
    Some(target.split('?').next().unwrap_or(target))
}

/// Serves one connection: the handshake, one request head, the answer, a clean close.
fn serve(stream: TcpStream, config: Arc<ServerConfig>, state: &Shared) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_write_timeout(Some(IDLE))?;
    let connection = ServerConnection::new(config).map_err(io::Error::other)?;
    let mut tls = StreamOwned::new(connection, stream);

    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if head.len() > MAX_HEAD_BYTES {
            return Err(io::Error::other("request head too long"));
        }
        let read = tls.read(&mut chunk)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&chunk[..read]);
    }
    let head = String::from_utf8_lossy(&head);
    let path = target_of(&head).map(str::to_owned);

    let body = path.as_ref().and_then(|path| {
        let mut state = locked(state);
        *state.requests.entry(path.clone()).or_default() += 1;
        state.objects.get(path).cloned()
    });
    let response = match body {
        Some(body) => {
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                 Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            response.extend_from_slice(&body);
            response
        }
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .as_bytes()
            .to_vec(),
    };
    // A client that stops reading loses its answer; the log already has its request.
    drop(tls.write_all(&response));
    tls.conn.send_close_notify();
    drop(tls.flush());
    Ok(())
}

/// The release index of one version, in the strict form `hostkeeper-package` reads: keys in
/// order, no whitespace, one package.
fn index_json(service: &str, version: &str, file_name: &str, archive: &[u8]) -> String {
    let platform = hostkeeper_config::platform::Platform::current().as_str();
    let sha256 = hex::encode(Sha256::digest(archive));
    format!(
        "{{\"packages\":[{{\"bytes\":{},\"file\":\"{file_name}\",\"platform\":\"{platform}\",\
         \"sha256\":\"{sha256}\"}}],\"schema\":1,\"service\":\"{service}\",\
         \"version\":\"{version}\"}}",
        archive.len()
    )
}

/// An HTTPS release channel on `127.0.0.1`, with a request log, a tamper switch and an outage.
pub struct ReleaseChannel {
    identity: Identity,
    state: Shared,
    listener: Option<Listener>,
    /// The port of the last listener, kept so [`ReleaseChannel::reachable`] can see an outage.
    port: u16,
}

impl ReleaseChannel {
    /// Makes the CA and the server certificate and starts serving on a free loopback port.
    /// Nothing is published yet: every path answers 404.
    pub fn start() -> ReleaseChannel {
        let identity = Identity::new();
        let state = Shared::default();
        let listener = Listener::start(Arc::clone(&identity.config), Arc::clone(&state));
        ReleaseChannel {
            identity,
            state,
            port: listener.port,
            listener: Some(listener),
        }
    }

    /// The PEM of the CA certificate: what the host's CA bundle gets so the daemon trusts this
    /// channel.
    pub fn ca_pem(&self) -> String {
        self.identity.ca_pem.clone()
    }

    /// The DER of the CA certificate.
    pub fn ca_der(&self) -> Vec<u8> {
        self.identity.ca_der.clone()
    }

    /// The port the channel listens on (or last listened on, after [`ReleaseChannel::stop`]).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `https://127.0.0.1:<port>`, the channel's base URL for the daemon's configuration.
    pub fn base_url(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }

    /// Publishes one version of `service`: its `index.json` (stating the size and SHA-256 of
    /// `archive`), the archive as `file_name`, and `bundle` beside it as
    /// `<file_name>.sigstore.json`. Publishing a version again replaces it.
    pub fn publish(
        &self,
        service: &str,
        version: &str,
        file_name: &str,
        archive: Vec<u8>,
        bundle: Vec<u8>,
    ) {
        let index = index_json(service, version, file_name, &archive);
        let archive_at = archive_path(service, version, file_name);
        let mut state = locked(&self.state);
        state.objects.insert(
            format!("/{service}/{version}/index.json"),
            index.into_bytes(),
        );
        state
            .objects
            .insert(format!("{archive_at}.sigstore.json"), bundle);
        state.objects.insert(archive_at.clone(), archive);
        state
            .archives
            .insert((service.to_owned(), version.to_owned()), archive_at);
    }

    /// The switch: from now on the archive of `version` differs from the digest its index
    /// states (its first byte is flipped), so a client that checks the digest must refuse it.
    ///
    /// # Panics
    ///
    /// When that version was not published, or its archive is empty.
    pub fn tamper(&self, service: &str, version: &str) {
        let mut state = locked(&self.state);
        let at = state
            .archives
            .get(&(service.to_owned(), version.to_owned()))
            .cloned()
            .unwrap_or_else(|| panic!("tamper: {service} {version} was not published"));
        let body = state.objects.get_mut(&at).expect("a published archive");
        let first = body.first_mut().expect("tamper: the archive is empty");
        *first ^= 0xff;
    }

    /// Every path requested so far with how many times, 404s included, across outages.
    pub fn requests(&self) -> BTreeMap<String, u64> {
        locked(&self.state).requests.clone()
    }

    /// How many times `path` was requested.
    pub fn count(&self, path: &str) -> u64 {
        locked(&self.state)
            .requests
            .get(path)
            .copied()
            .unwrap_or_default()
    }

    /// How many requests the channel has had in all.
    pub fn total(&self) -> u64 {
        locked(&self.state).requests.values().sum()
    }

    /// Starts an outage: closes the listener and joins the accept thread, so every
    /// connection is refused afterwards. Safe to call again.
    pub fn stop(&mut self) {
        if let Some(listener) = self.listener.take() {
            listener.finish();
        }
    }

    /// Serves again on a new ephemeral port ([`ReleaseChannel::port`] and
    /// [`ReleaseChannel::base_url`] change), with the same content and the same request log.
    /// Stops the current listener first if there is one.
    pub fn start_again(&mut self) {
        self.stop();
        let listener = Listener::start(Arc::clone(&self.identity.config), Arc::clone(&self.state));
        self.port = listener.port;
        self.listener = Some(listener);
    }

    /// Whether a TCP connection to the channel's port succeeds.
    pub fn reachable(&self) -> bool {
        TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)), IDLE)
            .is_ok()
    }
}

impl Drop for ReleaseChannel {
    fn drop(&mut self) {
        self.stop();
    }
}
