//! Test doubles of the daemon's ports, for this crate's tests and the tests of the crates that
//! build on it (behind the feature `testing`): a clock that moves only when told to, a launcher
//! that plays a script and can hold a step until released, a package store that builds version
//! directories in a test's directory, a readiness notifier that counts its notices, and a
//! connection that counts the bytes read from it. The launcher and the notifier can write what
//! they are asked to one shared [`Log`], so that a test sees the order across both.
//! Nothing here sleeps or reads the wall clock: a test drives every wait with a channel.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Step;
use hostkeeper_steps::{
    AdditiveCall, Clock, LaunchError, Seal, Staged, StepCall, StepExit, StepLauncher, tree_digest,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::transport::server::Connected;

use crate::notify::ReadyNotifier;
use crate::packages::{PackageError, Packages, VersionDir};
use crate::verb::PackageDigest;

/// One log of what the fakes were asked, in order: `run <unit>`, `stop <unit>`,
/// `additive <unit>` from [`ScriptedLauncher`], `ready` from [`FakeNotifier`].
pub type Log = Arc<Mutex<Vec<String>>>;

fn note(log: &Mutex<Option<Log>>, line: String) {
    if let Some(log) = &*lock(log) {
        lock(log).push(line);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A clock that moves only when a test, or something sleeping on it, moves it.
#[derive(Debug)]
pub struct FakeClock {
    mono: AtomicU64,
    boot: Mutex<String>,
    /// A sleep does not move the clock: only [`FakeClock::advance`] does.
    frozen: bool,
    /// The next sleep waits here until the test releases it.
    parked: Mutex<Option<Hold>>,
}

impl FakeClock {
    /// At second 1000 of the boot `boot-1`; a sleep moves it by its length at once.
    pub fn new() -> Arc<FakeClock> {
        FakeClock::starting(false)
    }

    /// At second 1000 of the boot `boot-1`; a sleep only yields, and the clock moves only when
    /// the test advances it: for a test with dead-men, which poll by sleeping.
    pub fn frozen() -> Arc<FakeClock> {
        FakeClock::starting(true)
    }

    fn starting(frozen: bool) -> Arc<FakeClock> {
        Arc::new(FakeClock {
            mono: AtomicU64::new(1000),
            boot: Mutex::new("boot-1".to_owned()),
            frozen,
            parked: Mutex::default(),
        })
    }

    /// Holds the next sleep on this clock until the test releases it: the test sees a waiter
    /// enter its wait.
    pub fn park_next_sleep(&self) -> Gate {
        let (entered, on_entered) = mpsc::channel();
        let (release, on_release) = mpsc::channel();
        *lock(&self.parked) = Some(Hold {
            entered,
            release: Mutex::new(on_release),
        });
        Gate {
            entered: on_entered,
            release,
        }
    }

    pub fn advance(&self, secs: u64) {
        self.mono.fetch_add(secs, Ordering::SeqCst);
    }

    /// The host booted again: a new boot id.
    pub fn set_boot_id(&self, id: &str) {
        *lock(&self.boot) = id.to_owned();
    }
}

impl Clock for FakeClock {
    fn mono_secs(&self) -> u64 {
        self.mono.load(Ordering::SeqCst)
    }

    fn boot_id(&self) -> String {
        lock(&self.boot).clone()
    }

    /// Moves the clock by `secs` at once, or, frozen, only yields.
    fn sleep_secs(&self, secs: u64) {
        let parked = lock(&self.parked).take();
        if let Some(hold) = parked {
            hold.entered.send(()).unwrap_or_default();
            lock(&hold.release).recv().unwrap_or_default();
        }
        match self.frozen {
            true => std::thread::yield_now(),
            false => self.advance(secs),
        }
    }
}

/// One run the launcher was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran {
    pub service: String,
    pub step: Step,
    pub unit: String,
    pub executable: PathBuf,
    /// The staged release, given to `prepare` only.
    pub artifact: Option<PathBuf>,
}

/// A step held inside the launcher, or a sleep held on the clock, until the test releases it.
#[derive(Debug)]
struct Hold {
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
}

/// The test's end of a held step: wait for the step to be entered, then release it.
#[derive(Debug)]
pub struct Gate {
    entered: Receiver<()>,
    release: Sender<()>,
}

impl Gate {
    /// Waits until the held step is running.
    pub fn entered(&self) {
        self.entered.recv().unwrap_or_default();
    }

    /// Lets the held step end.
    pub fn release(&self) {
        self.release.send(()).unwrap_or_default();
    }
}

type Key = (String, Step);

/// One run of an additive step the launcher was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdditiveRan {
    pub service: String,
    pub step: String,
    pub unit: String,
    pub executable: PathBuf,
    /// The arguments given before the step.
    pub hook_args: Vec<String>,
    /// The params file's text, as the step would have read it.
    pub params: String,
    pub timeout_secs: u64,
}

/// How the next run of an additive step of a service goes wrong in [`ScriptedLauncher`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdditiveFault {
    /// The hook does not start ([`LaunchError::NoStart`]).
    NoStart,
    /// The unit started, and the launcher lost it ([`LaunchError::Launcher`]); it can be stopped.
    Lost,
    /// As `Lost`, and from then on no unit of the service can be stopped.
    LostUnstoppable,
}

