//! The daemon's enrollment and heartbeat loops (`hostkeeper_daemon::links`): `serve` starts them
//! once the daemon is ready, the enrollment loop first; the enrollment loop runs its enroller
//! until it is told to stop, or says why it has none; the host's links read the pki directory
//! and the daemon's clock, and log through the daemon.

mod support;

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_daemon::cli::{HostFiles, host_enroller, host_links, image_of};
use hostkeeper_daemon::host_identity::{
    CERT_FILE, HostIdentity, IDENTITY_KEPT, IDENTITY_NOT_KEPT, IdentityOwner, KEY_FILE,
};
use hostkeeper_daemon::identity_group::{IDENTITY_GROUP, IdentityGroup};
use hostkeeper_daemon::links::{Links, NO_ENROLLMENT};
use hostkeeper_daemon::testing::{Ending, FakeNotifier, Log, Notice, Signal};
use hostkeeper_daemon::unix_clock::SystemUnixClock;
use hostkeeper_daemon::{Daemon, NoWatchdog, start};
use hostkeeper_enroll::{
    CoordinatorEnroll, EnrollAnswer, EnrollRequest, LinuxClaims, OsEntropy, PkiDir, Poll,
    PollAnswer, RenewAnswer, RenewRequest, Settings as EnrollSettings, SystemWallClock,
    TransportError,
};
use hostkeeper_launchd::{FakeLaunchctl, PlatformKind};
use hostkeeper_link::ClientIdentity;
use hostkeeper_link::testing::{FakeCerts, FakeLink, RecordingLog, ScriptedSleeper};
use hostkeeper_link::{
    CertSource, CertState, DEFAULT_HEARTBEAT_SECS, Dispatch, Job, StopFlag, Tick, UnixClock,
};
use hostkeeper_steps::Clock;
use support::{OWN, Rig, alerting, lines, open_once};

/// A dispatcher that hands each job to the test, which runs it when and where it likes.
struct Handed(Notice<Job>);

impl Dispatch for Handed {
    fn start(&self, job: Job) {
        self.0.send(job);
    }
}

fn handed() -> (Arc<Handed>, Signal<Job>) {
    let (notice, jobs) = Signal::new();
    (Arc::new(Handed(notice)), jobs)
}

/// Declares its party over when dropped: when the task that owns it ends, or is cancelled, or
/// panics.
struct OverOnDrop(Ending);

impl Drop for OverOnDrop {
    fn drop(&mut self) {
        self.0.over("the server ended");
    }
}

/// The certificate source of a host not enrolled, which notes in `events` that the heartbeat
/// loop asked for the certificate.
struct NotedCerts(Log);

impl CertSource for NotedCerts {
    fn current(&self) -> CertState {
        self.0
            .lock()
            .expect("the events")
            .push("heartbeat loop".to_owned());
        CertState::NotEnrolled {
            waiting_for: PathBuf::from("/pki/current"),
        }
    }
}

/// Links over fakes: the enroller is `enroller`, the heartbeat loop beats once and stops, and
/// each loop is handed to the test by `dispatch`.
fn links_with(
    enroller: hostkeeper_daemon::links::MakeEnroller,
    certs: Arc<dyn CertSource>,
    log: Arc<RecordingLog>,
    dispatch: Arc<Handed>,
) -> Links {
    Links {
        enroller,
        link: Arc::new(FakeLink::default()),
        certs,
        clock: Arc::new(hostkeeper_link::testing::FakeClock::new(0, 0)),
        log,
        sleeper: Box::new(ScriptedSleeper::new(0)),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch,
        stop: StopFlag::default(),
        enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
        renewal: hostkeeper_enroll::RenewalState::default(),
        watchdog: Box::new(NoWatchdog),
        console: None,
        identity: None,
        remote_login_probe: None,
        full_disk_access: None,
        operator_keys: None,
        prerequisites: None,
        maintenance: None,
        bmc: None,
        serial_console: None,
        health: None,
    }
}

