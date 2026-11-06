//! The real coordinator, run inside the test process, and the operator's `ctl` that drives it.
//!
//! The coordinator is the production one: [`hostkeeper_coordinator::config::build`] reads the
//! same flags `hostkeeper-coordinator serve` takes, and [`hostkeeper_coordinator::config::run`]
//! serves the heartbeat and private listeners with TLS on loopback ports the system picks. Its
//! enrollment CA is made at start (`--ca-init`), so a daemon enrolls against it exactly as a
//! real host does, and the private listener serves the CA-issued certificate for `localhost`.
//!
//! The operator side is the real `hostkeeper ctl` program, run as a child process over the
//! private listener with the operator token, with `--output json`. Nothing here calls the
//! coordinator's Rust API directly, so the tests exercise the surface an operator has.
//!
//! The coordinator runs on its own thread with a current-thread tokio runtime. It stops when
//! the handle is dropped: the serving task is aborted, which releases the lock of its state
//! files, and the thread is joined.

use std::ffi::OsString;
use std::fs;
use std::future::poll_fn;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::{Pin, pin};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::task::Poll;
use std::thread::JoinHandle;

use clap::Parser;
use hostkeeper_coordinator::config::bind_tcp;
use hostkeeper_coordinator::config::{
    Bound, Cli, Command as CoordinatorCommand, Serve, build, run,
};
use hostkeeper_coordinator::enrollment::pem;
use hostkeeper_oidc::Roots;
use hostkeeper_oidc::testkit::{STATIC_ISSUER, config_text};
use rcgen::KeyUsagePurpose;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use rustls::pki_types::CertificateDer;
use serde_json::{Value, json};

use super::host_root::DAEMON;
use super::wait_for;

/// How many half-second looks [`CoordinatorHandle::approve_enrollment`] waits for the daemon's
/// enrollment request to be listed (the daemon retries every 5 seconds).
const ENROLLMENT_TRIES: u32 = 240;

/// What the coordinator's start learned that the handle keeps.
struct Started {
    /// The enrollment CA certificate, DER.
    ca_der: Vec<u8>,
    /// Its SHA-256, 64 lower-case hex digits: the daemon config's `ca_sha256`.
    ca_sha256: String,
}

/// A coordinator serving in this process, and the operator's way to call it.
pub struct CoordinatorHandle {
    /// The directory of the coordinator's files (token, rollout file, CA, state).
    dir: PathBuf,
    /// The role the hosts of the fixture service enroll as.
    role: String,
    /// Where the listeners are bound.
    bound: Bound,
    started: Started,
    /// Ends the serving task when sent to (or dropped).
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

/// Writes `text` to `path` with mode `mode`.
fn put(path: &Path, text: &str, mode: u32) {
    fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
}

/// A throwaway CA: the certificate (PEM) the coordinator's `--client-ca` takes, and the same
/// as DER for the roots of its OIDC fetcher (which is never used: no token is presented).
fn throwaway_ca() -> (String, Vec<u8>) {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "hostkeeper e2e client CA");
    params.distinguished_name = name;
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let key = KeyPair::generate().expect("a CA key");
    let ca = params.self_signed(&key).expect("a CA certificate");
    (ca.pem(), ca.der().to_vec())
}

/// The flags of `hostkeeper-coordinator serve` for the files of `dir`.
fn serve_flags(dir: &Path, service: &str) -> Serve {
    let file = |name: &str| dir.join(name).into_os_string();
    let mut args: Vec<OsString> = vec!["hostkeeper-coordinator".into(), "serve".into()];
    for (flag, value) in [
        ("--heartbeat-listen", OsString::from("127.0.0.1:0")),
        ("--private-listen", OsString::from("127.0.0.1:0")),
        // The enrollment setup writes `server.pem` (certificate chain and key); both listeners
        // present it.
        ("--tls-cert", file("server.pem")),
        ("--tls-key", file("server.pem")),
        ("--client-ca", file("client-ca.pem")),
        ("--state-file", file("desired.json")),
        ("--oidc-config", file("oidc.toml")),
        ("--operator-token-file", file("operator.token")),
        ("--rollout-file", file("rollout.toml")),
        ("--legacy-service", OsString::from(service)),
        ("--enrollment-dir", file("enrollment")),
        ("--ca-key-file", file("ca.key")),
        ("--server-cert-file", file("server.pem")),
        ("--server-name", OsString::from("localhost")),
    ] {
        args.push(flag.into());
        args.push(value);
    }
    args.push("--ca-init".into());
    match Cli::try_parse_from(args)
        .expect("the serve flags parse")
        .command
    {
        CoordinatorCommand::Serve(flags) => flags,
        other => panic!("not serve: {other:?}"),
    }
}

