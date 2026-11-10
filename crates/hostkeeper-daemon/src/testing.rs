//! Test doubles of the daemon's ports, for this crate's tests and the tests of the crates that
//! build on it (behind the feature `testing`): a clock that moves only when told to, a launcher
//! that plays a script and can hold a step until released, a package store that builds version
//! directories in a test's directory, a readiness notifier that counts its notices, a
//! connection that counts the bytes read from it, and prunes that delete nothing or record
//! their calls. The launcher and the notifier can write what
//! they are asked to one shared [`Log`], so that a test sees the order across both.
//! Nothing here sleeps or reads the wall clock: a test drives every wait with a channel, and each
//! wait names the [`Party`] that would send what it waits for, so that a signal that never comes
//! fails the test rather than hanging it ([`signal`]).

mod signal;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Step;
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_steps::{
    AdditiveCall, BootId, BootIdError, Clock, LaunchError, Layout, Protected, PruneReport,
    PruneStep, Seal, Staged, StepCall, StepExit, StepLauncher, ToolCall, ToolExit, tree_digest,
};

use crate::locks::{ServiceGuard, ServiceLocks};
use crate::notify::ReadyNotifier;
use crate::packages::{PackageError, Packages, Pinned, VersionDir};
use crate::prune::Prune;
use crate::verb::PackageDigest;
use crate::watchers::Watchers;

pub use signal::{Ending, Missed, Notice, Party, Signal, Spawned, Tell, spawn};

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
pub struct FakeClock {
    mono: AtomicU64,
    boot: Mutex<String>,
    /// A sleep does not move the clock: only [`FakeClock::advance`] does.
    frozen: bool,
    /// The next sleep waits here until the test releases it.
    parked: Mutex<Option<Hold>>,
    /// The sleeps so far, and the parties of [`FakeClock::sleeps`] over at a count.
    slept: Mutex<Slept>,
}

/// How many times a [`FakeClock`] was slept on, and the parties over at a count.
#[derive(Default)]
struct Slept {
    count: u64,
    due: Vec<(u64, Ending)>,
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
            slept: Mutex::default(),
        })
    }

    /// A party over once the clock has been slept on `times` times more than now. A poller that
    /// ticks, then sleeps, has run a whole tick after this call once it has slept twice: what that
    /// tick would send comes before. Every sleep on the clock counts, so it is for a clock one
    /// poller sleeps on.
    pub fn sleeps(&self, times: u64) -> Ending {
        let ending = Ending::default();
        let mut slept = lock(&self.slept);
        let at = slept.count + times;
        slept.due.push((at, ending.clone()));
        ending
    }

    /// Holds the next sleep on this clock until the test releases it: the test sees a waiter
    /// enter its wait.
    pub fn park_next_sleep(&self) -> Gate {
        let (hold, gate) = Hold::new();
        *lock(&self.parked) = Some(hold);
        gate
    }

    pub fn advance(&self, secs: u64) {
        self.mono.fetch_add(secs, Ordering::SeqCst);
    }

    /// The host booted again: a new boot id.
    pub fn set_boot_id(&self, id: &str) {
        *lock(&self.boot) = id.to_owned();
    }

    /// The boot id can no longer be read: [`Clock::boot_id`] answers [`BootIdError::Empty`]
    /// until [`FakeClock::set_boot_id`] names one again.
    pub fn lose_boot_id(&self) {
        lock(&self.boot).clear();
    }
}

impl Clock for FakeClock {
    fn mono_secs(&self) -> u64 {
        self.mono.load(Ordering::SeqCst)
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the fake clock", &lock(&self.boot))
    }