/// Catches a loop started before the boot work is over and the readiness notice was sent, or
/// the heartbeat loop started before the enrollment loop, or a loop not started: the daemon
/// starts and says it is ready with no loop started; `serve` hands over two loops, and run in
/// the order they were started, the first makes the enroller and the second asks for the
/// certificate. Once `serve` has returned for the daemon's own update the loops are told to
/// stop.
#[test]
fn the_loops_start_only_after_ready_and_the_enrollment_loop_first() {
    let rig = Rig::new("links-order");
    let events = Log::default();
    let notifier = FakeNotifier::new(events.clone());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    let daemon = Arc::clone(started.daemon());
    let noted = events.clone();
    let enroller: hostkeeper_daemon::links::MakeEnroller = Box::new(move || {
        noted
            .lock()
            .expect("the events")
            .push("enrollment loop".to_owned());
        Err("no coordinator in this test".to_owned())
    });
    let log = Arc::new(RecordingLog::default());
    let (dispatch, jobs) = handed();
    let links = links_with(
        enroller,
        Arc::new(NotedCerts(events.clone())),
        Arc::clone(&log),
        dispatch,
    );
    let stop = links.stop.clone();
    assert_eq!(lines(&events), ["ready"], "ready, and no loop yet");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let serving = Ending::default();
    let over = OverOnDrop(serving.clone());
    let served = runtime.spawn(async move {
        let _over = over;
        start::serve(started, links).await
    });
    let first = jobs.wait(&serving).expect("the first loop");
    let second = jobs.wait(&serving).expect("the second loop");
    first();
    second();
    assert_eq!(
        lines(&events),
        ["ready", "enrollment loop", "heartbeat loop"]
    );
    assert_eq!(
        log.lines(),
        [
            format!("{NO_ENROLLMENT}no coordinator in this test"),
            "not enrolled: waiting for /pki/current".to_owned(),
        ]
    );
    assert!(!stop.is_stopped(), "the loops run while the daemon serves");

    daemon.ctx().restart().request();
    runtime.block_on(served).expect("the server task");
    assert!(stop.is_stopped(), "the loops are told to stop");
    assert!(
        jobs.wait(&serving).is_err(),
        "nothing else was started: the server is over, and no other loop was handed over"
    );
}

/// The coordinator of the enrollment loop's test: it cannot be reached, and the first time it is
/// asked for its CA it tells the loop to stop.
struct Unreachable {
    asked: Arc<AtomicUsize>,
    stop: StopFlag,
}

impl CoordinatorEnroll for Unreachable {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.stop.stop();
        Err(TransportError("down".to_owned()))
    }

    fn enroll(&mut self, _request: &EnrollRequest) -> Result<EnrollAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }

    fn poll(&mut self, _poll: &Poll) -> Result<PollAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }

    fn renew(&mut self, _request: &RenewRequest) -> Result<RenewAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }
}

fn pin() -> Sha256Hex {
    Sha256Hex::parse(&"00".repeat(32)).expect("a SHA-256")
}

/// Catches an enrollment loop that does not run its enroller, does not log its steps, or does
/// not stop when told: the loop's one step asks the coordinator for its CA, logs that the
/// coordinator could not be reached, and the loop, told to stop meanwhile, makes no other step.
#[test]
fn the_enrollment_loop_runs_its_enroller_until_it_is_told_to_stop() {
    let rig = Rig::new("links-enroll");
    let daemon = Arc::new(rig.daemon());
    let asked = Arc::new(AtomicUsize::new(0));
    let stop = StopFlag::default();
    let pki = rig.scratch.path().join("pki");
    let claims = rig.scratch.path().join("no-claims");
    let ports = hostkeeper_enroll::Ports {
        coordinator: Box::new(Unreachable {
            asked: Arc::clone(&asked),
            stop: stop.clone(),
        }),
        store: Box::new(PkiDir::new(&pki)),
        claims: Box::new(LinuxClaims::new(&claims, &claims, &claims, &claims)),
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(hostkeeper_link::testing::FakeClock::new(0, 0)),
        managed: None,
    };
    let settings = EnrollSettings {
        ca_sha256: pin(),
        image: "image-1".to_owned(),
    };
    let log = Arc::new(RecordingLog::default());
    let (dispatch, jobs) = handed();
    let links = Links {
        stop: stop.clone(),
        ..links_with(
            open_once(settings, ports),
            Arc::new(NotedCerts(Log::default())),
            Arc::clone(&log),
            dispatch,
        )
    };
    links.start(&daemon);
    // `start` hands over both loops before it returns.
    let started = Ending::default();
    started.over("links.start returned");
    let enrollment = jobs.wait(&started).expect("the enrollment loop");
    enrollment();
    assert_eq!(asked.load(Ordering::SeqCst), 1, "one step, then stopped");
    assert_eq!(log.lines(), ["the coordinator could not be reached: down"]);
    assert!(
        jobs.wait(&started).is_ok(),
        "the heartbeat loop was started too"
    );
}

