//! `hostkeeper-coordinator serve` with enrollment: its flags go together, the start makes the
//! CA only with `--ca-init` and the server certificate, and the heartbeat listener lets a client
//! without a certificate through the handshake (the heartbeat route refuses it) and admits the
//! certificate of a host its CA enrolled. The connections are in-memory and the handshakes the
//! real ones; nothing binds a socket.

mod support;

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use hostkeeper_coordinator::config::{Cli, Command, Parts, Serve, StartError, build, run};
use hostkeeper_coordinator::enrollment::{Enrollment, SetupError};
use hostkeeper_enrollment::enroll::{Decision, poll_message};
use hostkeeper_enrollment::names::{HostId, KeyHash, Role};
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_proto::hostkeeper::agent::v1::{Claims, EnrollRequest, PollRequest, PollState};
use hostkeeper_testkit::Scratch;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PublicKeyData, SigningKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ClientConfig, RootCertStore};
use support::{BEAT, Ca, Net, OPERATOR, accepting, exchange, recorded, scratch};

/// The files the flags name, in a directory of their own.
struct Files {
    dir: Scratch,
}

impl Files {
    fn new(name: &str) -> Self {
        let files = Self { dir: scratch(name) };
        let leaf = Ca::new("private listener CA").coordinator();
        files.put("tls.pem", &leaf.cert_pem, 0o644);
        files.put("tls.key", &leaf.key_pem, 0o600);
        files.put("ca.pem", &Ca::new("farm CA").pem, 0o644);
        files.put("oidc.toml", &config_text(STATIC_ISSUER), 0o644);
        files.put("operator.token", OPERATOR, 0o600);
        files.put("rollout.toml", &recorded("services.toml"), 0o600);
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

    /// The arguments of a `serve` without enrollment, then `extra`; each `=name` is the path of
    /// `name` in the directory.
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
            "--rollout-file",
            "=rollout.toml",
            "--legacy-service",
            "kbf",
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

/// The enrollment flags, with `--ca-init` when `init`.
fn enrollment_flags(init: bool) -> Vec<&'static str> {
    let mut flags = vec![
        "--enrollment-dir",
        "=enrollment",
        "--ca-key-file",
        "=ca.key",
        "--server-cert-file",
        "=server.pem",
        "--server-name",
        "localhost",
    ];
    if init {
        flags.push("--ca-init");
    }
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

/// Catches an enrollment flag accepted without the others, a missing CA made without
/// `--ca-init`, and a start with enrollment that does not make the CA and the server
/// certificate or does not give the coordinator its enrollment.
#[test]
fn enrollment_flags_go_together_and_a_missing_ca_needs_ca_init() {
    let files = Files::new("enrollment-flags");
    for alone in [
        vec!["--enrollment-dir", "=enrollment"],
        vec!["--ca-key-file", "=ca.key"],
        vec!["--server-name", "localhost"],
        vec!["--ca-init"],
    ] {
        let args = files.args(&alone);
        assert_eq!(
            hostkeeper_coordinator::config::main(args),
            ExitCode::from(2),
            "{alone:?}"
        );
    }

    match start(&files, &enrollment_flags(false)) {
        Err(StartError::Enrollment(SetupError::NoCa(path))) => {
            assert_eq!(path, files.path("ca.key"));
        }
        Err(other) => panic!("refused otherwise: {other}"),
        Ok(_) => panic!("started without a CA"),
    }
    assert!(!files.path("ca.key").exists());

    let parts = start(&files, &enrollment_flags(true)).expect("started");
    assert!(parts.coordinator.enrollment().is_some());
    for made in ["ca.key", "ca.key.cert", "server.pem"] {
        assert!(files.path(made).is_file(), "{made}");
    }
    // The state files' lock is the running coordinator's: a restart takes it after it.
    drop(parts);
    let again = start(&files, &enrollment_flags(false)).expect("started again");
    assert!(again.coordinator.enrollment().is_some());
}

/// The `Enroll` of a new P-256 key from machine `machine_id`.
fn new_request(machine_id: &str) -> EnrollRequest {
    let pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key");
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "host:pending");
    EnrollRequest {
        csr_der: params
            .serialize_request(&pair)
            .expect("a CSR")
            .der()
            .to_vec(),
        claims: Some(Claims {
            machine_id: machine_id.to_owned(),
            ..Claims::default()
        }),
        ..EnrollRequest::default()
    }
}

/// An allowlist of one Mac serial, approved as `mac-<serial>` with role `build` and
/// `service`.
fn allowlist(service: &str) -> String {
    format!(
        "[[mac]]\nserial = \"C02XK1ABJG5J\"\nhost_id_prefix = \"mac-\"\nrole = \"build\"\n\
         services = [\"{service}\"]\n"
    )
}

/// Catches the pending cap or the Mac allowlist accepted without enrollment, a cap of 0, an
/// allowlist that cannot be read or that names a service the rollout file does not define
/// letting the coordinator start, and the cap not reaching the enrollment it starts.
#[test]
fn the_pending_cap_and_the_mac_allowlist_need_enrollment_and_valid_values() {
    let files = Files::new("enrollment-cap-flags");
    let with = |extra: &[&'static str]| {
        let mut flags = enrollment_flags(true);
        flags.extend_from_slice(extra);
        flags
    };
    for refused in [
        vec!["--max-pending-enrollments", "5"],
        vec!["--mac-serial-allowlist", "=mac.toml"],
        with(&["--max-pending-enrollments", "0"]),
    ] {
        let args = files.args(&refused);
        assert_eq!(
            hostkeeper_coordinator::config::main(args),
            ExitCode::from(2),
            "{refused:?}"
        );
    }
    match start(&files, &with(&["--mac-serial-allowlist", "=absent.toml"])) {
        Err(StartError::MacAllowlist(why)) => {
            assert!(why.to_string().contains("absent.toml"), "{why}");
        }
        Err(other) => panic!("refused otherwise: {other}"),
        Ok(_) => panic!("started without its allowlist"),
    }
    files.put("mac.toml", &allowlist("nosuch"), 0o600);
    match start(&files, &with(&["--mac-serial-allowlist", "=mac.toml"])) {
        Err(StartError::MacService(service)) => assert_eq!(service, "nosuch"),
        Err(other) => panic!("refused otherwise: {other}"),
        Ok(_) => panic!("started with a service it does not manage"),
    }
    files.put("mac.toml", &allowlist("kbf"), 0o600);
    let parts = start(
        &files,
        &with(&[
            "--mac-serial-allowlist",
            "=mac.toml",
            "--max-pending-enrollments",
            "1",
        ]),
    )
    .expect("started");
    let enrollment = parts.coordinator.enrollment().expect("enrollment");
    enrollment
        .enroll(new_request("machine-1"))
        .expect("the first waits");
    let full = enrollment
        .enroll(new_request("machine-2"))
        .expect_err("the cap is 1");
    assert_eq!(full.token(), "pending_full");
}

/// Catches `--enrollment-request-ttl` accepted without enrollment, or with a value that is not
/// digits and one unit, is no time, or overflows; a unit read as another; a default other than
/// 72 hours; the TTL not reaching the enrollment the coordinator starts; and the flag missing
/// from the usage or read from the environment.
#[test]
fn the_request_ttl_needs_enrollment_and_a_whole_time() {
    let with = |extra: &[&'static str]| {
        let mut flags = enrollment_flags(true);
        flags.extend_from_slice(extra);
        flags
    };
    let files = Files::new("enrollment-ttl-refused");
    let mut refused = vec![vec!["--enrollment-request-ttl", "72h"]];
    for value in [
        "",
        "h",
        "72",
        "0s",
        "0d",
        "72H",
        "1.5h",
        "+1h",
        " 72h",
        "72 h",
        "72hh",
        "72é",
        "1w",
        "99999999999999999999s",
        "213503982334602d",
    ] {
        refused.push(with(&["--enrollment-request-ttl", value]));
    }
    // A value starting with `-` is given after `=`, so that it reaches the flag's parser.
    refused.push(with(&["--enrollment-request-ttl=-1h"]));
    for args in refused {
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&args)),
            ExitCode::from(2),
            "{args:?}"
        );
    }
    for (n, (value, secs)) in [
        (None, 72 * 3600),
        (Some("1s"), 1),
        (Some("90m"), 5400),
        (Some("72h"), 72 * 3600),
        (Some("7d"), 7 * 86_400),
        (Some("213503982334601d"), 213_503_982_334_601 * 86_400),
    ]
    .into_iter()
    .enumerate()
    {
        let files = Files::new(&format!("enrollment-ttl-{n}"));
        let extra: Vec<&str> = value.map_or_else(Vec::new, |v| vec!["--enrollment-request-ttl", v]);
        let parts = start(&files, &with(&extra)).expect("started");
        let enrollment = parts.coordinator.enrollment().expect("enrollment");
        assert_eq!(enrollment.request_ttl_secs(), secs, "{value:?}");
    }
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(
        help.contains("--enrollment-request-ttl <ENROLLMENT_REQUEST_TTL>"),
        "{help}"
    );
    assert!(help.contains("[default: 72h]"), "{help}");
    assert!(!help.contains("[env:"), "{help}");
}

