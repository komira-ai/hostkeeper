//! `hostkeeper-coordinator serve --lan-listen`: the listener network-installed hosts dial on the
//! farm LAN (docs/design/network-install.md section 7). It needs enrollment and the address of
//! one interface; it serves `Enroll` and `Poll` to a caller with no certificate, the heartbeat
//! and `Renew` to a host certificate only, with the heartbeat listener's checks, and never the
//! operator API, the JSON gateway, the JSON routes of the private listener or the UI; and a
//! daemon dials it by address, which the server certificate names. The connections are in
//! memory and the handshakes the real ones; nothing binds a socket.

mod support;

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use clap::Parser;
use hostkeeper_coordinator::config::{
    Bound, Cli, Command, Parts, Serve, StartError, build, run, start_line,
};
use hostkeeper_enrollment::enroll::{Decision, poll_message};
use hostkeeper_enrollment::names::{HostId, KeyHash, Role};
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_proto::ctl::COMMANDS;
use hostkeeper_proto::hostkeeper::agent::v1::{
    Claims, EnrollRequest, EnrollResponse, EnrollState, Health, HeartbeatRequest,
    HeartbeatResponse, Platform, PollRequest, PollResponse, PollState, RenewRequest, RenewResponse,
    ServiceBeat,
};
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PublicKeyData, SigningKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use support::{BEAT, Ca, Net, OPERATOR, accepting, exchange, scratch};
use tokio_rustls::TlsConnector;
use tonic::{Code, Status};

/// The LAN listener's address in the tests: a documentation address, never dialled for real.
const LAN: &str = "192.0.2.7:3";

/// The LAN listener's IP address, as a daemon dials it.
const LAN_IP: &str = "192.0.2.7";

/// The files the flags name, in a directory of their own.
struct Files {
    dir: Scratch,
}

impl Files {
    fn new(name: &str) -> Self {
        let files = Self { dir: scratch(name) };
        let private = Ca::new("private listener CA");
        let leaf = private.coordinator();
        files.put("private-ca.pem", &private.pem, 0o644);
        files.put("tls.pem", &leaf.cert_pem, 0o644);
        files.put("tls.key", &leaf.key_pem, 0o600);
        files.put("ca.pem", &Ca::new("farm CA").pem, 0o644);
        files.put("oidc.toml", &config_text(STATIC_ISSUER), 0o644);
        files.put("operator.token", OPERATOR, 0o600);
        std::fs::create_dir(files.path("ui")).expect("the UI directory");
        files.put("ui/index.html", "<!doctype html><title>ui</title>", 0o644);
        files
    }

    fn put(&self, file: &str, text: &str, mode: u32) {
        let path = self.path(file);
        std::fs::write(&path, text).expect("written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("mode");
    }

    fn path(&self, file: &str) -> PathBuf {
        self.dir.path().join(file)
    }

    /// The arguments of a `serve` with a UI directory and without enrollment, then `extra`;
    /// each `=name` is the path of `name` in the directory.
    fn args(&self, extra: &[&str]) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
        let base = [
            "--heartbeat-listen",
            "192.0.2.1:1",
            "--private-listen",
            "192.0.2.1:2",
            "--tls-cert",
            "=tls.pem",
            "--tls-key",
            "=tls.key",
            "--client-ca",
            "=ca.pem",
            "--state-file",
            "=desired.json",
            "--oidc-config",
            "=oidc.toml",
            "--operator-token-file",
            "=operator.token",
            "--legacy-service",
            "kbf",
            "--ui-dir",
            "=ui",
        ];
        for arg in base.iter().chain(extra) {
            args.push(match arg.strip_prefix('=') {
                Some(file) => self.path(file).into_os_string(),
                None => OsString::from(arg),
            });
        }
        args
    }
}

/// The enrollment flags, the CA made at the start, then `extra`.
fn with_enrollment<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut flags = vec![
        "--enrollment-dir",
        "=enrollment",
        "--ca-key-file",
        "=ca.key",
        "--ca-init",
        "--server-cert-file",
        "=server.pem",
        "--server-name",
        "localhost",
    ];
    flags.extend_from_slice(extra);
    flags
}

fn flags_of(args: Vec<OsString>) -> Serve {
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command else {
        panic!("serve")
    };
    flags
}

fn start(files: &Files, extra: &[&str]) -> Result<Parts, StartError> {
    let roots = Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)]);
    build(&flags_of(files.args(extra)), roots)
}

