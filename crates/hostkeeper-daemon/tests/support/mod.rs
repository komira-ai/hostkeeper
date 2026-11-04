//! Shared by the tests: a scratch directory with a state directory, a packages directory and a
//! daemon config, the fakes of the daemon's ports, orders, and a gRPC client over an in-process
//! pipe.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

pub mod force;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::boot::BootReport;
use hostkeeper_daemon::prune::NoPrune;
use hostkeeper_daemon::start::Settings;
use hostkeeper_daemon::testing::{FakeClock, FakePackages, Log, ScriptedLauncher, manifest};
use hostkeeper_daemon::{Daemon, Dirs, Placement, Ports, StoreSeam};
use hostkeeper_hook::Verdict;
use hostkeeper_placed::Roots;
use hostkeeper_placed::testing::{FakeUnitControl, FaultFs, Log as PlacedLog};
use hostkeeper_proto::hostkeeper::agent::v1::{Force, ForceRollback, Order, Update, order};
use hostkeeper_proto::hostkeeper::local::v1::admin_client::AdminClient;
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::{CrashPoint, Finished, RecordStore};
use hyper_util::rt::TokioIo;
use tokio::io::DuplexStream;
use tonic::transport::{Channel, Endpoint, Uri};

/// A directory under the target directory, unique to the test, removed when it is dropped.
///
/// The step runner checks a staged release and every directory above it before it runs a step
/// (`Staged::check`): none may be writable by group or others. The tests that run steps
/// therefore need a target directory whose parents are closed to group and others (a umask of
/// 022, as on the CI runners), as the step runner's own tests do.
pub struct Scratch(hostkeeper_testkit::Scratch);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let dir = hostkeeper_testkit::Scratch::under(base, name).expect("make the scratch dir");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).expect("set its mode");
        Scratch(dir)
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

/// A daemon config every rule accepts.
pub const CONFIG: &str = r#"coordinator = "https://coordinator.example"
ca_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// The version of the daemon under test.
pub const OWN: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 1,
    patch: 0,
};

pub fn config() -> DaemonConfig {
    DaemonConfig::parse(CONFIG).expect("the test config loads")
}

/// The uid the tests run as: the owner of what they make.
pub fn my_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// A package digest: `pair` 32 times.
pub fn digest(pair: &str) -> String {
    pair.repeat(32)
}

/// The digest most orders name.
pub fn d() -> String {
    digest("ab")
}

/// An `update` order.
pub fn update(service: &str, version: &str, package_digest: &str, order_try: u64) -> Order {
    Order {
        order_id: format!("order-{service}-{version}"),
        r#try: order_try,
        verb: Some(order::Verb::Update(Update {
            service: service.to_owned(),
            version: version.to_owned(),
            package_digest: package_digest.to_owned(),
        })),
    }
}

/// A `force_rollback` order.
pub fn force_rollback(service: &str, version: &str, package_digest: &str, order_try: u64) -> Order {
    Order {
        order_id: format!("rollback-{service}-{version}"),
        r#try: order_try,
        verb: Some(order::Verb::ForceRollback(ForceRollback {
            service: service.to_owned(),
            version: version.to_owned(),
            package_digest: package_digest.to_owned(),
        })),
    }
}

/// A `force` order.
pub fn force(
    service: &str,
    version: &str,
    package_digest: &str,
    order_try: u64,
    halt_generation: u64,
) -> Order {
    Order {
        order_id: format!("force-{service}-{version}"),
        r#try: order_try,
        verb: Some(order::Verb::Force(Force {
            service: service.to_owned(),
            version: version.to_owned(),
            package_digest: package_digest.to_owned(),
            halt_generation,
        })),
    }
}

/// A unit's `WantedBy=` target, spelt in two pieces so that this file names no host.
pub const TARGET: &str = concat!("multi-user.", "target");

/// A systemd unit wanted by [`TARGET`].
pub fn unit_text() -> String {
    format!(
        "[Unit]\nDescription=a test unit\n\n[Service]\nExecStart=/usr/bin/true\n\n[Install]\nWantedBy={TARGET}\n"
    )
}

/// The directories placed-path linking works in, as the rig's host names them: the rig's
/// scratch directory is that host's root, so `/packages` is [`Rig::packages`].
pub fn roots() -> Roots {
    Roots::new(
        Path::new("/placed"),
        Path::new("/units"),
        Path::new("/packages"),
    )
}

