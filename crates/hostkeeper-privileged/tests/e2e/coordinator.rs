//! The real coordinator, run inside the test process, and the operator's calls that drive it.
//!
//! The coordinator is the production one: [`hostkeeper_coordinator::config::build`] reads the
//! same flags `hostkeeper-coordinator serve` takes, and [`hostkeeper_coordinator::config::run`]
//! serves the heartbeat and private listeners with TLS on loopback ports the system picks. Its
//! enrollment CA is made at start (`--ca-init`), so a daemon enrolls against it exactly as a
//! real host does, and the private listener serves the CA-issued certificate for `localhost`.
//!
//! The operator side is the generated gRPC clients of `hostkeeper-proto` over the private
//! listener with the operator token ([`super::operator_api`]), each reply read as proto3 canonical
//! JSON. Nothing here calls the coordinator's Rust API directly, so the tests exercise the
//! surface an operator has.
//!
//! The coordinator runs on its own thread with a current-thread tokio runtime. It stops when
//! the handle is dropped: the serving task is aborted, which releases the lock of its state
//! files, and the thread is joined.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::future::poll_fn;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::pin::{Pin, pin};
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
use hostkeeper_proto::hostkeeper::operator::v1::{
    FailThreshold, HostGroup, RollbackMode, RolloutPolicy,
};
use hostkeeper_proto::hostkeeper::release::v1::{Repository, TargetList};
use rcgen::KeyUsagePurpose;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};
use rustls::pki_types::CertificateDer;
use serde_json::Value;

use super::operator_api::{Approval, OperatorApi, must, shown};
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

/// The policy of the group a coordinator is started with: the service rolls back by itself when
/// its release fails on the one host (`rollback` automatic, threshold 100% and at least one
/// host).
pub fn automatic_rollback() -> RolloutPolicy {
    RolloutPolicy {
        rollback: RollbackMode::Automatic as i32,
        rollback_after_failed: Some(FailThreshold {
            percent: Some(100),
            min_hosts: Some(1),
        }),
        ..RolloutPolicy::default()
    }
}

/// A coordinator serving in this process, and the operator's way to call it.
pub struct CoordinatorHandle {
    /// The host group the hosts of the fixture service enroll into: an approval's role is read
    /// as its name.
    role: String,
    /// The fixture service, which the group lists after `hostkeeper`.
    service: String,
    /// The group's policy, which a later put of the group keeps.
    policy: RolloutPolicy,
    /// Where the listeners are bound.
    bound: Bound,
    started: Started,
    /// The operator's calls to the private listener.
    api: OperatorApi,
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
    /// Starts a coordinator that manages `service` for hosts of the host group `host_role`, with
    /// its files in `dir` (made here, mode 0700), and waits until both listeners are bound. It
    /// puts the group, which lists `hostkeeper` and `service`, with the policy
    /// [`automatic_rollback`].
    ///
    /// The service comes from the registry's repository a test that orders `apt` entries
    /// registers ([`CoordinatorHandle::put_repository`], which also sets it as the service's
    /// repository); a test that orders none leaves the default, unregistered.
    ///
    /// # Panics
    /// A file cannot be written or the coordinator does not start.
    pub fn start(dir: &Path, service: &str, host_role: &str) -> CoordinatorHandle {
        CoordinatorHandle::start_with_policy(dir, service, host_role, automatic_rollback())
    }

