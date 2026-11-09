//! Fakes of every port of the loop, for this crate's tests and for the crates that wire the loop
//! into the daemon (in their dev-dependencies, with the feature `testing`). Each records what the
//! loop asked of it; none touches the network, the host or the wall clock.
//!
//! [`World`] holds one of each and builds the [`Ports`] and the [`Heartbeater`] over them.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::{BootId, BootIdError, Clock};

use crate::beat::{Health, Heartbeat, Liveness, Platform, Renewal, TrustedRootFacts};
use crate::heartbeater::{DEFAULT_HEARTBEAT_SECS, Heartbeater, LoopConfig, Ports};
use crate::order::{Order, OrderRef, OrderResult, Outcome, Reply};
use crate::ports::{
    CertSource, CertState, ClientIdentity, CoordinatorLink, Dispatch, HeldHalts, Job, LinkError,
    Log, Observation, OrderSink, RecordedService, RenewalSource, ServiceObserver, ServiceRecords,
    Sleeper, Tick, TrustedRootSource, UnixClock,
};

/// A lock that a panicking test thread did not spoil: what it holds is still the fake's state.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A coordinator that answers from a script, and [`Reply::default`] once the script is out.
#[derive(Default)]
pub struct FakeLink {
    answers: Mutex<VecDeque<Result<Reply, LinkError>>>,
    calls: Mutex<Vec<(ClientIdentity, Heartbeat)>>,
}

impl FakeLink {
    /// Adds `answer` to the end of the script.
    pub fn answer(&self, answer: Result<Reply, LinkError>) {
        lock(&self.answers).push_back(answer);
    }

    /// Every call so far: the pair it was made with, and the heartbeat.
    pub fn calls(&self) -> Vec<(ClientIdentity, Heartbeat)> {
        lock(&self.calls).clone()
    }

    /// The heartbeats sent so far.
    pub fn beats(&self) -> Vec<Heartbeat> {
        self.calls().into_iter().map(|(_, beat)| beat).collect()
    }
}

impl CoordinatorLink for FakeLink {
    fn heartbeat(&self, identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        lock(&self.calls).push((identity.clone(), beat.clone()));
        lock(&self.answers)
            .pop_front()
            .unwrap_or_else(|| Ok(Reply::default()))
    }
}

/// A pair of a certificate and a key, both `tag` in their text.
pub fn identity(tag: &str) -> ClientIdentity {
    ClientIdentity::new(
        format!("certificate {tag}").into_bytes(),
        format!("key {tag}").into_bytes(),
    )
}

/// A certificate source whose state the test sets.
pub struct FakeCerts {
    state: Mutex<CertState>,
}

impl FakeCerts {
    pub fn new(state: CertState) -> FakeCerts {
        FakeCerts {
            state: Mutex::new(state),
        }
    }

    pub fn set(&self, state: CertState) {
        *lock(&self.state) = state;
    }
}

impl CertSource for FakeCerts {
    fn current(&self) -> CertState {
        lock(&self.state).clone()
    }
}

/// Step records the test writes, as the daemon's executor writes its own.
#[derive(Default)]
pub struct FakeRecords {
    services: Mutex<Vec<RecordedService>>,
    failing: Mutex<Option<io::ErrorKind>>,
    acknowledged: Mutex<Vec<(ServiceName, OrderRef)>>,
    unkept: Mutex<Option<io::ErrorKind>>,
}

impl FakeRecords {
    pub fn set(&self, services: Vec<RecordedService>) {
        *lock(&self.services) = services;
    }

    /// From now on the records cannot be read, with `kind`; `None` makes them readable again.
    pub fn fail(&self, kind: Option<io::ErrorKind>) {
        *lock(&self.failing) = kind;
    }

    /// From now on an acknowledgement cannot be kept, with `kind`; `None` makes it kept again.
    pub fn fail_acknowledged(&self, kind: Option<io::ErrorKind>) {
        *lock(&self.unkept) = kind;
    }

    /// Every acknowledgement the loop passed on, in order, kept or not.
    pub fn acknowledged_calls(&self) -> Vec<(ServiceName, OrderRef)> {
        lock(&self.acknowledged).clone()
    }
}

