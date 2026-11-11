//! `hostkeeper-coordinator serve` with the MDM policy gate: its flags go together and need
//! enrollment, the URL, the key file and the webhook address are refused unless they are what
//! the gate needs, a torn device record stops the start, and `run` serves NanoHUB's webhook on
//! its own address, whose events show in `Mdm.ListDevices`. The connections are in-memory; the
//! gate's API URL names a loopback port nothing listens on, and no event makes the gate call it.
//! The enrollment side's two flags go with the gate, are refused when wrong, and, parsed and
//! started, make the gate install the farm's config profile through a NanoHUB on loopback,
//! port 0, that queues every command.

mod support;

use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::Parser;
use hostkeeper_coordinator::config::{Bound, Cli, Command, Parts, Serve, StartError, build, run};
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;
use rustls::pki_types::CertificateDer;
use serde_json::{Value, json};
use support::{Ca, Net, OPERATOR, accepting, client_config, exchange, scratch};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// The URL of NanoHUB's API: loopback, and nothing listens there.
const API_URL: &str = "http://127.0.0.1:9004";

/// Where the webhook is served.
const WEBHOOK: &str = "127.0.0.1:9100";

/// The files the flags name, in a directory of their own, and the CA of the private listener.
struct Files {
    dir: Scratch,
    private_ca: Ca,
}

impl Files {
    fn new(name: &str) -> Self {
        let files = Self {
            dir: scratch(name),
            private_ca: Ca::new("private listener CA"),
        };
        let leaf = files.private_ca.coordinator();
        files.put("tls.pem", &leaf.cert_pem, 0o644);
        files.put("tls.key", &leaf.key_pem, 0o600);
        files.put("ca.pem", &Ca::new("farm CA").pem, 0o644);
        files.put("oidc.toml", &config_text(STATIC_ISSUER), 0o644);
        files.put("operator.token", OPERATOR, 0o600);
        files.put("mdm.key", "a-nanohub-api-key-for-the-tests\n", 0o600);
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

    /// The arguments of a `serve`, then `extra`; each `=name` is the path of `name` in the
    /// directory.
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

/// The enrollment flags, which make the CA.
fn enrollment() -> Vec<&'static str> {
    vec![
        "--enrollment-dir",
        "=enrollment",
        "--ca-key-file",
        "=ca.key",
        "--server-cert-file",
        "=server.pem",
        "--server-name",
        "localhost",
        "--ca-init",
    ]
}

/// The enrollment flags and the three gate flags, with `url`, `key` and `listen` for them.
fn gate(url: &'static str, key: &'static str, listen: &'static str) -> Vec<&'static str> {
    let mut flags = enrollment();
    flags.extend(["--mdm-api-url", url, "--mdm-api-key-file", key]);
    flags.extend(["--mdm-webhook-listen", listen]);
    flags
}

/// The good gate flags.
fn good() -> Vec<&'static str> {
    gate(API_URL, "=mdm.key", WEBHOOK)
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

/// The text of why a start was refused.
fn refusal(files: &Files, extra: &[&str]) -> String {
    match start(files, extra) {
        Err(why) => why.to_string(),
        Ok(_) => panic!("started with {extra:?}"),
    }
}

/// Catches: a gate flag accepted without its partners, or the gate asked for without
/// enrollment (its records live in the enrollment store): each of the three alone, each pair,
/// and all three without `--enrollment-dir` is a usage error, exit 2.
#[test]
fn the_gate_flags_go_together_and_need_enrollment() {
    let files = Files::new("mdm-flags-together");
    let url = ["--mdm-api-url", API_URL];
    let key = ["--mdm-api-key-file", "=mdm.key"];
    let listen = ["--mdm-webhook-listen", WEBHOOK];
    let with = |parts: &[&[&'static str]]| {
        let mut flags = enrollment();
        for part in parts {
            flags.extend_from_slice(part);
        }
        flags
    };
    let refused = [
        with(&[&url]),
        with(&[&key]),
        with(&[&listen]),
        with(&[&url, &key]),
        with(&[&url, &listen]),
        with(&[&key, &listen]),
        [url, key].concat().into_iter().chain(listen).collect(),
    ];
    for args in refused {
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&args)),
            ExitCode::from(2),
            "{args:?}"
        );
    }
    // Without the gate flags the same enrollment starts.
    assert!(start(&files, &enrollment()).is_ok());
}

