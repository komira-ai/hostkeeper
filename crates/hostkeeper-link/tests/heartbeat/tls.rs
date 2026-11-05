//! What the link's tests share: a test PKI made at run time (no key is committed), a pki
//! directory holding a CA bundle, an in-process transport whose connections are pipes, and a TLS
//! server on the other end of a pipe that reads one request and answers it. The server's TLS is
//! real: it requires a client certificate of its CA, and hands the certificate the client
//! presented to its answer, so the peer identity is explicit. Nothing here opens a socket.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_link::{ClientIdentity, Conn, JsonMtlsLink, Transport};
use hostkeeper_testkit::Scratch;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};

/// The coordinator's name in the tests' daemon config, and in its certificate.
pub const COORDINATOR: &str = "coordinator.test";

/// The certificate prefixes the coordinator reads by default.
pub const UPDATER_ROLE_URI: &str = "urn:hostkeeper:role:updater";

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn dns(name: &str) -> SanType {
    SanType::DnsName(name.to_owned().try_into().expect("a DNS name"))
}

fn uri(text: &str) -> SanType {
    SanType::URI(text.to_owned().try_into().expect("a URI"))
}

fn named(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

/// A certificate and its key, PEM.
#[derive(Clone)]
pub struct Leaf {
    pub cert_pem: String,
    pub key_pem: String,
}

impl Leaf {
    /// The pair as the certificate source gives it.
    pub fn identity(&self) -> ClientIdentity {
        ClientIdentity::new(
            self.cert_pem.as_bytes().to_vec(),
            self.key_pem.as_bytes().to_vec(),
        )
    }
}

/// A test certificate authority.
pub struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
    pub der: Vec<u8>,
    pub pem: String,
}

impl Ca {
    pub fn new(common_name: &str) -> Ca {
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

    /// Its SHA-256, as the daemon config pins it.
    pub fn sha256_hex(&self) -> String {
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
        let key = KeyPair::generate().expect("a key");
        let cert = params.signed_by(&key, &self.issuer).expect("signed");
        Leaf {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        }
    }

    /// A server certificate for `name`.
    pub fn server(&self, name: &str) -> Leaf {
        self.leaf(
            "coordinator",
            vec![dns(name)],
            ExtendedKeyUsagePurpose::ServerAuth,
        )
    }

    /// The client certificate of the updater `host` on a host of role `role`, as the
    /// coordinator reads one by default.
    pub fn host(&self, host: &str, role: &str) -> Leaf {
        self.leaf(
            host,
            vec![
                uri(UPDATER_ROLE_URI),
                uri(&format!("urn:hostkeeper:host-role:{role}")),
                dns(host),
            ],
            ExtendedKeyUsagePurpose::ClientAuth,
        )
    }

    /// A client certificate for the coordinator's own name: a host's certificate, which the CA
    /// makes for `clientAuth` only, presented by what answers in the coordinator's place.
    pub fn client_for_coordinator_name(&self) -> Leaf {
        self.leaf(
            "impostor",
            vec![dns(COORDINATOR)],
            ExtendedKeyUsagePurpose::ClientAuth,
        )
    }
}

/// The text of a daemon config whose coordinator is `coordinator` and whose CA pin is `pin`.
pub fn config_text(coordinator: &str, pin: &str) -> String {
    format!(
        "coordinator = \"{coordinator}\"\nca_sha256 = \"{pin}\"\n\
         release_channel = \"https://releases.test\"\n\n[trusted_signers]\n\
         organisation = \"komira-ai\"\nworkflow = \".github/workflows/release.yml\"\n\
         refs = [\"refs/tags/*\"]\nhosted_runner = true\n"
    )
}

/// The daemon config of the tests: the coordinator at `https://coordinator.test:8443`, the CA
/// `pinned`.
pub fn config(pinned: &Ca) -> DaemonConfig {
    let text = config_text(&format!("https://{COORDINATOR}:8443"), &pinned.sha256_hex());
    DaemonConfig::parse(&text).expect("a daemon config")
}

/// A pki directory whose current generation holds a CA bundle.
pub struct Pki {
    scratch: Scratch,
}

impl Pki {
    /// The pki directory of the test `name`, whose CA bundle is `bundle`.
    pub fn with_bundle(name: &str, bundle: &str) -> Pki {
        let pki = Pki::empty(name);
        let generation = pki.root().join("gen-1");
        std::fs::create_dir_all(&generation).expect("a generation");
        std::fs::write(generation.join("ca.pem"), bundle).expect("the CA bundle");
        std::os::unix::fs::symlink("gen-1", pki.root().join("current")).expect("current");
        pki
    }

