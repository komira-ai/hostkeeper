//! The watchdog notice (`hostkeeper_daemon::notify::Watchdog`): systemd's sends `WATCHDOG=1` to
//! the notify socket only when systemd set `WATCHDOG_USEC`; the daemon has it only with
//! `--notify-systemd`; and the heartbeat loop sends it after each heartbeat and nowhere else
//! (`hostkeeper_daemon::links`), so a heartbeat that does not return stops the notices.

mod support;

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use clap::Parser;
use hostkeeper_daemon::cli::{self, Cli, Command, SystemdVars};
use hostkeeper_daemon::links::{Links, MakeEnroller, NO_ENROLLMENT, NO_WATCHDOG_NOTICE};
use hostkeeper_daemon::testing::{FakeNotifier, Log};
use hostkeeper_daemon::{NoWatchdog, SystemdWatchdog, Watchdog, start};
use hostkeeper_link::testing::{FakeClock, FakeLink, RecordingLog, ScriptedSleeper};
use hostkeeper_link::{CertSource, CertState, DEFAULT_HEARTBEAT_SECS, Dispatch, Job, Sleeper};
use hostkeeper_link::{StopFlag, Tick};
use support::{OWN, Rig, lines};

/// A value of `WATCHDOG_USEC` as systemd sets it: 60 seconds.
const USEC: &str = "60000000";

/// What the service manager is told when a notice cannot be sent, in these tests.
const GONE: &str = "the service manager is gone";

fn push(events: &Log, event: &str) {
    events.lock().expect("the events").push(event.to_owned());
}

/// A watchdog that notes each notice in `events`, and fails each one when `fails`.
struct Recorder {
    events: Log,
    fails: bool,
}

impl Watchdog for Recorder {
    fn alive(&self) -> io::Result<()> {
        push(&self.events, "watchdog");
        if self.fails {
            return Err(io::Error::other(GONE));
        }
        Ok(())
    }
}

fn recorder(events: &Log) -> Box<Recorder> {
    Box::new(Recorder {
        events: events.clone(),
        fails: false,
    })
}

/// The certificate source of a host not enrolled, which notes in `events` that a heartbeat
/// asked for the certificate.
struct NotedCerts(Log);

impl CertSource for NotedCerts {
    fn current(&self) -> CertState {
        push(&self.0, "heartbeat");
        CertState::NotEnrolled {
            waiting_for: PathBuf::from("/pki/current"),
        }
    }
}

/// A sleeper that notes each wait in `events` and lets the loop beat `beats` more times.
struct NotedSleeper {
    events: Log,
    beats: Mutex<u32>,
}

impl Sleeper for NotedSleeper {
    fn sleep(&self, secs: u64) -> Tick {
        push(&self.events, &format!("sleep {secs}"));
        let mut left = self.beats.lock().expect("the beats");
        match left.checked_sub(1) {
            Some(next) => {
                *left = next;
                Tick::Beat
            }
            None => Tick::Stop,
        }
    }
}

/// A dispatcher that hands each job to the test, which runs it when and where it likes.
struct Handed(Mutex<Sender<Job>>);

impl Dispatch for Handed {
    fn start(&self, job: Job) {
        let sent = self.0.lock().expect("the sender").send(job);
        assert!(sent.is_ok(), "the test takes every job");
    }
}

/// Links over fakes, each loop handed to the test: the heartbeat loop asks `certs`, waits on
/// `sleeper` and tells `watchdog`.
fn links(
    enroller: MakeEnroller,
    certs: Arc<dyn CertSource>,
    sleeper: Box<dyn Sleeper + Send>,
    watchdog: Box<dyn Watchdog>,
    log: Arc<RecordingLog>,
) -> (Links, Receiver<Job>) {
    let (sender, jobs) = mpsc::channel();
    let links = Links {
        enroller,
        link: Arc::new(FakeLink::default()),
        certs,
        clock: Arc::new(FakeClock::new(0, 0)),
        log,
        sleeper,
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: Arc::new(Handed(Mutex::new(sender))),
        stop: StopFlag::default(),
        watchdog,
    };
    (links, jobs)
}

fn no_enroller() -> MakeEnroller {
    Box::new(|| Err("no coordinator in this test".to_owned()))
}

