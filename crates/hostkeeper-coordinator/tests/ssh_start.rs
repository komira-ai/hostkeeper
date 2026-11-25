//! `serve` with the SSH flags on a database (`docs/design/ssh-through-coordinator.md`, section
//! 10): the coordinator that is started with `--ssh-ca-secret-file` holds tunnels, and one
//! started without answers `Ssh` `not_configured`; the tunnel flags are checked at start; and a
//! stop with no tunnel open ends at once. The connections are in memory (real TLS, real
//! HTTP/2), the database a test's own.
//!
//! A tunnel that is carried to a host's sshd is tested in the crate (`ssh_tunnel_serve_tests.rs`,
//! over the same listener code): the port the coordinator dials is the host's 22, which a test
//! has no listener on, and no flag moves it.

mod support;

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::{BufMut, Bytes, BytesMut};
use hostkeeper_coordinator::config::{Parts, StartError, run, start};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use support::database::Database;
use support::serve::{Files, build_given, exit_code, flags_of, refusal, roots, with, without};
use support::{Ca, Net, OPERATOR, accepting};
use tokio_rustls::TlsConnector;
use tonic::{Code, Status};

const PRIVATE: &str = "192.0.2.1:2";

const HOST_KEYS: &str = "/hostkeeper.operator.v1.Ssh/HostKeys";
const TUNNEL: &str = "/hostkeeper.operator.v1.Ssh/Tunnel";

/// The files of a start on `database` that holds tunnels: the CA key `ssh-ca init` made, an
/// operators file that lists the operator of the token, and the flags that name them; `extra`
/// after them.
fn ssh_args(files: &Files, database: &Database, extra: &[(&str, &str)]) -> Vec<OsString> {
    files.put("database.dsn", &database.dsn_text(), 0o600);
    files.put(
        "operators.toml",
        "[[operator]]\nname = \"operator\"\nclasses = [\"fleet\"]\n",
        0o600,
    );
    let keys = files.path("ca-keys");
    let made = exit_code(
        ["hostkeeper-coordinator", "ssh-ca", "init", "--out"]
            .into_iter()
            .map(OsString::from)
            .chain([keys.clone().into()])
            .collect(),
    );
    assert_eq!(made, std::process::ExitCode::SUCCESS);
    let mut args = without(&files.args(), "--state-file");
    let path = |file: &str| files.path(file).display().to_string();
    let named = [
        ("--database-dsn-file", path("database.dsn")),
        ("--halt-marker-file", path("halt.marker")),
        (
            "--ssh-ca-secret-file",
            keys.join("ssh_user_ca").display().to_string(),
        ),
        ("--ssh-operators-file", path("operators.toml")),
    ];
    for (flag, value) in named {
        args.push(flag.into());
        args.push(value.into());
    }
    for (flag, value) in extra {
        args.push((*flag).into());
        args.push((*value).into());
    }
    args
}

/// The same start with no SSH flag.
fn plain_args(files: &Files, database: &Database) -> Vec<OsString> {
    files.put("database.dsn", &database.dsn_text(), 0o600);
    let mut args = without(&files.args(), "--state-file");
    for (flag, file) in [
        ("--database-dsn-file", "database.dsn"),
        ("--halt-marker-file", "halt.marker"),
    ] {
        args.push(flag.into());
        args.push(files.path(file).into());
    }
    args
}

/// Runs `work` on a runtime of its own, which is gone when it returns: the tasks it spawned are
/// dropped, so a coordinator it served no longer holds its database. The tests are not async
/// themselves: the database helpers are blocking clients, which a runtime refuses.
fn on_runtime<F: std::future::Future>(work: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let out = runtime.block_on(work);
    drop(runtime);
    out
}

