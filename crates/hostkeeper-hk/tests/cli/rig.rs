//! What the tests of `hk` share: a scratch directory with the token, the CA and the home; a
//! loopback HTTPS server of port 0 that records each request and answers it with a canned reply or
//! the real gateway; and a run of `hostkeeper_hk::run` against a [`World`] of fakes.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use hostkeeper_hk::ssh::{Coordinator, Launch, Verb};
use hostkeeper_hk::tailnet_wait::Tailnet;
use hostkeeper_hk::xcode_push::Mac;
use hostkeeper_hk::{World, run};
use hostkeeper_proto::ctl::Command;
use hostkeeper_testkit::Scratch;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};

/// The version stamp the tests run `hk` with.
pub const STAMP: &str = "1.2.3+0123456789ab";

/// The operator token of the tests: what the token file holds and what the coordinator's tokens
/// file holds the SHA-256 of.
pub const TOKEN: &str = "hk-operator-token-of-the-test";

/// The fixture's command table, which `build.rs` generated from tests/fixtures.
#[expect(
    dead_code,
    reason = "the generated file also holds the local-service and reserved-flag lists"
)]
pub mod fixture {
    use hostkeeper_proto::ctl::{Command, EnumValue, Field, FieldKind, Input, Label};

    include!(concat!(env!("OUT_DIR"), "/fixture_commands.rs"));
}

/// The fixture's encoded descriptor set.
pub const FIXTURE_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fixture.binpb"));

/// A CA made at run time and a certificate it signs for `localhost` and `127.0.0.1`.
pub struct Pki {
    pub ca_pem: String,
    pub cert_pem: String,
    pub key_pem: String,
}

/// A new CA, and a server certificate of it.
pub fn pki() -> Pki {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().expect("a CA key");
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("a CA");
    let key = rcgen::KeyPair::generate().expect("a server key");
    let cert = rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
        .expect("server params")
        .signed_by(&key, &*ca)
        .expect("a server certificate");
    Pki {
        ca_pem: ca.pem(),
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    }
}

/// The DER of the first certificate of `pem`.
pub fn der_of(pem: &str) -> Vec<u8> {
    CertificateDer::from_pem_slice(pem.as_bytes())
        .expect("a certificate")
        .to_vec()
}

/// One request a server answered.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    /// The path and query.
    pub target: String,
    /// The headers, names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub status: u16,
    pub answer: Vec<u8>,
}

impl Seen {
    /// The value of the header `name`.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// What a handler answers.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub location: Option<String>,
    /// Hang up with no answer.
    pub hang_up: bool,
    /// Announce more body than is sent, then hang up.
    pub short: bool,
}

impl Reply {
    /// An answer of `status` with `body`.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Reply {
        Reply {
            status,
            body: body.into(),
            location: None,
            hang_up: false,
            short: false,
        }
    }

    /// A reply that is no answer: the server hangs up.
    pub fn hang_up() -> Reply {
        Reply {
            hang_up: true,
            ..Reply::new(0, "")
        }
    }
}

/// A request as the server read it: what a handler answers.
pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// An HTTPS server on 127.0.0.1, port 0, answering one request per connection with its handler;
/// it records each request.
pub struct Server {
    pub port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// Reads one request: its head up to the blank line, then `Content-Length` bytes. A request that
/// says `Expect: 100-continue` is answered `100 Continue` before its body is read.
fn read_request(stream: &mut (impl Read + Write)) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let split = loop {
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
    let mut body = bytes[split + 4..].to_vec();
    let mut lines = head.lines();
    let mut start = lines.next()?.split(' ');
    let method = start.next()?.to_owned();
    let target = start.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let expects = headers
        .iter()
        .any(|(n, v)| n == "expect" && v.eq_ignore_ascii_case("100-continue"));
    if expects {
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").ok()?;
        stream.flush().ok()?;
    }
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    while body.len() < length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Request {
        method,
        target,
        headers,
        body,
    })
}