/// Proves: `--lan-listen` without enrollment is a usage error (exit 2); a wildcard address,
/// IPv4, IPv6 or IPv4 written as IPv6, is refused at start with `LanListen` and a text that
/// names the flag and the address; the flag is in the usage and read from no environment
/// variable; and a start with it hands the address to `run`.
///
/// Catches: a LAN listener that serves no enrollment (no CA, no certificate a bare machine can
/// check), one bound to every interface (its certificate would name an address no daemon
/// dials, and it would answer on networks it was not meant for), and the address lost between
/// the flags and the listener.
#[test]
fn lan_listen_needs_enrollment_and_the_address_of_one_interface() {
    let files = Files::new("lan-flags");
    assert_eq!(
        hostkeeper_coordinator::config::main(files.args(&["--lan-listen", LAN])),
        ExitCode::from(2)
    );
    for wildcard in ["0.0.0.0:3", "[::]:3", "[::ffff:0.0.0.0]:3"] {
        let want: SocketAddr = wildcard.parse().expect("an address");
        match start(&files, &with_enrollment(&["--lan-listen", wildcard])) {
            Err(StartError::LanListen(got)) => assert_eq!(got, want),
            Err(other) => panic!("refused otherwise: {other}"),
            Ok(_) => panic!("started on {wildcard}"),
        }
    }
    assert_eq!(
        StartError::LanListen("0.0.0.0:3".parse().expect("an address")).to_string(),
        "--lan-listen: 0.0.0.0:3 is a wildcard address; the LAN listener's address is named in \
         the server certificate, so name the address of one interface, or give --lan-listen-wildcard \
         and --lan-address where the port is published on the host"
    );
    assert!(
        !files.path("server.pem").exists(),
        "refused before enrollment starts"
    );
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(help.contains("--lan-listen <LAN_LISTEN>"), "{help}");
    assert!(!help.contains("[env:"), "{help}");

    let parts = start(&files, &with_enrollment(&["--lan-listen", LAN])).expect("started");
    assert_eq!(parts.lan_listen, Some(LAN.parse().expect("an address")));
    drop(parts);
    let parts = start(&files, &with_enrollment(&[])).expect("started without it");
    assert_eq!(parts.lan_listen, None);
}

/// The subjectAltNames of the certificate the server certificate file starts with, as
/// `dns:<name>` and `ip:<bytes>`, and its serial.
fn names_and_serial(files: &Files) -> (Vec<String>, Vec<u8>) {
    let bytes = std::fs::read(files.path("server.pem")).expect("the server certificate");
    let (_, block) = x509_parser::pem::parse_x509_pem(&bytes).expect("PEM");
    let certificate = block.parse_x509().expect("a certificate");
    let san = certificate
        .subject_alternative_name()
        .expect("one")
        .expect("present");
    let names = san
        .value
        .general_names
        .iter()
        .map(|name| match name {
            x509_parser::extensions::GeneralName::DNSName(dns) => format!("dns:{dns}"),
            x509_parser::extensions::GeneralName::IPAddress(ip) => format!("ip:{ip:?}"),
            other => panic!("an unexpected name {other:?}"),
        })
        .collect();
    (names, certificate.raw_serial().to_vec())
}

/// Proves: the server certificate a start makes names the LAN listener's address as an IP
/// address after the DNS names of `--server-name`; a coordinator started before with no LAN
/// listener has its certificate made again when `--lan-listen` is added, and again when the
/// address or a name changes; a restart with the same flags keeps it.
///
/// Catches: a certificate a daemon dialing the address refuses (no IP name), and a changed name
/// list that takes effect only when someone deletes the file.
#[test]
fn the_server_certificate_names_the_lan_address_and_follows_the_flags() {
    let files = Files::new("lan-certificate");
    drop(start(&files, &with_enrollment(&[])).expect("started"));
    let (names, before) = names_and_serial(&files);
    assert_eq!(names, ["dns:localhost"]);

    drop(start(&files, &with_enrollment(&["--lan-listen", LAN])).expect("started"));
    let (names, with_lan) = names_and_serial(&files);
    assert_eq!(names, ["dns:localhost", "ip:[192, 0, 2, 7]"]);
    assert_ne!(with_lan, before, "made again");

    drop(start(&files, &with_enrollment(&["--lan-listen", LAN])).expect("started"));
    assert_eq!(names_and_serial(&files).1, with_lan, "kept: the same names");

    let mapped = "[::ffff:192.0.2.8]:3";
    drop(start(&files, &with_enrollment(&["--lan-listen", mapped])).expect("started"));
    let (names, moved) = names_and_serial(&files);
    assert_eq!(
        names,
        ["dns:localhost", "ip:[192, 0, 2, 8]"],
        "named as IPv4"
    );
    assert_ne!(moved, with_lan, "made again");

    let renamed = with_enrollment(&["--lan-listen", mapped, "--server-name", "coordinator"]);
    drop(start(&files, &renamed).expect("started"));
    let (names, _) = names_and_serial(&files);
    assert_eq!(
        names,
        ["dns:localhost", "dns:coordinator", "ip:[192, 0, 2, 8]"]
    );
}