/// The coordinator of the identity tests: it cannot be reached; at its call number `k` it makes
/// the host's pair `k` (certificate `cert k`, key `key k`) what the certificate source gives, notes
/// what the identity directory held when it was called, and at the call number `stop_at` it tells
/// the loop to stop.
struct StepsOf {
    calls: usize,
    stop_at: usize,
    certs: Arc<FakeCerts>,
    certificate: PathBuf,
    held: Arc<std::sync::Mutex<Vec<Option<String>>>>,
    stop: StopFlag,
}

impl CoordinatorEnroll for StepsOf {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        self.held
            .lock()
            .expect("the notes")
            .push(std::fs::read_to_string(&self.certificate).ok());
        self.calls += 1;
        let k = self.calls;
        self.certs.set(CertState::Enrolled(ClientIdentity::new(
            format!("cert {k}").into_bytes(),
            format!("key {k}").into_bytes(),
        )));
        if k == self.stop_at {
            self.stop.stop();
        }
        Err(TransportError("down".to_owned()))
    }

    fn enroll(&mut self, _request: &EnrollRequest) -> Result<EnrollAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }

    fn poll(&mut self, _poll: &Poll) -> Result<PollAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }

    fn renew(&mut self, _request: &RenewRequest) -> Result<RenewAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }
}

/// A certificate source that notes, at each read, whether the daemon's node holds an enrollment.
struct SeesNode {
    inner: Arc<FakeCerts>,
    daemon: Arc<Daemon>,
    saw: Arc<std::sync::Mutex<Vec<bool>>>,
}

impl hostkeeper_link::CertSource for SeesNode {
    fn current(&self) -> CertState {
        self.saw
            .lock()
            .expect("the notes")
            .push(self.daemon.ctx().node.enrollment().is_some());
        self.inner.current()
    }
}

/// What [`three_steps`] saw: the identity directory's certificate as each step began, the log, the
/// identity directory, whether the node held an enrollment at each read of the copy's source, and
/// the rig.
struct Ran {
    notes: Vec<Option<String>>,
    lines: Vec<String>,
    dir: PathBuf,
    node_held: Vec<bool>,
    _rig: Rig,
}