type AdditiveEffect = Box<dyn Fn(&AdditiveCall<'_>) + Send + Sync>;

/// A launcher that plays a script: per service and step, the exit codes in order (with a last
/// line, when one is given), then `0`.
#[derive(Default)]
pub struct ScriptedLauncher {
    exits: Mutex<BTreeMap<Key, VecDeque<i32>>>,
    lines: Mutex<BTreeMap<Key, VecDeque<String>>>,
    additive_exits: Mutex<BTreeMap<String, VecDeque<i32>>>,
    additive_faults: Mutex<BTreeMap<String, AdditiveFault>>,
    during_additive: Mutex<Option<AdditiveEffect>>,
    additive: Mutex<Vec<AdditiveRan>>,
    log: Mutex<Option<Log>>,
    holds: Mutex<BTreeMap<Key, Hold>>,
    runs: Mutex<Vec<Ran>>,
    stops: Mutex<Vec<String>>,
    failing_stops: Mutex<BTreeSet<String>>,
    losses: Mutex<BTreeSet<Key>>,
    stop_seen: Mutex<Vec<Sender<String>>>,
    running: AtomicUsize,
    most_running: AtomicUsize,
}

impl ScriptedLauncher {
    pub fn new() -> Arc<ScriptedLauncher> {
        Arc::new(ScriptedLauncher::default())
    }

    /// The exit codes of the next runs of `step` for `service`.
    pub fn exits(&self, service: &str, step: Step, codes: &[i32]) {
        lock(&self.exits).insert((service.to_owned(), step), codes.iter().copied().collect());
    }

    /// The exit codes and last lines of the next runs of `step` for `service`.
    pub fn replies(&self, service: &str, step: Step, replies: &[(i32, &str)]) {
        let key = (service.to_owned(), step);
        let codes = replies.iter().map(|(code, _)| *code).collect();
        let lines = replies.iter().map(|(_, line)| (*line).to_owned()).collect();
        lock(&self.exits).insert(key.clone(), codes);
        lock(&self.lines).insert(key, lines);
    }

    /// The exit codes of the next runs of an additive step for `service`.
    pub fn additive_exits(&self, service: &str, codes: &[i32]) {
        lock(&self.additive_exits).insert(service.to_owned(), codes.iter().copied().collect());
    }

    /// The next run of an additive step of `service` goes wrong as `fault` says.
    pub fn additive_fault(&self, service: &str, fault: AdditiveFault) {
        lock(&self.additive_faults).insert(service.to_owned(), fault);
    }

    /// Runs `effect` inside every run of an additive step, as the step would act on the world.
    pub fn during_additive(&self, effect: impl Fn(&AdditiveCall<'_>) + Send + Sync + 'static) {
        *lock(&self.during_additive) = Some(Box::new(effect));
    }

    /// Every run of an additive step, in order.
    pub fn additive_runs(&self) -> Vec<AdditiveRan> {
        lock(&self.additive).clone()
    }

    /// Writes every run, stop and additive run asked for from now on to `log`.
    pub fn log_to(&self, log: Log) {
        *lock(&self.log) = Some(log);
    }

    /// Holds the next run of `step` for `service` until the test releases it.
    pub fn hold(&self, service: &str, step: Step) -> Gate {
        let (entered, on_entered) = mpsc::channel();
        let (release, on_release) = mpsc::channel();
        let hold = Hold {
            entered,
            release: Mutex::new(on_release),
        };
        lock(&self.holds).insert((service.to_owned(), step), hold);
        Gate {
            entered: on_entered,
            release,
        }
    }

    /// The next run of `step` for `service` fails in the launcher itself
    /// ([`LaunchError::Launcher`]), as when the launcher loses a unit it started.
    pub fn lose(&self, service: &str, step: Step) {
        lock(&self.losses).insert((service.to_owned(), step));
    }

    /// Makes every stop of a unit of `service` fail.
    pub fn fail_stops(&self, service: &str) {
        lock(&self.failing_stops).insert(service.to_owned());
    }

    /// Lets the stops of the units of `service` work again.
    pub fn allow_stops(&self, service: &str) {
        lock(&self.failing_stops).remove(service);
    }

    /// A channel that gets the unit of every stop asked for from now on.
    pub fn stops_seen(&self) -> Receiver<String> {
        let (tx, rx) = mpsc::channel();
        lock(&self.stop_seen).push(tx);
        rx
    }

    /// Every run, in order.
    pub fn runs(&self) -> Vec<Ran> {
        lock(&self.runs).clone()
    }

    /// The steps run for `service`, in order.
    pub fn steps(&self, service: &str) -> Vec<Step> {
        let runs = lock(&self.runs);
        runs.iter()
            .filter(|ran| ran.service == service)
            .map(|ran| ran.step)
            .collect()
    }

    /// Every unit a stop was asked for, in order.
    pub fn stops(&self) -> Vec<String> {
        lock(&self.stops).clone()
    }

    /// The most runs that were under way at one time.
    pub fn most_running(&self) -> usize {
        self.most_running.load(Ordering::SeqCst)
    }
}

impl StepLauncher for ScriptedLauncher {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        let key = (call.service.to_owned(), call.invocation.step);
        note(&self.log, format!("run {}", call.unit));
        lock(&self.runs).push(Ran {
            service: call.service.to_owned(),
            step: call.invocation.step,
            unit: call.unit.to_owned(),
            executable: call.executable.to_path_buf(),
            artifact: call.invocation.artifact.clone(),
        });
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_running.fetch_max(now, Ordering::SeqCst);
        let hold = lock(&self.holds).remove(&key);
        if let Some(hold) = hold {
            hold.entered.send(()).unwrap_or_default();
            lock(&hold.release).recv().unwrap_or_default();
        }
        let lost = lock(&self.losses).remove(&key);
        let code = lock(&self.exits)
            .get_mut(&key)
            .and_then(VecDeque::pop_front)
            .unwrap_or(0);
        let last_line = lock(&self.lines)
            .get_mut(&key)
            .and_then(VecDeque::pop_front);
        self.running.fetch_sub(1, Ordering::SeqCst);
        match lost {
            true => Err(LaunchError::Launcher(format!("{} was lost", call.unit))),
            false => Ok(StepExit {
                code: Some(code),
                last_line,
            }),
        }
    }

    fn run_additive(&self, call: &AdditiveCall<'_>) -> Result<StepExit, LaunchError> {
        note(&self.log, format!("additive {}", call.unit));
        lock(&self.additive).push(AdditiveRan {
            service: call.service.to_owned(),
            step: call.step.to_owned(),
            unit: call.unit.to_owned(),
            executable: call.executable.to_path_buf(),
            hook_args: call.hook_args.to_vec(),
            params: fs::read_to_string(call.params).unwrap_or_default(),
            timeout_secs: call.timeout_secs,
        });
        if let Some(effect) = &*lock(&self.during_additive) {
            effect(call);
        }
        let code = lock(&self.additive_exits)
            .get_mut(call.service)
            .and_then(VecDeque::pop_front)
            .unwrap_or(0);
        let fault = lock(&self.additive_faults).remove(call.service);
        let lost = || LaunchError::Launcher(format!("{} was lost", call.unit));
        match fault {
            None => Ok(StepExit {
                code: Some(code),
                last_line: None,
            }),
            Some(AdditiveFault::NoStart) => Err(LaunchError::NoStart("no such file".to_owned())),
            Some(AdditiveFault::Lost) => Err(lost()),
            Some(AdditiveFault::LostUnstoppable) => {
                self.fail_stops(call.service);
                Err(lost())
            }
        }
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        note(&self.log, format!("stop {unit}"));
        lock(&self.stops).push(unit.to_owned());
        for seen in lock(&self.stop_seen).iter() {
            seen.send(unit.to_owned()).unwrap_or_default();
        }
        let failing = lock(&self.failing_stops)
            .iter()
            .any(|service| unit.starts_with(&format!("updater-{service}-")));
        match failing {
            true => Err(LaunchError::Launcher(format!(
                "{unit} could not be stopped"
            ))),
            false => Ok(()),
        }
    }
}

/// A manifest for `service` at `version` that every load rule accepts, its hook `bin/hook`.
pub fn manifest(service: &str, version: &str) -> String {
    format!(
        r#"schema = 1
name = "{service}"
version = "{version}"
min_hostkeeper = "0.1.0"

[hook]
command = ["bin/hook", "update-hook"]
verify_deadline_secs = 1600

[hook.timeout_secs]
prepare = 600
cutover = 300
verify = 1000
rollback = 600
cleanup = 1300
"#
    )
}

/// A package store that serves version directories a test made under the packages directory:
/// `<packages-dir>/<service>/<version>/`, with its `manifest.toml` and a hook at `bin/hook`. It
/// does not compare digests; a test makes a package fail with [`FakePackages::fail`].
pub struct FakePackages {
    root: PathBuf,
    failures: Mutex<BTreeMap<(String, String), PackageError>>,
    asked: Mutex<Vec<(String, String)>>,
    notify: Mutex<Vec<Sender<String>>>,
    holds: Mutex<BTreeMap<(String, String), Hold>>,
}

fn dir_0755(dir: &Path) {
    fs::create_dir_all(dir).unwrap_or_default();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap_or_default();
}

impl FakePackages {
    /// Serves version directories under `packages_dir`.
    pub fn new(packages_dir: &Path) -> Arc<FakePackages> {
        Arc::new(FakePackages {
            root: packages_dir.to_path_buf(),
            failures: Mutex::default(),
            asked: Mutex::default(),
            notify: Mutex::default(),
            holds: Mutex::default(),
        })
    }

    /// Makes the version directory of `service` at `version` with `manifest` as its manifest.
    pub fn add(&self, service: &str, version: &str, manifest: &str) -> PathBuf {
        let dir = self.root.join(service).join(version);
        for made in [&self.root, &self.root.join(service), &dir, &dir.join("bin")] {
            dir_0755(made);
        }
        let files = [
            (dir.join("manifest.toml"), manifest, 0o644),
            (dir.join("bin/hook"), "#!hook\n", 0o755),
        ];
        for (path, text, mode) in files {
            fs::write(&path, text).unwrap_or_default();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap_or_default();
        }
        dir
    }

    /// Writes the seal of `service` at `version` (`<version>.seal` beside it): the tree digest
    /// of its directory as it is now, and `package_digest`.
    pub fn seal(&self, service: &str, version: &str, package_digest: &str) {
        let dir = self.root.join(service);
        let tree = tree_digest(&dir.join(version)).unwrap_or_default();
        let seal = Seal::new(&tree, package_digest).map(|seal| seal.to_json());
        fs::write(
            dir.join(format!("{version}.seal")),
            seal.unwrap_or_default(),
        )
        .unwrap_or_default();
    }

    /// Points the link `link` (`current`, `previous`) of `service` at `version`.
    pub fn link(&self, service: &str, link: &str, version: &str) {
        let path = self.root.join(service).join(link);
        fs::remove_file(&path).unwrap_or_default();
        std::os::unix::fs::symlink(version, &path).unwrap_or_default();
    }

    /// Holds the next ask for the package of `service` at `version` until the test releases it.
    pub fn hold(&self, service: &str, version: &str) -> Gate {
        let (entered, on_entered) = mpsc::channel();
        let (release, on_release) = mpsc::channel();
        let hold = Hold {
            entered,
            release: Mutex::new(on_release),
        };
        lock(&self.holds).insert((service.to_owned(), version.to_owned()), hold);
        Gate {
            entered: on_entered,
            release,
        }
    }

    /// Makes the package of `service` at `version` fail with `error`.
    pub fn fail(&self, service: &str, version: &str, error: PackageError) {
        lock(&self.failures).insert((service.to_owned(), version.to_owned()), error);
    }

    /// Every package asked for, as (service, version), in order.
    pub fn asked(&self) -> Vec<(String, String)> {
        lock(&self.asked).clone()
    }

    /// Sends `<service> <version>` of every package asked for from now on to `tx`.
    pub fn notify(&self, tx: Sender<String>) {
        lock(&self.notify).push(tx);
    }
}

impl Packages for FakePackages {
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &Version,
        _digest: &PackageDigest,
    ) -> Result<VersionDir, PackageError> {
        let key = (service.to_string(), version.to_string());
        lock(&self.asked).push(key.clone());
        for tx in lock(&self.notify).iter() {
            tx.send(format!("{service} {version}")).unwrap_or_default();
        }
        let held = lock(&self.holds).remove(&key);
        if let Some(hold) = held {
            hold.entered.send(()).unwrap_or_default();
            lock(&hold.release).recv().unwrap_or_default();
        }
        let failure = lock(&self.failures).get(&key).cloned();
        let dir = self.root.join(&key.0).join(&key.1);
        let seal = || {
            Staged::seal(&dir)
                .map(VersionDir::new)
                .map_err(|e| PackageError::Other(e.to_string()))
        };
        failure.map_or_else(seal, Err)
    }
}