/// A running coordinator on an in-memory network, with the CA of its private listener's
/// certificate.
struct Running {
    net: Net,
    ca: Vec<u8>,
    serving: tokio::task::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

/// Serves `parts` on a network of its own.
async fn serve(parts: Parts, ca: Vec<u8>) -> Running {
    let net = Net::default();
    let registry = net.clone();
    let bind = move |addr: SocketAddr| {
        let receive = registry.listen(addr);
        async move { Ok::<_, io::Error>((addr, accepting(receive))) }
    };
    let (told, bound) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        let _ = run(parts, bind, move |bound| {
            let _ = told.send(bound);
        })
        .await;
    });
    bound.await.expect("bound");
    Running { net, ca, serving }
}

/// The gRPC call of `path` with the operator's token, whose body is `body`, over HTTP/2 to the
/// private listener: the status it ended with.
async fn call(running: &Running, path: &str, body: Bytes) -> Status {
    let mut roots = RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(running.ca.clone()))
        .expect("a root");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let stream = running
        .net
        .connect(PRIVATE.parse().expect("an address"))
        .expect("connected");
    let tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("localhost").expect("a name"), stream)
        .await
        .expect("a handshake");
    let (mut send, driver) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .expect("an HTTP/2 handshake");
    tokio::spawn(driver);
    let request = Request::builder()
        .method("POST")
        .uri(format!("https://localhost{path}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {OPERATOR}"))
        .body(Full::new(body))
        .expect("a request");
    let response = send.send_request(request).await.expect("an answer");
    let (parts, body) = response.into_parts();
    let collected = body.collect().await.expect("the body");
    collected
        .trailers()
        .and_then(Status::from_header_map)
        .or_else(|| Status::from_header_map(&parts.headers))
        .expect("a grpc-status")
}

/// A gRPC message of `message`.
fn framed(message: &impl Message) -> Bytes {
    let encoded = message.encode_to_vec();
    let mut bytes = BytesMut::new();
    bytes.put_u8(0);
    bytes.put_u32(u32::try_from(encoded.len()).expect("a short message"));
    bytes.put_slice(&encoded);
    bytes.freeze()
}

fn host_keys(host: &str) -> Bytes {
    framed(
        &hostkeeper_proto::hostkeeper::operator::v1::HostKeysRequest {
            host_id: host.to_owned(),
        },
    )
}

fn tunnel_open(host: &str) -> Bytes {
    use hostkeeper_proto::hostkeeper::operator::v1 as proto;
    framed(&proto::TunnelFrame {
        frame: Some(proto::tunnel_frame::Frame::Open(proto::Open {
            host_id: host.to_owned(),
            cert_serial: 1,
        })),
    })
}

/// Makes the private listener's certificate and key a CA's, in `files`: the DER of the CA.
fn own_tls(files: &Files) -> Vec<u8> {
    let ca = Ca::new("private listener CA");
    let leaf = ca.coordinator();
    files.put("tls.pem", &leaf.cert_pem, 0o644);
    files.put("tls.key", &leaf.key_pem, 0o600);
    ca.der
}

/// Proves: a coordinator started with the SSH flags on a database answers `HostKeys` and the
/// first frame of `Tunnel` as one that holds tunnels (`NOT_FOUND` `unknown_host` for a host that
/// is none), over HTTP/2 on its private listener; the same start without the flags answers both
/// `FAILED_PRECONDITION` `not_configured`.
///
/// Catches: a start that signs and holds no tunnels (the methods would stay `not_configured`), a
/// start that builds tunnels the CA flags did not ask for, and a tunnel path the router does not
/// reach.
#[test]
fn a_coordinator_started_with_the_ssh_flags_holds_tunnels_and_one_without_does_not() {
    let database = Database::new();
    let files = Files::new("ssh-start-on");
    let ca = own_tls(&files);
    let on = build_given(&flags_of(ssh_args(&files, &database, &[]))).expect("a start");
    let asked = ca.clone();
    on_runtime(async move {
        let running = serve(on, asked).await;
        for (path, body) in [
            (HOST_KEYS, host_keys("node-99")),
            (TUNNEL, tunnel_open("node-99")),
        ] {
            let status = call(&running, path, body).await;
            assert_eq!(
                (status.code(), status.message()),
                (Code::NotFound, "unknown_host"),
                "{path}"
            );
        }
    });
    database.lock_is_free();

    let off = build_given(&flags_of(plain_args(&files, &database))).expect("a start");
    on_runtime(async move {
        let running = serve(off, ca).await;
        for (path, body) in [
            (HOST_KEYS, host_keys("node-99")),
            (TUNNEL, tunnel_open("node-99")),
        ] {
            let status = call(&running, path, body).await;
            assert_eq!(
                (status.code(), status.message()),
                (Code::FailedPrecondition, "not_configured"),
                "{path}"
            );
        }
    });
}

