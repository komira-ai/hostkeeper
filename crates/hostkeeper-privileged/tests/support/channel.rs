//! An HTTPS server of the test's on loopback or on the address of the test's farm LAN: it answers
//! `GET` of the files it was given, as bytes or as a file read at each request, over a certificate
//! of the test's CA. The network-install test (`tests/net_install.rs`) serves from it, bound to the
//! address of its farm LAN ([`Channel::start_at`]), the release `hostkeeper-pxe fetch` downloads
//! (its ISO streamed from the file the job downloaded, [`Channel::serve_file`]) and the files of the
//! signed apt repository the installed guest installs the fixture service from
//! ([`Channel::serve_tree`]).

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use super::pair::TestCa;

/// What the channel answers a path with.
#[derive(Clone)]
enum Body {
    Bytes(Vec<u8>),
    /// A file, read as it is sent: an ISO is larger than the test should hold in memory.
    File(PathBuf),
}

type Routes = Arc<Mutex<BTreeMap<String, Body>>>;

/// An HTTPS server on a port the system picks, answering `GET` of the
/// files it was given and 404 for any other path. Dropping it stops it.
pub struct Channel {
    pub addr: SocketAddr,
    routes: Routes,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Channel {
    /// Starts the channel, with a server certificate of `ca` for `127.0.0.1`.
    pub fn start(ca: &TestCa) -> Channel {
        Channel::start_at(ca, Ipv4Addr::LOCALHOST)
    }

    /// Starts the channel on `address`, with a server certificate of `ca` for that address.
    pub fn start_at(ca: &TestCa, address: Ipv4Addr) -> Channel {
        let leaf = ca.server_for(address);
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf.key_der));
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("the protocol versions")
                .with_no_client_auth()
                .with_single_cert(vec![CertificateDer::from(leaf.cert_der)], key)
                .expect("the server config");
        let config = Arc::new(config);
        let listener = TcpListener::bind(SocketAddr::from((address, 0))).expect("bind");
        let addr = listener.local_addr().expect("the address");
        let routes: Routes = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (served, stopped) = (Arc::clone(&routes), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    // One request per connection; a client that goes away is no failure of the
                    // channel's.
                    let _ = answer(&config, stream, &served);
                }
            }
        });
        Channel {
            addr,
            routes,
            stop,
            thread: Some(thread),
        }
    }

    /// The channel's URL.
    pub fn url(&self) -> String {
        format!("https://{}", self.addr)
    }

    /// Serves `body` at `path` from now on.
    pub fn serve_bytes(&self, path: &str, body: Vec<u8>) {
        let mut routes = self.routes.lock().expect("the routes");
        routes.insert(path.to_owned(), Body::Bytes(body));
    }

    /// Serves the file `file` at `path` from now on, read from disk at each request.
    pub fn serve_file(&self, path: &str, file: &Path) {
        let mut routes = self.routes.lock().expect("the routes");
        routes.insert(path.to_owned(), Body::File(file.to_path_buf()));
    }
}

impl Channel {
    /// Serves every regular file below `dir` at its path relative to `dir`, read from disk at each
    /// request: a repository's `dists/` and `pool/` as the `file:` repository holds them.
    pub fn serve_tree(&self, dir: &Path) {
        let mut pending = vec![PathBuf::new()];
        while let Some(rel) = pending.pop() {
            for entry in fs::read_dir(dir.join(&rel)).expect("a directory of the tree") {
                let entry = entry.expect("an entry");
                let path = rel.join(entry.file_name());
                if entry.file_type().expect("its type").is_dir() {
                    pending.push(path);
                } else {
                    self.serve_file(&format!("/{}", path.display()), &dir.join(&path));
                }
            }
        }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The accept waits for a connection: this one ends it.
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answers one request on `stream`: the body of its path, or 404.
fn answer(config: &Arc<ServerConfig>, stream: TcpStream, routes: &Routes) -> std::io::Result<()> {
    let conn = ServerConnection::new(Arc::clone(config)).map_err(std::io::Error::other)?;
    let mut tls = BufReader::new(StreamOwned::new(conn, stream));
    let mut line = String::new();
    tls.read_line(&mut line)?;
    let path = line.split(' ').nth(1).unwrap_or("").to_owned();
    // The rest of the head, to its blank line.
    loop {
        let mut header = String::new();
        if tls.read_line(&mut header)? == 0 || header == "\r\n" {
            break;
        }
    }
    let body = routes.lock().expect("the routes").get(&path).cloned();
    let tls = tls.get_mut();
    match body {
        Some(Body::Bytes(body)) => {
            write!(
                tls,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            tls.write_all(&body)?;
        }
        Some(Body::File(file)) => {
            let mut file = fs::File::open(file)?;
            let len = file.metadata()?.len();
            write!(
                tls,
                "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
            )?;
            std::io::copy(&mut file, tls)?;
        }
        None => write!(
            tls,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?,
    }
    tls.conn.send_close_notify();
    tls.flush()
}