/// Catches: an API URL that is not plain HTTP on loopback let through (a routable address,
/// TLS, a name), and an error text that differs from the one the operator is told to fix.
#[test]
fn the_api_url_must_be_http_on_loopback() {
    let files = Files::new("mdm-url");
    let rule = "it is http://<loopback address>:<port>, as http://127.0.0.1:9004, since NanoHUB's API is reached on loopback only";
    for (url, why) in [
        (
            "http://192.0.2.1:9004",
            "its address is not a loopback address",
        ),
        ("https://127.0.0.1:9004", "it must start http://"),
        ("http://localhost:9004", "its host is not an IP address"),
        ("http://127.0.0.1", "it names no port"),
        ("http://127.0.0.1:0", "its port is not 1 to 65535"),
    ] {
        assert_eq!(
            refusal(&files, &gate(url, "=mdm.key", WEBHOOK)),
            format!("--mdm-api-url: {why}; {rule}"),
            "{url}"
        );
    }
}

/// Catches: an API key file that breaks the secret file rule let through (mode 0644), a key
/// file that is missing or empty, and a message that names the path or the key instead of the
/// flag.
#[test]
fn the_api_key_file_must_pass_the_secret_file_rule() {
    let files = Files::new("mdm-key");
    files.put("loose.key", "a-nanohub-api-key-for-the-tests\n", 0o644);
    let text = refusal(&files, &gate(API_URL, "=loose.key", WEBHOOK));
    assert!(text.starts_with("--mdm-api-key-file: "), "{text}");
    assert!(!text.contains("loose.key"), "{text}");
    assert!(!text.contains("a-nanohub-api-key"), "{text}");
    assert_eq!(text, "--mdm-api-key-file: its mode must be 0600 or 0400");

    files.put("empty.key", "\n", 0o600);
    assert_eq!(
        refusal(&files, &gate(API_URL, "=empty.key", WEBHOOK)),
        "--mdm-api-key-file: it must hold one key of 1 to 4096 visible ASCII characters"
    );
    let missing = refusal(&files, &gate(API_URL, "=absent.key", WEBHOOK));
    assert!(missing.starts_with("--mdm-api-key-file: "), "{missing}");
    assert!(!missing.contains("absent.key"), "{missing}");
}

/// Catches: a webhook address NanoHUB's host could not be the only one to reach let through: a
/// routable address, and the wildcard.
#[test]
fn the_webhook_address_must_be_on_loopback() {
    let files = Files::new("mdm-listen");
    for listen in ["192.0.2.1:9100", "0.0.0.0:9100", "[::]:9100"] {
        assert_eq!(
            refusal(&files, &gate(API_URL, "=mdm.key", listen)),
            format!(
                "--mdm-webhook-listen: {listen} is not a loopback address; NanoHUB posts to it from the same host"
            ),
            "{listen}"
        );
    }
    for listen in ["127.0.0.1:9100", "[::1]:9100", "[::ffff:127.0.0.1]:9100"] {
        let parts = start(&files, &gate(API_URL, "=mdm.key", listen)).expect("started");
        let mdm = parts.coordinator.mdm().expect("the gate");
        assert_eq!(
            mdm.webhook_listen,
            Some(listen.parse::<SocketAddr>().expect("address"))
        );
    }
}

/// Catches: a start over a device record the gate cannot read: the coordinator must stop and
/// name the file rather than guess what the device's record said.
#[test]
fn a_torn_device_record_stops_the_start() {
    let files = Files::new("mdm-torn");
    drop(start(&files, &good()).expect("started"));
    let records = files.path("enrollment").join("mdm-devices");
    assert!(records.is_dir());
    let torn = records.join("AAAA-1.pb");
    std::fs::write(&torn, b"\xff\xff torn").expect("a torn record");
    match start(&files, &good()) {
        Err(StartError::Mdm(why)) => {
            assert_eq!(
                why.to_string(),
                format!(
                    "the MDM device records: {}: the file is not a record; the gate does not guess what a device's record said, so fix or remove the file to start",
                    torn.display()
                )
            );
        }
        Err(other) => panic!("refused otherwise: {other}"),
        Ok(_) => panic!("started over a torn record"),
    }
    std::fs::remove_file(&torn).expect("removed");
    assert!(start(&files, &good()).is_ok());
}