    /// [`CoordinatorHandle::start`], with the group's policy `policy` instead of
    /// [`automatic_rollback`]: the keys of a rollout that it does not set (for example
    /// `bake_beats: 2`, a host baked for two healthy heartbeats; build it from
    /// [`automatic_rollback`] with `..`).
    ///
    /// # Panics
    /// A file cannot be written or the coordinator does not start.
    pub fn start_with_policy(
        dir: &Path,
        service: &str,
        host_role: &str,
        policy: RolloutPolicy,
    ) -> CoordinatorHandle {
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).expect("chmod the directory");
        // 64 hex digits: the token must be at least 32 visible ASCII characters.
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).expect("random bytes");
        put(&dir.join("operator.token"), &hex::encode(random), 0o600);
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
        // The CA the daemon pins is also what the operator trusts for the private listener.
        put(
            &dir.join("ca.pem"),
            &pem("CERTIFICATE", &started.ca_der),
            0o644,
        );
        let api = OperatorApi::open(
            &format!("https://localhost:{}", bound.control.port()),
            &dir.join("ca.pem"),
            &dir.join("operator.token"),
        );
        let handle = CoordinatorHandle {
            role: host_role.to_owned(),
            service: service.to_owned(),
            policy,
            bound,
            started,
            api,
            stop: Some(stop_tx),
            thread: Some(thread),
        };
        handle.put_group(&[service]);
        handle
    }

    /// Puts the host group the hosts enroll into, listing `hostkeeper` and then `packages`
    /// (`hostkeeper` once), with the policy of this handle.
    fn put_group(&self, packages: &[&str]) {
        let packages = std::iter::once("hostkeeper")
            .chain(packages.iter().copied().filter(|p| *p != "hostkeeper"))
            .map(str::to_owned)
            .collect();
        must(
            "host-groups put",
            self.api.put_host_group(HostGroup {
                name: self.role.clone(),
                packages,
                policy: Some(self.policy.clone()),
            }),
        );
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

    /// The operator's calls to the coordinator's private listener, for the calls this handle
    /// has no method for.
    pub fn api(&self) -> &OperatorApi {
        &self.api
    }

    /// Waits for the daemon's enrollment request to be listed, then approves it as `host_id`
    /// with the role the coordinator was started for, which is read as the name of the host
    /// group the host goes into.
    ///
    /// # Panics
    /// No request is listed (the daemon cannot reach the coordinator, or pins another CA) or
    /// the approval is refused.
    pub fn approve_enrollment(&self, host_id: &str) {
        let mut request = None;
        wait_for("a pending enrollment request", ENROLLMENT_TRIES, || {
            let Ok(listed) = self.api.pending() else {
                return false;
            };
            request = listed["requests"].get(0).cloned();
            request.is_some()
        });
        let request = request.expect("wait_for returned with a request");
        let id = request["requestId"].as_str().expect("a request id");
        // The request id is the key's SHA-256 too; the listing says it in both fields.
        let key = request["keySha256"].as_str().unwrap_or(id);
        must(
            "enrollment approve",
            self.api.approve(&Approval {
                request_id: id,
                key_sha256: key,
                host_id,
                role: &self.role,
                reason: "privileged test",
                ..Approval::default()
            }),
        );
    }

    /// The host `node_id` as `Hosts.Get` gives it (`Null` when the coordinator knows none): among
    /// the rest, its last heartbeat as it sent it, `heartbeat`.
    pub fn host(&self, node_id: &str) -> Value {
        self.api
            .get_host(node_id)
            .map_or(Value::Null, |got| got["host"].clone())
    }

    /// Sets the desired version of `service` to `version` by targets alone, as CI's set of a
    /// release whose entries are all native does: `targets` by platform, no release index and no
    /// index digest.
    pub fn set_desired_targets_alone(
        &self,
        service: &str,
        version: &str,
        targets: BTreeMap<String, TargetList>,
    ) {
        must(
            "releases set-desired by targets alone",
            self.api
                .set_desired_targets_alone(service, version, targets),
        );
    }

    /// Registers `repository` in the coordinator's registry of package repositories and sets it
    /// as the repository of the fixture service (a set of apt entries needs the service's
    /// repository, with the entries' repository one of its channels).
    pub fn put_repository(&self, repository: Repository) {
        let name = repository.name.clone();
        must("repositories put", self.api.put_repository(repository));
        must(
            "services set-repository",
            self.api.set_repository(&self.service, &name),
        );
    }

    /// Forces `service` to `version` on the host `host_id` at once, with no hook.
    pub fn force(&self, service: &str, version: &str, host_id: &str) {
        must(
            "releases force",
            self.api.force(service, version, host_id, "privileged test"),
        );
    }

    /// How far the hosts of `service` are from its desired version: the `convergence` of
    /// `Releases.Converge`, with `state` as `CONVERGENCE_STATE_*`.
    pub fn converge(&self, service: &str) -> Value {
        let reply = must("releases converge", self.api.converge(service));
        reply["convergence"].clone()
    }

    /// The status of `service` in the host group the hosts enroll into: its entry of `services`
    /// in `Releases.Status` (the built-in group `all` has one too, with none of the hosts, since
    /// a package the host's group lists is run as that group says), with its hosts, their
    /// versions and last verdicts, and its marks.
    pub fn status(&self, service: &str) -> Value {
        let reply = must("releases status", self.api.status(service));
        reply["services"]
            .as_array()
            .and_then(|entries| entries.iter().find(|entry| entry["hostGroup"] == self.role))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// The services the coordinator lists, as `Services.List` gives them (`services`).
    pub fn services(&self) -> Value {
        must("services list", self.api.list_services())
    }

    /// Takes `service` out of the packages of the host group, which orders its hosts to
    /// uninstall it.
    pub fn uninstall(&self, service: &str) {
        let packages: Vec<&str> = [self.service.as_str()]
            .into_iter()
            .filter(|p| *p != service)
            .collect();
        self.put_group(&packages);
    }

    /// The text of `Releases.Status` of `service` for a failed test's report: the JSON of the
    /// reply, or the status the call ended with. Never panics.
    pub fn status_text(&self, service: &str) -> String {
        shown(self.api.status(service))
    }

    /// The text of `Releases.Converge` of `service`, as [`CoordinatorHandle::status_text`].
    pub fn converge_text(&self, service: &str) -> String {
        shown(self.api.converge(service))
    }

    /// The text of `Services.List`, as [`CoordinatorHandle::status_text`].
    pub fn services_text(&self) -> String {
        shown(self.api.list_services())
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
