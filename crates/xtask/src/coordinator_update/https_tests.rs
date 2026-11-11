//! A fake of GitHub for the coordinator updater's tests: HTTPS on `127.0.0.1`, port 0, in the
//! test process. [`Server::start`] makes a throwaway CA and a server certificate it signed (IP SAN
//! `127.0.0.1`, server authentication) at run time, so no key is committed, and answers each
//! request with what the test's [`Route`] says, on plain `std` threads over rustls. HTTP/1.1,
//! `Connection: close`, one request per connection. Every request is recorded, before it is
//! answered, with its path and its headers ([`Server::requests`]): a test reads them after the
//! calls it makes have returned, so nothing waits on a clock.
//!
//! Self-contained: the unit tests (`coordinator_update::public_tests`) and the process test of
//! the committed unit (`tests/coordinator_update_unit.rs`, through `#[path]`) both use it.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// The most bytes of a request head the server reads.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// How long a connection may sit idle before the server drops it: a bound for a client that
/// never speaks, not a timing any test asserts on.
const IDLE: Duration = Duration::from_secs(30);

/// One request, as the server read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The request target: the path and the query.
    pub path: String,
    /// Each header, its name in lower case, in the order sent.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// The value of the header `name` (lower case), when it was sent.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// One answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Answer {
    /// `status` with `body` and no other header.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Answer {
        Answer {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// The same answer with the header `name: value` too.
    pub fn with(mut self, name: &str, value: &str) -> Answer {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// What the server answers to a request: its own base URL and the request in, the answer out.
pub type Route = Arc<dyn Fn(&str, &Request) -> Answer + Send + Sync>;

/// The fake, stopped when it is dropped.
pub struct Server {
    port: u16,
    ca_pem: String,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

fn named(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

/// A fresh CA and the server configuration of a certificate it signed for `127.0.0.1`, with the
/// CA's PEM.
fn identity() -> (String, Arc<ServerConfig>) {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.distinguished_name = named("coordinator updater test CA");
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().expect("a CA key"))
        .expect("a CA certificate");
    let mut leaf_params = CertificateParams::new(Vec::<String>::new()).expect("leaf params");
    leaf_params.distinguished_name = named("coordinator updater test server");
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
    (ca.pem(), Arc::new(config))
}

/// The reason phrase of `status`, for the status line.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        304 => "Not Modified",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        _ => "Status",
    }
}

/// Reads one request head off `tls`.
fn read_head(tls: &mut impl Read) -> io::Result<Request> {
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
    let mut lines = head.lines();
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Request { path, headers })
}

/// Serves one connection: the handshake, one request head (recorded), the answer, a clean close.
fn serve(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    (base, route): (&str, &Route),
    requests: &Mutex<Vec<Request>>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_write_timeout(Some(IDLE))?;
    let connection = ServerConnection::new(config).map_err(io::Error::other)?;
    let mut tls = StreamOwned::new(connection, stream);
    let request = read_head(&mut tls)?;
    requests
        .lock()
        .map_err(|_| io::Error::other("a poisoned log"))?
        .push(request.clone());
    let answer = route(base, &request);
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        answer.status,
        reason(answer.status),
        answer.body.len()
    );
    for (name, value) in &answer.headers {
        out.push_str(&format!("{name}: {value}\r\n"));
    }
    out.push_str("\r\n");
    tls.write_all(out.as_bytes())?;
    tls.write_all(&answer.body)?;
    tls.conn.send_close_notify();
    tls.flush()
}

impl Server {
    /// Starts the fake on a free loopback port; `route` answers every request.
    pub fn start(route: Route) -> Server {
        let (ca_pem, config) = identity();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let base = format!("https://127.0.0.1:{port}");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (stopped, log) = (Arc::clone(&stop), Arc::clone(&requests));
        let accept = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (config, route, log, base) = (
                    Arc::clone(&config),
                    Arc::clone(&route),
                    Arc::clone(&log),
                    base.clone(),
                );
                handlers.push(std::thread::spawn(move || {
                    // A client that hangs up early ends its own connection, nothing else.
                    drop(serve(stream, config, (&base, &route), &log));
                }));
            }
            for handler in handlers {
                drop(handler.join());
            }
        });
        Server {
            port,
            ca_pem,
            requests,
            stop,
            accept: Some(accept),
        }
    }

    /// `https://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }

    /// The PEM of the CA the server's certificate chains to.
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    /// Every request so far, in the order they were read.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("the request log").clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // One connection wakes the accept loop, which then sees the flag and ends.
        drop(TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)));
        if let Some(accept) = self.accept.take() {
            drop(accept.join());
        }
    }
}