/// Catches: the gate not reaching the coordinator the flags start, or a gate running without
/// its flags: with them `mdm()` is the gate and names the webhook address, its records are in
/// `mdm-devices` of the enrollment directory; without them there is none.
#[test]
fn the_flags_start_the_gate_and_without_them_there_is_none() {
    let files = Files::new("mdm-start");
    let parts = start(&files, &good()).expect("started");
    let mdm = parts.coordinator.mdm().expect("the gate");
    assert_eq!(
        mdm.webhook_listen,
        Some(WEBHOOK.parse::<SocketAddr>().expect("an address"))
    );
    assert!(files.path("enrollment").join("mdm-devices").is_dir());
    drop(parts);
    let parts = start(&files, &enrollment()).expect("started");
    assert!(parts.coordinator.mdm().is_none());
    assert!(parts.coordinator.enrollment().is_some());
    drop(parts);
    let plain = start(&files, &[]).expect("started");
    assert!(plain.coordinator.mdm().is_none());
}

/// Catches: the gate flags missing from the usage, or read from the environment.
#[test]
fn serve_help_lists_the_gate_flags_and_no_environment_variable() {
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    for flag in [
        "--mdm-api-url <MDM_API_URL>",
        "--mdm-api-key-file <MDM_API_KEY_FILE>",
        "--mdm-webhook-listen <MDM_WEBHOOK_LISTEN>",
        "--mdm-profile-identifier <MDM_PROFILE_IDENTIFIER>",
        "--mdm-daemon-config <MDM_DAEMON_CONFIG>",
        "--mdm-stores <MDM_STORES>",
        "--mdm-macos-pkg <MDM_MACOS_PKG>",
        "--mdm-pkg-allowlist <MDM_PKG_ALLOWLIST>",
        "--mdm-host <MDM_HOST>",
    ] {
        assert!(help.contains(flag), "{flag}: {help}");
    }
    assert!(!help.contains("[env:"), "{help}");
}

/// Runs `parts` over a new in-memory network: the network, the addresses its bind function was
/// asked for in order (as `ready` was called), where the listeners are, and the task serving.
async fn serve(parts: Parts) -> (Net, Vec<SocketAddr>, Bound, tokio::task::JoinHandle<()>) {
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
    let log = Arc::clone(&asked);
    let serving = tokio::spawn(async move {
        let _ = run(parts, bind, move |bound| {
            let before = log.lock().expect("log").clone();
            let _ = told.send((bound, before));
        })
        .await;
    });
    let (bound, before) = bound.await.expect("ready");
    (net, before, bound, serving)
}

/// `POST /webhook` with `body` over a new connection of `net` to `addr`: the status.
async fn post_webhook(net: &Net, addr: SocketAddr, body: &[u8]) -> u16 {
    let mut stream = net.connect(addr).expect("connected");
    let head = format!(
        "POST /webhook HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.expect("head");
    stream.write_all(body).await.expect("body");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("an answer");
    let text = String::from_utf8_lossy(&raw).into_owned();
    text.strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no HTTP answer: {text:?}"))
}

/// `Mdm.ListDevices` by the gateway of the private listener at `control`.
async fn list_devices(net: &Net, files: &Files, control: SocketAddr) -> Value {
    let config = client_config(&files.private_ca, None);
    let (status, body) = exchange(
        net,
        control,
        &config,
        "POST",
        "/hostkeeper.operator.v1.Mdm/ListDevices",
        Some(OPERATOR),
        "{}",
    )
    .await
    .expect("answered");
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).expect("JSON")
}

/// The bytes of NanoMDM's recorded `Authenticate` event, which the gate's own tests post too.
fn authenticate_event() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hostkeeper-mdm-gate/tests/fixtures/nanohub/Authenticate.2.json");
    std::fs::read(path).expect("the recorded event")
}

