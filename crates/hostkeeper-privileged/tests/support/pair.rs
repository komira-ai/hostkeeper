//! A coordinator and a daemon, each the real program started as a child process, on loopback:
//! the harness of the first-boot tests (`tests/first_boot.rs`, `tests/install_uninstall.rs`).
//!
//! The tests drive the two only through what an operator and a host have: the coordinator's
//! flags and its operator API (the generated gRPC clients, through [`OperatorApi`]), the
//! daemon's flags and config file, and the files the daemon keeps on the host. Each file the
//! programs are given is in the test's scratch directory, except the paths the daemon fixes
//! ([`HostPaths`]).

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SanType,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::operator_api::{Approval, OperatorApi, must};
use super::spawn::{Running, poll};

/// The coordinator program, built by cargo with these tests: it forwards to the coordinator's
/// command line, as the `hostkeeper-coordinator` binary does.
pub const COORDINATOR: &str = env!("CARGO_BIN_EXE_hostkeeper-privileged-coordinator");
/// The daemon program, built by cargo with these tests: it forwards to the daemon's command
/// line, as the `hostkeeper` binary does.
pub const DAEMON: &str = env!("CARGO_BIN_EXE_hostkeeper-device-daemon");

/// Where the daemon keeps its key and certificates: a path it fixes, with no flag.
pub const PKI_DIR: &str = "/etc/updater/pki";
/// The role every test host is approved for.
pub const ROLE: &str = "ci";
/// The service the rollout file runs on [`ROLE`], which CI's allow-list names (an entry names
/// services the rollout file defines); no test sets a desired version of it, so the coordinator
/// orders nothing for it.
pub const BEACON: &str = "beacon";
/// The operator's token.
const OPERATOR_TOKEN: &str = "operator-token-of-the-first-boot-tests-0123456789";

/// The paths the daemon writes outside its flags, each refused at the start if the host already
/// has it (so that a test never touches a host's real service directories) and removed when the
/// guard is dropped, failed or not: the pki directory's parent `/etc/updater`, the self-update
/// units it installs (`hostkeeper-fallback.service`, `hostkeeper.service.d/`), and the placement
/// state `/var/lib/hostkeeper`; and, given to [`HostPaths::claim`], those an install test adds
/// and the units it may leave running, which are stopped first.
pub struct HostPaths {
    paths: Vec<PathBuf>,
    units: Vec<String>,
}

/// The directory of the daemon's placement state, which it does not make itself.
const PLACEMENT_DIR: &str = "/var/lib/hostkeeper";

/// The daemon's packages directory: under [`PLACEMENT_DIR`], so that it is removed with it. A hook
/// runs only from a version directory whose every parent is root's and writable by no one else,
/// which no directory of the test's checkout is (the build is the runner's user's).
pub const PACKAGES_DIR: &str = "/var/lib/hostkeeper/packages";

/// The daemon's own fixed paths.
const DAEMON_PATHS: [&str; 4] = [
    "/etc/updater",
    "/etc/systemd/system/hostkeeper-fallback.service",
    "/etc/systemd/system/hostkeeper.service.d",
    PLACEMENT_DIR,
];

impl HostPaths {
    /// Claims the daemon's fixed paths and `more`, and the units `units`: fails the test, naming
    /// the path, unless none of the paths exists.
    pub fn claim(more: &[&str], units: &[&str]) -> HostPaths {
        let paths: Vec<PathBuf> = DAEMON_PATHS.iter().chain(more).map(PathBuf::from).collect();
        for path in &paths {
            assert!(
                fs::symlink_metadata(path).is_err(),
                "{} exists on this host: the first-boot tests run only on a host that has no \
                 hostkeeper (privileged.yml's thrown-away runner), and never touch one that has",
                path.display()
            );
        }
        // The placement state's directory, which the package that installs the daemon makes.
        fs::create_dir_all(PLACEMENT_DIR).expect("the placement state directory");
        HostPaths {
            paths,
            units: units.iter().map(|u| (*u).to_owned()).collect(),
        }
    }
}