    /// A pki directory with nothing in it.
    pub fn empty(name: &str) -> Pki {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let scratch = Scratch::under(base, name).expect("a scratch directory");
        std::fs::create_dir_all(scratch.path().join("pki")).expect("the pki directory");
        Pki { scratch }
    }

    pub fn root(&self) -> PathBuf {
        self.scratch.path().join("pki")
    }

    /// Writes generation `number` with the CA bundle `bundle` and switches `current` to it by
    /// rename, as enrollment does.
    pub fn switch_to(&self, number: u64, bundle: &str) {
        let generation = self.root().join(format!("gen-{number}"));
        std::fs::create_dir_all(&generation).expect("a generation");
        std::fs::write(generation.join("ca.pem"), bundle).expect("the CA bundle");
        let staged = self.root().join("current.tmp");
        std::os::unix::fs::symlink(format!("gen-{number}"), &staged).expect("a staged link");
        std::fs::rename(&staged, self.root().join("current")).expect("current switched");
    }
}

/// One end of an in-process connection. A write sends its bytes to the other end, and is lost
/// once the other end is gone (as bytes written to a socket whose peer closed are, until the
/// reset); a read waits for the other end's bytes, and reads the end of the stream once the
/// other end closed. So which of two threads finishes first decides nothing a test sees.
pub struct End {
    send: Option<Sender<Vec<u8>>>,
    receive: Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
}

/// The two ends of a new connection.
pub fn pipe() -> (End, End) {
    let (to_b, from_a) = mpsc::channel();
    let (to_a, from_b) = mpsc::channel();
    let a = End {
        send: Some(to_b),
        receive: from_b,
        pending: VecDeque::new(),
    };
    let b = End {
        send: Some(to_a),
        receive: from_a,
        pending: VecDeque::new(),
    };
    (a, b)
}

impl End {
    /// Closes the sending half: the other end reads the end of the stream.
    pub fn close(&mut self) {
        self.send = None;
    }
}

impl Read for End {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            match self.receive.recv() {
                Ok(chunk) => self.pending.extend(chunk),
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len());
        for (slot, byte) in buf.iter_mut().zip(self.pending.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

impl Write for End {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(send) = &self.send {
            let _ = send.send(buf.to_vec());
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The in-process transport: each connect takes the next client end queued, and records the
/// host and port it was asked for. With none queued, the connection is refused.
#[derive(Default)]
pub struct PipeTransport {
    ends: Mutex<VecDeque<End>>,
    dialed: Mutex<Vec<(String, u16)>>,
}

impl PipeTransport {
    pub fn queue(&self, end: End) {
        self.ends.lock().expect("the ends").push_back(end);
    }

    /// The hosts and ports dialed, in order.
    pub fn dialed(&self) -> Vec<(String, u16)> {
        self.dialed.lock().expect("the dialed").clone()
    }
}

impl Transport for PipeTransport {
    fn connect(&self, host: &str, port: u16) -> io::Result<Box<dyn Conn>> {
        self.dialed
            .lock()
            .expect("the dialed")
            .push((host.to_owned(), port));
        let end = self.ends.lock().expect("the ends").pop_front();
        end.map(|end| Box::new(end) as Box<dyn Conn>)
            .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionRefused))
    }
}

/// The TLS configuration of a server presenting `leaf` and requiring a client certificate of
/// `client_ca`.
pub fn server_config(leaf: &Leaf, client_ca: &Ca) -> Arc<ServerConfig> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(client_ca.der.clone()))
        .expect("a client CA");
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider())
        .build()
        .expect("a client verifier");
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(leaf.cert_pem.as_bytes())
            .collect::<Result<_, _>>()
            .expect("the server chain");
    let key = PrivateKeyDer::from_pem_slice(leaf.key_pem.as_bytes()).expect("the server key");
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)
        .expect("a server configuration");
    Arc::new(config)
}