impl ServiceRecords for FakeRecords {
    fn installed(&self) -> io::Result<Vec<RecordedService>> {
        match *lock(&self.failing) {
            Some(kind) => Err(io::Error::from(kind)),
            None => Ok(lock(&self.services).clone()),
        }
    }

    fn acknowledged(&self, service: &ServiceName, order: &OrderRef) -> io::Result<()> {
        lock(&self.acknowledged).push((service.clone(), order.clone()));
        match *lock(&self.unkept) {
            Some(kind) => Err(io::Error::from(kind)),
            None => Ok(()),
        }
    }
}

/// An observer that sees what the test set, and a running, healthy service otherwise.
#[derive(Default)]
pub struct FakeObserver {
    seen: Mutex<BTreeMap<String, Observation>>,
}

impl FakeObserver {
    pub fn set(&self, service: &ServiceName, observation: Observation) {
        lock(&self.seen).insert(service.as_str().to_owned(), observation);
    }
}

impl ServiceObserver for FakeObserver {
    fn observe(&self, service: &ServiceName) -> Observation {
        lock(&self.seen)
            .get(service.as_str())
            .cloned()
            .unwrap_or(Observation {
                liveness: Liveness::Running,
                version: None,
                health: Health::Healthy,
                health_detail: None,
                drift: None,
            })
    }
}

/// The verdict of `order` with `outcome`, naming `target`, and nothing else reported.
pub fn verdict(order: &Order, target: &Version, outcome: Outcome) -> OrderResult {
    OrderResult {
        order: order.reference(),
        target: target.clone(),
        outcome,
        phase: None,
        class: None,
        reason: None,
        forced: order.is_forced(),
        detail: None,
    }
}

/// The answer of a [`FakeSink`].
type Answer = Box<dyn Fn(&Order, &Version) -> OrderResult + Send + Sync>;

/// An executor that answers with a function of the order and the version its verdict names,
/// and records every order it was given, with that version.
pub struct FakeSink {
    answer: Answer,
    orders: Mutex<Vec<(Order, Version)>>,
}

impl FakeSink {
    pub fn new(
        answer: impl Fn(&Order, &Version) -> OrderResult + Send + Sync + 'static,
    ) -> FakeSink {
        FakeSink {
            answer: Box::new(answer),
            orders: Mutex::new(Vec::new()),
        }
    }

    /// An executor under which every order succeeds.
    pub fn succeeding() -> FakeSink {
        FakeSink::new(|order, target| verdict(order, target, Outcome::Succeeded))
    }

    /// The orders it ran, in order.
    pub fn orders(&self) -> Vec<Order> {
        lock(&self.orders)
            .iter()
            .map(|(order, _)| order.clone())
            .collect()
    }

    /// The versions the loop gave with the orders it ran, in order.
    pub fn targets(&self) -> Vec<Version> {
        lock(&self.orders)
            .iter()
            .map(|(_, target)| target.clone())
            .collect()
    }
}

impl OrderSink for FakeSink {
    fn execute(&self, order: &Order, target: &Version) -> OrderResult {
        lock(&self.orders).push((order.clone(), target.clone()));
        (self.answer)(order, target)
    }
}

/// A dispatcher that keeps the jobs until the test runs them, on the test's own thread.
#[derive(Default)]
pub struct ManualDispatch {
    jobs: Mutex<VecDeque<Job>>,
}

impl ManualDispatch {
    /// Runs the oldest job; false when there is none.
    pub fn run_next(&self) -> bool {
        let job = lock(&self.jobs).pop_front();
        job.map(|job| job()).is_some()
    }

    /// Drops the oldest job without running it, as a worker that died would; false when there
    /// is none.
    pub fn drop_next(&self) -> bool {
        lock(&self.jobs).pop_front().is_some()
    }

    /// How many jobs wait.
    pub fn pending(&self) -> usize {
        lock(&self.jobs).len()
    }
}