impl Drop for HostPaths {
    fn drop(&mut self) {
        for unit in &self.units {
            let _ = super::run(
                hostkeeper_steps::systemd::SYSTEMCTL,
                &["stop", "--system", unit.as_str()],
            );
        }
        for path in &self.paths {
            // A path the test never made is not there; anything else is the test's own.
            let _ = fs::remove_dir_all(path).or_else(|_| fs::remove_file(path));
        }
        let _ = super::run(hostkeeper_steps::systemd::SYSTEMCTL, &["daemon-reload"]);
    }
}

/// Writes `text` to `path` with `mode`.
pub fn put(path: &Path, text: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir {}: {e}", parent.display()));
    }
    fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set the mode");
}

/// The SHA-256 of `bytes`, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A test certificate authority: of the coordinator's private listener (which the operator dials) and
/// of the release channel.
pub struct TestCa {
    issuer: CertifiedIssuer<'static, KeyPair>,
}

/// A certificate and its key, PEM.
pub struct Leaf {
    pub cert_pem: String,
    pub key_pem: String,
    pub cert_der: Vec<u8>,
    /// PKCS#8.
    pub key_der: Vec<u8>,
}

impl TestCa {
    pub fn new(common_name: &str) -> TestCa {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, common_name);
        params.distinguished_name = name;
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let issuer = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("a key"))
            .expect("a CA certificate");
        TestCa { issuer }
    }

    pub fn pem(&self) -> String {
        self.issuer.pem()
    }

    /// A server certificate for the loopback address `127.0.0.1`.
    pub fn loopback_server(&self) -> Leaf {
        self.server_for(Ipv4Addr::LOCALHOST)
    }

    /// A server certificate for the address `ip`.
    pub fn server_for(&self, ip: Ipv4Addr) -> Leaf {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.subject_alt_names = vec![SanType::IpAddress(IpAddr::V4(ip))];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate().expect("a key");
        let cert = params.signed_by(&key, &self.issuer).expect("signed");
        Leaf {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
            cert_der: cert.der().to_vec(),
            key_der: key.serialize_der(),
        }
    }
}

/// The rollout file: [`BEACON`] on [`ROLE`], and `more` (TOML `[[service]]` tables) after it.
pub fn rollout(more: &str) -> String {
    format!("[[service]]\nname = \"{BEACON}\"\nroles = [\"{ROLE}\"]\n{more}")
}

/// Whether `host`, as `hosts list` gives it, was heard since the coordinator started: it has a
/// heartbeat time. A host listed from its enrollment alone has none (proto3 JSON leaves the 0
/// out), and may still carry the daemon its enrollment kept in `heartbeat.agent`.
fn heard_since_start(host: &Value) -> bool {
    host["lastHeartbeatUnix"].is_string()
}

/// The coordinator: the files its flags name, under `<scratch>/coordinator`, and the running
/// program. It keeps its ports across a restart, so that the daemon, which dials one address,
/// finds it again.
pub struct Coordinator {
    dir: PathBuf,
    extra: Vec<String>,
    pub heartbeat: SocketAddr,
    pub control: SocketAddr,
    /// The LAN listener (`--lan-listen`), when it has one.
    pub lan: Option<SocketAddr>,
    run: Option<Running>,
    /// The operator's calls, made at the first.
    api: OnceLock<OperatorApi>,
}

impl Coordinator {
    /// Writes the coordinator's files (the rollout file is `rollout`) and starts it with
    /// enrollment, making its CA (`--ca-init`), on ports of loopback the system picks; `extra`
    /// are more flags of `serve`, given at every start.
    pub fn start(scratch: &Path, rollout: &str, extra: &[&str]) -> Coordinator {
        Coordinator::start_with(scratch, rollout, extra, Ipv4Addr::LOCALHOST, false)
    }

    /// [`Self::start`], with the private listener on `address` (its certificate for that
    /// address) and a LAN listener there too (`--lan-listen`), each on a port the system picks:
    /// the coordinator of the network-install tests, which `hostkeeper-pxe` reads over the
    /// farm LAN and a network-installed host dials.
    pub fn start_on_lan(
        scratch: &Path,
        rollout: &str,
        extra: &[&str],
        address: Ipv4Addr,
    ) -> Coordinator {
        Coordinator::start_with(scratch, rollout, extra, address, true)
    }