/// Proves: `--lan-listen-wildcard` and `--lan-address` are in the usage and off unless given;
/// each is a usage error (exit 2) without the other or without `--lan-listen`; with both, each
/// spelling of a wildcard `--lan-listen` is taken as given and the server certificate names the
/// address of `--lan-address` (an IPv4 address written as IPv6 named as IPv4) and not the
/// wildcard; a wildcard `--lan-address` is refused (`LanAddress`, exact text); the wildcard
/// `--lan-listen` without the opt-in is still refused; and an interface address with the opt-in
/// is taken as given while the certificate names `--lan-address`.
///
/// Catches: a wildcard bind whose certificate names no address a daemon dials (or `0.0.0.0`), an
/// opt-in that is on by default or works without the address, and a certificate that is not
/// made again when the address moves.
#[test]
fn a_wildcard_lan_listener_needs_the_address_the_certificate_and_daemons_use() {
    let files = Files::new("lan-wildcard");
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(help.contains("--lan-listen-wildcard"), "{help}");
    assert!(help.contains("--lan-address <LAN_ADDRESS>"), "{help}");

    for flags in [
        with_enrollment(&["--lan-listen", "0.0.0.0:3", "--lan-listen-wildcard"]),
        with_enrollment(&["--lan-listen", LAN, "--lan-address", LAN_IP]),
        with_enrollment(&["--lan-listen-wildcard", "--lan-address", LAN_IP]),
        with_enrollment(&["--lan-address", LAN_IP]),
        with_enrollment(&["--lan-listen-wildcard"]),
    ] {
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&flags)),
            ExitCode::from(2),
            "{flags:?}"
        );
    }

    let refused = start(&files, &with_enrollment(&["--lan-listen", "0.0.0.0:3"]));
    assert!(matches!(refused, Err(StartError::LanListen(_))));

    for (wildcard, address, named) in [
        ("0.0.0.0:3", LAN_IP, "ip:[192, 0, 2, 7]"),
        ("[::]:3", LAN_IP, "ip:[192, 0, 2, 7]"),
        (
            "[::ffff:0.0.0.0]:3",
            "::ffff:192.0.2.8",
            "ip:[192, 0, 2, 8]",
        ),
    ] {
        let flags = with_enrollment(&[
            "--lan-listen",
            wildcard,
            "--lan-listen-wildcard",
            "--lan-address",
            address,
        ]);
        let parts = start(&files, &flags).expect("the opt-in takes it");
        assert_eq!(
            parts.lan_listen,
            Some(wildcard.parse().expect("an address"))
        );
        drop(parts);
        let (names, _) = names_and_serial(&files);
        assert_eq!(names, ["dns:localhost", named], "{wildcard}");
    }

    let flags = with_enrollment(&[
        "--lan-listen",
        LAN,
        "--lan-listen-wildcard",
        "--lan-address",
        "192.0.2.9",
    ]);
    let parts = start(&files, &flags).expect("an interface address with the opt-in");
    assert_eq!(parts.lan_listen, Some(LAN.parse().expect("an address")));
    drop(parts);
    let (names, _) = names_and_serial(&files);
    assert_eq!(names, ["dns:localhost", "ip:[192, 0, 2, 9]"]);

    for address in ["0.0.0.0", "::", "::ffff:0.0.0.0"] {
        let flags = with_enrollment(&[
            "--lan-listen",
            "0.0.0.0:3",
            "--lan-listen-wildcard",
            "--lan-address",
            address,
        ]);
        match start(&files, &flags) {
            Err(StartError::LanAddress(got)) => {
                assert_eq!(got, address.parse::<std::net::IpAddr>().expect("an IP"))
            }
            Err(other) => panic!("refused otherwise: {other}"),
            Ok(_) => panic!("started with {address}"),
        }
    }
    assert_eq!(
        StartError::LanAddress("0.0.0.0".parse().expect("an IP")).to_string(),
        "--lan-address: 0.0.0.0 is a wildcard address; it is the address daemons dial and the \
         server certificate names, so name the address of one interface of the LAN"
    );
}

/// The LAN listener's DNS name in the tests: an example.com name, never resolved.
const LAN_NAME: &str = "coordinator.internal.example.com";

/// The enrollment flags with the LAN listener at [`LAN`] and each of `names` as a `--lan-name`.
fn with_lan_names<'a>(names: &[&'a str]) -> Vec<&'a str> {
    let mut flags = vec!["--lan-listen", LAN];
    for name in names {
        flags.extend(["--lan-name", name]);
    }
    with_enrollment(&flags)
}