    /// Moves the clock by `secs` at once, or, frozen, only yields.
    fn sleep_secs(&self, secs: u64) {
        let parked = lock(&self.parked).take();
        if let Some(hold) = parked {
            hold.entered.send(());
            lock(&hold.release).recv().unwrap_or_default();
        }
        match self.frozen {
            true => std::thread::yield_now(),
            false => self.advance(secs),
        }
        let over: Vec<(u64, Ending)> = {
            let mut slept = lock(&self.slept);
            slept.count += 1;
            let count = slept.count;
            let (over, due) = std::mem::take(&mut slept.due)
                .into_iter()
                .partition(|(at, _)| *at <= count);
            slept.due = due;
            over
        };
        for (at, ending) in over {
            ending.over(&format!("the clock was slept on {at} times"));
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
    /// The arguments given before the step.
    pub hook_args: Vec<String>,
}

/// A step held inside the launcher, or a sleep held on the clock, until the test releases it.
struct Hold {
    entered: Notice<()>,
    release: Mutex<Receiver<()>>,
}

impl Hold {
    /// A hold, and the test's end of it.
    fn new() -> (Hold, Gate) {
        let (entered, on_entered) = Signal::new();
        let (release, on_release) = mpsc::channel();
        let hold = Hold {
            entered,
            release: Mutex::new(on_release),
        };
        let gate = Gate {
            entered: on_entered,
            release,
        };
        (hold, gate)
    }
}

/// The test's end of a held step: wait for the step to be entered, then release it. A gate let go
/// of releases its step, so a test that fails leaves nothing held.
pub struct Gate {
    entered: Signal<()>,
    release: Sender<()>,
}

impl Gate {
    /// Waits until the held step is running, entered by `by`; [`Missed`] when `by` is over first.
    pub fn entered(&self, by: &dyn Party) -> Result<(), Missed> {
        self.entered.wait(by)
    }

    /// Lets the held step end.
    pub fn release(&self) {
        self.release.send(()).unwrap_or_default();
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.release();
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
    /// The step ran past its bound and was killed: it ends with no exit code.
    Killed,
}

type AdditiveEffect = Box<dyn Fn(&AdditiveCall<'_>) + Send + Sync>;

/// A launcher that plays a script: per service and step, the exit codes in order (with a last
/// line, when one is given), then `0`.
#[derive(Default)]
pub struct ScriptedLauncher {
    exits: Mutex<BTreeMap<Key, VecDeque<i32>>>,
    lines: Mutex<BTreeMap<Key, VecDeque<String>>>,
    additive_exits: Mutex<BTreeMap<String, VecDeque<i32>>>,
    additive_lines: Mutex<BTreeMap<String, VecDeque<String>>>,
    additive_faults: Mutex<BTreeMap<String, AdditiveFault>>,
    during_additive: Mutex<Option<AdditiveEffect>>,
    additive: Mutex<Vec<AdditiveRan>>,
    log: Mutex<Option<Log>>,
    holds: Mutex<BTreeMap<Key, Hold>>,
    runs: Mutex<Vec<Ran>>,
    stops: Mutex<Vec<String>>,
    failing_stops: Mutex<BTreeSet<String>>,
    /// Per service, how many of its stops fail before they work.
    failing_times: Mutex<BTreeMap<String, u32>>,
    /// The held steps that a stop of their unit ends, by step until they run, then by unit.
    ended_by_stop: Mutex<BTreeMap<Key, Sender<()>>>,
    stoppable: Mutex<BTreeMap<String, Sender<()>>>,
    losses: Mutex<BTreeSet<Key>>,
    stop_seen: Mutex<Vec<Notice<String>>>,
    running: AtomicUsize,
    most_running: AtomicUsize,
    tools: Mutex<Option<ToolAnswer>>,
}

/// What answers each tool run ([`StepLauncher::run_tool`]) of a [`ScriptedLauncher`].
pub type ToolAnswer = Arc<dyn Fn(&ToolCall<'_>) -> Result<ToolExit, LaunchError> + Send + Sync>;

impl ScriptedLauncher {
    pub fn new() -> Arc<ScriptedLauncher> {
        Arc::new(ScriptedLauncher::default())
    }

    /// Every later tool run is answered by `answer`; with none, a tool run is refused as by a
    /// launcher that runs no tool.
    pub fn answer_tools(&self, answer: ToolAnswer) {
        *lock(&self.tools) = Some(answer);
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

    /// The exit codes and last lines of the next runs of an additive step for `service`.
    pub fn additive_replies(&self, service: &str, replies: &[(i32, &str)]) {
        let codes = replies.iter().map(|(code, _)| *code).collect();
        let lines = replies.iter().map(|(_, line)| (*line).to_owned()).collect();
        lock(&self.additive_exits).insert(service.to_owned(), codes);
        lock(&self.additive_lines).insert(service.to_owned(), lines);
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
        let (hold, gate) = Hold::new();
        lock(&self.holds).insert((service.to_owned(), step), hold);
        gate
    }

    /// Holds the next run of `step` for `service` until a stop of its unit works, as a unit
    /// that runs until it is stopped (an unbounded `prepare`), or the test releases it.
    pub fn hold_until_stopped(&self, service: &str, step: Step) -> Gate {
        let gate = self.hold(service, step);
        let key = (service.to_owned(), step);
        lock(&self.ended_by_stop).insert(key, gate.release.clone());
        gate
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

    /// Makes the next `times` stops of a unit of `service` fail, and the later ones work.
    pub fn fail_stops_times(&self, service: &str, times: u32) {
        lock(&self.failing_times).insert(service.to_owned(), times);
    }

    /// Lets the stops of the units of `service` work again.
    pub fn allow_stops(&self, service: &str) {
        lock(&self.failing_stops).remove(service);
        lock(&self.failing_times).remove(service);
    }

    /// A signal of the unit of every stop asked for from now on.
    pub fn stops_seen(&self) -> Signal<String> {
        let (notice, signal) = Signal::new();
        lock(&self.stop_seen).push(notice);
        signal
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
    fn run_tool(&self, call: &ToolCall<'_>) -> Result<ToolExit, LaunchError> {
        let answer = lock(&self.tools).clone();
        match answer {
            Some(answer) => answer(call),
            None => Err(LaunchError::NoStart(format!(
                "the scripted launcher runs no tool: {}",
                call.program.display()
            ))),
        }
    }

    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        let key = (call.service.to_owned(), call.invocation.step);
        note(&self.log, format!("run {}", call.unit));
        lock(&self.runs).push(Ran {
            service: call.service.to_owned(),
            step: call.invocation.step,
            unit: call.unit.to_owned(),
            executable: call.executable.to_path_buf(),
            artifact: call.invocation.artifact.clone(),
            hook_args: call.hook_args.to_vec(),
        });
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_running.fetch_max(now, Ordering::SeqCst);
        let hold = lock(&self.holds).remove(&key);
        if let Some(hold) = hold {
            let ends = lock(&self.ended_by_stop).remove(&key);
            let stoppable = ends.map(|end| (call.unit.to_owned(), end));
            lock(&self.stoppable).extend(stoppable);
            hold.entered.send(());
            lock(&hold.release).recv().unwrap_or_default();
            lock(&self.stoppable).remove(call.unit);
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
        let last_line = lock(&self.additive_lines)
            .get_mut(call.service)
            .and_then(VecDeque::pop_front);
        let fault = lock(&self.additive_faults).remove(call.service);
        let lost = || LaunchError::Launcher(format!("{} was lost", call.unit));
        match fault {
            None => Ok(StepExit {
                code: Some(code),
                last_line,
            }),
            Some(AdditiveFault::Killed) => Ok(StepExit {
                code: None,
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
            seen.send(unit.to_owned());
        }
        let of = |service: &str| unit.starts_with(&format!("updater-{service}-"));
        let always = lock(&self.failing_stops).iter().any(|s| of(s.as_str()));
        let mut times = lock(&self.failing_times);
        let counted = times
            .iter_mut()
            .find(|(service, left)| of(service.as_str()) & (**left > 0));
        let counted = counted.map(|(_, left)| *left -= 1).is_some();
        drop(times);
        match always | counted {
            true => Err(LaunchError::Launcher(format!(
                "{unit} could not be stopped"
            ))),
            false => {
                // A unit that is stopped ends: a step held until then returns.
                let held = lock(&self.stoppable).remove(unit);
                held.iter().for_each(|end| end.send(()).unwrap_or_default());
                Ok(())
            }
        }
    }
}

/// A manifest for `service` at `version` that every load rule accepts, its hook `bin/hook`, built
/// for the platform these tests run on (the daemon's own, `Platform::current()`).
pub fn manifest(service: &str, version: &str) -> String {
    let platform = hostkeeper_config::platform::Platform::current();
    format!(
        r#"schema = 1
name = "{service}"
version = "{version}"
platform = "{platform}"
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
    index_digests: Mutex<Vec<Option<String>>>,
    entries: Mutex<Vec<Option<Target>>>,
    pinned: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    swept: Mutex<Vec<String>>,
    notify: Mutex<Vec<Notice<String>>>,
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
            index_digests: Mutex::default(),
            entries: Mutex::default(),
            pinned: Mutex::default(),
            swept: Mutex::default(),
            notify: Mutex::default(),
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

    /// Makes the package of `service` at `version` fail with `error`.
    pub fn fail(&self, service: &str, version: &str, error: PackageError) {
        lock(&self.failures).insert((service.to_owned(), version.to_owned()), error);
    }

    /// Every package asked for, as (service, version), in order.
    pub fn asked(&self) -> Vec<(String, String)> {
        lock(&self.asked).clone()
    }

    /// The index digest of every package asked for, in the order of [`FakePackages::asked`]:
    /// `None` for an order that named none.
    pub fn index_digests(&self) -> Vec<Option<String>> {
        lock(&self.index_digests).clone()
    }

    /// The entry of every package asked for, in the order of [`FakePackages::asked`]: `None` for
    /// an order that carried none.
    pub fn entries(&self) -> Vec<Option<Target>> {
        lock(&self.entries).clone()
    }

    /// Serves `bytes` as the artifact of `service` whose entry's digest is `digest`
    /// ([`Packages::pinned`]); an entry with another digest is `package_digest_mismatch`
    /// ([`PackageError::DigestMismatch`]).
    pub fn add_pinned(&self, service: &str, digest: &str, bytes: &[u8]) {
        lock(&self.pinned).insert((service.to_owned(), digest.to_owned()), bytes.to_vec());
    }

    /// Every service whose orphans were asked to be removed, in order.
    pub fn swept(&self) -> Vec<String> {
        lock(&self.swept).clone()
    }

    /// A signal of `<service> <version>` of every package asked for from now on.
    pub fn notified(&self) -> Signal<String> {
        let (notice, signal) = Signal::new();
        lock(&self.notify).push(notice);
        signal
    }
}

impl Packages for FakePackages {
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &Version,
        _digest: &PackageDigest,
        index_digest: Option<&PackageDigest>,
        entry: Option<&Target>,
    ) -> Result<VersionDir, PackageError> {
        let key = (service.to_string(), version.to_string());
        lock(&self.entries).push(entry.cloned());
        lock(&self.asked).push(key.clone());
        lock(&self.index_digests).push(index_digest.map(|d| d.as_str().to_owned()));
        for notice in lock(&self.notify).iter() {
            notice.send(format!("{service} {version}"));
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

    /// The bytes [`FakePackages::add_pinned`] gave for the entry's digest, written to a file
    /// under the service's package directory that is removed when the answer is dropped; the
    /// failure [`FakePackages::fail`] set for the service at the entry's digest wins. Each ask is
    /// one of [`FakePackages::asked`], keyed by the digest, and its entry one of
    /// [`FakePackages::entries`].
    fn pinned(&self, service: &ServiceName, entry: &Target) -> Result<Pinned, PackageError> {
        let key = (service.to_string(), entry.digest.clone());
        lock(&self.entries).push(Some(entry.clone()));
        lock(&self.asked).push(key.clone());
        if let Some(failure) = lock(&self.failures).get(&key).cloned() {
            return Err(failure);
        }
        let bytes = lock(&self.pinned)
            .get(&key)
            .cloned()
            .ok_or_else(|| PackageError::DigestMismatch(format!("no artifact of {}", key.1)))?;
        let dir = self.root.join(service.as_str());
        dir_0755(&dir);
        let file = PinnedFile(dir.join(format!(".fetch-pinned-{}", entry.digest)));
        fs::write(&file.0, bytes).map_err(|e| PackageError::Other(e.to_string()))?;
        let path = file.0.clone();
        Ok(Pinned::new(&path, Box::new(file)))
    }

    fn remove_orphans(&self, service: &ServiceName) -> Result<(), PackageError> {
        lock(&self.swept).push(service.to_string());
        Ok(())
    }
}

/// A file [`FakePackages::pinned`] wrote, removed when dropped.
struct PinnedFile(PathBuf);

impl Drop for PinnedFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A signal of the service of each caller that begins to wait for the lock of a service to be
/// let go of ([`ServiceLocks::wait_let_go`]): an abort that has asked the runner and waits for
/// the order. It takes the place of any hook set on `locks` before.
pub fn lock_waits(locks: &ServiceLocks) -> Signal<String> {
    let (notice, signal) = Signal::new();
    locks.on_wait(move |service| notice.send(service.to_string()));
    signal
}

/// A signal of the service of each watcher a caller has told to stop ([`Watchers::stop`]), such
/// as an order that replaces a watcher; the daemon's own end stops its watchers without it. It
/// takes the place of any `on_stop` hook set on `watchers` before.
pub fn watcher_stops(watchers: &Watchers) -> Signal<String> {
    let (notice, signal) = Signal::new();
    watchers.on_stop(move |service| notice.send(service.to_string()));
    signal
}

/// A party over once a watcher has ended, saying whose: the party of a signal a watcher sends,
/// such as a held `verify` entered, in a test whose watchers are of one service. It takes the
/// place of any `on_end` hook set on `watchers` before.
pub fn watcher_ends(watchers: &Watchers) -> Ending {
    let ending = Ending::default();
    let over = ending.clone();
    watchers.on_end(move |service| over.over(&format!("the watcher of {service} ended")));
    ending
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

/// A prune that deletes nothing: the prune of the tests that do not look at pruning.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPrune;

impl Prune for NoPrune {
    fn prune(
        &self,
        _layout: &Layout,
        _protect: &Protected,
        _held: &ServiceGuard<'_>,
    ) -> io::Result<PruneReport> {
        Ok(PruneReport::default())
    }
}

/// The points of a prune's deletion a test picks to fail.
type Picks = Box<dyn Fn(&PruneStep<'_>) -> bool + Send + Sync>;

/// A fault [`RecordingPrune`] puts in its next call.
enum PruneFault {
    /// The whole prune fails with this text, and nothing is deleted.
    Whole(String),
    /// Each point of the deletion the test picks fails, as a crash or a failed removal there
    /// would.
    At(Picks),
}

/// The host's prune ([`Layout::prune_with`]) that records the service of each call and the
/// versions each was told to protect, and fails a call when told.
#[derive(Default)]
pub struct RecordingPrune {
    calls: Mutex<Vec<(String, Protected)>>,
    faults: Mutex<VecDeque<PruneFault>>,
}

impl RecordingPrune {
    pub fn new() -> Arc<RecordingPrune> {
        Arc::new(RecordingPrune::default())
    }

    /// The service of each call, in order.
    pub fn calls(&self) -> Vec<String> {
        lock(&self.calls)
            .iter()
            .map(|(service, _)| service.clone())
            .collect()
    }

    /// What each call was told to protect, in order.
    pub fn protected(&self) -> Vec<Protected> {
        lock(&self.calls)
            .iter()
            .map(|(_, protect)| protect.clone())
            .collect()
    }

    /// The next call fails with `why` and deletes nothing.
    pub fn fail_next(&self, why: &str) {
        lock(&self.faults).push_back(PruneFault::Whole(why.to_owned()));
    }

    /// In the next call, each point of the deletion `at` picks fails.
    pub fn fault_next(&self, at: impl Fn(&PruneStep<'_>) -> bool + Send + Sync + 'static) {
        lock(&self.faults).push_back(PruneFault::At(Box::new(at)));
    }
}

impl Prune for RecordingPrune {
    fn prune(
        &self,
        layout: &Layout,
        protect: &Protected,
        _held: &ServiceGuard<'_>,
    ) -> io::Result<PruneReport> {
        let service = layout
            .dir()
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        lock(&self.calls).push((service, protect.clone()));
        let fault = lock(&self.faults).pop_front();
        let picks: Picks = match fault {
            Some(PruneFault::Whole(why)) => return Err(io::Error::other(why)),
            Some(PruneFault::At(picks)) => picks,
            None => Box::new(|_: &PruneStep<'_>| false),
        };
        layout.prune_with(protect, &mut |step| match picks(&step) {
            true => Err(io::Error::other("the removal was refused")),
            false => Ok(()),
        })
    }
}

/// The repository installer's ports of a test host whose root is `root` and whose packages
/// directory is `packages_dir`, with the repository entries `repositories`: what is staged is
/// given to the owner of `root`, the test's own user.
pub fn repository_ports(
    packages_dir: &Path,
    root: &Path,
    repositories: hostkeeper_install::repository::Repositories,
) -> Arc<crate::repository::RepositoryPorts> {
    use std::os::unix::fs::MetadataExt;
    let owner = fs::metadata(root).map_or(hostkeeper_unpack::Owner::ROOT, |made| {
        hostkeeper_unpack::Owner {
            uid: made.uid(),
            gid: made.gid(),
        }
    });
    Arc::new(crate::repository::RepositoryPorts::new(
        packages_dir,
        root,
        repositories,
        owner,
    ))
}
