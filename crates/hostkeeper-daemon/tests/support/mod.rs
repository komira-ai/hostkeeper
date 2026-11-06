//! Shared by the tests: a scratch directory with a state directory, a packages directory and a
//! daemon config, the fakes of the daemon's ports, orders, and a gRPC client over an in-process
//! pipe.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

pub mod force;
pub mod selfupdate;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::boot::BootReport;
use hostkeeper_daemon::cli::DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS;
use hostkeeper_daemon::links::{Links, MakeEnroller};
use hostkeeper_daemon::prune::NoLockPoint;
use hostkeeper_daemon::start::Settings;
use hostkeeper_daemon::testing::{
    FakeClock, FakePackages, Log, NoPrune, ScriptedLauncher, manifest,
};
use hostkeeper_daemon::{Daemon, Dirs, Placement, Ports, StoreSeam};
use hostkeeper_enroll::{Enroller, RenewalState, Settings as EnrollSettings};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_link::testing::{
    FakeCerts, FakeLink, ManualDispatch, RecordingLog, ScriptedSleeper,
};
use hostkeeper_link::{CertState, DEFAULT_HEARTBEAT_SECS, StopFlag};
use hostkeeper_observe::fake::{
    FakeExpectedExe, FakeHealthClient, FakeProcessProbe, FakeServiceManager,
};
use hostkeeper_observe::{ObserveOptions, Observer};
use hostkeeper_placed::Roots;
use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, Log as PlacedLog};
use hostkeeper_proto::hostkeeper::agent::v1::{
    Force, ForceRollback, Install, Order, Uninstall, Update, order,
};
use hostkeeper_proto::hostkeeper::local::v1::admin_client::AdminClient;
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::{
    AdditiveCall, CrashPoint, Finished, LaunchError, RecordStore, StepCall, StepExit, StepLauncher,
};
use hyper_util::rt::TokioIo;
use tokio::io::DuplexStream;
use tonic::transport::{Channel, Endpoint, Uri};

/// Links whose loops never run: both are left unstarted on a dispatcher nobody runs, for a test
/// that serves the daemon and is not about its loops.
pub fn idle_links() -> Links {
    Links {
        enroller: Box::new(|| Err("idle".to_owned())),
        link: Arc::new(FakeLink::default()),
        certs: Arc::new(FakeCerts::new(CertState::NotEnrolled {
            waiting_for: PathBuf::from("idle"),
        })),
        clock: Arc::new(hostkeeper_link::testing::FakeClock::new(0, 0)),
        log: Arc::new(RecordingLog::default()),
        sleeper: Box::new(ScriptedSleeper::new(0)),
        enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
        renewal: RenewalState::default(),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: Arc::new(ManualDispatch::default()),
        stop: StopFlag::default(),
    }
}

/// An enroller opened from `settings` and `ports` at the first try; a later try is refused,
/// since the ports went to the first.
pub fn open_once(settings: EnrollSettings, ports: hostkeeper_enroll::Ports) -> MakeEnroller {
    let mut parts = Some((settings, ports));
    Box::new(move || {
        let (settings, ports) = parts
            .take()
            .ok_or_else(|| "the enroller was opened already".to_owned())?;
        Enroller::open(settings, ports).map_err(|e| e.to_string())
    })
}

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
            index_digest: String::new(),
        })),
    }
}

/// An `install` order.
pub fn install(service: &str, version: &str, package_digest: &str, order_try: u64) -> Order {
    Order {
        order_id: format!("install-{service}-{version}"),
        r#try: order_try,
        verb: Some(order::Verb::Install(Install {
            service: service.to_owned(),
            version: version.to_owned(),
            package_digest: package_digest.to_owned(),
            index_digest: String::new(),
        })),
    }
}

/// An `uninstall` order.
pub fn uninstall(service: &str, order_try: u64) -> Order {
    Order {
        order_id: format!("uninstall-{service}"),
        r#try: order_try,
        verb: Some(order::Verb::Uninstall(Uninstall {
            service: service.to_owned(),
        })),
    }
}

/// The release a service that a test updates is installed at, when the test installed none:
/// an update needs a release installed.
pub const BASE: &str = "0.0.0";