/// Catches: a webhook that is not served, or served on another address than the flag names, or
/// bound before the two listeners or after `ready`; an event the webhook takes that does not
/// reach the gate's records; and a coordinator without the gate that binds a third address.
#[tokio::test]
async fn run_serves_the_webhook_and_its_events_show_in_list_devices() {
    let files = Files::new("mdm-run");
    let parts = start(&files, &good()).expect("started");
    let (net, asked, bound, serving) = serve(parts).await;
    let webhook: SocketAddr = WEBHOOK.parse().expect("an address");
    assert_eq!(
        asked,
        vec![
            "192.0.2.1:1".parse::<SocketAddr>().expect("an address"),
            "192.0.2.1:2".parse().expect("an address"),
            webhook,
        ],
        "the heartbeat, the control and then the webhook address were bound before ready"
    );

    let before = list_devices(&net, &files, bound.control).await;
    assert_eq!(before, json!({"gate": true, "missing": not_configured()}));
    assert_eq!(
        post_webhook(&net, webhook, &authenticate_event()).await,
        200
    );
    assert_eq!(post_webhook(&net, webhook, b"not an event").await, 400);
    let listed = list_devices(&net, &files, bound.control).await;
    let devices = listed["devices"].as_array().expect("devices");
    assert_eq!(devices.len(), 1, "{listed}");
    assert_eq!(
        devices[0]["enrollmentId"],
        "66ADE930-5FDF-5EC4-8429-15640684C489"
    );
    assert_eq!(devices[0]["phase"], "MDM_PHASE_AUTHENTICATED");
    assert_eq!(listed["gate"], true);
    serving.abort();

    let files = Files::new("mdm-run-none");
    let parts = start(&files, &enrollment()).expect("started");
    let (net, asked, bound, serving) = serve(parts).await;
    assert_eq!(asked.len(), 2, "no third address: {asked:?}");
    assert!(net.connect(webhook).is_err(), "nothing serves the webhook");
    let none = list_devices(&net, &files, bound.control).await;
    assert_eq!(none, json!({"missing": not_configured()}));
    serving.abort();
}

/// The conditions of a coordinator whose MDM setup holds nothing, as the JSON gateway writes them.
fn not_configured() -> Value {
    json!([
        "MDM_CONDITION_PUSH_CERT_MISSING",
        "MDM_CONDITION_PUSH_KEY_MISSING",
        "MDM_CONDITION_ABM_TOKEN_MISSING",
        "MDM_CONDITION_NAME_MISSING",
        "MDM_CONDITION_TLS_CERT_MISSING",
        "MDM_CONDITION_TLS_KEY_MISSING"
    ])
}

/// The farm's profile identifier in these tests.
const IDENTIFIER: &str = "com.example.hostkeeper.mdm";

/// The serial and UDID of NanoMDM's recorded `Authenticate`.
const SERIAL: &str = "C02MT66KFLHH";
const UDID: &str = "66ADE930-5FDF-5EC4-8429-15640684C489";

/// A daemon config the daemon's loader accepts.
const DAEMON_CONFIG: &str = r#"coordinator = "https://coordinator.example:8443"
ca_sha256 = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// The good gate flags, NanoHUB at `url`, with the enrollment side's two flags and an
/// allowlist of [`SERIAL`].
fn side_args(files: &Files, url: &str) -> Vec<OsString> {
    files.put("hostkeeper.toml", DAEMON_CONFIG, 0o644);
    files.put(
        "allow.toml",
        &format!("[[mac]]\nserial = \"{SERIAL}\"\nhost_id_prefix = \"mac-\"\nrole = \"build\"\n"),
        0o644,
    );
    let mut extra = enrollment();
    extra.extend(["--mdm-api-url", url, "--mdm-api-key-file", "=mdm.key"]);
    extra.extend(["--mdm-webhook-listen", WEBHOOK]);
    extra.extend(["--mdm-profile-identifier", IDENTIFIER]);
    extra.extend(["--mdm-daemon-config", "=hostkeeper.toml"]);
    extra.extend(["--mac-serial-allowlist", "=allow.toml"]);
    files.args(&extra)
}