impl Server {
    /// A server with the certificate of `pki`, whose `handler` answers each request; `start` runs
    /// first on the server's thread, and its value is handed to every call of the handler.
    pub fn serve<S, F>(pki: &Pki, start: impl FnOnce() -> S + Send + 'static, handler: F) -> Server
    where
        S: 'static,
        F: Fn(&S, &Request) -> Reply + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let config = server_config(pki);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            let state = start();
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let Ok(conn) = rustls::ServerConnection::new(Arc::clone(&config)) else {
                    continue;
                };
                let mut tls = rustls::StreamOwned::new(conn, stream);
                let Some(request) = read_request(&mut tls) else {
                    continue;
                };
                let reply = handler(&state, &request);
                if reply.hang_up {
                    record.lock().expect("the record").push(Seen {
                        method: request.method,
                        target: request.target,
                        headers: request.headers,
                        body: request.body,
                        status: 0,
                        answer: Vec::new(),
                    });
                    continue;
                }
                let mut out = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len() + if reply.short { 50 } else { 0 }
                );
                if let Some(location) = &reply.location {
                    out.push_str(&format!("Location: {location}\r\n"));
                }
                out.push_str("\r\n");
                let mut out = out.into_bytes();
                out.extend_from_slice(&reply.body);
                // Recorded before it is answered, so a test that reads the record after `hk`
                // returns sees it.
                record.lock().expect("the record").push(Seen {
                    method: request.method,
                    target: request.target,
                    headers: request.headers,
                    body: request.body,
                    status: reply.status,
                    answer: reply.body,
                });
                let _ = tls.write_all(&out);
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
        });
        Server { port, seen }
    }

    /// A server that answers every request with `status` and `body`.
    pub fn canned(pki: &Pki, status: u16, body: &str) -> Server {
        let body = body.to_owned();
        Server::serve(pki, || (), move |(), _| Reply::new(status, body.clone()))
    }

    /// A server that answers its `nth` (from 0) request with `replies[nth]`, and every later one
    /// with the last.
    pub fn sequence(pki: &Pki, replies: Vec<Reply>) -> Server {
        let count = Mutex::new(0_usize);
        Server::serve(
            pki,
            || (),
            move |(), _| {
                let mut count = count.lock().expect("the count");
                let reply = replies[(*count).min(replies.len() - 1)].clone();
                *count += 1;
                reply
            },
        )
    }

    /// Every request answered so far.
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("the record").clone()
    }
}

/// The TLS configuration of a test server with the certificate of `pki`.
fn server_config(pki: &Pki) -> Arc<rustls::ServerConfig> {
    let chain = vec![CertificateDer::from_pem_slice(pki.cert_pem.as_bytes()).expect("a cert")];
    let key = PrivateKeyDer::from_pem_slice(pki.key_pem.as_bytes()).expect("a key");
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .expect("a server certificate"),
    )
}

/// What [`Early`] heard: the request's target, and the bytes of body that arrived after its
/// head, until the client went away.
type Heard = (String, usize);

/// An HTTPS server on 127.0.0.1, port 0, for one request: it answers a final status as soon as
/// the request's head is read (with no `100 Continue`, before any body), then counts the bytes
/// of body that still arrive until the client closes the connection.
pub struct Early {
    pub port: u16,
    heard: Arc<(Mutex<Option<Heard>>, Condvar)>,
}

impl Early {
    /// A server that refuses its one request with `status` and `body` from its head alone, after
    /// `lists` requests that are read whole and answered `200` with `first` (what `Xcode.List` is
    /// asked before an upload).
    pub fn refusing_after(pki: &Pki, lists: usize, first: &str, status: u16, body: &str) -> Early {
        let first = first.to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let config = server_config(pki);
        let answer = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        let heard = Arc::new((Mutex::new(None), Condvar::new()));
        let record = Arc::clone(&heard);
        let config_first = Arc::clone(&config);
        std::thread::spawn(move || {
            let serve = || -> Option<Heard> {
                for _ in 0..lists {
                    let (stream, _) = listener.accept().ok()?;
                    let conn = rustls::ServerConnection::new(Arc::clone(&config_first)).ok()?;
                    let mut tls = rustls::StreamOwned::new(conn, stream);
                    read_request(&mut tls)?;
                    let reply = format!(
                        "HTTP/1.1 200 X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n{first}",
                        first.len()
                    );
                    tls.write_all(reply.as_bytes()).ok()?;
                    tls.conn.send_close_notify();
                    let _ = tls.flush();
                }
                let (stream, _) = listener.accept().ok()?;
                let conn = rustls::ServerConnection::new(config).ok()?;
                let mut tls = rustls::StreamOwned::new(conn, stream);
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 4096];
                let split = loop {
                    if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break at;
                    }
                    let n = tls.read(&mut chunk).ok()?;
                    if n == 0 {
                        return None;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                };
                let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
                let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
                let mut after = bytes.len() - split - 4;
                tls.write_all(answer.as_bytes()).ok()?;
                tls.conn.send_close_notify();
                let _ = tls.flush();
                while let Ok(n) = tls.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    after += n;
                }
                Some((target, after))
            };
            let heard = serve().unwrap_or((String::new(), usize::MAX));
            let (lock, ready) = &*record;
            *lock.lock().expect("the record") = Some(heard);
            ready.notify_all();
        });
        Early { port, heard }
    }

    /// The target of the request and the bytes of body that arrived, once the client is gone.
    pub fn heard(&self) -> Heard {
        let (lock, ready) = &*self.heard;
        let mut heard = lock.lock().expect("the record");
        loop {
            if let Some(done) = heard.clone() {
                return done;
            }
            heard = ready.wait(heard).expect("the record");
        }
    }
}