/// A readiness notifier that counts its notices, writes `ready` to its log, and fails when told.
#[derive(Default)]
pub struct FakeNotifier {
    sent: AtomicUsize,
    failing: bool,
    log: Mutex<Option<Log>>,
}

impl FakeNotifier {
    /// A notifier whose notices go through, writing `ready` to `log`.
    pub fn new(log: Log) -> Arc<FakeNotifier> {
        Arc::new(FakeNotifier {
            log: Mutex::new(Some(log)),
            ..FakeNotifier::default()
        })
    }

    /// A notifier whose every notice fails, counted all the same.
    pub fn failing() -> Arc<FakeNotifier> {
        Arc::new(FakeNotifier {
            failing: true,
            ..FakeNotifier::default()
        })
    }

    /// How many notices were asked for.
    pub fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }
}

impl ReadyNotifier for FakeNotifier {
    fn ready(&self) -> io::Result<()> {
        self.sent.fetch_add(1, Ordering::SeqCst);
        note(&self.log, "ready".to_owned());
        match self.failing {
            true => Err(io::Error::other("the service manager is gone")),
            false => Ok(()),
        }
    }
}

/// A connection that counts the bytes read from it: a test of a server shows with it that
/// nothing of a refused connection was read.
#[derive(Debug)]
pub struct Counted<IO> {
    io: IO,
    read: Arc<AtomicUsize>,
}

impl<IO> Counted<IO> {
    /// `io`, and the count of the bytes read from it.
    pub fn new(io: IO) -> (Counted<IO>, Arc<AtomicUsize>) {
        let read = Arc::new(AtomicUsize::new(0));
        let counted = Counted {
            io,
            read: Arc::clone(&read),
        };
        (counted, read)
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for Counted<IO> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.io).poll_read(cx, buf);
        let read = buf.filled().len() - before;
        self.read.fetch_add(read, Ordering::SeqCst);
        polled
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for Counted<IO> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

impl<IO> Connected for Counted<IO> {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}