/// One request the server read.
#[derive(Clone, Debug)]
pub struct Request {
    /// The request line and headers, without the blank line.
    pub head: String,
    pub body: Vec<u8>,
    /// The leaf certificate the client presented, DER.
    pub peer: Vec<u8>,
}

/// How the server ends the connection after its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Close {
    /// With the TLS close.
    Notify,
    /// By dropping the connection, with no TLS close.
    Drop,
}

/// Starts a server on the other end of a new connection, which it queues on `transport`: it
/// completes the TLS handshake, reads one request (its head and a body of its
/// `Content-Length`), writes what `answer` makes of it, and closes as `close` says. The
/// handle gives the request, or none when the handshake or the request failed.
pub fn serve(
    transport: &PipeTransport,
    config: Arc<ServerConfig>,
    close: Close,
    answer: impl FnOnce(&Request) -> Vec<u8> + Send + 'static,
) -> JoinHandle<Option<Request>> {
    let (client, server) = pipe();
    transport.queue(client);
    std::thread::spawn(move || {
        let connection = ServerConnection::new(config).expect("a server connection");
        let mut tls = StreamOwned::new(connection, server);
        let request = read_request(&mut tls)?;
        let reply = answer(&request);
        // A client that stopped reading leaves the rest unwritten.
        let _ = tls.write_all(&reply);
        if close == Close::Notify {
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
        tls.sock.close();
        Some(request)
    })
}

/// One request, read from `tls`: none when the stream ends or fails first.
fn read_request(tls: &mut StreamOwned<ServerConnection, End>) -> Option<Request> {
    let mut raw = Vec::new();
    let mut byte = [0_u8; 1];
    while !raw.ends_with(b"\r\n\r\n") {
        match tls.read(&mut byte) {
            Ok(1) => raw.push(byte[0]),
            _ => return None,
        }
    }
    let head = String::from_utf8(raw[..raw.len() - 4].to_vec()).ok()?;
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))?
        .parse()
        .ok()?;
    let mut body = vec![0_u8; length];
    tls.read_exact(&mut body).ok()?;
    let peer = tls.conn.peer_certificates()?.first()?.as_ref().to_vec();
    Some(Request { head, body, peer })
}

/// An HTTP/1.1 answer of `status` with `body` and its length.
pub fn http(status: u16, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// The PKI of most tests: the pinned CA, the coordinator's certificate for its name, and the
/// certificate of the updater `d1` on a host of role `daemon`.
pub struct Farm {
    pub ca: Ca,
    pub coordinator: Leaf,
    pub host: Leaf,
}

impl Farm {
    pub fn new() -> Farm {
        let ca = Ca::new("hostkeeper test ca");
        let coordinator = ca.server(COORDINATOR);
        let host = ca.host("d1", "daemon");
        Farm {
            ca,
            coordinator,
            host,
        }
    }

    /// The coordinator's server configuration.
    pub fn server(&self) -> Arc<ServerConfig> {
        server_config(&self.coordinator, &self.ca)
    }
}

/// A link of the tests: the config of `farm`'s CA, over `pki`, through `transport`, logging to
/// `log`.
pub fn link(
    farm: &Farm,
    pki: &Pki,
    transport: &Arc<PipeTransport>,
    log: &Arc<RecordingLog>,
) -> JsonMtlsLink {
    JsonMtlsLink::new(
        &config(&farm.ca),
        &pki.root(),
        transport.clone(),
        log.clone(),
    )
    .expect("a link")
}
