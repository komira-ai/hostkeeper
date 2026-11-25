//! The fake coordinator and the double for the OpenSSH programs.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use hostkeeper_hk::ssh::{Coordinator, Env, Error, Programs, Verb, run};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ssh_server::{Ssh, SshServer};
use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use hostkeeper_testkit::Scratch;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};

/// The key type of the certificates the fake signs.
pub fn cert_type() -> String {
    format!("ssh-ed25519{}", hostkeeper_hk::ssh::CERT_KEY_TYPE_SUFFIX)
}

/// The public key every test uses.
pub const PUBLIC: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAItestkeytestkeytestkeytestkeytestkey";
pub const TOKEN: &str = "operator-secret-token";
pub const HOST: &str = "host-7";
pub const NOW: u64 = 1_000_000;

/// How the next tunnel behaves.
#[derive(Clone, Debug)]
pub enum Tunnel {
    /// `opened`, then every `data` frame echoed, and at the client's `close` `bye` and
    /// `host_closed`; the cause the coordinator audits is sent to [`Rig::audited`].
    Echo,
    /// As [`Tunnel::Echo`], but at the client's `close` the host answers `half-closed` and does
    /// not close: the tunnel ends at the client's next move.
    Linger,
    /// Refused with this token.
    Refuse(&'static str),
    /// A first answer that is not `opened`.
    NotOpened,
    /// `opened`, then `close` with this cause.
    CloseAfterOpen(&'static str),
    /// `opened`, then the stream ends with no `close`.
    EndAfterOpen,
    /// `opened`, then the stream fails with this status message.
    FailAfterOpen(&'static str),
    /// The stream ends before anything is answered.
    EmptyAnswer,
    /// The stream fails with this status message before anything is answered.
    FailBeforeOpened(&'static str),
}

/// What the fake saw.
#[derive(Default)]
pub struct Seen {
    pub signs: Vec<proto::SignUserCertRequest>,
    pub key_reads: Vec<String>,
    pub revokes: Vec<proto::RevokeRequest>,
    pub opens: Vec<proto::Open>,
    pub frames: Vec<Vec<u8>>,
    pub authorizations: Vec<String>,
}

pub struct Script {
    /// The cause of each close the fake audits, in order.
    pub audit: Mutex<std::sync::mpsc::Sender<&'static str>>,
    pub sign: Mutex<Result<proto::SignUserCertResponse, Status>>,
    pub keys: Mutex<Result<proto::HostKeysResponse, Status>>,
    pub revoke: Mutex<Result<proto::RevokeResponse, Status>>,
    pub tunnels: Mutex<VecDeque<Tunnel>>,
    pub seen: Mutex<Seen>,
}

pub fn certificate_reply(serial: u64, expires: u64) -> proto::SignUserCertResponse {
    proto::SignUserCertResponse {
        certificate: format!("{} AAAAcertbytes", cert_type()),
        login_user: "root".into(),
        expires_unix: expires,
        key_id: format!("alice/{serial}"),
    }
}

impl Script {
    fn new(audit: std::sync::mpsc::Sender<&'static str>) -> Script {
        Script {
            audit: Mutex::new(audit),
            sign: Mutex::new(Ok(certificate_reply(41, NOW + 3600))),
            keys: Mutex::new(Ok(proto::HostKeysResponse {
                fingerprints: vec![PIN.into()],
                state: "pinned".into(),
            })),
            revoke: Mutex::new(Ok(proto::RevokeResponse { revoked: 1 })),
            tunnels: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Seen::default()),
        }
    }
}

pub const PIN: &str = "SHA256:0123456789012345678901234567890123456789abc";

fn note(script: &Script, request: &Request<impl Sized>) {
    let auth = request
        .metadata()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("none")
        .to_owned();
    script.seen.lock().expect("seen").authorizations.push(auth);
}

struct Service(Arc<Script>);

/// The relay of an open tunnel, ended as the coordinator's (`pump` in hostkeeper-coordinator's
/// `ssh_tunnel.rs`) ends it, and the cause it audits: a client's `close` shuts the host's input
/// and the host answers `bye` and closes (`host_closed`); a request stream that ends is `eof`;
/// one that fails, which is a client that went away, is `error`. With `linger`, the host answers
/// a `close` with `half-closed` and stays; then a second `close` or more data is audited as such.
async fn relay(
    script: &Script,
    incoming: &mut Streaming<proto::TunnelFrame>,
    out: &mpsc::Sender<Result<proto::TunnelFrame, Status>>,
    linger: bool,
) -> &'static str {
    let mut closed = false;
    loop {
        match incoming.message().await {
            Ok(Some(proto::TunnelFrame {
                frame: Some(Frame::Data(bytes)),
            })) => {
                if closed {
                    return "data after close";
                }
                script.seen.lock().expect("seen").frames.push(bytes.clone());
                let _ = out.send(Ok(frame(Frame::Data(bytes)))).await;
            }
            Ok(Some(proto::TunnelFrame {
                frame: Some(Frame::Close(_)),
            })) => {
                if closed {
                    return "a second close";
                }
                if !linger {
                    let _ = out.send(Ok(frame(Frame::Data(b"bye".to_vec())))).await;
                    return "host_closed";
                }
                closed = true;
                let _ = out
                    .send(Ok(frame(Frame::Data(b"half-closed".to_vec()))))
                    .await;
            }
            Ok(Some(_)) => {}
            Ok(None) => return "eof",
            Err(_) => return "error",
        }
    }
}

type Answers = Pin<Box<dyn tokio_stream::Stream<Item = Result<proto::TunnelFrame, Status>> + Send>>;

fn frame(frame: Frame) -> proto::TunnelFrame {
    proto::TunnelFrame { frame: Some(frame) }
}

#[tonic::async_trait]
impl Ssh for Service {
    async fn sign_user_cert(
        &self,
        request: Request<proto::SignUserCertRequest>,
    ) -> Result<Response<proto::SignUserCertResponse>, Status> {
        note(&self.0, &request);
        self.0
            .seen
            .lock()
            .expect("seen")
            .signs
            .push(request.into_inner());
        self.0.sign.lock().expect("sign").clone().map(Response::new)
    }

    async fn host_keys(
        &self,
        request: Request<proto::HostKeysRequest>,
    ) -> Result<Response<proto::HostKeysResponse>, Status> {
        note(&self.0, &request);
        self.0
            .seen
            .lock()
            .expect("seen")
            .key_reads
            .push(request.into_inner().host_id);
        self.0.keys.lock().expect("keys").clone().map(Response::new)
    }

    async fn revoke(
        &self,
        request: Request<proto::RevokeRequest>,
    ) -> Result<Response<proto::RevokeResponse>, Status> {
        note(&self.0, &request);
        self.0
            .seen
            .lock()
            .expect("seen")
            .revokes
            .push(request.into_inner());
        self.0
            .revoke
            .lock()
            .expect("revoke")
            .clone()
            .map(Response::new)
    }

    /// The ssh verbs never pin: `hk ssh pin-host-keys` is a gateway call.
    async fn pin_host_keys(
        &self,
        request: Request<proto::PinHostKeysRequest>,
    ) -> Result<Response<proto::HostKeysResponse>, Status> {
        note(&self.0, &request);
        Err(Status::unimplemented("the ssh verbs do not pin"))
    }

    type TunnelStream = Answers;

    async fn tunnel(
        &self,
        request: Request<Streaming<proto::TunnelFrame>>,
    ) -> Result<Response<Answers>, Status> {
        note(&self.0, &request);
        let mut incoming = request.into_inner();
        let first = incoming.message().await?;
        let Some(proto::TunnelFrame {
            frame: Some(Frame::Open(open)),
        }) = first
        else {
            return Err(Status::invalid_argument("invalid_request"));
        };
        self.0.seen.lock().expect("seen").opens.push(open);
        let mode = self
            .0
            .tunnels
            .lock()
            .expect("tunnels")
            .pop_front()
            .unwrap_or(Tunnel::Echo);
        if let Tunnel::Refuse(token) = mode {
            return Err(Status::unavailable(token));
        }
        let (out, answers) = mpsc::channel(8);
        let script = Arc::clone(&self.0);
        tokio::spawn(async move {
            let opened = |u: &str| {
                frame(Frame::Opened(proto::Opened {
                    login_user: u.into(),
                }))
            };
            match mode {
                Tunnel::NotOpened => {
                    let _ = out.send(Ok(frame(Frame::Data(b"x".to_vec())))).await;
                }
                Tunnel::EmptyAnswer => {}
                Tunnel::FailBeforeOpened(message) => {
                    let _ = out.send(Err(Status::unavailable(message))).await;
                }
                Tunnel::CloseAfterOpen(cause) => {
                    let _ = out.send(Ok(opened("root"))).await;
                    let close = Frame::Close(proto::Close {
                        cause: cause.into(),
                    });
                    let _ = out.send(Ok(frame(close))).await;
                }
                Tunnel::EndAfterOpen => {
                    let _ = out.send(Ok(opened("root"))).await;
                }
                Tunnel::FailAfterOpen(message) => {
                    let _ = out.send(Ok(opened("root"))).await;
                    let _ = out.send(Err(Status::unavailable(message))).await;
                }
                Tunnel::Echo | Tunnel::Linger => {
                    let _ = out.send(Ok(opened("root"))).await;
                    // A frame a client has no use for (a second `opened`) is passed over.
                    let _ = out.send(Ok(opened("root"))).await;
                    let linger = matches!(mode, Tunnel::Linger);
                    let cause = relay(&script, &mut incoming, &out, linger).await;
                    // The coordinator writes the close in its audit, then tells the client.
                    let _ = script.audit.lock().expect("audit").send(cause);
                    let close = Frame::Close(proto::Close {
                        cause: cause.into(),
                    });
                    let _ = out.send(Ok(frame(close))).await;
                }
                Tunnel::Refuse(_) => {}
            }
        });
        let answers: Answers = Box::pin(ReceiverStream::new(answers));
        Ok(Response::new(answers))
    }
}

/// A CA, and a server certificate it signed for `localhost` and `127.0.0.1`.
pub struct Pki {
    pub ca_pem: String,
    server_cert: String,
    server_key: String,
}

pub fn pki() -> Pki {
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().expect("a CA key");
    let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("a CA");
    let server_key = rcgen::KeyPair::generate().expect("a server key");
    let server =
        rcgen::CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .expect("server params")
            .signed_by(&server_key, &*ca)
            .expect("a server certificate");
    Pki {
        ca_pem: ca.pem(),
        server_cert: server.pem(),
        server_key: server_key.serialize_pem(),
    }
}

/// The fake coordinator on loopback, a state directory, and the files the flags name.
pub struct Rig {
    pub script: Arc<Script>,
    audited: Mutex<std::sync::mpsc::Receiver<&'static str>>,
    pub scratch: Scratch,
    pub port: u16,
    _runtime: tokio::runtime::Runtime,
}

impl Rig {
    pub fn start() -> Rig {
        let pki = pki();
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "hk")
            .expect("a scratch directory");
        std::fs::write(scratch.path().join("ca.pem"), &pki.ca_pem).expect("the CA file");
        write_private(&scratch.path().join("token"), TOKEN);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime");
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let tls =
            ServerTlsConfig::new().identity(Identity::from_pem(&pki.server_cert, &pki.server_key));
        let (audit, audited) = std::sync::mpsc::channel();
        let script = Arc::new(Script::new(audit));
        let server = Server::builder()
            .tls_config(tls)
            .expect("TLS")
            .serve_with_incoming(
                SshServer::new(Service(Arc::clone(&script))),
                TcpListenerStream::new(listener),
            );
        runtime.spawn(server);
        Rig {
            script,
            audited: Mutex::new(audited),
            scratch,
            port,
            _runtime: runtime,
        }
    }

    pub fn dir(&self) -> &Path {
        self.scratch.path()
    }

    pub fn state_dir(&self) -> PathBuf {
        self.dir().join("state")
    }

    /// How to reach the fake, with the files the rig wrote.
    pub fn coordinator(&self) -> Coordinator {
        Coordinator {
            endpoint: format!("https://localhost:{}", self.port),
            ca_file: self.dir().join("ca.pem"),
            token_file: self.dir().join("token"),
            timeout_secs: 30,
        }
    }

    /// The cause of the next close the fake audits, waiting for it.
    pub fn audited(&self) -> &'static str {
        self.audited
            .lock()
            .expect("audited")
            .recv()
            .expect("the fake audits each tunnel it relays")
    }