    fn start_with(
        scratch: &Path,
        rollout: &str,
        extra: &[&str],
        address: Ipv4Addr,
        lan: bool,
    ) -> Coordinator {
        let dir = scratch.join("coordinator");
        let ca = TestCa::new("private listener CA");
        let leaf = ca.server_for(address);
        put(&dir.join("private-ca.pem"), ca.pem().as_bytes(), 0o644);
        put(&dir.join("tls.pem"), leaf.cert_pem.as_bytes(), 0o644);
        put(&dir.join("tls.key"), leaf.key_pem.as_bytes(), 0o600);
        // The farm CA of the hosts that are not enrolled by this coordinator: none here, so a
        // CA of its own that signs nothing.
        put(
            &dir.join("client-ca.pem"),
            TestCa::new("no farm CA").pem().as_bytes(),
            0o644,
        );
        put(
            &dir.join("operator.token"),
            OPERATOR_TOKEN.as_bytes(),
            0o600,
        );
        put(&dir.join("rollout.toml"), rollout.as_bytes(), 0o600);
        // CI's allow-list: required, and no test presents a CI token.
        put(
            &dir.join("oidc.toml"),
            format!(
                "[remote_operator]\nissuer = \"https://ci-issuer.invalid\"\naudience = \
                 \"hostkeeper\"\nrepository_id = \"1\"\nrepository = \"example-org/example-repo\"\n\
                 job_workflow_ref = [\"example-org/example-repo/.github/workflows/release.yml@refs/heads/main\"]\n\
                 refs = [\"refs/heads/main\"]\nservices = [\"{BEACON}\"]\n"
            )
            .as_bytes(),
            0o644,
        );
        for sub in ["state", "ca", "enrollment"] {
            fs::create_dir_all(dir.join(sub)).expect("a directory of the coordinator");
            fs::set_permissions(dir.join(sub), fs::Permissions::from_mode(0o700))
                .expect("set the mode");
        }
        let mut coordinator = Coordinator {
            dir,
            extra: extra.iter().map(|s| (*s).to_owned()).collect(),
            heartbeat: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            control: SocketAddr::from((address, 0)),
            lan: lan.then(|| SocketAddr::from((address, 0))),
            run: None,
            api: OnceLock::new(),
        };
        coordinator.launch(true);
        coordinator
    }

    fn launch(&mut self, init: bool) {
        let path = |file: &str| self.dir.join(file).display().to_string();
        let mut args: Vec<String> = [
            "serve",
            "--heartbeat-listen",
            &self.heartbeat.to_string(),
            "--private-listen",
            &self.control.to_string(),
            "--tls-cert",
            &path("tls.pem"),
            "--tls-key",
            &path("tls.key"),
            "--client-ca",
            &path("client-ca.pem"),
            "--state-file",
            &path("state/desired.json"),
            "--oidc-config",
            &path("oidc.toml"),
            "--operator-token-file",
            &path("operator.token"),
            "--rollout-file",
            &path("rollout.toml"),
            "--enrollment-dir",
            &path("enrollment"),
            "--ca-key-file",
            &path("ca/ca.key"),
            "--server-cert-file",
            &path("ca/server.pem"),
            "--server-name",
            "localhost",
        ]
        .map(String::from)
        .to_vec();
        if let Some(lan) = self.lan {
            args.extend(["--lan-listen".to_owned(), lan.to_string()]);
        }
        if init {
            args.push("--ca-init".to_owned());
        }
        args.extend(self.extra.iter().cloned());
        // The start lines of the earlier starts, counted before this one can print its own.
        let starts = self.starts();
        let mut run = Running::start("coordinator", &self.dir, COORDINATOR, &args);
        let line = poll("the coordinator's start line", || {
            run.assert_running();
            let out = run.stdout();
            out.lines()
                .filter(|l| l.starts_with("hostkeeper-coordinator "))
                .nth(starts)
                .map(str::to_owned)
                .ok_or(out)
        });
        let addr = |key: &str| -> SocketAddr {
            line.split(' ')
                .find_map(|word| word.strip_prefix(key))
                .and_then(|a| a.parse().ok())
                .unwrap_or_else(|| panic!("no {key} in the start line {line:?}"))
        };
        self.heartbeat = addr("heartbeat=");
        self.control = addr("control=");
        if self.lan.is_some() {
            self.lan = Some(addr("lan="));
        }
        self.run = Some(run);
    }