/// Catches `--host-cert-validity-secs` accepted without enrollment, below its floor of 60, above
/// the CA's 7-day cap, or not a whole number of seconds; a default other than 7 days; a value
/// not reaching the enrollment the coordinator starts; and the flag missing from the usage or
/// read from the environment.
#[test]
fn the_host_cert_validity_needs_enrollment_and_fits_between_a_minute_and_the_cap() {
    let with = |extra: &[&'static str]| {
        let mut flags = enrollment_flags(true);
        flags.extend_from_slice(extra);
        flags
    };
    let files = Files::new("host-validity-refused");
    let mut refused = vec![vec!["--host-cert-validity-secs", "60"]];
    for value in ["", "0", "59", "604801", "60s", "1m", "6e1", " 60"] {
        refused.push(with(&["--host-cert-validity-secs", value]));
    }
    refused.push(with(&["--host-cert-validity-secs=-60"]));
    for args in refused {
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&args)),
            ExitCode::from(2),
            "{args:?}"
        );
    }
    for (n, (value, secs)) in [
        (None, 604_800),
        (Some("60"), 60),
        (Some("61"), 61),
        (Some("604799"), 604_799),
        (Some("604800"), 604_800),
    ]
    .into_iter()
    .enumerate()
    {
        let files = Files::new(&format!("host-validity-{n}"));
        let extra: Vec<&str> =
            value.map_or_else(Vec::new, |v| vec!["--host-cert-validity-secs", v]);
        let parts = start(&files, &with(&extra)).expect("started");
        let enrollment = parts.coordinator.enrollment().expect("enrollment");
        assert_eq!(enrollment.store().host_validity_secs(), secs, "{value:?}");
    }
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(
        help.contains("--host-cert-validity-secs <HOST_CERT_VALIDITY_SECS>"),
        "{help}"
    );
    assert!(help.contains("[default: 604800]"), "{help}");
    assert!(!help.contains("[env:"), "{help}");
}