/// A scratch directory with the daemon's directories, its config and fakes for its ports.
pub struct Rig {
    pub scratch: Scratch,
    pub state: PathBuf,
    pub packages: PathBuf,
    pub config: PathBuf,
    pub socket: PathBuf,
    pub clock: Arc<FakeClock>,
    pub launcher: Arc<ScriptedLauncher>,
    pub store: Arc<FakePackages>,
    /// The service manager; its calls are in [`Rig::placed_log`].
    pub units: Arc<FakeUnitControl>,
    /// The writes of placed-path linking and the calls of the service manager, in order.
    pub placed_log: PlacedLog,
    /// The filesystem placed-path linking goes through: the scratch directory as `/`.
    pub placed_fs: Arc<FaultFs>,
}

impl Rig {
    pub fn new(name: &str) -> Rig {
        let scratch = Scratch::new(name);
        let root = scratch.path().to_path_buf();
        let config = root.join("hostkeeper.toml");
        fs::write(&config, CONFIG).expect("write the config");
        fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).expect("its mode");
        let packages = root.join("packages");
        fs::create_dir(root.join("placed")).expect("the directory of placed.json");
        let placed_log = PlacedLog::new();
        Rig {
            state: root.join("state"),
            store: FakePackages::new(&packages),
            packages,
            config,
            socket: root.join("r").join("a.sock"),
            clock: FakeClock::new(),
            launcher: ScriptedLauncher::new(),
            units: Arc::new(FakeUnitControl::new(placed_log.clone())),
            placed_fs: Arc::new(FaultFs::new(&root, placed_log.clone())),
            placed_log,
            scratch,
        }
    }

    pub fn ports(&self) -> Ports {
        Ports {
            launcher: self.launcher.clone(),
            packages: self.store.clone(),
            clock: self.clock.clone(),
            deadman_poll_secs: None,
            units: self.units.clone(),
            placement: Placement {
                roots: roots(),
                fs: self.placed_fs.clone(),
            },
            prune: Arc::new(NoPrune),
        }
    }

    /// A version of `service` with units, sealed with the package digest [`d`]: each of `units`
    /// is a file of `units/` wanted by [`TARGET`], `run` are the units its manifest starts, and
    /// each of `files` is a file `files/<name>` placed at its path.
    pub fn add_units(
        &self,
        service: &str,
        version: &str,
        units: &[&str],
        run: &[&str],
        files: &[(&str, &str)],
    ) -> PathBuf {
        let mut text = manifest(service, version);
        if !run.is_empty() {
            let run: Vec<String> = run.iter().map(|unit| format!("\"{unit}\"")).collect();
            text.push_str(&format!("\n[units]\nrun = [{}]\n", run.join(", ")));
        }
        for (name, path) in files {
            text.push_str(&format!(
                "\n[[files]]\nsource = \"files/{name}\"\npath = \"{path}\"\n"
            ));
        }
        let dir = self.store.add(service, version, &text);
        for (sub, names) in [
            ("units", units.to_vec()),
            ("files", files.iter().map(|(name, _)| *name).collect()),
        ] {
            fs::create_dir_all(dir.join(sub)).expect("a directory of the package");
            for name in names {
                fs::write(dir.join(sub).join(name), unit_text()).expect("a file of the package");
            }
        }
        self.store.seal(service, version, &d());
        dir
    }

    pub fn dirs(&self) -> Dirs {
        Dirs {
            state: self.state.clone(),
            packages: self.packages.clone(),
        }
    }

    /// A daemon on the rig's directories, with no dead-men.
    pub fn daemon(&self) -> Daemon {
        Daemon::new(config(), self.dirs(), self.ports(), OWN)
    }

    pub fn settings(&self) -> Settings {
        Settings {
            config: self.config.clone(),
            state_dir: self.state.clone(),
            packages_dir: self.packages.clone(),
            admin_socket: self.socket.clone(),
            trusted_uid: my_uid(),
        }
    }

    /// The package of `service` at `version`, with a manifest every rule accepts.
    pub fn add(&self, service: &str, version: &str) -> PathBuf {
        self.store
            .add(service, version, &manifest(service, version))
    }

    /// `service` runs `version`: its package is there and `current` names it.
    pub fn running(&self, service: &str, version: &str) -> PathBuf {
        let dir = self.add(service, version);
        self.store.link(service, "current", version);
        dir
    }

    /// The package of `service` at `version` with `manifest`, sealed with the package digest
    /// [`d`].
    pub fn add_with(&self, service: &str, version: &str, manifest: &str) -> PathBuf {
        let dir = self.store.add(service, version, manifest);
        self.store.seal(service, version, &d());
        dir
    }

    /// A daemon on the rig's directories whose start pass ran: the boot id of the rig's clock
    /// is written.
    pub fn booted(&self) -> (Daemon, BootReport) {
        fs::create_dir_all(&self.state).expect("the state dir");
        fs::set_permissions(&self.state, fs::Permissions::from_mode(0o700)).expect("its mode");
        let daemon = self.daemon();
        let report = daemon.boot_pass().expect("the start pass");
        (daemon, report)
    }

    /// The host booted again: the clock's boot id is `boot-2`.
    pub fn reboot(&self) {
        self.clock.set_boot_id("boot-2");
    }

    /// The step record of `service`, `None` when there is none.
    pub fn record(&self, service: &str) -> Option<Record> {
        match RecordStore::new(&self.state.join(service)).load() {
            Ok(Loaded::Current(rec)) => Some(*rec),
            _ => None,
        }
    }

    /// How the last update of `service` ended, `None` while it is unfinished or there is none.
    pub fn verdict(&self, service: &str) -> Option<Finished> {
        match self.record(service)?.state {
            State::Finished { verdict, reason } => Some(Finished { verdict, reason }),
            _ => None,
        }
    }
}