/// Catches: an enrollment-side flag accepted without its partner or without the gate (each
/// alone, both without `--mdm-api-url`), or a bad identifier or a daemon config the daemon's
/// loader refuses let through the start, or refused with a text that does not name the flag.
#[test]
fn the_enrollment_side_flags_go_together_with_the_gate() {
    let files = Files::new("mdm-side-flags");
    files.put("hostkeeper.toml", DAEMON_CONFIG, 0o644);
    let identifier = ["--mdm-profile-identifier", IDENTIFIER];
    let config = ["--mdm-daemon-config", "=hostkeeper.toml"];
    let refused: [Vec<&str>; 3] = [
        good().into_iter().chain(identifier).collect(),
        good().into_iter().chain(config).collect(),
        enrollment()
            .into_iter()
            .chain(identifier)
            .chain(config)
            .collect(),
    ];
    for args in refused {
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&args)),
            ExitCode::from(2),
            "{args:?}"
        );
    }
    let both: Vec<&str> = good().into_iter().chain(identifier).chain(config).collect();
    assert!(start(&files, &both).is_ok());

    let bad: Vec<&str> = good()
        .into_iter()
        .chain(["--mdm-profile-identifier", "hostkeeper"])
        .chain(config)
        .collect();
    assert_eq!(
        refusal(&files, &bad),
        "--mdm-profile-identifier: \"hostkeeper\" is not a reverse-DNS name (two or more labels of letters, digits and -, as com.example.hostkeeper.mdm)"
    );
    files.put(
        "refused.toml",
        &DAEMON_CONFIG.replace("release_channel", "channel"),
        0o644,
    );
    let refused: Vec<&str> = good()
        .into_iter()
        .chain(identifier)
        .chain(["--mdm-daemon-config", "=refused.toml"])
        .collect();
    let text = refusal(&files, &refused);
    assert!(text.starts_with("--mdm-daemon-config: "), "{text}");
}

/// Catches: `--mdm-stores` or `--mdm-macos-pkg` accepted without the profile identifier (each
/// alone, with the gate only), or a stores file with a credential file or a signers file the
/// daemon's loader refuses let through the start, or refused with a text that does not name the
/// flag.
#[test]
fn the_stores_and_signers_flags_need_the_profile_and_are_checked_at_start() {
    let files = Files::new("mdm-stores-flags");
    files.put("hostkeeper.toml", DAEMON_CONFIG, 0o644);
    let stores = "[[store]]\nname = \"lan\"\nkind = \"https\"\nbase = \"https://m.example/\"\n";
    files.put("stores.toml", stores, 0o644);
    files.put(
        "macos-pkg.toml",
        &format!("signers = [\"{}\"]\n", "ab".repeat(32)),
        0o644,
    );
    let stores_flag = ["--mdm-stores", "=stores.toml"];
    let signers_flag = ["--mdm-macos-pkg", "=macos-pkg.toml"];
    for alone in [stores_flag, signers_flag] {
        let args: Vec<&str> = good().into_iter().chain(alone).collect();
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&args)),
            ExitCode::from(2),
            "{args:?}"
        );
    }
    let side = [
        "--mdm-profile-identifier",
        IDENTIFIER,
        "--mdm-daemon-config",
        "=hostkeeper.toml",
    ];
    let all: Vec<&str> = good()
        .into_iter()
        .chain(side)
        .chain(stores_flag)
        .chain(signers_flag)
        .collect();
    assert!(start(&files, &all).is_ok());

    files.put(
        "stores.toml",
        &format!("{stores}basic_auth_file = \"/x\"\n"),
        0o644,
    );
    assert_eq!(
        refusal(&files, &all),
        "--mdm-stores: store \"lan\" names a credential file (basic_auth_file or bearer_file): a profile carries no secret, and the file is not on the Mac"
    );
    files.put("stores.toml", stores, 0o644);
    files.put("macos-pkg.toml", "signers = [\"zz\"]\n", 0o644);
    assert_eq!(
        refusal(&files, &all),
        "--mdm-macos-pkg: the signers names \"zz\", which is not 64 hex digits"
    );
}

