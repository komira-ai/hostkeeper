//! A static file server over HTTPS on `127.0.0.1`, for the apt repositories of the device rigs.
//!
//! The coordinator's registry takes `https://` repositories only, so the repository a rig builds in
//! a directory is served from here: [`Server::start`] makes a throwaway CA and a server
//! certificate it signed (IP SAN `127.0.0.1`, server authentication), binds `127.0.0.1:0`, and
//! answers `GET` and `HEAD` for the files below the directory on plain `std` threads over rustls.
//! The files are read at each request, so a repository that grows while the server runs is served
//! as it is. Any other path (a missing file, a directory, a path that leaves the directory) is
//! 404, any other method 405. HTTP/1.1, `Connection: close`, one request per connection.
//!
//! The server knows nothing of apt: [`Server::ca_pem`] is what the host's apt is told to trust
//! (the cross-release rig's `deb`, and `aptrepo::AptRepo`).

use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// The most bytes of a request head the server reads before it gives up on the connection.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// How long a connection may sit idle before the server drops it: a bound for a client that
/// never speaks, not a timing the tests assert on.
const IDLE: Duration = Duration::from_secs(30);

/// An HTTPS server of the files below a directory, stopped when it is dropped.
pub struct Server {
    port: u16,
    ca_pem: String,
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
    ca_params.distinguished_name = named("hostkeeper apt repository CA");
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().expect("a CA key"))
        .expect("a CA certificate");

    let mut leaf_params = CertificateParams::new(Vec::<String>::new()).expect("leaf params");
    leaf_params.distinguished_name = named("hostkeeper apt repository");
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

/// The bytes `%XX` stands for in `text`, or `None` for a `%` not followed by two hex digits or a
/// result that is not UTF-8.
fn unescaped(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let pair = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The file below `root` that the request target `target` names, when it names one: only normal
/// path components count, so no target leaves `root`.
fn file_of(root: &Path, target: &str) -> Option<PathBuf> {
    let path = unescaped(target.split('?').next().unwrap_or(target))?;
    let relative = Path::new(path.strip_prefix('/')?);
    if !relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let file = root.join(relative);
    file.is_file().then_some(file)
}

/// Serves one connection: the handshake, one request head, the answer, a clean close.
fn serve(stream: TcpStream, config: Arc<ServerConfig>, root: &Path) -> io::Result<()> {
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
    let mut words = head.lines().next().unwrap_or_default().split_whitespace();
    let (method, target) = (words.next().unwrap_or_default(), words.next());
    let response = if method != "GET" && method != "HEAD" {
        b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\n\
          Connection: close\r\n\r\n"
            .to_vec()
    } else if let Some(body) = target
        .and_then(|target| file_of(root, target))
        .and_then(|file| fs::read(file).ok())
    {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
             Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        if method == "GET" {
            response.extend_from_slice(&body);
        }
        response
    } else {
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
    };
    // A client that stops reading loses its answer.
    drop(tls.write_all(&response));
    tls.conn.send_close_notify();
    drop(tls.flush());
    Ok(())
}

impl Server {
    /// Serves the files below `root` on a free loopback port.
    ///
    /// # Panics
    /// The certificate cannot be made or the port cannot be bound.
    pub fn start(root: &Path) -> Server {
        let (ca_pem, config) = identity();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let root = root.to_path_buf();
        let accept = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let (config, root) = (Arc::clone(&config), root.clone());
                handlers.push(std::thread::spawn(move || {
                    // A client that gives up mid-handshake ends only its connection.
                    drop(serve(stream, config, &root));
                }));
            }
            for handler in handlers {
                drop(handler.join());
            }
        });
        Server {
            port,
            ca_pem,
            stop,
            accept: Some(accept),
        }
    }

    /// The port the server listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The PEM of the CA that signed the server's certificate.
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // `accept` blocks until a connection arrives; this one is only to wake it.
        drop(TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)));
        if let Some(accept) = self.accept.take() {
            drop(accept.join());
        }
    }
}