impl Dispatch for ManualDispatch {
    fn start(&self, job: Job) {
        lock(&self.jobs).push_back(job);
    }
}

/// A held-halt store in memory: monotone like the daemon's, with every raise recorded.
#[derive(Default)]
pub struct FakeHalts {
    held: Mutex<BTreeMap<String, u64>>,
    raises: Mutex<Vec<(String, u64)>>,
    failing: AtomicBool,
}

impl FakeHalts {
    /// Sets the held generation of `service`, as another writer of the store (the executor,
    /// `clear_halt`) would.
    pub fn set(&self, service: &ServiceName, generation: u64) {
        lock(&self.held).insert(service.as_str().to_owned(), generation);
    }

    /// From now on a raise fails, or no longer does.
    pub fn fail(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    /// The raises the loop asked for, by service name and generation, in order.
    pub fn raises(&self) -> Vec<(String, u64)> {
        lock(&self.raises).clone()
    }
}

impl HeldHalts for FakeHalts {
    fn held(&self, service: &ServiceName) -> u64 {
        lock(&self.held).get(service.as_str()).copied().unwrap_or(0)
    }

    fn raise(&self, service: &ServiceName, generation: u64) -> io::Result<()> {
        lock(&self.raises).push((service.as_str().to_owned(), generation));
        if self.failing.load(Ordering::SeqCst) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        let mut held = lock(&self.held);
        let entry = held.entry(service.as_str().to_owned()).or_insert(0);
        *entry = (*entry).max(generation);
        Ok(())
    }
}

/// A clock that moves only when told to, or when a wait passes: its unix seconds and its
/// monotonic seconds, which start far apart so a test tells them apart.
pub struct FakeClock {
    mono: AtomicU64,
    unix: AtomicU64,
    waits: Mutex<Vec<u64>>,
}

impl FakeClock {
    /// A clock at `unix` unix seconds and `mono` monotonic seconds.
    pub fn new(unix: u64, mono: u64) -> FakeClock {
        FakeClock {
            mono: AtomicU64::new(mono),
            unix: AtomicU64::new(unix),
            waits: Mutex::new(Vec::new()),
        }
    }

    /// Sets the unix clock to `unix`, forward or back, as a clock that was set would; the
    /// monotonic clock does not move.
    pub fn set_unix(&self, unix: u64) {
        self.unix.store(unix, Ordering::SeqCst);
    }

    /// Moves both clocks on by `secs`.
    pub fn advance(&self, secs: u64) {
        self.mono.fetch_add(secs, Ordering::SeqCst);
        self.unix.fetch_add(secs, Ordering::SeqCst);
    }

    /// The waits asked of it, in seconds.
    pub fn waits(&self) -> Vec<u64> {
        lock(&self.waits).clone()
    }
}

impl UnixClock for FakeClock {
    fn unix_secs(&self) -> u64 {
        self.unix.load(Ordering::SeqCst)
    }
}

impl Clock for FakeClock {
    fn mono_secs(&self) -> u64 {
        self.mono.load(Ordering::SeqCst)
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the fake clock", "fake-boot")
    }

    fn sleep_secs(&self, secs: u64) {
        lock(&self.waits).push(secs);
        self.advance(secs);
    }
}

/// A log that keeps its lines.
#[derive(Default)]
pub struct RecordingLog {
    lines: Mutex<Vec<String>>,
}

impl RecordingLog {
    pub fn lines(&self) -> Vec<String> {
        lock(&self.lines).clone()
    }
}

impl Log for RecordingLog {
    fn line(&self, text: &str) {
        lock(&self.lines).push(text.to_owned());
    }
}

/// A sleeper that lets the loop beat `beats` more times, then stops it, recording each wait.
pub struct ScriptedSleeper {
    beats: AtomicU64,
    waits: Mutex<Vec<u64>>,
}

impl ScriptedSleeper {
    pub fn new(beats: u64) -> ScriptedSleeper {
        ScriptedSleeper {
            beats: AtomicU64::new(beats),
            waits: Mutex::new(Vec::new()),
        }
    }