    /// How many start lines the coordinator has printed so far, over all its starts.
    fn starts(&self) -> usize {
        fs::read_to_string(self.dir.join("coordinator.out"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("hostkeeper-coordinator "))
            .count()
    }

    /// Kills the coordinator and starts it again on the same files and ports, without
    /// `--ca-init`: the CA, the enrollment store and the state files are what it reads back.
    pub fn restart(&mut self) {
        self.kill();
        self.launch(false);
    }

    /// Kills the coordinator.
    pub fn kill(&mut self) {
        if let Some(run) = self.run.take() {
            run.kill();
        }
    }

    /// Starts the coordinator again after [`Self::kill`].
    pub fn start_again(&mut self) {
        assert!(self.run.is_none(), "the coordinator is running");
        self.launch(false);
    }

    /// Fails the test at once if the coordinator has ended.
    pub fn assert_running(&mut self) {
        if let Some(run) = self.run.as_mut() {
            run.assert_running();
        }
    }

    /// What the coordinator wrote to its standard error, over all its starts.
    pub fn log(&self) -> String {
        fs::read_to_string(self.dir.join("coordinator.err")).unwrap_or_default()
    }

    /// The SHA-256 of the enrollment CA's certificate (DER, beside its key): the pin a daemon
    /// config holds.
    pub fn ca_sha256(&self) -> String {
        sha256_hex(&fs::read(self.ca_cert_path()).expect("the CA certificate"))
    }

    /// The file of the verdicts it acknowledged, beside its state file (`--state-file`).
    pub fn verdicts_path(&self) -> PathBuf {
        self.dir.join("state/desired.json.verdicts")
    }

    /// The enrollment CA's certificate, DER: beside its key (`--ca-key-file`), `.cert` added.
    pub fn ca_cert_path(&self) -> PathBuf {
        self.dir.join("ca/ca.key.cert")
    }

    /// The CA of the private listener's certificate, PEM: what the operator's calls and
    /// `hostkeeper-pxe serve` take as the CA.
    pub fn private_ca_path(&self) -> PathBuf {
        self.dir.join("private-ca.pem")
    }

    /// The operator's token file (root's, mode 0600): what the operator's calls and
    /// `hostkeeper-pxe serve` take as `--token-file`.
    pub fn token_path(&self) -> PathBuf {
        self.dir.join("operator.token")
    }

    /// The URL of the private listener, by its address.
    pub fn control_url(&self) -> String {
        format!("https://{}", self.control)
    }

    /// The URL a network-installed host dials: the LAN listener, by its address, which the
    /// server certificate holds as an IP SAN.
    pub fn lan_url(&self) -> String {
        let lan = self.lan.expect("a coordinator with a LAN listener");
        format!("https://{lan}")
    }

    /// The URL a daemon dials: the heartbeat listener, by the name its server certificate holds.
    pub fn url(&self) -> String {
        format!("https://localhost:{}", self.heartbeat.port())
    }

    /// The operator's calls to the private listener, with the operator's token. Made at the first
    /// call, when the listener's address is known.
    pub fn api(&self) -> &OperatorApi {
        self.api.get_or_init(|| {
            OperatorApi::open(
                &self.control_url(),
                &self.private_ca_path(),
                &self.token_path(),
            )
        })
    }

    /// The pending enrollment requests, as `enrollment list` gives them.
    pub fn pending(&self) -> Result<Vec<Value>, String> {
        let list = self.api().pending().map_err(|e| e.to_string())?;
        Ok(list["requests"].as_array().cloned().unwrap_or_default())
    }

    /// The entry of `service` in `services list`.
    pub fn service(&self, service: &str) -> Result<Value, String> {
        let list = self.api().list_services().map_err(|e| e.to_string())?;
        list["services"]
            .as_array()
            .and_then(|all| all.iter().find(|s| s["service"] == service).cloned())
            .ok_or_else(|| format!("services list has no {service}: {list}"))
    }

    /// The host `node_id` as `hosts list` gives it, or `None`: the coordinator lists the hosts it
    /// has heard from since it started, each with its role, its enrollment, its last heartbeat and
    /// how each service sees it, and every enrolled host not denied that it has not heard since,
    /// as its enrollment kept it (not live, no heartbeat time).
    pub fn host(&self, node_id: &str) -> Result<Option<Value>, String> {
        let list = self.api().list_hosts().map_err(|e| e.to_string())?;
        Ok(list["hosts"]
            .as_array()
            .and_then(|hosts| hosts.iter().find(|h| h["nodeId"] == node_id).cloned()))
    }

    /// Waits until `hosts list` lists `node_id` with a heartbeat time: the coordinator has heard
    /// the host's heartbeat since it started. Gives the host as listed. A host listed from its
    /// enrollment alone (right after its approval, or after the coordinator's restart) is waited
    /// past.
    pub fn wait_heard(&mut self, node_id: &str, daemon: &mut Daemon) -> Value {
        poll(&format!("a heartbeat of {node_id}"), || {
            self.assert_running();
            daemon.assert_running();
            let host = self
                .host(node_id)?
                .ok_or_else(|| format!("hosts list lists no {node_id}"))?;
            if heard_since_start(&host) {
                Ok(host)
            } else {
                Err(format!("hosts list has not heard {node_id}: {host}"))
            }
        })
    }

    /// `hosts list` lists `node_id` as enrolled and not heard since the coordinator started: not
    /// live and with no heartbeat time. Gives the host as listed.
    pub fn assert_unheard(&self, node_id: &str) -> Value {
        let host = self
            .host(node_id)
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|| panic!("hosts list lists no {node_id}, an enrolled host"));
        assert!(
            !heard_since_start(&host),
            "not heard since the start: {host}"
        );
        assert_eq!(host["live"], Value::Null, "not live: {host}");
        host
    }