/// Catches a watchdog fed from anywhere but the heartbeat (the start pass, the readiness notice,
/// the enrollment loop or the admin server, any of which goes on when the heartbeat hangs), a
/// notice before the heartbeat it follows instead of after it, a heartbeat with no notice or
/// with two, or a wait other than the heartbeat period: through the daemon's start and `serve`,
/// nothing but the heartbeat loop sends a notice, and three heartbeats send three, each after its
/// heartbeat and before the wait.
#[test]
fn each_heartbeat_is_followed_by_one_watchdog_notice_and_nothing_else_sends_one() {
    let rig = Rig::new("watchdog-beats");
    let events = Log::default();
    let notifier = FakeNotifier::new(events.clone());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    let daemon = Arc::clone(started.daemon());
    let noted = events.clone();
    let enroller: MakeEnroller = Box::new(move || {
        push(&noted, "enrollment loop");
        Err("no coordinator in this test".to_owned())
    });
    let sleeper = NotedSleeper {
        events: events.clone(),
        beats: Mutex::new(2),
    };
    let log = Arc::new(RecordingLog::default());
    let (links, jobs) = links(
        enroller,
        Arc::new(NotedCerts(events.clone())),
        Box::new(sleeper),
        recorder(&events),
        Arc::clone(&log),
    );
    assert_eq!(
        lines(&events),
        ["ready"],
        "the start sends no watchdog notice"
    );

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let served = runtime.spawn(start::serve(started, links));
    let enrollment = jobs.recv().expect("the enrollment loop");
    let heartbeat = jobs.recv().expect("the heartbeat loop");
    enrollment();
    assert_eq!(
        lines(&events),
        ["ready", "enrollment loop"],
        "neither serving nor the enrollment loop sends a watchdog notice"
    );
    heartbeat();
    let period = format!("sleep {DEFAULT_HEARTBEAT_SECS}");
    let mut want = vec!["ready".to_owned(), "enrollment loop".to_owned()];
    for _ in 0..3 {
        want.extend([
            "heartbeat".to_owned(),
            "watchdog".to_owned(),
            period.clone(),
        ]);
    }
    assert_eq!(lines(&events), want);

    daemon.ctx().restart().request();
    let ended = runtime.block_on(served).expect("the server task");
    assert!(ended.is_ok(), "{ended:?}");
    assert_eq!(lines(&events), want, "stopping sends no watchdog notice");
    assert_eq!(
        log.lines(),
        [
            format!("{NO_ENROLLMENT}no coordinator in this test"),
            "not enrolled: waiting for /pki/current".to_owned(),
        ]
    );
}

/// The certificate source of a heartbeat that hangs: it says it was asked, then waits for the
/// test to let it go.
struct Hung {
    entered: Mutex<Sender<()>>,
    release: Mutex<Receiver<()>>,
}

impl CertSource for Hung {
    fn current(&self) -> CertState {
        let told = self.entered.lock().expect("the sender").send(());
        assert!(told.is_ok(), "the test waits for the heartbeat");
        let released = self.release.lock().expect("the receiver").recv();
        assert!(released.is_ok(), "the test lets the heartbeat go");
        CertState::NotEnrolled {
            waiting_for: PathBuf::from("/pki/current"),
        }
    }
}

/// Catches a watchdog notice sent while a heartbeat has not returned (from a thread or timer of
/// its own, or before the heartbeat's work): while the heartbeat hangs no notice is sent, and
/// the one notice comes once it returns.
#[test]
fn a_heartbeat_that_has_not_returned_sends_no_watchdog_notice() {
    let rig = Rig::new("watchdog-hung");
    let daemon = Arc::new(rig.daemon());
    let events = Log::default();
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let hung = Hung {
        entered: Mutex::new(entered_tx),
        release: Mutex::new(release_rx),
    };
    let (links, jobs) = links(
        no_enroller(),
        Arc::new(hung),
        Box::new(ScriptedSleeper::new(0)),
        recorder(&events),
        Arc::new(RecordingLog::default()),
    );
    links.start(&daemon);
    let _enrollment = jobs.recv().expect("the enrollment loop");
    let heartbeat = jobs.recv().expect("the heartbeat loop");
    let beating = std::thread::spawn(heartbeat);
    entered.recv().expect("the heartbeat began");
    assert_eq!(
        lines(&events),
        Vec::<String>::new(),
        "no notice while the heartbeat hangs"
    );
    release.send(()).expect("the heartbeat waits");
    beating.join().expect("the heartbeat loop ends");
    assert_eq!(lines(&events), ["watchdog"]);
}

/// Catches a notice that cannot be sent ending the heartbeat loop, or lost without a word: each
/// failed notice is logged, as the line names it, and the loop beats on.
#[test]
fn a_watchdog_notice_that_fails_is_logged_and_the_loop_beats_on() {
    let rig = Rig::new("watchdog-fails");
    let daemon = Arc::new(rig.daemon());
    let events = Log::default();
    let log = Arc::new(RecordingLog::default());
    let (links, jobs) = links(
        no_enroller(),
        Arc::new(NotedCerts(events.clone())),
        Box::new(ScriptedSleeper::new(1)),
        Box::new(Recorder {
            events: events.clone(),
            fails: true,
        }),
        Arc::clone(&log),
    );
    links.start(&daemon);
    let _enrollment = jobs.recv().expect("the enrollment loop");
    let heartbeat = jobs.recv().expect("the heartbeat loop");
    heartbeat();
    assert_eq!(
        lines(&events),
        ["heartbeat", "watchdog", "heartbeat", "watchdog"]
    );
    assert_eq!(
        log.lines(),
        [
            "not enrolled: waiting for /pki/current".to_owned(),
            format!("{NO_WATCHDOG_NOTICE}{GONE}"),
            format!("{NO_WATCHDOG_NOTICE}{GONE}"),
        ]
    );
    assert_eq!(
        NO_WATCHDOG_NOTICE,
        "error: the watchdog notice was not sent: "
    );
}

