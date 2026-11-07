//! `hostkeeper-coordinator serve` with the MDM policy gate: its flags go together and need
//! enrollment, the URL, the key file and the webhook address are refused unless they are what
//! the gate needs, a torn device record stops the start, and `run` serves NanoHUB's webhook on
//! its own address, whose events show in `Mdm.ListDevices`. The connections are in-memory; the
//! gate's API URL names a loopback port nothing listens on, and no event makes the gate call it.

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
use hostkeeper_testkit::Scratch;
use rustls::pki_types::CertificateDer;
use serde_json::{Value, json};
use support::{Ca, Net, OPERATOR, accepting, client_config, exchange, recorded, scratch};
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
        files.put("rollout.toml", &recorded("services.toml"), 0o600);
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
            listen.parse::<SocketAddr>().expect("address")
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
        WEBHOOK.parse::<SocketAddr>().expect("an address")
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
    assert_eq!(before, json!({"gate": true}));
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
    assert_eq!(none, json!({}));
    serving.abort();
}