/// Proves: `--lan-name` is in the usage; it is a usage error (exit 2) without `--lan-listen`;
/// the server certificate names each name given as a DNS name, after the `--server-name` names
/// and before the address, with and without the wildcard opt-in; a name given to `--server-name`
/// too is named once; a restart with the same names keeps the certificate (in whatever order),
/// and one that adds, changes or drops a name makes it again.
///
/// Catches: a certificate a daemon dialing the DNS name refuses, a changed name list that takes
/// effect only when someone deletes the file, and a certificate made again at every start.
#[test]
fn the_server_certificate_names_the_lan_names_and_follows_them() {
    let files = Files::new("lan-names");
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(help.contains("--lan-name <LAN_NAMES>"), "{help}");
    assert_eq!(
        hostkeeper_coordinator::config::main(
            files.args(&with_enrollment(&["--lan-name", LAN_NAME]))
        ),
        ExitCode::from(2),
        "no --lan-listen"
    );

    drop(start(&files, &with_lan_names(&[LAN_NAME])).expect("started"));
    let (names, first) = names_and_serial(&files);
    assert_eq!(
        names,
        [
            "dns:localhost".to_owned(),
            format!("dns:{LAN_NAME}"),
            "ip:[192, 0, 2, 7]".to_owned()
        ]
    );

    drop(start(&files, &with_lan_names(&[LAN_NAME])).expect("started"));
    assert_eq!(names_and_serial(&files).1, first, "kept: the same names");

    drop(start(&files, &with_lan_names(&[LAN_NAME, "localhost"])).expect("started"));
    assert_eq!(
        names_and_serial(&files).1,
        first,
        "kept: a name of --server-name is named once"
    );

    drop(start(&files, &with_lan_names(&[LAN_NAME, "other.example.com"])).expect("started"));
    let (names, second) = names_and_serial(&files);
    assert_eq!(
        names,
        [
            "dns:localhost".to_owned(),
            format!("dns:{LAN_NAME}"),
            "dns:other.example.com".to_owned(),
            "ip:[192, 0, 2, 7]".to_owned()
        ]
    );
    assert_ne!(second, first, "made again: a name added");

    drop(start(&files, &with_lan_names(&["other.example.com", LAN_NAME])).expect("started"));
    assert_eq!(
        names_and_serial(&files).1,
        second,
        "kept: the order is not looked at"
    );

    drop(start(&files, &with_lan_names(&["third.example.com"])).expect("started"));
    let (names, third) = names_and_serial(&files);
    assert_eq!(
        names,
        [
            "dns:localhost",
            "dns:third.example.com",
            "ip:[192, 0, 2, 7]"
        ]
    );
    assert_ne!(third, second, "made again: names changed and dropped");

    let wildcard = with_enrollment(&[
        "--lan-listen",
        "0.0.0.0:3",
        "--lan-listen-wildcard",
        "--lan-address",
        "192.0.2.9",
        "--lan-name",
        LAN_NAME,
    ]);
    drop(start(&files, &wildcard).expect("started"));
    let (names, _) = names_and_serial(&files);
    assert_eq!(
        names,
        [
            "dns:localhost".to_owned(),
            format!("dns:{LAN_NAME}"),
            "ip:[192, 0, 2, 9]".to_owned()
        ],
        "with the wildcard opt-in"
    );
}

/// Proves: a `--lan-name` that is no DNS name the certificate can name stops the start with
/// `LanName`, whose text names the flag, the name and why, before the enrollment starts (no
/// certificate file is written): an empty name, a trailing dot, a wildcard, an IPv4 and an IPv6
/// address, a name over 253 bytes, an empty label, a label over 63 bytes, a label that starts or
/// ends with `-`, an uppercase letter and an underscore; the longest valid name is taken.
///
/// Catches: a bad name that stops the start only at the certificate's signing with a text that
/// does not name the flag, or one that is taken and makes a certificate no daemon can check.
#[test]
fn a_lan_name_that_is_no_dns_name_stops_the_start() {
    let files = Files::new("lan-name-refused");
    let long_label = "a".repeat(64);
    let long_name = format!("{0}.{0}.{0}.{0}.com", "a".repeat(62));
    for (name, reason) in [
        ("", "is empty"),
        (
            "coordinator.example.com.",
            "has a trailing dot; write the name without it",
        ),
        ("*.example.com", "is a wildcard; name each host"),
        (
            "192.0.2.7",
            "is an IP address; --lan-address or --lan-listen names the address",
        ),
        (
            "2001:db8::1",
            "is an IP address; --lan-address or --lan-listen names the address",
        ),
        (long_name.as_str(), "is longer than 253 bytes"),
        ("a..example.com", "has an empty label"),
        (".example.com", "has an empty label"),
        (long_label.as_str(), "has a label longer than 63 bytes"),
        (
            "a.-b.example.com",
            "has a label that starts or ends with '-'",
        ),
        ("a-.example.com", "has a label that starts or ends with '-'"),
        (
            "Coordinator.example.com",
            "has a character other than a-z, 0-9, '-' and '.'",
        ),
        (
            "a_b.example.com",
            "has a character other than a-z, 0-9, '-' and '.'",
        ),
    ] {
        let flags = with_enrollment(&["--lan-listen", LAN, "--lan-name", name]);
        match start(&files, &flags) {
            Err(error @ StartError::LanName { .. }) => assert_eq!(
                error.to_string(),
                format!("--lan-name {name:?}: {reason}"),
                "{name}"
            ),
            Err(other) => panic!("refused otherwise: {other}"),
            Ok(_) => panic!("started with {name:?}"),
        }
    }
    assert!(
        !files.path("server.pem").exists(),
        "refused before enrollment starts"
    );

    let longest = format!("{0}.{0}.{0}.{1}", "a".repeat(63), "a".repeat(61));
    assert_eq!(longest.len(), 253);
    let flags = with_enrollment(&[
        "--lan-listen",
        LAN,
        "--lan-name",
        &longest,
        "--lan-name",
        "a-1.b",
    ]);
    drop(start(&files, &flags).expect("the longest valid name is taken"));
    let (names, _) = names_and_serial(&files);
    assert_eq!(
        names,
        [
            "dns:localhost".to_owned(),
            format!("dns:{longest}"),
            "dns:a-1.b".to_owned(),
            "ip:[192, 0, 2, 7]".to_owned()
        ]
    );
}