/// Catches: a pkg flag accepted without its partner or without the enrollment side
/// (`--mdm-pkg-allowlist` without `--mdm-host`, `--mdm-host` alone, both without
/// `--mdm-profile-identifier`), a pkg allowlist that is not root's let through the start (it
/// says what may be installed as root on every Mac), or a host name with a scheme; each
/// refusal names its flag.
#[test]
fn the_pkg_flags_go_together_and_the_allowlist_must_be_roots() {
    let files = Files::new("mdm-pkg-flags");
    files.put("hostkeeper.toml", DAEMON_CONFIG, 0o644);
    let digest = "a".repeat(64);
    files.put(
        "pkgs.toml",
        &format!(
            "[[pkg]]\nsha256 = \"{digest}\"\nversion = \"0.1.66\"\npath = \"/var/lib/hostkeeper-mdm-pkg/{digest}.pkg\"\n"
        ),
        0o644,
    );
    let side = [
        "--mdm-profile-identifier",
        IDENTIFIER,
        "--mdm-daemon-config",
        "=hostkeeper.toml",
    ];
    let allowlist = ["--mdm-pkg-allowlist", "=pkgs.toml"];
    let host = ["--mdm-host", "mdm.example.com"];
    let refused: [Vec<&str>; 3] = [
        good().into_iter().chain(side).chain(allowlist).collect(),
        good().into_iter().chain(side).chain(host).collect(),
        good().into_iter().chain(allowlist).chain(host).collect(),
    ];
    for args in refused {
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&args)),
            ExitCode::from(2),
            "{args:?}"
        );
    }
    let all: Vec<&str> = good()
        .into_iter()
        .chain(side)
        .chain(allowlist)
        .chain(host)
        .collect();
    assert_eq!(
        refusal(&files, &all),
        format!(
            "--mdm-pkg-allowlist {}: it is not owned by root; the pkg allowlist must be a file of root's that no one else can write",
            files.path("pkgs.toml").display()
        )
    );
    let schemed: Vec<&str> = good()
        .into_iter()
        .chain(side)
        .chain(allowlist)
        .chain(["--mdm-host", "https://mdm.example.com"])
        .collect();
    assert_eq!(
        refusal(&files, &schemed),
        "--mdm-host: \"https://mdm.example.com\" is not a DNS name (the MDM host's name alone, with no scheme, port or path)"
    );
}

