//! The fake coordinator and the double for the OpenSSH programs.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use hostkeeper_hkssh::{Cli, Env, Error, Programs, run};
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
    format!("ssh-ed25519{}", hostkeeper_hkssh::CERT_KEY_TYPE_SUFFIX)
}

/// The public key every test uses.
pub const PUBLIC: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAItestkeytestkeytestkeytestkeytestkey";
pub const TOKEN: &str = "operator-secret-token";
pub const HOST: &str = "host-7";
pub const NOW: u64 = 1_000_000;

/// How the next tunnel behaves.
#[derive(Clone, Debug)]
pub enum Tunnel {
    /// `opened`, then every `data` frame echoed, and at end of input `bye` and `host_closed`.
    Echo,
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
    fn new() -> Script {
        Script {
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
                _ => {
                    let _ = out.send(Ok(opened("root"))).await;
                    // A frame a client has no use for (a second `opened`) is passed over.
                    let _ = out.send(Ok(opened("root"))).await;
                    while let Ok(Some(next)) = incoming.message().await {
                        match next.frame {
                            Some(Frame::Data(bytes)) => {
                                script.seen.lock().expect("seen").frames.push(bytes.clone());
                                let _ = out.send(Ok(frame(Frame::Data(bytes)))).await;
                            }
                            Some(Frame::Close(_)) => {
                                let _ = out.send(Ok(frame(Frame::Data(b"bye".to_vec())))).await;
                                let close = Frame::Close(proto::Close {
                                    cause: "host_closed".into(),
                                });
                                let _ = out.send(Ok(frame(close))).await;
                                break;
                            }
                            _ => {}
                        }
                    }
                }
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
    pub scratch: Scratch,
    pub port: u16,
    _runtime: tokio::runtime::Runtime,
}

impl Rig {
    pub fn start() -> Rig {
        let pki = pki();
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "hkssh")
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
        let script = Arc::new(Script::new());
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

    /// The flags every command takes, then `rest`.
    pub fn args(&self, rest: &[&str]) -> Vec<String> {
        let mut args = vec![
            "hkssh".to_owned(),
            "--endpoint".into(),
            format!("https://localhost:{}", self.port),
            "--ca-file".into(),
            self.dir().join("ca.pem").display().to_string(),
            "--token-file".into(),
            self.dir().join("token").display().to_string(),
            "--state-dir".into(),
            self.state_dir().display().to_string(),
        ];
        args.extend(rest.iter().map(|s| (*s).to_owned()));
        args
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
    pub ssh_calls: Mutex<Vec<Vec<String>>>,
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
        std::fs::write(public, format!("{PUBLIC} hkssh\n")).expect("the public key");
        Ok(())
    }

    fn agent_key(&self) -> Option<String> {
        self.agent.clone()
    }

    fn run_ssh(&self, args: &[String]) -> Result<i32, Error> {
        self.ssh_calls.lock().expect("calls").push(args.to_vec());
        Ok(self.ssh_code)
    }
}

/// The outcome of one command line.
pub struct Outcome {
    pub result: Result<i32, Error>,
    pub stdout: Vec<u8>,
    pub diag: String,
}

impl Outcome {
    pub fn text(&self) -> String {
        String::from_utf8(self.stdout.clone()).expect("text")
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

/// Runs `args` (a whole command line, program name first) with `input` as stdin and `output`
/// as stdout; the result and what went to the diagnostics.
pub fn invoke_into<W: tokio::io::AsyncWrite + Unpin>(
    args: &[String],
    programs: &Double,
    now: u64,
    input: Vec<u8>,
    output: W,
) -> (Result<i32, Error>, String) {
    use clap::Parser as _;
    let cli = Cli::try_parse_from(args).expect("a command line that parses");
    let env = Env {
        programs,
        now,
        exe: PathBuf::from("/opt/hkssh"),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let mut diag = Vec::new();
    let result = runtime.block_on(run(
        cli,
        &env,
        std::io::Cursor::new(input),
        output,
        &mut diag,
    ));
    (result, String::from_utf8(diag).expect("text"))
}

/// Runs `args` with `input` as stdin, collecting stdout.
pub fn invoke(args: &[String], programs: &Double, now: u64, input: Vec<u8>) -> Outcome {
    let mut stdout = Vec::new();
    let (result, diag) = invoke_into(args, programs, now, input, &mut stdout);
    Outcome {
        result,
        stdout,
        diag,
    }
}