    pub fn seen<T>(&self, read: impl FnOnce(&Seen) -> T) -> T {
        read(&self.script.seen.lock().expect("seen"))
    }
}

pub fn write_private(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, text).expect("a file");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("its mode");
}

/// The OpenSSH programs as a double: what it was asked, and what it answers.
pub struct Double {
    pub agent: Option<String>,
    pub ssh_code: i32,
    pub generated: Mutex<Vec<PathBuf>>,
    pub ssh_calls: Mutex<Vec<Vec<OsString>>>,
}

impl Double {
    pub fn new() -> Double {
        Double {
            agent: None,
            ssh_code: 0,
            generated: Mutex::new(Vec::new()),
            ssh_calls: Mutex::new(Vec::new()),
        }
    }

    /// The arguments of each ssh run, as text.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.ssh_calls
            .lock()
            .expect("calls")
            .iter()
            .map(|call| {
                call.iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }
}

impl Programs for Double {
    fn generate_key(&self, private: &Path) -> Result<(), Error> {
        self.generated
            .lock()
            .expect("generated")
            .push(private.to_owned());
        std::fs::write(private, "private").expect("the private key");
        let mut public = private.as_os_str().to_owned();
        public.push(".pub");
        std::fs::write(public, format!("{PUBLIC} hk\n")).expect("the public key");
        Ok(())
    }