/// A datagram socket bound at `name` in the rig's scratch directory, which never blocks.
fn bound(rig: &Rig, name: &str) -> (UnixDatagram, OsString) {
    let path = rig.scratch.path().join(name);
    let socket = UnixDatagram::bind(&path).expect("bind the socket");
    socket
        .set_nonblocking(true)
        .expect("a socket that never blocks");
    (socket, path.into_os_string())
}

/// The one datagram waiting on `socket`, or `None` when nothing was sent to it.
fn received(socket: &UnixDatagram) -> Option<Vec<u8>> {
    let mut buf = [0u8; 64];
    match socket.recv(&mut buf) {
        Ok(n) => Some(buf[..n].to_vec()),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => None,
        Err(e) => panic!("the socket failed: {e}"),
    }
}

/// Catches a notice that does not reach systemd, reaches it as anything but `WATCHDOG=1`, or is
/// sent twice: with `WATCHDOG_USEC` set, the notice is exactly `WATCHDOG=1`, in one datagram to
/// the socket at the path it is given; a path where no socket listens is an error.
#[test]
fn with_watchdog_usec_the_systemd_watchdog_writes_watchdog_to_the_notify_socket() {
    let rig = Rig::new("watchdog-notify");
    let (socket, path) = bound(&rig, "n.sock");
    let watchdog = SystemdWatchdog::new(path, Some(OsStr::new(USEC)));
    watchdog.alive().expect("the notice");
    assert_eq!(received(&socket), Some(b"WATCHDOG=1".to_vec()));
    assert_eq!(received(&socket), None, "one datagram per notice");

    let nobody = SystemdWatchdog::new(
        rig.scratch.path().join("nobody.sock"),
        Some(OsStr::new(USEC)),
    );
    assert!(nobody.alive().is_err());
}

/// Catches a notice sent when systemd runs no watchdog for the daemon, or such a notice taken
/// for an error: without `WATCHDOG_USEC` the notice sends nothing and is `Ok`, to a socket that
/// listens and to a path where none does; [`NoWatchdog`] is `Ok` too.
#[test]
fn without_watchdog_usec_the_systemd_watchdog_sends_nothing() {
    let rig = Rig::new("watchdog-unset");
    let (socket, path) = bound(&rig, "n.sock");
    assert!(SystemdWatchdog::new(path, None).alive().is_ok());
    assert_eq!(received(&socket), None);
    let nobody = SystemdWatchdog::new(rig.scratch.path().join("nobody.sock"), None);
    assert!(nobody.alive().is_ok());
    assert!(NoWatchdog.alive().is_ok());
}

fn daemon_flags(args: &[&str]) -> hostkeeper_daemon::cli::DaemonFlags {
    let all = ["hostkeeper", "daemon"].iter().chain(args);
    let Command::Daemon(flags) = Cli::parse_from(all).command else {
        panic!("`hostkeeper daemon` parses as the daemon command");
    };
    flags
}

/// Catches the daemon's watchdog chosen from anything but `--notify-systemd` and what systemd
/// set: with the flag, the notify socket and `WATCHDOG_USEC`, the notice reaches the socket;
/// without `WATCHDOG_USEC`, or without the flag, nothing is sent; without a notify socket the
/// watchdog sends nowhere (the readiness notifier refuses that start).
#[test]
fn the_daemon_watchdog_needs_the_flag_the_socket_and_watchdog_usec() {
    let rig = Rig::new("watchdog-cli");
    let (socket, path) = bound(&rig, "n.sock");
    let flagged = daemon_flags(&["--notify-systemd"]);
    let plain = daemon_flags(&[]);
    let vars = |socket: Option<&OsString>, usec: Option<&str>| SystemdVars {
        notify_socket: socket.cloned(),
        watchdog_usec: usec.map(OsString::from),
    };
    let watched = vars(Some(&path), Some(USEC));

    cli::watchdog(&flagged, &watched)
        .alive()
        .expect("the notice");
    assert_eq!(received(&socket), Some(b"WATCHDOG=1".to_vec()));

    let cases = [
        (&flagged, vars(Some(&path), None), "no WATCHDOG_USEC"),
        (&plain, watched.clone(), "no --notify-systemd"),
        (&flagged, vars(None, Some(USEC)), "no notify socket"),
    ];
    for (flags, systemd, case) in cases {
        assert!(cli::watchdog(flags, &systemd).alive().is_ok(), "{case}");
        assert_eq!(received(&socket), None, "{case}");
    }
    assert!(cli::notifier(&flagged, None).is_err());
}