/// NanoHUB's API on loopback, port 0: it queues every command and keeps each body.
struct Hub {
    url: String,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Hub {
    async fn start() -> Hub {
        use http_body_util::BodyExt as _;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let url = format!("http://{}", listener.local_addr().expect("its address"));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&bodies);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let kept = Arc::clone(&kept);
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(
                        move |request: hyper::Request<hyper::body::Incoming>| {
                            let kept = Arc::clone(&kept);
                            async move {
                                let body = request
                                    .into_body()
                                    .collect()
                                    .await
                                    .map(|body| body.to_bytes().to_vec())
                                    .unwrap_or_default();
                                kept.lock().expect("bodies").push(body);
                                Ok::<_, std::convert::Infallible>(hyper::Response::new(
                                    http_body_util::Full::new(bytes::Bytes::from("{}")),
                                ))
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Hub { url, bodies }
    }

    /// The `Command` dictionary of every command heard, in order.
    fn commands(&self) -> Vec<plist::Dictionary> {
        self.bodies
            .lock()
            .expect("bodies")
            .iter()
            .map(|body| {
                plist::Value::from_reader(std::io::Cursor::new(body))
                    .expect("a plist")
                    .into_dictionary()
                    .expect("a dictionary")
            })
            .map(|root| root["Command"].as_dictionary().expect("a Command").clone())
            .collect()
    }

    /// The `CommandUUID` of the command heard of `kind`.
    fn uuid_of(&self, kind: &str) -> String {
        self.bodies
            .lock()
            .expect("bodies")
            .iter()
            .map(|body| {
                plist::Value::from_reader(std::io::Cursor::new(body))
                    .expect("a plist")
                    .into_dictionary()
                    .expect("a dictionary")
            })
            .find(|root| {
                root["Command"].as_dictionary().expect("a Command")["RequestType"].as_string()
                    == Some(kind)
            })
            .and_then(|root| root["CommandUUID"].as_string().map(str::to_owned))
            .expect("a command of that kind")
    }
}

/// The base64 of the XML property list of `dict`.
fn payload(dict: plist::Dictionary) -> String {
    use base64::Engine as _;
    let mut bytes = Vec::new();
    plist::Value::Dictionary(dict)
        .to_writer_xml(&mut bytes)
        .expect("a plist");
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A dictionary of `entries`.
fn dict(entries: Vec<(&str, plist::Value)>) -> plist::Dictionary {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// The device's first `TokenUpdate`, as NanoHUB 0.2.0's webhook posts it.
fn token_update() -> Vec<u8> {
    let raw = payload(dict(vec![
        ("MessageType", "TokenUpdate".into()),
        ("UDID", UDID.into()),
        ("Token", plist::Value::Data(vec![1, 2, 3])),
    ]));
    serde_json::to_vec(&json!({
        "topic": "mdm.TokenUpdate",
        "checkin_event": {
            "ids": {"id": UDID, "type": "Device"},
            "udid": UDID,
            "raw_payload": raw,
            "token_update_tally": 1,
        },
    }))
    .expect("json")
}

/// The device's `DeviceInformation` answer to the command `uuid`: [`SERIAL`], supervised.
fn device_information(uuid: &str) -> Vec<u8> {
    let answers = dict(vec![
        ("SerialNumber", SERIAL.into()),
        ("UDID", UDID.into()),
        ("IsSupervised", plist::Value::Boolean(true)),
    ]);
    let raw = payload(dict(vec![
        ("CommandUUID", uuid.into()),
        ("Status", "Acknowledged".into()),
        ("UDID", UDID.into()),
        ("RequestType", "DeviceInformation".into()),
        ("QueryResponses", plist::Value::Dictionary(answers)),
    ]));
    serde_json::to_vec(&json!({
        "topic": "mdm.Connect",
        "acknowledge_event": {
            "ids": {"id": UDID, "type": "Device"},
            "udid": UDID,
            "status": "Acknowledged",
            "command_uuid": uuid,
            "raw_payload": raw,
        },
    }))
    .expect("json")
}

/// Catches: the two flags parsed but not reaching the gate the coordinator starts (no config
/// profile sent, or one of another identifier or without the daemon config's keys), or the
/// gate's enrollment not attached at start (the allowlisted serial not allowed).
#[tokio::test]
async fn the_enrollment_side_flags_make_the_gate_install_the_config_profile() {
    let hub = Hub::start().await;
    let files = Files::new("mdm-side-start");
    let roots = || Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)]);
    // The allowlist's entry names the host group `build`, which the state files must hold.
    let first = build(&flags_of(files.args(&enrollment())), roots()).expect("started");
    first
        .coordinator
        .host_groups_put(
            &first.coordinator.caller(Some(OPERATOR)).await,
            proto::PutHostGroupRequest {
                host_group: Some(proto::HostGroup {
                    name: "build".to_owned(),
                    packages: vec!["hostkeeper".to_owned()],
                    policy: None,
                }),
                if_generation: None,
                reason: "the Mac builders".to_owned(),
            },
        )
        .expect("put");
    drop(first);
    let parts = build(&flags_of(side_args(&files, &hub.url)), roots()).expect("started");
    let gate = Arc::clone(&parts.coordinator.mdm().expect("the gate").gate);

    hostkeeper_mdm_gate::webhook::take(&gate, &authenticate_event())
        .await
        .expect("taken");
    hostkeeper_mdm_gate::webhook::take(&gate, &token_update())
        .await
        .expect("taken");
    let uuid = hub.uuid_of("DeviceInformation");
    hostkeeper_mdm_gate::webhook::take(&gate, &device_information(&uuid))
        .await
        .expect("taken");

    let installs: Vec<plist::Dictionary> = hub
        .commands()
        .into_iter()
        .filter(|command| command["RequestType"].as_string() == Some("InstallProfile"))
        .collect();
    assert_eq!(installs.len(), 1, "{installs:?}");
    let profile = plist::Value::from_reader(std::io::Cursor::new(
        installs[0]["Payload"].as_data().expect("a Payload"),
    ))
    .expect("a profile")
    .into_dictionary()
    .expect("a dictionary");
    assert_eq!(
        profile["PayloadIdentifier"].as_string(),
        Some(format!("{IDENTIFIER}.config").as_str())
    );
    let settings = profile["PayloadContent"].as_array().expect("payloads")[0]
        .as_dictionary()
        .expect("a payload")["PayloadContent"]
        .as_dictionary()
        .expect("domains")["dev.hostkeeper.config"]
        .as_dictionary()
        .expect("the domain")["Forced"]
        .as_array()
        .expect("Forced")[0]
        .as_dictionary()
        .expect("an entry")["mcx_preference_settings"]
        .as_dictionary()
        .expect("settings")
        .clone();
    assert_eq!(
        settings["coordinator"].as_string(),
        Some("https://coordinator.example:8443")
    );
    assert_eq!(
        settings["trusted_signers"]
            .as_dictionary()
            .expect("a table")["hosted_runner"]
            .as_boolean(),
        Some(true)
    );
}

/// The address `--mdm-listen` names in these tests: one interface of the LAN.
const MACS: &str = "192.0.2.7:443";

/// The flags of a coordinator with the Macs' listener: enrollment, the gate's records and the queue.
fn macs() -> Vec<&'static str> {
    let mut flags = enrollment();
    flags.extend(["--mdm-listen", MACS]);
    flags
}

/// Catches: `--mdm-listen` accepted without the enrollment store the gate's records live in, a
/// wildcard address (the Macs reach the server by the address of one interface), the timeout and
/// sweep flags accepted without the listener, and a timeout of 0.
#[test]
fn the_macs_listener_needs_enrollment_and_a_named_interface() {
    let files = Files::new("mdm-listen-flags");
    for args in [
        vec!["--mdm-listen", MACS],
        vec!["--mdm-command-timeout-s", "60"],
        vec!["--mdm-queue-sweep-s", "5"],
    ] {
        assert_eq!(
            hostkeeper_coordinator::config::main(files.args(&args)),
            ExitCode::from(2),
            "{args:?}"
        );
    }
    let mut zero = macs();
    zero.extend(["--mdm-command-timeout-s", "0"]);
    assert_eq!(
        hostkeeper_coordinator::config::main(files.args(&zero)),
        ExitCode::from(2)
    );
    for wildcard in ["0.0.0.0:443", "[::]:443", "[::ffff:0.0.0.0]:443"] {
        let mut flags = enrollment();
        flags.extend(["--mdm-listen", wildcard]);
        assert_eq!(
            refusal(&files, &flags),
            format!("--mdm-listen: {wildcard} is a wildcard address; the Macs' listener is reached by the name of the MDM server, which is the address of one interface of the LAN, so name that address").replace("[::ffff:0.0.0.0]:443", "[::ffff:0.0.0.0]:443"),
        );
    }
}

/// Catches: the gate not running over the coordinator's own queue with `--mdm-listen` (it needs no
/// NanoHUB flag), a webhook served all the same, the NanoHUB flags of an old unit not being
/// ignored when both are given, and the listener bound before `ready` or not told to the operator
/// in the start line.
#[tokio::test]
async fn run_binds_the_macs_listener_and_serves_no_webhook() {
    let files = Files::new("mdm-listen-run");
    let parts = start(&files, &macs()).expect("started");
    let mdm = parts
        .coordinator
        .mdm()
        .expect("the gate runs over the coordinator's queue");
    assert_eq!(mdm.webhook_listen, None);
    assert_eq!(parts.mdm_listen, Some(MACS.parse().expect("an address")));
    let (net, asked, bound, serving) = serve(parts).await;
    let macs: SocketAddr = MACS.parse().expect("an address");
    assert_eq!(
        asked.last(),
        Some(&macs),
        "bound before ready, after the control listener"
    );
    assert_eq!(bound.mdm, Some(macs));
    assert!(hostkeeper_coordinator::config::start_line(&bound).contains(" mdm=192.0.2.7:443"));
    // No certificate of the name is stored: the connection is closed without a handshake.
    let mut stream = net.connect(macs).expect("connected");
    stream
        .write_all(b"\x16\x03\x01\x00\x00")
        .await
        .expect("sent");
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw).await;
    assert!(raw.is_empty(), "nothing is answered");
    serving.abort();
    let _ = serving.await;

    // With NanoHUB's flags beside it the listener wins: no webhook is bound.
    let mut both = good();
    both.extend(["--mdm-listen", MACS]);
    let parts = start(&files, &both).expect("started");
    assert_eq!(
        parts.coordinator.mdm().expect("the gate").webhook_listen,
        None
    );
    let (_net, asked, _bound, serving) = serve(parts).await;
    assert!(
        !asked.contains(&WEBHOOK.parse().expect("an address")),
        "{asked:?}"
    );
    serving.abort();
    let _ = serving.await;
}

/// Catches: the new flags missing from the usage, or read from the environment.
#[test]
fn serve_help_lists_the_macs_listener_flags() {
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    for flag in [
        "--mdm-listen <MDM_LISTEN>",
        "--mdm-command-timeout-s <MDM_COMMAND_TIMEOUT_S>",
        "--mdm-queue-sweep-s <MDM_QUEUE_SWEEP_S>",
    ] {
        assert!(help.contains(flag), "{flag}: {help}");
    }
}