    /// Approves the one pending request as host `node_id` of [`ROLE`], once the daemon's request
    /// is pending, and gives the request as `enrollment list` gave it.
    pub fn approve_first(&mut self, node_id: &str, daemon: &mut Daemon) -> Value {
        let request = poll("the daemon's enrollment request", || {
            self.assert_running();
            daemon.assert_running();
            let pending = self.pending()?;
            match pending.as_slice() {
                [one] => Ok(one.clone()),
                other => Err(format!("{} pending: {other:?}", other.len())),
            }
        });
        let id = request["requestId"].as_str().expect("a request id");
        let key = request["keySha256"].as_str().expect("a key hash");
        must(
            "enrollment approve",
            self.api().approve(&Approval {
                request_id: id,
                key_sha256: key,
                host_id: node_id,
                role: ROLE,
                reason: "first-boot test",
                ..Approval::default()
            }),
        );
        request
    }
}

/// The daemon: its config and state under `<scratch>/daemon`, its packages in [`PACKAGES_DIR`], and
/// the running program.
pub struct Daemon {
    dir: PathBuf,
    socket: PathBuf,
    run: Option<Running>,
}

impl Daemon {
    /// Writes the daemon's config (the coordinator's URL and CA pin, the release channel
    /// `channel`) and starts it, beating every second.
    pub fn start(scratch: &Path, coordinator: &Coordinator, channel: &str) -> Daemon {
        let dir = scratch.join("daemon");
        let config = format!(
            "coordinator = \"{}\"\nca_sha256 = \"{}\"\nrelease_channel = \"{channel}\"\n\n\
             [trusted_signers]\norganisation = \"example-org\"\nworkflow = \
             \".github/workflows/release.yml\"\nrefs = [\"refs/tags/*\"]\nhosted_runner = true\n",
            coordinator.url(),
            coordinator.ca_sha256()
        );
        put(&dir.join("hostkeeper.toml"), config.as_bytes(), 0o644);
        // The packages directory is the host's (made by the package that installs the daemon);
        // the daemon makes its state directory.
        fs::create_dir_all(PACKAGES_DIR).expect("the packages directory");
        // The `--admin-socket` path: the daemon of this checkout ignores the flag, an older
        // release binds it, and a socket's path is at most 107 bytes, so it is right under the
        // scratch directory.
        let socket = scratch.join("run/a.sock");
        let mut daemon = Daemon {
            dir,
            socket,
            run: None,
        };
        daemon.launch();
        daemon
    }