/// The coordinator's thread: builds it, reports what the start learned, serves until told to
/// stop. A failure is sent as text on whichever channel the handle is waiting on.
fn serve_until_stopped(
    flags: Serve,
    roots: Roots,
    started: mpsc::Sender<Result<Started, String>>,
    ready: mpsc::Sender<Result<Bound, String>>,
    stop: tokio::sync::oneshot::Receiver<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let _ = started.send(Err(format!("cannot make the runtime: {e}")));
            return;
        }
    };
    runtime.block_on(async move {
        let parts = match build(&flags, roots) {
            Ok(parts) => parts,
            Err(e) => {
                let _ = started.send(Err(e.to_string()));
                return;
            }
        };
        // `run` consumes the parts: read the CA before.
        let bundle = parts
            .coordinator
            .enrollment()
            .expect("--ca-init and the enrollment flags give the coordinator its enrollment")
            .ca_bundle()
            .clone();
        let _ = started.send(Ok(Started {
            ca_der: bundle.certificate_der().to_vec(),
            ca_sha256: bundle.sha256_hex(),
        }));
        let announce = ready.clone();
        // Polled here rather than spawned: the serving future needs no `Send` bound this way,
        // and dropping it at the end releases the lock of the state files.
        let mut serving = pin!(run(parts, bind_tcp, move |bound| {
            let _ = announce.send(Ok(bound));
        }));
        let mut stop = stop;
        // Ends when told or because the handle is gone, or when the serving future fails.
        let ended = poll_fn(|cx| {
            if Pin::new(&mut stop).poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            serving.as_mut().poll(cx).map(Some)
        })
        .await;
        if let Some(Err(e)) = ended {
            let _ = ready.send(Err(e.to_string()));
        }
    });
}

