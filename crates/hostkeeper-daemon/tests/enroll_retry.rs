//! The enrollment loop's enroller that cannot be made (`hostkeeper_daemon::links`): it is made
//! again after a wait that doubles up to a bound, for as long as the loop runs; each failure is
//! logged once per reason and noted in the state every heartbeat reports, and the heartbeat
//! reports what the enroller then notes.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_daemon::link_adapters::DaemonRenewal;
use hostkeeper_daemon::links::{
    Links, MakeEnroller, NO_ENROLLMENT, OPEN_RETRY_FIRST_SECS, OPEN_RETRY_MAX_SECS,
};
use hostkeeper_enroll::{
    CoordinatorEnroll, EnrollAnswer, EnrollRequest, LinuxClaims, OsEntropy, PkiDir, Poll,
    PollAnswer, RenewAnswer, RenewRequest, Renewal, RenewalState, Settings as EnrollSettings,
    SystemWallClock, TransportError,
};
use hostkeeper_link::testing::{FakeClock, ManualDispatch, RecordingLog, ScriptedSleeper};
use hostkeeper_link::{RenewalSource, Sleeper, StopFlag, Tick};
use support::{Rig, idle_links, open_once};

/// A [`ScriptedSleeper`] the test keeps a hold of, to read the waits the loop asked for.
struct Shared(Arc<ScriptedSleeper>);

impl Sleeper for Shared {
    fn sleep(&self, secs: u64) -> Tick {
        self.0.sleep(secs)
    }
}

/// A coordinator that cannot be reached, and that tells the loop to stop the first time it is
/// asked anything.
struct Down(StopFlag);

impl CoordinatorEnroll for Down {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        self.0.stop();
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

/// An enroller over a coordinator that is down and stops the loop at its first step.
fn down_enroller(rig: &Rig, stop: &StopFlag) -> MakeEnroller {
    let claims = rig.scratch.path().join("no-claims");
    let ports = hostkeeper_enroll::Ports {
        coordinator: Box::new(Down(stop.clone())),
        store: Box::new(PkiDir::new(&rig.scratch.path().join("pki"))),
        claims: Box::new(LinuxClaims::new(&claims, &claims, &claims, &claims)),
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(FakeClock::new(0, 0)),
    };
    let settings = EnrollSettings {
        ca_sha256: Sha256Hex::parse(&"00".repeat(32)).expect("a SHA-256"),
        image: "image-1".to_owned(),
    };
    open_once(settings, ports)
}

/// The reason the `n`th try (from 0) fails: the first four one reason, the next four another.
fn reason(n: usize) -> &'static str {
    if n < 4 {
        "the pki directory is locked"
    } else {
        "the coordinator's address is refused"
    }
}