/// Runs the enrollment loop for three steps with an identity copy in `identity_dir`, whose
/// certificate source the coordinator of [`StepsOf`] sets at each step.
fn three_steps(name: &str, prepare: impl FnOnce(&Path)) -> Ran {
    let rig = Rig::new(name);
    let daemon = Arc::new(rig.daemon());
    let stop = StopFlag::default();
    let root = rig.scratch.path();
    let identity_dir = root.join("identity");
    prepare(&identity_dir);
    let certs = Arc::new(FakeCerts::new(CertState::NotEnrolled {
        waiting_for: PathBuf::from("current"),
    }));
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let saw = Arc::new(std::sync::Mutex::new(Vec::new()));
    let claims = root.join("no-claims");
    let ports = hostkeeper_enroll::Ports {
        coordinator: Box::new(StepsOf {
            calls: 0,
            stop_at: 3,
            certs: Arc::clone(&certs),
            certificate: identity_dir.join(CERT_FILE),
            held: Arc::clone(&held),
            stop: stop.clone(),
        }),
        store: Box::new(PkiDir::new(&root.join("pki"))),
        claims: Box::new(LinuxClaims::new(&claims, &claims, &claims, &claims)),
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(hostkeeper_link::testing::FakeClock::new(0, 0)),
        managed: None,
    };
    let settings = EnrollSettings {
        ca_sha256: pin(),
        image: "image-1".to_owned(),
    };
    let group_file = root.join("group");
    let gid = rustix::process::getegid().as_raw();
    std::fs::write(&group_file, format!("{IDENTITY_GROUP}:x:{gid}:\n")).expect("the group file");
    let group = IdentityGroup::in_group_file(
        IDENTITY_GROUP,
        group_file,
        Box::new(FakeLaunchctl::new()),
        PlatformKind::Linux,
    );
    let me = rustix::process::geteuid().as_raw();
    let log = Arc::new(RecordingLog::default());
    let (dispatch, jobs) = handed();
    let links = Links {
        stop: stop.clone(),
        identity: Some(HostIdentity::new(
            Arc::new(SeesNode {
                inner: certs,
                daemon: Arc::clone(&daemon),
                saw: Arc::clone(&saw),
            }),
            identity_dir.clone(),
            group,
            IdentityOwner { uid: me, gid },
        )),
        ..links_with(
            open_once(settings, ports),
            Arc::new(NotedCerts(Log::default())),
            Arc::clone(&log),
            dispatch,
        )
    };
    links.start(&daemon);
    let started = Ending::default();
    started.over("links.start returned");
    let enrollment = jobs.wait(&started).expect("the enrollment loop");
    enrollment();
    let notes = held.lock().expect("the notes").clone();
    let node_held = saw.lock().expect("the notes").clone();
    Ran {
        notes,
        lines: log.lines(),
        dir: identity_dir,
        node_held,
        _rig: rig,
    }
}

/// Catches an identity copy that is not made after every step of the enrollment loop (only at
/// the end, only at the first step, or never): the copy of the pair of step `k` is in place by the
/// time step `k + 1` begins, and the last step's pair is there when the loop ends. The copy is
/// logged once.
#[test]
fn the_enrollment_loop_makes_the_identity_copy_after_each_step() {
    let Ran {
        notes,
        lines,
        dir,
        _rig,
        ..
    } = three_steps("links-identity", |_| {});
    assert_eq!(
        notes,
        [None, Some("cert 1".to_owned()), Some("cert 2".to_owned())]
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(CERT_FILE)).expect("host.crt"),
        "cert 3"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(KEY_FILE)).expect("host.key"),
        "key 3"
    );
    let down = "the coordinator could not be reached: down".to_owned();
    assert_eq!(lines, [down, format!("{IDENTITY_KEPT}{}", dir.display())]);
}

/// Catches an identity copy made before the node holds the step's enrollment, which would let a
/// slow group tool delay what the node shows: the node holds an enrollment at every read the copy
/// makes of its source.
#[test]
fn the_node_holds_the_enrollment_before_the_identity_copy_is_made() {
    let ran = three_steps("links-identity-node", |_| {});
    assert_eq!(ran.node_held, [true, true, true]);
}

/// Catches an identity copy whose failure reaches the enroller: the identity directory is a link,
/// which the copy refuses, and the loop still makes its three steps; the failure is logged once.
#[test]
fn a_failed_identity_copy_does_not_stop_the_enrollment_loop() {
    let Ran {
        notes,
        lines,
        dir,
        _rig,
        ..
    } = three_steps("links-identity-fail", |dir| {
        let elsewhere = dir.with_file_name("elsewhere");
        std::fs::create_dir(&elsewhere).expect("the target");
        std::os::unix::fs::symlink(&elsewhere, dir).expect("the link");
    });
    assert_eq!(notes.len(), 3, "three steps were made");
    let down = "the coordinator could not be reached: down".to_owned();
    assert_eq!(
        lines,
        [
            down,
            format!("{IDENTITY_NOT_KEPT}{} is not a directory", dir.display())
        ]
    );
}

/// A daemon config's coordinator address that `GrpcEnroll` does not dial.
const NO_ENDPOINT: &str = "https://coordinator.example/a/path";