/// Proves: a tunnel flag outside its range, or given without `--ssh-ca-secret-file`, is a usage
/// error at start (exit 2); the flags at the two ends of their ranges start.
///
/// Catches: a flag that is taken whatever its value, or that is accepted on a coordinator that
/// holds no CA.
#[test]
fn the_tunnel_flags_are_checked_at_start() {
    let database = Database::new();
    let files = Files::new("ssh-start-flags");
    let args = ssh_args(&files, &database, &[]);
    for (flag, value) in [
        ("--ssh-live-s", "9"),
        ("--ssh-idle-s", "86401"),
        ("--ssh-max-session-s", "59"),
        ("--ssh-dial-timeout-s", "0"),
        ("--ssh-max-tunnels", "1025"),
        ("--ssh-max-tunnels-per-operator", "65"),
        ("--ssh-revoke-poll-s", "301"),
        ("--ssh-drain-s", "3601"),
    ] {
        let mut bad = args.clone();
        bad.push(flag.into());
        bad.push(value.into());
        assert_eq!(
            exit_code(bad),
            std::process::ExitCode::from(2),
            "{flag} {value}"
        );
    }
    let mut alone = plain_args(&files, &database);
    alone.push("--ssh-drain-s".into());
    alone.push("0".into());
    assert_eq!(exit_code(alone), std::process::ExitCode::from(2));
    for (flag, value) in [("--ssh-drain-s", "0"), ("--ssh-max-tunnels", "1024")] {
        let mut good = args.clone();
        good.push(flag.into());
        good.push(value.into());
        build_given(&flags_of(good)).expect("a start");
    }
}

/// Proves: `start` of a coordinator that holds tunnels, none of them open, ends `Ok` as soon as
/// the stop resolves, though the drain is the default 600 seconds, and the database's lock is
/// free after it.
///
/// Catches: a stop that waits out `--ssh-drain-s` for tunnels that are not there, or that leaves
/// the database held.
#[test]
fn a_stop_with_no_tunnel_open_ends_at_once() {
    let database = Database::new();
    let files = Files::new("ssh-start-stop");
    let args = ssh_args(
        &files,
        &database,
        &[("--ssh-drain-s", "3600"), ("--drain-delay-secs", "0")],
    );
    let bound = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&bound);
    let bind = move |addr: SocketAddr| {
        log.lock().expect("log").push(addr);
        let (_keep, receive) = tokio::sync::mpsc::unbounded_channel();
        async move { Ok::<_, io::Error>((addr, accepting(receive))) }
    };
    let stop = async {
        tokio::task::yield_now().await;
        Ok(())
    };
    let flags = flags_of(args);
    on_runtime(start(flags, roots(), bind, stop)).expect("a stop is a clean end");
    assert_eq!(bound.lock().expect("log").len(), 2);
    database.lock_is_free();
}

/// Proves: a start whose SSH CA secret is not a key is refused at start naming the flag.
///
/// Catches: a coordinator that starts holding tunnels it can never sign certificates for.
#[test]
fn a_ca_secret_that_is_not_a_key_refuses_the_start() {
    let database = Database::new();
    let files = Files::new("ssh-start-refused");
    let args = ssh_args(&files, &database, &[]);
    files.put("junk", "not a key", 0o600);
    let junk = files.path("junk").display().to_string();
    let why = refusal(with(&args, "--ssh-ca-secret-file", &junk));
    assert!(matches!(why, StartError::Ssh(_)), "{why}");
    assert!(why.to_string().contains("--ssh-ca-secret-file"), "{why}");
}