/// Catches an enrollment loop that ends at its first failure to make the enroller, that tries
/// again at once or after a fixed wait, whose wait grows without bound, that logs every failure,
/// or whose failures the heartbeat cannot see: eight tries fail, each after the wait before it
/// (5, 10, 20, 40, 80, 160, then 300 twice: doubling, and capped), the ninth makes the
/// enroller, which runs its step; each reason is logged once, each failure is in the state by
/// the next try, and the enroller's own step error replaces it.
#[test]
fn an_enroller_that_cannot_be_made_is_made_again_after_a_doubling_capped_wait() {
    const FAILURES: usize = 8;
    let rig = Rig::new("enroll-retry");
    let daemon = Arc::new(rig.daemon());
    let stop = StopFlag::default();
    let renewal = RenewalState::default();
    let tries = Arc::new(AtomicUsize::new(0));
    let noted: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let mut enroller = Some(down_enroller(&rig, &stop));
    let (counted, seen, state) = (tries.clone(), noted.clone(), renewal.clone());
    let make: MakeEnroller = Box::new(move || {
        let n = counted.fetch_add(1, Ordering::SeqCst);
        seen.lock().expect("noted").push(state.get().last_error);
        if n < FAILURES {
            return Err(reason(n).to_owned());
        }
        let mut open = enroller.take().ok_or("made twice")?;
        open()
    });
    let sleeper = Arc::new(ScriptedSleeper::new(FAILURES as u64));
    let log = Arc::new(RecordingLog::default());
    let dispatch = Arc::new(ManualDispatch::default());
    let links = Links {
        enroller: make,
        enroll_sleeper: Box::new(Shared(sleeper.clone())),
        renewal: renewal.clone(),
        log: log.clone(),
        dispatch: dispatch.clone(),
        stop: stop.clone(),
        ..idle_links()
    };
    links.start(&daemon);
    assert!(dispatch.run_next(), "the enrollment loop");

    assert_eq!(tries.load(Ordering::SeqCst), FAILURES + 1);
    assert_eq!(sleeper.waits(), [5, 10, 20, 40, 80, 160, 300, 300]);
    assert_eq!(OPEN_RETRY_FIRST_SECS, 5);
    assert_eq!(OPEN_RETRY_MAX_SECS, 300);
    let before: Vec<Option<String>> = (0..=FAILURES)
        .map(|n| {
            n.checked_sub(1)
                .map(|last| format!("the enroller cannot be opened: {}", reason(last)))
        })
        .collect();
    assert_eq!(*noted.lock().expect("noted"), before);
    assert_eq!(
        log.lines(),
        [
            format!("{NO_ENROLLMENT}the pki directory is locked"),
            format!("{NO_ENROLLMENT}the coordinator's address is refused"),
            "the coordinator could not be reached: down".to_owned(),
        ]
    );
    assert_eq!(
        renewal.get(),
        Renewal {
            not_after: None,
            last_error: Some("the coordinator could not be reached: down".to_owned()),
        }
    );
    assert!(stop.is_stopped());
}

/// Catches a loop that waits out a stop: told to stop at the end of its first wait, it makes no
/// second try; told to stop before it starts, it makes none.
#[test]
fn a_stopped_enrollment_loop_makes_no_more_tries() {
    for (stopped_first, expected_tries) in [(false, 1), (true, 0)] {
        let rig = Rig::new("enroll-retry-stop");
        let daemon = Arc::new(rig.daemon());
        let stop = StopFlag::default();
        if stopped_first {
            stop.stop();
        }
        let tries = Arc::new(AtomicUsize::new(0));
        let counted = tries.clone();
        let renewal = RenewalState::default();
        let dispatch = Arc::new(ManualDispatch::default());
        let links = Links {
            enroller: Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                Err("no coordinator".to_owned())
            }),
            enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
            renewal: renewal.clone(),
            dispatch: dispatch.clone(),
            stop,
            ..idle_links()
        };
        links.start(&daemon);
        assert!(dispatch.run_next(), "the enrollment loop");
        assert_eq!(tries.load(Ordering::SeqCst), expected_tries);
        let error = renewal.get().last_error;
        if stopped_first {
            assert_eq!(error, None, "nothing was tried");
        } else {
            assert_eq!(
                error.as_deref(),
                Some("the enroller cannot be opened: no coordinator")
            );
        }
    }
}

/// Catches the heartbeat reading something else than the enrollment loop's state: what the
/// state holds, with and without an error and a certificate, is what the heartbeat's port says.
#[test]
fn the_heartbeat_reports_what_the_enrollment_loop_noted() {
    let state = RenewalState::default();
    let port = DaemonRenewal::new(state.clone());
    assert_eq!(port.renewal(), hostkeeper_link::Renewal::default());
    state.failed("the enroller cannot be opened: locked".to_owned());
    assert_eq!(
        port.renewal(),
        hostkeeper_link::Renewal {
            cert_not_after_unix: None,
            last_renew_error: Some("the enroller cannot be opened: locked".to_owned()),
        }
    );
}