/// Proves: a caller that dials the LAN listener by the DNS name of `--lan-name`, with the farm
/// CA as its only root, finishes the handshake against the certificate the start made and posts
/// `Enroll` (answered pending); one that dials by a DNS name the certificate does not name is
/// refused in the handshake.
///
/// Catches: a certificate that names the address and not the name the daemons dial, and one that
/// names more than it was asked.
#[tokio::test]
async fn a_daemon_dials_the_lan_listener_by_its_dns_name() {
    let files = Files::new("lan-dns-dial");
    let flags = with_enrollment(&["--lan-listen", LAN, "--lan-name", LAN_NAME]);
    let parts = start(&files, &flags).expect("started");
    let ca = parts
        .coordinator
        .enrollment()
        .expect("enrollment")
        .ca_bundle()
        .certificate_der()
        .to_vec();
    let running = serve(parts).await;
    let addr = running.bound.lan.expect("the LAN listener");
    let config = client(&ca, None);

    let key = HostKey::new();
    let (status, body) = call(
        &running.net,
        addr,
        LAN_NAME,
        &config,
        ENROLL,
        None,
        &key.enroll("machine-1"),
    )
    .await;
    assert_eq!((status.code(), status.message()), (Code::Ok, ""));
    let enrolled = EnrollResponse::decode(body).expect("an answer");
    assert_eq!(enrolled.state, EnrollState::Pending as i32);

    let stream = running.net.connect(addr).expect("connected");
    let name = ServerName::try_from("unnamed.example.com").expect("a name");
    let refused = TlsConnector::from(config).connect(name, stream).await;
    assert!(refused.is_err(), "a name the certificate does not carry");
}

/// A running coordinator on an in-memory network: the addresses it bound, in the order it bound
/// them, and the network.
struct Running {
    net: Net,
    bound: Bound,
    asked: Vec<SocketAddr>,
    serving: tokio::task::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

/// Serves `parts` on a network of its own.
async fn serve(parts: Parts) -> Running {
    let net = Net::default();
    let registry = net.clone();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    let bind = move |addr: SocketAddr| {
        log.lock().expect("log").push(addr);
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
    let bound = bound.await.expect("bound");
    let asked = asked.lock().expect("log").clone();
    Running {
        net,
        bound,
        asked,
        serving,
    }
}

/// A client that trusts the DER certificate `ca`, offers `h2` and `http/1.1`, and presents
/// `identity` (a certificate and its PKCS#8 key) if given.
fn client(ca: &[u8], identity: Option<(&[u8], &[u8])>) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca.to_vec()))
        .expect("a root");
    let builder =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_root_certificates(roots);
    let mut config = match identity {
        Some((certificate, key)) => builder
            .with_client_auth_cert(
                vec![CertificateDer::from(certificate.to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec())),
            )
            .expect("an identity"),
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Arc::new(config)
}

/// The gRPC call of `path` with `message`, over HTTP/2 on a new connection to `addr` that
/// checks the server's certificate against the name `name`: the status the call ended with and
/// the bytes of its answer.
async fn call(
    net: &Net,
    addr: SocketAddr,
    name: &str,
    config: &Arc<ClientConfig>,
    path: &str,
    bearer: Option<&str>,
    message: &impl Message,
) -> (Status, Bytes) {
    let stream = net.connect(addr).expect("connected");
    let name = ServerName::try_from(name.to_owned()).expect("a name");
    let tls = TlsConnector::from(Arc::clone(config))
        .connect(name, stream)
        .await
        .expect("a handshake");
    let (mut send, driver) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .expect("an HTTP/2 handshake");
    tokio::spawn(driver);
    let encoded = message.encode_to_vec();
    let mut framed = BytesMut::with_capacity(5 + encoded.len());
    framed.put_u8(0);
    framed.put_u32(u32::try_from(encoded.len()).expect("a small message"));
    framed.put_slice(&encoded);
    let request = Request::builder()
        .method("POST")
        .uri(format!("https://{LAN_IP}{path}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    let request = match bearer {
        Some(token) => request.header("authorization", format!("Bearer {token}")),
        None => request,
    };
    let request = request.body(Full::new(framed.freeze())).expect("a request");
    let response = send.send_request(request).await.expect("an answer");
    let (parts, body) = response.into_parts();
    let collected = body.collect().await.expect("the body");
    let status = collected
        .trailers()
        .and_then(Status::from_header_map)
        .or_else(|| Status::from_header_map(&parts.headers))
        .expect("a grpc-status");
    let mut bytes = collected.to_bytes();
    let message = if bytes.len() >= 5 {
        bytes.advance(5);
        bytes
    } else {
        Bytes::new()
    };
    (status, message)
}

/// The P-256 key of a host and the CSR of `Enroll` and `Renew` for it.
struct HostKey {
    pair: KeyPair,
}

impl HostKey {
    fn new() -> Self {
        Self {
            pair: KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key"),
        }
    }

    fn csr(&self) -> Vec<u8> {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "host:pending");
        params
            .serialize_request(&self.pair)
            .expect("a CSR")
            .der()
            .to_vec()
    }

    fn id(&self) -> KeyHash {
        KeyHash::of_spki(&self.pair.subject_public_key_info())
    }

    fn enroll(&self, machine_id: &str) -> EnrollRequest {
        EnrollRequest {
            csr_der: self.csr(),
            claims: Some(Claims {
                machine_id: machine_id.to_owned(),
                ..Claims::default()
            }),
            ..EnrollRequest::default()
        }
    }
}