/// The address of a loopback port where nothing listens.
pub fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let SocketAddr::V4(addr) = listener.local_addr().expect("its address") else {
        panic!("an IPv4 address");
    };
    addr.port()
}

/// What a run of `hk` printed and its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub code: u8,
    pub stdout: String,
    pub stderr: String,
}

/// A double for the program that runs the ssh verbs: it records what it was asked, and answers
/// `code`.
#[derive(Default)]
pub struct Recorder {
    pub calls: Mutex<Vec<Launched>>,
    pub code: u8,
    pub says: &'static str,
}

/// One run of an ssh verb the recorder was asked for.
#[derive(Debug)]
pub struct Launched {
    pub verb: Verb,
    pub coordinator: Coordinator,
    pub args: Vec<OsString>,
    pub name: String,
    pub home: PathBuf,
}

impl Launch for Recorder {
    fn launch(
        &self,
        verb: Verb,
        coordinator: Coordinator,
        args: Vec<OsString>,
        name: &str,
        home: &Path,
        stderr: &mut dyn Write,
    ) -> u8 {
        let _ = stderr.write_all(self.says.as_bytes());
        self.calls.lock().expect("the calls").push(Launched {
            verb,
            coordinator,
            args,
            name: name.to_owned(),
            home: home.to_path_buf(),
        });
        self.code
    }
}

/// How one run of `hk` is set up.
pub struct Opts<'a> {
    pub table: &'a [Command],
    pub stdin: Vec<u8>,
    pub stamp: &'a str,
    pub oidc: Option<(&'a str, &'a str)>,
    pub roots: Vec<Vec<u8>>,
    pub home: Option<PathBuf>,
    pub no_home: bool,
    /// The user id the run is as; the test's own when `None`.
    pub uid: Option<u32>,
    pub ssh: Option<&'a Recorder>,
    /// The system and the programs of `hk xcode-push`; the real ones when `None`.
    pub mac: Option<&'a Mac>,
    /// The `tailscale` and the wait of `hk tailnet-wait`; when `None`, a `tailscale` that does
    /// not exist and a wait that returns at once, so no test runs the host's own.
    pub tailnet: Option<&'a Tailnet<'a>>,
    /// Whether standard error is a terminal.
    pub terminal: bool,
}

impl<'a> Opts<'a> {
    /// A run against the real protos' table.
    pub fn real() -> Opts<'a> {
        Opts {
            table: hostkeeper_proto::ctl::COMMANDS,
            stdin: Vec::new(),
            stamp: STAMP,
            oidc: None,
            roots: Vec::new(),
            home: None,
            no_home: false,
            uid: None,
            ssh: None,
            mac: None,
            tailnet: None,
            terminal: false,
        }
    }

    /// A run against the fixture's table.
    pub fn fixture() -> Opts<'a> {
        Opts {
            table: fixture::COMMANDS,
            ..Opts::real()
        }
    }

    pub fn stdin(mut self, bytes: &[u8]) -> Self {
        self.stdin = bytes.to_vec();
        self
    }

    pub fn stamped(mut self, stamp: &'a str) -> Self {
        self.stamp = stamp;
        self
    }

    pub fn runner(mut self, url: &'a str, token: &'a str, roots: Vec<Vec<u8>>) -> Self {
        self.oidc = Some((url, token));
        self.roots = roots;
        self
    }

    pub fn ssh(mut self, recorder: &'a Recorder) -> Self {
        self.ssh = Some(recorder);
        self
    }

    pub fn mac(mut self, mac: &'a Mac) -> Self {
        self.mac = Some(mac);
        self
    }

    pub fn tailnet(mut self, tailnet: &'a Tailnet<'a>) -> Self {
        self.tailnet = Some(tailnet);
        self
    }

    /// A run whose standard error is a terminal.
    pub fn terminal(mut self) -> Self {
        self.terminal = true;
        self
    }

    pub fn home(mut self, home: &Path) -> Self {
        self.home = Some(home.to_path_buf());
        self
    }

    /// A run as the user `uid`, whoever the test is.
    pub fn uid(mut self, uid: u32) -> Self {
        self.uid = Some(uid);
        self
    }

    /// A run of a process with no home directory.
    pub fn without_home(mut self) -> Self {
        self.no_home = true;
        self
    }
}