    /// The waits asked of it, in seconds.
    pub fn waits(&self) -> Vec<u64> {
        lock(&self.waits).clone()
    }
}

impl Sleeper for ScriptedSleeper {
    fn sleep(&self, secs: u64) -> Tick {
        lock(&self.waits).push(secs);
        let left = self.beats.load(Ordering::SeqCst);
        self.beats.store(left.saturating_sub(1), Ordering::SeqCst);
        if left == 0 { Tick::Stop } else { Tick::Beat }
    }
}

/// The enrollment loop's word on the certificate, as a test sets it: none until it does.
#[derive(Default)]
pub struct FakeRenewal {
    renewal: Mutex<Renewal>,
}

impl FakeRenewal {
    pub fn set(&self, renewal: Renewal) {
        *lock(&self.renewal) = renewal;
    }
}

impl RenewalSource for FakeRenewal {
    fn renewal(&self) -> Renewal {
        lock(&self.renewal).clone()
    }
}

/// The daemon's report of its trusted root, as a test sets it: none until it does.
#[derive(Default)]
pub struct FakeTrustedRoot {
    root: Mutex<Option<TrustedRootFacts>>,
}

impl FakeTrustedRoot {
    pub fn set(&self, root: Option<TrustedRootFacts>) {
        *lock(&self.root) = root;
    }
}

impl TrustedRootSource for FakeTrustedRoot {
    fn trusted_root(&self) -> Option<TrustedRootFacts> {
        *lock(&self.root)
    }
}

/// The unix second the [`World`]'s clock starts at.
pub const START_UNIX: u64 = 2_000_000_000;

/// The monotonic second the [`World`]'s clock starts at.
pub const START_MONO: u64 = 7;

/// One fake of every port: an enrolled host (pair `a`), no installed service, an executor under
/// which every order succeeds, and a manual dispatcher.
pub struct World {
    pub link: Arc<FakeLink>,
    pub certs: Arc<FakeCerts>,
    pub records: Arc<FakeRecords>,
    pub observer: Arc<FakeObserver>,
    pub orders: Arc<FakeSink>,
    pub dispatch: Arc<ManualDispatch>,
    pub halts: Arc<FakeHalts>,
    pub renewal: Arc<FakeRenewal>,
    pub trusted_root: Arc<FakeTrustedRoot>,
    pub clock: Arc<FakeClock>,
    pub log: Arc<RecordingLog>,
}

impl Default for World {
    fn default() -> World {
        World::with_orders(FakeSink::succeeding())
    }
}

impl World {
    /// The world with `orders` as its executor.
    pub fn with_orders(orders: FakeSink) -> World {
        World {
            link: Arc::default(),
            certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("a")))),
            records: Arc::default(),
            observer: Arc::default(),
            orders: Arc::new(orders),
            dispatch: Arc::default(),
            halts: Arc::default(),
            renewal: Arc::default(),
            trusted_root: Arc::default(),
            clock: Arc::new(FakeClock::new(START_UNIX, START_MONO)),
            log: Arc::default(),
        }
    }

    /// The ports over these fakes.
    pub fn ports(&self) -> Ports {
        Ports {
            link: self.link.clone(),
            certs: self.certs.clone(),
            records: self.records.clone(),
            observer: self.observer.clone(),
            orders: self.orders.clone(),
            dispatch: self.dispatch.clone(),
            halts: self.halts.clone(),
            renewal: self.renewal.clone(),
            trusted_root: self.trusted_root.clone(),
            clock: self.clock.clone(),
            log: self.log.clone(),
        }
    }

    /// A loop over these fakes, for a daemon of version `agent_version` on Linux x86-64 whose
    /// state directory is `state_dir`, beating every 15 seconds.
    pub fn heartbeater(&self, state_dir: &Path, agent_version: Version) -> Heartbeater {
        Heartbeater::new(
            LoopConfig {
                platform: Platform::LinuxX86_64,
                agent_version,
                state_dir: PathBuf::from(state_dir),
                period_secs: DEFAULT_HEARTBEAT_SECS,
                installers: Vec::new(),
            },
            self.ports(),
        )
    }
}