/// A coordinator on an in-memory network, with enrollment and the LAN listener, and the DER of
/// its CA certificate.
struct Lan {
    _files: Files,
    running: Running,
    ca: Vec<u8>,
    enrollment: Arc<hostkeeper_coordinator::enrollment::Enrollment>,
}

impl Lan {
    async fn start(name: &str) -> Self {
        let files = Files::new(name);
        let parts = start(&files, &with_enrollment(&["--lan-listen", LAN])).expect("started");
        let enrollment = Arc::clone(parts.coordinator.enrollment().expect("enrollment"));
        let ca = enrollment.ca_bundle().certificate_der().to_vec();
        let running = serve(parts).await;
        Self {
            _files: files,
            running,
            ca,
            enrollment,
        }
    }

    fn lan(&self) -> SocketAddr {
        self.running.bound.lan.expect("the LAN listener")
    }

    /// The gRPC call of `path` on the LAN listener, dialed by its address, by a caller that
    /// presents `identity` if given.
    async fn call(
        &self,
        identity: Option<(&[u8], &[u8])>,
        path: &str,
        bearer: Option<&str>,
        message: &impl Message,
    ) -> (Status, Bytes) {
        let config = client(&self.ca, identity);
        call(
            &self.running.net,
            self.lan(),
            LAN_IP,
            &config,
            path,
            bearer,
            message,
        )
        .await
    }

    /// An HTTP/1.1 exchange with the listener at `addr`, by a caller with no certificate that
    /// trusts `ca`.
    async fn http1(
        &self,
        addr: SocketAddr,
        ca: &[u8],
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: &str,
    ) -> (u16, String) {
        let mut config = (*client(ca, None)).clone();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        exchange(
            &self.running.net,
            addr,
            &Arc::new(config),
            method,
            path,
            bearer,
            body,
        )
        .await
        .expect("answered")
    }

    /// `Enroll` and then `Poll` of `key` over the LAN listener with no certificate; when the
    /// request is approved, the certificate it was issued.
    async fn enroll_and_poll(&self, key: &HostKey, machine_id: &str) -> PollResponse {
        let (status, body) = self.call(None, ENROLL, None, &key.enroll(machine_id)).await;
        assert_eq!(status.code(), Code::Ok, "{status:?}");
        let enrolled = EnrollResponse::decode(body).expect("an answer");
        assert_eq!(enrolled.request_id, key.id().to_hex());
        let poll = PollRequest {
            request_id: enrolled.request_id.clone(),
            ..PollRequest::default()
        };
        let (status, body) = self.call(None, POLL, None, &poll).await;
        assert_eq!(status.code(), Code::Ok, "{status:?}");
        let nonce = PollResponse::decode(body).expect("an answer").nonce;
        let signature = key
            .pair
            .sign(&poll_message(&key.id(), &nonce))
            .expect("signed");
        let signed = PollRequest {
            request_id: enrolled.request_id,
            nonce,
            signature,
        };
        let (status, body) = self.call(None, POLL, None, &signed).await;
        assert_eq!(status.code(), Code::Ok, "{status:?}");
        PollResponse::decode(body).expect("an answer")
    }

    /// Approves the request of `key` as the storage host `s1`, replacing the key it held when
    /// `replace`.
    fn approve(&self, key: &HostKey, replace: bool) {
        self.enrollment
            .store()
            .approve(
                &key.id(),
                &key.id(),
                &Decision {
                    host_id: HostId::parse("s1").expect("a host id"),
                    role: Role::parse("storage").expect("a role"),
                    services: Vec::new(),
                    replace,
                    host_group: None,
                },
            )
            .expect("approved");
    }
}

const ENROLL: &str = "/hostkeeper.agent.v1.Coordinator/Enroll";
const POLL: &str = "/hostkeeper.agent.v1.Coordinator/Poll";
const HEARTBEAT: &str = "/hostkeeper.agent.v1.Coordinator/Heartbeat";
const RENEW: &str = "/hostkeeper.agent.v1.Coordinator/Renew";