    fn agent_key(&self) -> Option<String> {
        self.agent.clone()
    }

    fn run_ssh(&self, args: &[OsString]) -> Result<i32, Error> {
        self.ssh_calls.lock().expect("calls").push(args.to_vec());
        Ok(self.ssh_code)
    }
}

/// The outcome of one command line.
pub struct Outcome {
    pub code: u8,
    pub stdout: Vec<u8>,
    pub diag: String,
}

impl Outcome {
    pub fn text(&self) -> String {
        String::from_utf8(self.stdout.clone()).expect("text")
    }

    /// The exit code of a run that ended in success.
    pub fn ok(&self) -> u8 {
        assert_eq!(self.code, 0, "{}", self.diag);
        self.code
    }

    /// The message of a run that failed: its diagnostic without the program name and the line
    /// end.
    pub fn error(&self) -> String {
        assert_ne!(self.code, 0, "the run succeeded");
        self.diag
            .strip_prefix("hk: ")
            .and_then(|rest| rest.strip_suffix('\n'))
            .unwrap_or_else(|| panic!("not a diagnostic of hk: {:?}", self.diag))
            .to_owned()
    }
}

/// An output that refuses every write, as a closed pipe does.
pub struct Closed;

impl tokio::io::AsyncWrite for Closed {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// The path ssh would run again as the proxy: not the test binary, and not named `hk`, so that
/// a line built from the program's name instead would show.
pub const EXE: &str = "/opt/hostkeeper/bin/hk";

/// One run of a verb: the program's name, the verb, its tail and where it runs.
pub struct Call<'a> {
    pub name: &'a str,
    pub verb: Verb,
    pub coordinator: &'a Coordinator,
    pub state_dir: &'a Path,
    pub args: Vec<OsString>,
}

/// Runs `call` with `input` as stdin and `output` as stdout; the exit code and what went to the
/// diagnostics.
pub fn invoke_as<W: tokio::io::AsyncWrite + Unpin>(
    call: &Call<'_>,
    programs: &Double,
    now: u64,
    input: Vec<u8>,
    output: W,
) -> (u8, String) {
    let env = Env {
        programs,
        now,
        exe: PathBuf::from(EXE),
        state_dir: call.state_dir.to_owned(),
        keepalive_secs: 30,
        name: call.name,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let mut diag = Vec::new();
    let code = runtime.block_on(run(
        call.verb,
        call.coordinator,
        &call.args,
        &env,
        std::io::Cursor::new(input),
        output,
        &mut diag,
    ));
    (code, String::from_utf8(diag).expect("text"))
}

/// The tail as the OS gives it.
pub fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// Runs `verb` against the rig with `args` as the tail, `input` as stdin and `output` as
/// stdout, as `hk`.
pub fn invoke_into<W: tokio::io::AsyncWrite + Unpin>(
    rig: &Rig,
    verb: Verb,
    args: &[&str],
    programs: &Double,
    now: u64,
    input: Vec<u8>,
    output: W,
) -> (u8, String) {
    let call = Call {
        name: "hk",
        verb,
        coordinator: &rig.coordinator(),
        state_dir: &rig.state_dir(),
        args: os(args),
    };
    invoke_as(&call, programs, now, input, output)
}

/// Runs `verb` against the rig with `args` as the tail and `input` as stdin, collecting stdout.
pub fn invoke(
    rig: &Rig,
    verb: Verb,
    args: &[&str],
    programs: &Double,
    now: u64,
    input: Vec<u8>,
) -> Outcome {
    let mut stdout = Vec::new();
    let (code, diag) = invoke_into(rig, verb, args, programs, now, input, &mut stdout);
    Outcome { code, stdout, diag }
}