/// The directory of one test: the token file, the CA file and a home.
pub struct Rig {
    dir: Scratch,
    pub pki: Pki,
}

impl Rig {
    /// A new directory for the test `name`, with the token file (0600), the CA file and a home.
    pub fn new(name: &str) -> Self {
        let dir = Scratch::under(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!("hk-{name}"),
        )
        .expect("a scratch directory");
        let rig = Self { dir, pki: pki() };
        rig.put("token", format!("{TOKEN}\n").as_bytes(), 0o600);
        rig.put("ca.pem", rig.pki.ca_pem.as_bytes(), 0o644);
        std::fs::create_dir(rig.path("home")).expect("a home");
        rig
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Writes `name` in the test's directory with `mode`; answers its path as text.
    pub fn put(&self, name: &str, bytes: &[u8], mode: u32) -> String {
        let path = self.path(name);
        // A file of a mode that refuses its owner a write is replaced, not written over.
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, bytes).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
        path.display().to_string()
    }

    /// The global flags of a call with the token file: `--endpoint`, `--ca-file`,
    /// `--token-file`, for a coordinator on `port`.
    pub fn globals(&self, port: u16) -> Vec<String> {
        vec![
            "--endpoint".to_owned(),
            format!("https://127.0.0.1:{port}"),
            "--ca-file".to_owned(),
            self.path("ca.pem").display().to_string(),
            "--token-file".to_owned(),
            self.path("token").display().to_string(),
        ]
    }

    /// `hk` with `args` against the real table, nothing on stdin.
    pub fn hk(&self, args: &[String]) -> Ran {
        self.run(Opts::real(), args)
    }

    /// `hk` with `args` against the fixture's table, nothing on stdin.
    pub fn fx(&self, args: &[String]) -> Ran {
        self.run(Opts::fixture(), args)
    }

    /// `hk` with `args` and `opts`: in process, against a [`World`] of fakes.
    pub fn run(&self, opts: Opts<'_>, args: &[String]) -> Ran {
        let os: Vec<OsString> = args.iter().map(OsString::from).collect();
        self.run_os(opts, &os)
    }

    /// [`Rig::run`] with arguments that need not be text.
    pub fn run_os(&self, opts: Opts<'_>, args: &[OsString]) -> Ran {
        let mut stdin = std::io::Cursor::new(opts.stdin);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let roots = opts.roots;
        let web_roots = move || roots.clone();
        let default_ssh = Recorder::default();
        let ssh: &dyn Launch = match opts.ssh {
            Some(recorder) => recorder,
            None => &default_ssh,
        };
        let default_mac = Mac::system();
        let mac = opts.mac.unwrap_or(&default_mac);
        let default_tailnet = Tailnet {
            tailscale: self.path("no-tailscale"),
            limit: std::time::Duration::from_secs(3600),
            wait: &|_| {},
        };
        let tailnet = opts.tailnet.unwrap_or(&default_tailnet);
        let own = std::fs::metadata(self.path("")).expect("the scratch").uid();
        let mut world = World {
            table: opts.table,
            stamp: opts.stamp,
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
            home: if opts.no_home {
                None
            } else {
                opts.home.or_else(|| Some(self.path("home")))
            },
            uid: opts.uid.unwrap_or(own),
            stderr_terminal: opts.terminal,
            oidc_url: opts.oidc.map(|(url, _)| url.as_bytes().to_vec()),
            oidc_token: opts.oidc.map(|(_, token)| token.as_bytes().to_vec()),
            web_roots: &web_roots,
            ssh,
            mac,
            tailnet,
        };
        let code = run(&mut world, args);
        Ran {
            code,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }
}

/// `words` as owned arguments.
pub fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// The global flags of a call on `port`, then `words`.
pub fn call(rig: &Rig, port: u16, words: &[&str]) -> Vec<String> {
    let mut all = rig.globals(port);
    all.extend(args(words));
    all
}