/// A heartbeat of a host that runs kbf 1.0, as `Coordinator.Heartbeat` carries it.
fn heartbeat() -> HeartbeatRequest {
    HeartbeatRequest {
        platform: Platform::LinuxX8664 as i32,
        services: vec![ServiceBeat {
            name: "kbf".to_owned(),
            version: "1.0".to_owned(),
            health: Health::Healthy as i32,
            ..ServiceBeat::default()
        }],
        ..HeartbeatRequest::default()
    }
}

/// Proves: `run` binds the heartbeat listener, the private one and then the LAN listener, and
/// says where all three are; on the LAN listener, a caller with no certificate that dials the
/// listener by its IP address finishes the handshake, posts `Enroll` (pending) and is answered
/// an unsigned `Poll` (a nonce to sign); its `Heartbeat` and `Renew` are refused `PERMISSION_DENIED`
/// `forbidden`, and its JSON heartbeat 403 `forbidden`.
///
/// Catches: a LAN listener that is not bound or not reported, one that asks a bare machine for
/// a certificate (it could never enroll), one whose certificate does not name its address (the
/// daemon's handshake would fail), and one that lets a caller without a certificate
/// heartbeat or renew.
#[tokio::test]
async fn the_lan_listener_serves_enrollment_without_a_certificate_and_nothing_else_to_one() {
    let lan = Lan::start("lan-anonymous").await;
    assert_eq!(
        lan.running.asked,
        ["192.0.2.1:1", "192.0.2.1:2", LAN].map(|a| a.parse::<SocketAddr>().expect("an address"))
    );
    assert_eq!(lan.lan(), LAN.parse::<SocketAddr>().expect("an address"));

    let key = HostKey::new();
    let (status, body) = lan.call(None, ENROLL, None, &key.enroll("machine-1")).await;
    assert_eq!((status.code(), status.message()), (Code::Ok, ""));
    let enrolled = EnrollResponse::decode(body).expect("an answer");
    assert_eq!(
        (enrolled.request_id.as_str(), enrolled.state),
        (key.id().to_hex().as_str(), EnrollState::Pending as i32)
    );
    let poll = PollRequest {
        request_id: enrolled.request_id,
        ..PollRequest::default()
    };
    let (status, body) = lan.call(None, POLL, None, &poll).await;
    assert_eq!((status.code(), status.message()), (Code::Ok, ""));
    let polled = PollResponse::decode(body).expect("an answer");
    assert!(!polled.nonce.is_empty(), "a nonce to sign");
    assert_eq!(
        polled,
        PollResponse {
            nonce: polled.nonce.clone(),
            ..PollResponse::default()
        },
        "a nonce and nothing else"
    );

    let (status, _) = lan.call(None, HEARTBEAT, None, &heartbeat()).await;
    assert_eq!(
        (status.code(), status.message()),
        (Code::PermissionDenied, "forbidden")
    );
    let renew = RenewRequest { csr_der: key.csr() };
    let (status, _) = lan.call(None, RENEW, None, &renew).await;
    assert_eq!(
        (status.code(), status.message()),
        (Code::PermissionDenied, "forbidden")
    );
    let json = lan
        .http1(lan.lan(), &lan.ca, "POST", "/v1/heartbeat", None, BEAT)
        .await;
    assert_eq!(json, (403, r#"{"error":"forbidden"}"#.to_owned()));
}

/// Proves: on the LAN listener every method of the operator API, called with the operator's
/// token, is `UNIMPLEMENTED` with no message; a JSON gateway call of the operator API, the
/// operator's JSON routes and the UI's files are not found (404 `not_found`), where the private
/// listener of the same coordinator answers the same gateway call and the same UI path 200.
///
/// Catches: a LAN listener that serves the private surface, or any part of it, to the farm LAN.
#[tokio::test]
async fn the_lan_listener_serves_no_operator_call_gateway_route_or_ui() {
    let lan = Lan::start("lan-operator").await;
    let paths: Vec<&str> = COMMANDS
        .iter()
        .filter(|command| command.package == "hostkeeper.operator.v1")
        .map(|command| command.path)
        .collect();
    assert!(paths.len() > 10, "{paths:?}");
    for path in paths {
        let (status, body) = lan.call(None, path, Some(OPERATOR), &()).await;
        assert_eq!(
            (status.code(), status.message(), body),
            (Code::Unimplemented, "", Bytes::new()),
            "{path}"
        );
    }

    let private_ca = {
        let pem = std::fs::read(lan._files.path("private-ca.pem")).expect("the private CA");
        let (_, block) = x509_parser::pem::parse_x509_pem(&pem).expect("PEM");
        block.contents
    };
    let control = lan.running.bound.control;
    let list = "/hostkeeper.operator.v1.Hosts/List";
    let ui = "/ui/";
    let on_control = lan
        .http1(control, &private_ca, "POST", list, Some(OPERATOR), "{}")
        .await;
    assert_eq!(on_control.0, 200, "{on_control:?}");
    let ui_on_control = lan.http1(control, &private_ca, "GET", ui, None, "").await;
    assert_eq!(ui_on_control.0, 200, "{ui_on_control:?}");

    let not_found = (404, r#"{"error":"not_found"}"#.to_owned());
    for (method, path, body) in [
        ("POST", list, "{}"),
        ("GET", ui, ""),
        ("GET", "/ui/index.html", ""),
        ("GET", "/v1/status", ""),
        ("GET", "/v1/convergence", ""),
        ("POST", "/v1/operator", "{}"),
        ("POST", "/v1/desired", "{}"),
    ] {
        let answer = lan
            .http1(lan.lan(), &lan.ca, method, path, Some(OPERATOR), body)
            .await;
        assert_eq!(answer, not_found, "{method} {path}");
    }
}

/// Proves: a host that enrolled over the LAN listener (`Enroll`, approval, signed `Poll`)
/// heartbeats there with the certificate it was issued, over gRPC and the JSON route, and
/// renews it there; after the machine is reinstalled with a new key and the new request is
/// approved in place of the old (`replace`), the old certificate's heartbeat on the LAN
/// listener is refused `forbidden` and the new one's is answered.
///
/// Catches: a LAN listener that does not admit a host certificate of the coordinator's CA (a
/// network-installed host would enroll and then never heartbeat), one that checks less than the
/// heartbeat listener does (the replaced key would still speak for the host), and a `Renew` that
/// does not reach its handler there.
#[tokio::test]
async fn an_enrolled_host_heartbeats_and_renews_on_the_lan_listener_until_it_is_replaced() {
    let lan = Lan::start("lan-host").await;
    let old = HostKey::new();
    let _ = lan.enroll_and_poll(&old, "machine-1").await;
    lan.approve(&old, false);
    let issued = lan.enroll_and_poll(&old, "machine-1").await;
    assert_eq!(issued.state, PollState::Issued as i32);
    let old_identity = (issued.certificate_der, old.pair.serialize_der());
    let as_old = Some((old_identity.0.as_slice(), old_identity.1.as_slice()));

    let (status, body) = lan.call(as_old, HEARTBEAT, None, &heartbeat()).await;
    assert_eq!((status.code(), status.message()), (Code::Ok, ""));
    HeartbeatResponse::decode(body).expect("an answer");
    let mut config = (*client(&lan.ca, as_old)).clone();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let json = exchange(
        &lan.running.net,
        lan.lan(),
        &Arc::new(config),
        "POST",
        "/v1/heartbeat",
        None,
        BEAT,
    )
    .await
    .expect("answered");
    assert_eq!(json, (200, r#"{"reply":"nothing"}"#.to_owned()));

    let renew = RenewRequest { csr_der: old.csr() };
    let (status, body) = lan.call(as_old, RENEW, None, &renew).await;
    assert_eq!((status.code(), status.message()), (Code::Ok, ""));
    let renewed = RenewResponse::decode(body).expect("an answer");
    assert!(!renewed.certificate_der.is_empty());
    assert_ne!(renewed.certificate_der, old_identity.0, "a new certificate");

    let new = HostKey::new();
    let _ = lan.enroll_and_poll(&new, "machine-1").await;
    lan.approve(&new, true);
    let issued = lan.enroll_and_poll(&new, "machine-1").await;
    assert_eq!(issued.state, PollState::Issued as i32);
    let new_key = new.pair.serialize_der();
    let as_new = Some((issued.certificate_der.as_slice(), new_key.as_slice()));

    let (status, _) = lan.call(as_old, HEARTBEAT, None, &heartbeat()).await;
    assert_eq!(
        (status.code(), status.message()),
        (Code::PermissionDenied, "forbidden"),
        "the old key names the host no more"
    );
    let renewed_identity = Some((
        renewed.certificate_der.as_slice(),
        old_identity.1.as_slice(),
    ));
    let (status, _) = lan
        .call(renewed_identity, HEARTBEAT, None, &heartbeat())
        .await;
    assert_eq!(
        (status.code(), status.message()),
        (Code::PermissionDenied, "forbidden"),
        "nor does its renewed certificate"
    );
    let (status, _) = lan.call(as_new, HEARTBEAT, None, &heartbeat()).await;
    assert_eq!((status.code(), status.message()), (Code::Ok, ""));
}

/// Proves: the line `serve` prints once its listeners are bound names the LAN listener's bound
/// address as `lan=<address:port>` after `heartbeat=` and `control=`, and has no `lan=` word
/// when there is no LAN listener.
///
/// Catches: a start line that leaves out where the LAN listener is (the end-to-end test of
/// network install reads the address from it), or that names it where there is none.
#[tokio::test]
async fn the_start_line_names_the_lan_listener_after_the_other_two() {
    let lan = Lan::start("lan-start-line").await;
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        start_line(&lan.running.bound),
        format!(
            "hostkeeper-coordinator {version} heartbeat=192.0.2.1:1 control=192.0.2.1:2 \
             lan=192.0.2.7:3"
        )
    );
    let without = Bound {
        lan: None,
        ..lan.running.bound
    };
    assert_eq!(
        start_line(&without),
        format!("hostkeeper-coordinator {version} heartbeat=192.0.2.1:1 control=192.0.2.1:2")
    );
}