/// `daemon`, its alerts written to the returned log, one line each.
pub fn alerting(daemon: Daemon) -> (Daemon, Log) {
    let log = Log::default();
    let sink = log.clone();
    let daemon = daemon.with_alerts(Arc::new(move |line: &str| {
        sink.lock().expect("the log").push(line.to_owned());
    }));
    (daemon, log)
}

/// The lines of `log`.
pub fn lines(log: &Log) -> Vec<String> {
    log.lock().expect("the log").clone()
}

pub fn name(text: &str) -> ServiceName {
    ServiceName::new(text).expect("a service name")
}

/// The last line of a step that committed the update.
pub const IRREVERSIBLE: &str = r#"{"reversible":false}"#;
/// The last line of a `verify` whose promotion is pending.
pub const PENDING_LINE: &str = r#"{"promotion_pending":true}"#;

pub fn finished(verdict: Verdict, reason: Option<&str>) -> Finished {
    Finished {
        verdict,
        reason: reason.map(str::to_owned),
    }
}

pub fn ok() -> Finished {
    finished(
        Verdict::Succeeded {
            promotion_pending: false,
        },
        None,
    )
}

/// `manifest` of `service` at `version` with these lines added to its `[hook]` table.
pub fn manifest_with(service: &str, version: &str, hook_lines: &str) -> String {
    manifest(service, version).replace("[hook]\n", &format!("[hook]\n{hook_lines}\n"))
}

/// A seam that stops the daemon at `at`, the writes of step records counted across every store
/// it makes; `count` counts them. Once stopped, the daemon is dead: every later write fails too.
pub fn crash_at(at: Option<CrashPoint>, count: Arc<AtomicU64>) -> StoreSeam {
    let dead = Arc::new(std::sync::atomic::AtomicBool::new(false));
    Arc::new(move |_: &ServiceName, store: RecordStore| {
        let (count, dead) = (Arc::clone(&count), Arc::clone(&dead));
        store.with_crash_gate(Arc::new(move |point| {
            let here = match point {
                CrashPoint::BeforeWrite(_) => {
                    let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                    at == Some(CrashPoint::BeforeWrite(n))
                }
                CrashPoint::AfterWrite(_) => {
                    at == Some(CrashPoint::AfterWrite(count.load(Ordering::SeqCst)))
                }
            };
            dead.fetch_or(here, Ordering::SeqCst) | here
        }))
    })
}

/// A seam that stops the daemon after the first write of `record` whose text holds `needle`.
pub fn crash_after_writing(record: PathBuf, needle: &'static str) -> StoreSeam {
    Arc::new(move |_: &ServiceName, store: RecordStore| {
        let record = record.clone();
        store.with_crash_gate(Arc::new(move |point| {
            matches!(point, CrashPoint::AfterWrite(_))
                && fs::read_to_string(&record).is_ok_and(|text| text.contains(needle))
        }))
    })
}

/// Every entry under `dir`, with its kind and its bytes for a file: what a test compares to
/// show that nothing changed.
pub fn snapshot(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let mut entries: Vec<_> = fs::read_dir(at)
            .expect("list")
            .map(|e| e.expect("entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let meta = fs::symlink_metadata(&path).expect("metadata");
            let kind = format!("{:?}", meta.file_type());
            let body = fs::read(&path).map(|b| String::from_utf8_lossy(&b).into_owned());
            out.push((path.clone(), format!("{kind} {}", body.unwrap_or_default())));
            if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A client of the admin service over `io`, the other end of an in-process pipe.
pub fn client(io: DuplexStream) -> AdminClient<Channel> {
    let slot = Arc::new(Mutex::new(Some(io)));
    let connector = tower::service_fn(move |_: Uri| {
        let io = slot.lock().expect("the slot").take();
        async move {
            io.map(TokioIo::new)
                .ok_or_else(|| std::io::Error::other("the pipe was used"))
        }
    });
    let channel =
        Endpoint::from_static("http://admin.invalid").connect_with_connector_lazy(connector);
    AdminClient::new(channel)
}