impl CoordinatorHandle {
    /// Starts a coordinator that manages `service` for hosts of role `host_role`, with its
    /// files in `dir` (made here, mode 0700), and waits until both listeners are bound.
    ///
    /// The service rolls back by itself when its release fails on the one host
    /// (`rollback = "automatic"`, threshold 100% and at least one host).
    ///
    /// # Panics
    /// A file cannot be written or the coordinator does not start.
    pub fn start(dir: &Path, service: &str, host_role: &str) -> CoordinatorHandle {
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).expect("chmod the directory");
        // 64 hex digits: the token must be at least 32 visible ASCII characters.
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).expect("random bytes");
        put(&dir.join("operator.token"), &hex::encode(random), 0o600);
        put(
            &dir.join("rollout.toml"),
            &format!(
                "[[service]]\n\
                 name = \"{service}\"\n\
                 roles = [\"{host_role}\"]\n\
                 rollback = \"automatic\"\n\
                 rollback_after_failed = {{ percent = 100, min_hosts = 1 }}\n"
            ),
            0o600,
        );
        put(&dir.join("oidc.toml"), &config_text(STATIC_ISSUER), 0o600);
        let (client_ca_pem, client_ca_der) = throwaway_ca();
        put(&dir.join("client-ca.pem"), &client_ca_pem, 0o644);

        let flags = serve_flags(dir, service);
        // The roots only feed the OIDC key fetcher, which no test reaches.
        let roots = Roots::Given(vec![CertificateDer::from(client_ca_der)]);
        let (started_tx, started_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("e2e-coordinator".to_owned())
            .spawn(move || serve_until_stopped(flags, roots, started_tx, ready_tx, stop_rx))
            .expect("spawn the coordinator's thread");

        let started = started_rx
            .recv()
            .expect("the coordinator's thread ended before it started")
            .unwrap_or_else(|why| panic!("the coordinator did not start: {why}"));
        let bound = ready_rx
            .recv()
            .expect("the coordinator ended before it bound its listeners")
            .unwrap_or_else(|why| panic!("the coordinator did not bind: {why}"));
        // The CA the daemon pins is also what `ctl` trusts for the private listener.
        put(
            &dir.join("ca.pem"),
            &pem("CERTIFICATE", &started.ca_der),
            0o644,
        );
        CoordinatorHandle {
            dir: dir.to_path_buf(),
            role: host_role.to_owned(),
            bound,
            started,
            stop: Some(stop_tx),
            thread: Some(thread),
        }
    }

    /// Where the daemon's `coordinator_url` points: the heartbeat listener. The name is
    /// `localhost` because the server certificate has DNS names only.
    pub fn heartbeat_url(&self) -> String {
        format!("https://localhost:{}", self.bound.heartbeat.port())
    }

    /// The SHA-256 of the enrollment CA certificate, for the daemon config's `ca_sha256`.
    pub fn ca_sha256(&self) -> String {
        self.started.ca_sha256.clone()
    }

    /// Runs `hostkeeper ctl <args>` against the private listener with the operator token and
    /// `--output json`, and gives what it ended with.
    pub fn ctl_output(&self, args: &[&str]) -> Output {
        let endpoint = format!("https://localhost:{}", self.bound.control.port());
        Command::new(DAEMON)
            .arg("ctl")
            .args(args)
            // The global flags may follow the command.
            .arg("--endpoint")
            .arg(endpoint)
            .arg("--ca-file")
            .arg(self.dir.join("ca.pem"))
            .arg("--token-file")
            .arg(self.dir.join("operator.token"))
            .args(["--output", "json"])
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("cannot run {DAEMON}: {e}"))
    }

    /// Runs `hostkeeper ctl <args>` ([`CoordinatorHandle::ctl_output`]) and gives the JSON it
    /// printed (`{}` when it printed nothing: a response with every field at its default).
    ///
    /// # Panics
    /// The command does not exit 0 (its output is in the message) or prints no JSON.
    pub fn ctl(&self, args: &[&str]) -> Value {
        let out = self.ctl_output(args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "ctl {args:?} failed: {}\nstdout: {stdout}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr),
        );
        if stdout.trim().is_empty() {
            return json!({});
        }
        serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("ctl {args:?} printed no JSON ({e}): {stdout}"))
    }

    /// Writes `request` as the file `name` of the coordinator's directory and gives its path.
    fn request_file(&self, name: &str, request: &Value) -> String {
        let path = self.dir.join(name);
        put(&path, &request.to_string(), 0o600);
        path.to_string_lossy().into_owned()
    }

    /// Waits for the daemon's enrollment request to be listed, then approves it as `host_id`
    /// with the role the coordinator was started for and the service `service` assigned.
    ///
    /// # Panics
    /// No request is listed (the daemon cannot reach the coordinator, or pins another CA) or
    /// the approval is refused.
    pub fn approve_enrollment(&self, host_id: &str, service: &str) {
        let mut request = None;
        wait_for("a pending enrollment request", ENROLLMENT_TRIES, || {
            let out = self.ctl_output(&["enrollment", "list"]);
            if !out.status.success() {
                return false;
            }
            let listed: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
            request = listed["requests"].get(0).cloned();
            request.is_some()
        });
        let request = request.expect("wait_for returned with a request");
        let id = request["requestId"].as_str().expect("a request id");
        // The request id is the key's SHA-256 too; the listing says it in both fields.
        let key = request["keySha256"].as_str().unwrap_or(id);
        self.ctl(&[
            "enrollment",
            "approve",
            "--request-id",
            id,
            "--key-sha256",
            key,
            "--host-id",
            host_id,
            "--role",
            &self.role,
            "--services",
            service,
            "--reason",
            "privileged test",
        ]);
    }

    /// Sets the desired version of `service` to `version`, whose package has the SHA-256
    /// `digest` (64 lower-case hex digits).
    pub fn set_desired(&self, service: &str, version: &str, digest: &str) {
        let file = self.request_file(
            "set-desired.json",
            &json!({"desired": {"version": version}}),
        );
        self.ctl(&[
            "releases",
            "set-desired",
            "--service",
            service,
            "--package-digest",
            digest,
            "--request",
            &file,
        ]);
    }

    /// Forces `service` to `version` on the host `host_id` at once, with no hook.
    pub fn force(&self, service: &str, version: &str, host_id: &str) {
        let file = self.request_file("force.json", &json!({"hosts": {"nodeIds": [host_id]}}));
        self.ctl(&[
            "releases",
            "force",
            "--service",
            service,
            "--version",
            version,
            "--reason",
            "privileged test",
            "--request",
            &file,
        ]);
    }

    /// How far the hosts of `service` are from its desired version: the `convergence` of
    /// `releases converge`, with `state` as `CONVERGENCE_STATE_*`.
    pub fn converge(&self, service: &str) -> Value {
        let reply = self.ctl(&["releases", "converge", "--service", service]);
        reply["convergence"].clone()
    }

    /// The status of `service`: the first of `services` in `releases status`, with its hosts,
    /// their versions and last verdicts, and its marks.
    pub fn status(&self, service: &str) -> Value {
        let reply = self.ctl(&["releases", "status", "--service", service]);
        reply["services"][0].clone()
    }
}

impl Drop for CoordinatorHandle {
    /// Stops the serving task and waits for the thread, which releases the state files' lock.
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