/// A `force_rollback` order.
pub fn force_rollback(service: &str, version: &str, package_digest: &str, order_try: u64) -> Order {
    Order {
        order_id: format!("rollback-{service}-{version}"),
        r#try: order_try,
        verb: Some(order::Verb::ForceRollback(ForceRollback {
            service: service.to_owned(),
            version: version.to_owned(),
            package_digest: package_digest.to_owned(),
            index_digest: String::new(),
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
            index_digest: String::new(),
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
///
/// Placement reads and writes the real paths under the scratch directory (a `FaultFs` rooted at
/// `/`, given only paths below the scratch directory), units in `<scratch>/units`, and
/// `placed.json` in the scratch directory, above the packages directory. Its writes and the
/// calls of its unit control go to `placed_log`.
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
    /// The observer of the installed services, reading the four fakes below.
    pub observer: Arc<Observer>,
    /// What the service manager says of each unit.
    pub manager: FakeServiceManager,
    /// The processes the units run.
    pub process: FakeProcessProbe,
    /// What each health URL answers.
    pub health: FakeHealthClient,
    /// The digest each sealed version's executable has.
    pub expected: FakeExpectedExe,
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
        let (manager, process, health, expected) = (
            FakeServiceManager::default(),
            FakeProcessProbe::default(),
            FakeHealthClient::default(),
            FakeExpectedExe::default(),
        );
        let observer = Observer::new(
            hostkeeper_observe::Ports {
                manager: Box::new(manager.clone()),
                process: Box::new(process.clone()),
                health: Box::new(health.clone()),
                expected: Box::new(expected.clone()),
            },
            ObserveOptions::default(),
        );
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
            observer: Arc::new(observer),
            manager,
            process,
            health,
            expected,
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
            force_lock_point: Arc::new(NoLockPoint),
            observer: Arc::clone(&self.observer),
        }
    }

    /// A daemon on the rig's directories whose placement goes through `fs` and `units`, with no
    /// dead-men.
    pub fn daemon_with(&self, fs: Arc<FaultFs>, units: Arc<FakeUnitControl>) -> Daemon {
        let ports = Ports {
            units,
            placement: Placement { roots: roots(), fs },
            ..self.ports()
        };
        Daemon::new(config(), self.dirs(), ports, OWN)
    }

    /// Where the host path `path` is on disk: the scratch directory is the rig's host's root.
    pub fn host(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        self.scratch
            .path()
            .join(path.strip_prefix("/").unwrap_or(path))
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
            trusted_root_max_age_days: DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS,
        }
    }

    /// The package of `service` at `version`, with a manifest every rule accepts. A service
    /// with no `current` is installed at [`BASE`] (its package there, `current` naming it), so
    /// that an update of it runs; [`Rig::stage`] adds a package and installs nothing.
    pub fn add(&self, service: &str, version: &str) -> PathBuf {
        let dir = self.stage(service, version);
        if fs::symlink_metadata(self.packages.join(service).join("current")).is_err() {
            self.stage(service, BASE);
            self.store.link(service, "current", BASE);
        }
        dir
    }

    /// The package of `service` at `version`, with a manifest every rule accepts, and nothing
    /// else.
    pub fn stage(&self, service: &str, version: &str) -> PathBuf {
        self.store
            .add(service, version, &manifest(service, version))
    }

    /// `service` runs `version`: its package is there and `current` names it.
    pub fn running(&self, service: &str, version: &str) -> PathBuf {
        let dir = self.stage(service, version);
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

impl Rig {
    /// Where the `[[files]]` entry of a package of `service` that [`Rig::placed`] makes is
    /// placed, as the host names it.
    pub fn conf_path(&self, service: &str) -> PathBuf {
        PathBuf::from(format!("/opt/{service}/{service}.conf"))
    }

    /// The text of the unit `<service>.service`: its `ExecStart=` through `current`, wanted by
    /// [`TARGET`].
    pub fn unit_text(&self, service: &str) -> String {
        format!(
            "[Unit]\nDescription=a test unit\n\n[Service]\nExecStart={}/{service}/current/bin/hook serve\n\n[Install]\nWantedBy={TARGET}\n",
            roots().packages().dir().display()
        )
    }

    /// The package of `service` at `version` that places something: `units/<service>.service`,
    /// its one `run` unit, and `files/<service>.conf` at [`Rig::conf_path`]; with
    /// `uninstall = <bound>` in `[hook.timeout_secs]` when `uninstall` is given. Sealed with the
    /// package digest [`d`]. Nothing is installed.
    pub fn placed(&self, service: &str, version: &str, uninstall: Option<u64>) -> PathBuf {
        let mut text = manifest(service, version);
        if let Some(bound) = uninstall {
            text.push_str(&format!("uninstall = {bound}\n"));
        }
        text.push_str(&format!(
            "\n[units]\nrun = [\"{service}.service\"]\n\n[[files]]\nsource = \"files/{service}.conf\"\npath = \"{}\"\n",
            self.conf_path(service).display()
        ));
        let dir = self.store.add(service, version, &text);
        for sub in ["units", "files"] {
            fs::create_dir_all(dir.join(sub)).expect("a directory of the package");
            fs::set_permissions(dir.join(sub), fs::Permissions::from_mode(0o755))
                .expect("its mode");
        }
        let unit = dir.join("units").join(format!("{service}.service"));
        fs::write(&unit, self.unit_text(service)).expect("the unit");
        let conf = dir.join("files").join(format!("{service}.conf"));
        fs::write(&conf, format!("{service} {version}\n")).expect("the file");
        for file in [&unit, &conf] {
            fs::set_permissions(file, fs::Permissions::from_mode(0o644)).expect("its mode");
        }
        self.store.seal(service, version, &d());
        dir
    }

    /// The paths a package of `service` from [`Rig::placed`] places, as the host names them: its
    /// unit, its `WantedBy=` link and its file.
    pub fn placed_paths(&self, service: &str) -> [PathBuf; 3] {
        let unit = format!("{service}.service");
        let units = roots().unit_dir().to_path_buf();
        [
            units.join(&unit),
            units.join(format!("{TARGET}.wants")).join(&unit),
            self.conf_path(service),
        ]
    }

    /// Whether anything is at the host path `path`, a link included.
    pub fn there(&self, path: impl AsRef<Path>) -> bool {
        fs::symlink_metadata(self.host(path)).is_ok()
    }

    /// `placed.json` as it is on disk.
    pub fn placed_record(&self) -> hostkeeper_placed::PlacedRecord {
        hostkeeper_placed::Store::new(&*self.placed_fs, roots().state_dir())
            .load()
            .expect("placed.json loads")
    }

    /// Writes `record` as `placed.json`, under its lock.
    pub fn save_placed(&self, record: &hostkeeper_placed::PlacedRecord) {
        let daemon = self.daemon();
        let placer = daemon.ctx().placer();
        let lock = placer.lock().expect("the lock of placed.json");
        placer
            .store()
            .save(record, &lock)
            .expect("placed.json saves");
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

/// Serves `daemon`'s admin service on one in-process connection from `peer`, as the admin socket
/// does: the client's end, and the count of the bytes the server read. Needs a tokio runtime.
pub fn admin_from(
    daemon: Arc<Daemon>,
    peer: hostkeeper_daemon::admin::Peer,
) -> (AdminClient<Channel>, Arc<std::sync::atomic::AtomicUsize>) {
    use tokio_stream::StreamExt;
    let (near, far) = tokio::io::duplex(64 * 1024);
    let (counted, read) = hostkeeper_daemon::testing::Counted::new(far);
    let incoming = tokio_stream::iter([Ok((counted, peer))]).chain(tokio_stream::pending());
    tokio::spawn(hostkeeper_daemon::admin::serve(daemon, incoming));
    (client(near), read)
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

/// A step launcher that puts a mark in the placed log for each step it runs (`run cutover`, and
/// `additive restore` for a boot step), so that the writes and the calls of the service manager
/// read in their place among the steps of the hook, and then runs `inner`. `before` is called
/// with the step first.
pub struct Tap {
    inner: Arc<ScriptedLauncher>,
    log: PlacedLog,
    before: Box<dyn Fn(Step) + Send + Sync>,
}

impl Tap {
    pub fn new(inner: Arc<ScriptedLauncher>, log: PlacedLog) -> Arc<Tap> {
        Tap::before(inner, log, |_| {})
    }

    pub fn before(
        inner: Arc<ScriptedLauncher>,
        log: PlacedLog,
        before: impl Fn(Step) + Send + Sync + 'static,
    ) -> Arc<Tap> {
        Arc::new(Tap {
            inner,
            log,
            before: Box::new(before),
        })
    }
}

impl StepLauncher for Tap {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        let step = call.invocation.step;
        self.log.push(Event::Mark(format!("run {step}")));
        (self.before)(step);
        self.inner.run(call)
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        self.inner.stop(unit)
    }

    fn run_additive(&self, call: &AdditiveCall<'_>) -> Result<StepExit, LaunchError> {
        self.log
            .push(Event::Mark(format!("additive {}", call.step)));
        self.inner.run_additive(call)
    }
}