    fn launch(&mut self) {
        let path = |p: &str| self.dir.join(p).display().to_string();
        let args = [
            "daemon",
            "--config",
            &path("hostkeeper.toml"),
            "--state-dir",
            &path("state"),
            "--packages-dir",
            PACKAGES_DIR,
            "--admin-socket",
            &self.socket.display().to_string(),
            "--heartbeat-secs",
            "1",
        ]
        .map(String::from);
        self.run = Some(Running::start("daemon", &self.dir, DAEMON, &args));
    }

    /// Kills the daemon (as a crash or a power cut would) and starts it again on the same
    /// config, state and packages: its start runs the boot pass.
    pub fn restart(&mut self) {
        self.kill();
        self.launch();
    }

    /// Kills the daemon.
    pub fn kill(&mut self) {
        if let Some(run) = self.run.take() {
            run.kill();
        }
    }

    /// Starts the daemon again after [`Self::kill`].
    pub fn start_again(&mut self) {
        assert!(self.run.is_none(), "the daemon is running");
        self.launch();
    }

    /// Fails the test at once if the daemon has ended.
    pub fn assert_running(&mut self) {
        if let Some(run) = self.run.as_mut() {
            run.assert_running();
        }
    }

    /// What the daemon wrote to its standard error, over all its starts.
    pub fn log(&self) -> String {
        fs::read_to_string(self.dir.join("daemon.err")).unwrap_or_default()
    }

    /// The step record of `service` the daemon keeps, as JSON: `null` when it cannot be read.
    pub fn step_record(&self, service: &str) -> Value {
        fs::read(
            self.dir
                .join("state")
                .join(service)
                .join("step-record.json"),
        )
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
    }

    /// The daemon's packages directory.
    pub fn packages(&self) -> PathBuf {
        PathBuf::from(PACKAGES_DIR)
    }
}

/// The host certificate the daemon holds now: `<pki>/current`, read from the host's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCertificate {
    /// The generation `current` points to.
    pub generation: PathBuf,
    /// The subject, as `CN=...`.
    pub subject: String,
    /// The issuer, as `CN=...`.
    pub issuer: String,
    /// The serial number, hex.
    pub serial: String,
    /// The SHA-256 of the certificate's SubjectPublicKeyInfo, lowercase hex: the key hash an
    /// enrollment request is listed by.
    pub key_sha256: String,
}

impl HostCertificate {
    /// The certificate of `<pki>/current`, or why there is none yet.
    pub fn current() -> Result<HostCertificate, String> {
        let current = Path::new(PKI_DIR).join("current");
        let generation =
            fs::read_link(&current).map_err(|e| format!("{}: {e}", current.display()))?;
        let pem = fs::read(current.join("cert.pem"))
            .map_err(|e| format!("{}/cert.pem: {e}", current.display()))?;
        let (_, pem) =
            x509_parser::pem::parse_x509_pem(&pem).map_err(|e| format!("cert.pem: {e}"))?;
        let cert = pem.parse_x509().map_err(|e| format!("cert.pem: {e}"))?;
        Ok(HostCertificate {
            generation,
            subject: cert.subject().to_string(),
            issuer: cert.issuer().to_string(),
            serial: cert.raw_serial_as_string(),
            key_sha256: sha256_hex(cert.public_key().raw),
        })
    }
}