/// Catches a host whose enroller is made from something else than its daemon config and its pki
/// directory, or whose refusals are lost: the enroller of the config's coordinator over a pki
/// directory that can be made opens; an address with a path, and a pki directory under a file,
/// are refused naming what refused.
#[test]
fn the_host_enroller_is_made_from_the_config_and_the_pki_directory() {
    let rig = Rig::new("links-host-enroller");
    let settings = || EnrollSettings {
        ca_sha256: pin(),
        image: image_of(OWN),
    };
    let pki = rig.scratch.path().join("pki");
    let enroller = host_enroller(
        "https://coordinator.example:8443",
        &pki,
        settings(),
        PlatformKind::Linux,
    );
    assert!(enroller.is_ok(), "{:?}", enroller.err());
    assert!(pki.join("SCHEMA").is_file(), "the pki directory was opened");
    drop(enroller);

    let refused = host_enroller(NO_ENDPOINT, &pki, settings(), PlatformKind::Linux)
        .err()
        .expect("an address with a path");
    assert!(refused.contains(NO_ENDPOINT), "{refused}");

    let file = rig.scratch.path().join("a-file");
    std::fs::write(&file, "x").expect("a file");
    let refused = host_enroller(
        "https://coordinator.example",
        &file.join("pki"),
        settings(),
        PlatformKind::Macos,
    )
    .err()
    .expect("a pki directory under a file");
    assert!(!refused.is_empty());
    assert_eq!(image_of(OWN), "hostkeeper-0.1.0");
}

fn files_under(root: &Path) -> HostFiles {
    HostFiles {
        repositories: None,
        os_root: root.join("os"),
        stores: root.join("stores.toml"),
        macos_pkg_signers: None,
        pki: root.join("pki"),
        console: None,
        managed_config: None,
        managed_stores: None,
        managed_macos_pkg: None,
    }
}

/// Catches the host's links built from something else than the pki directory, the flag and the
/// daemon: the certificate is read under the pki directory's `current` link; the period is the
/// one given; the wait is the daemon's clock and ends the loop once it is told to stop; the
/// lines go through the daemon's alert sink; and the enroller is the config's. The heartbeat
/// link is tested against a real coordinator in `tests/coordinator_link.rs`.
#[test]
fn the_host_links_read_the_pki_directory_and_log_through_the_daemon() {
    let rig = Rig::new("links-host");
    // The state directory the daemon's start makes, where the host's links keep its health.
    std::fs::create_dir_all(&rig.state).expect("the state directory");
    let (daemon, alerts) = alerting(rig.daemon());
    let daemon = Arc::new(daemon);
    let files = files_under(rig.scratch.path());
    let period = NonZeroU64::new(7).expect("not zero");
    let links = host_links(
        &daemon,
        &files,
        period,
        PlatformKind::Linux,
        hostkeeper_daemon::maintenance_collect::LinuxScope::default(),
        hostkeeper_daemon::maintenance_softwareupdate::MacosMajor::default(),
        Box::new(NoWatchdog),
    );
    assert_eq!(links.period, period);
    assert_eq!(
        links.certs.current(),
        CertState::NotEnrolled {
            waiting_for: files.pki.join("current"),
        }
    );
    assert!(links.clock.unix_secs() > 0);
    assert!(links.identity.is_some(), "the host keeps the identity copy");
    let before = rig.clock.mono_secs();
    assert_eq!(links.sleeper.sleep(5), Tick::Beat);
    assert_eq!(
        rig.clock.mono_secs(),
        before + 5,
        "the wait is on the daemon's clock"
    );
    links.stop.stop();
    assert_eq!(links.sleeper.sleep(5), Tick::Stop);
    links.log.line("a line of a loop");
    assert_eq!(lines(&alerts), ["hostkeeper daemon: a line of a loop"]);
    let mut make = links.enroller;
    let enroller = make();
    assert!(enroller.is_ok(), "{:?}", enroller.err());
}

/// Catches the deferrals' clock read from somewhere else than the wall clock: the unix clock is
/// past the epoch.
#[test]
fn the_unix_clock_is_the_wall_clock() {
    assert!(SystemUnixClock.unix_secs() > 0);
}