/// A host's P-256 key, enrolled as `s1`, a storage host: its certificate, DER, and its key,
/// PKCS#8.
fn enroll_host(enrollment: &Enrollment) -> (Vec<u8>, Vec<u8>) {
    let pair = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key");
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, "host:pending");
    let csr_der = params
        .serialize_request(&pair)
        .expect("a CSR")
        .der()
        .to_vec();
    let id = KeyHash::of_spki(&pair.subject_public_key_info());
    enrollment
        .enroll(EnrollRequest {
            csr_der,
            claims: Some(Claims {
                machine_id: "machine-1".to_owned(),
                ..Claims::default()
            }),
            ..EnrollRequest::default()
        })
        .expect("enrolled");
    enrollment
        .store()
        .approve(
            &id,
            &id,
            &Decision {
                host_id: HostId::parse("s1").expect("a host id"),
                role: Role::parse("storage").expect("a role"),
                services: Vec::new(),
                replace: false,
                host_group: None,
            },
        )
        .expect("approved");
    let request_id = id.to_hex();
    let nonce = enrollment
        .poll(PollRequest {
            request_id: request_id.clone(),
            ..PollRequest::default()
        })
        .expect("a nonce")
        .nonce;
    let signature = pair.sign(&poll_message(&id, &nonce)).expect("signed");
    let issued = enrollment
        .poll(PollRequest {
            request_id,
            nonce,
            signature,
        })
        .expect("polled");
    assert_eq!(issued.state, PollState::Issued as i32);
    (issued.certificate_der, pair.serialize_der())
}

/// A client that trusts the CA `ca` and presents `identity` (a certificate and its key) if
/// given.
fn client(ca: &[u8], identity: Option<(Vec<u8>, Vec<u8>)>) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(ca.to_vec()))
        .expect("a root");
    let builder =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("versions")
            .with_root_certificates(roots);
    Arc::new(match identity {
        Some((certificate, key)) => builder
            .with_client_auth_cert(
                vec![CertificateDer::from(certificate)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .expect("an identity"),
        None => builder.with_no_client_auth(),
    })
}

/// Catches a heartbeat listener that, with enrollment, still requires a client certificate at
/// the handshake (no host could enroll), lets a heartbeat without one through the route, does
/// not serve the server certificate of its own CA, or does not admit a host its CA enrolled.
#[tokio::test]
async fn the_heartbeat_listener_admits_a_client_without_a_certificate_and_an_enrolled_host() {
    let files = Files::new("enrollment-listener");
    let parts = start(&files, &enrollment_flags(true)).expect("started");
    let enrollment = Arc::clone(parts.coordinator.enrollment().expect("enrollment"));
    let ca = enrollment.ca_bundle().certificate_der().to_vec();
    let host = enroll_host(&enrollment);

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
    let heartbeat = bound.await.expect("bound").heartbeat;

    let beat = |config: Arc<ClientConfig>| {
        let net = net.clone();
        async move {
            exchange(
                &net,
                heartbeat,
                &config,
                "POST",
                "/v1/heartbeat",
                None,
                BEAT,
            )
            .await
        }
    };
    let anonymous = beat(client(&ca, None)).await.expect("answered");
    assert_eq!(anonymous, (403, r#"{"error":"forbidden"}"#.to_owned()));
    let enrolled = beat(client(&ca, Some(host))).await.expect("answered");
    assert_eq!(enrolled, (200, r#"{"reply":"nothing"}"#.to_owned()));
    serving.abort();
}
