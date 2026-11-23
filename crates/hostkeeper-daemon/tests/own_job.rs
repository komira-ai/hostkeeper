//! A Mac's daemon keeps its own launchd job enabled (`hostkeeper_daemon::own_job`): at its start
//! and after each heartbeat it reads launchd's override database, and a job marked disabled there
//! is enabled again and alerted. Everything runs against a scripted `launchctl`; Linux's adapters
//! have no such check and run no command.

mod support;

use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_daemon::executor::{AlertSink, Daemon};
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::own_job::{
    ALERT_OWN_JOB_CHECK, ALERT_OWN_JOB_DISABLED, OwnJob, OwnJobCheck,
};
use hostkeeper_daemon::start;
use hostkeeper_daemon::testing::{FakeNotifier, Log};
use hostkeeper_launchd::launchctl::{enable_args, print_disabled_args};
use hostkeeper_launchd::{FakeLaunchctl, Label, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::testing::{FakeCerts, FakeClock, FakeLink, ManualDispatch, ScriptedSleeper};
use hostkeeper_link::{CertState, Reply};
use support::{OWN, Rig, config, idle_links};

const DISABLED: &str = "disabled services = {\n\t\"dev.hostkeeper.hostkeeper\" => disabled\n}\n";
const ENABLED: &str = "disabled services = {\n\t\"dev.hostkeeper.hostkeeper\" => enabled\n}\n";
const NOT_LISTED: &str = "disabled services = {\n\t\"com.example.other\" => disabled\n}\n";

fn print_disabled() -> Vec<String> {
    print_disabled_args()
}

fn enable() -> Vec<String> {
    enable_args(&Label::daemon())
}

/// A `launchctl` that has printed `printed` once per queued answer, and enables the job.
fn launchctl(printed: &[&str]) -> FakeLaunchctl {
    let fake = FakeLaunchctl::new();
    for text in printed {
        fake.then(&print_disabled(), Ok(LaunchctlOutput::success(text)));
    }
    fake.always(&enable(), Ok(LaunchctlOutput::success("")));
    fake
}

/// The calls the fake saw, each as its arguments joined by spaces.
fn calls(fake: &FakeLaunchctl) -> Vec<String> {
    fake.calls().iter().map(|args| args.join(" ")).collect()
}

const PRINT: &str = "print-disabled system";
const ENABLE: &str = "enable system/dev.hostkeeper.hostkeeper";

/// The lines a daemon alerted, and the sink that collects them.
fn alerts() -> (AlertSink, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&lines);
    let sink: AlertSink = Arc::new(move |line: &str| {
        kept.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line.to_owned());
    });
    (sink, lines)
}

fn said(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    lines.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A daemon on `rig` that checks its job through `fake`, its alerts collected.
fn daemon(rig: &Rig, fake: &FakeLaunchctl) -> (Arc<Daemon>, Arc<Mutex<Vec<String>>>) {
    let mut ports = rig.ports();
    ports.own_job = Some(Arc::new(OwnJob::new(Arc::new(fake.clone()))));
    let (sink, lines) = alerts();
    let daemon = Daemon::new(config(), rig.dirs(), ports, OWN).with_alerts(sink);
    (Arc::new(daemon), lines)
}

/// Runs `beats` heartbeats of `daemon`'s loop, each answered, and returns the link's call count.
fn beat(daemon: &Arc<Daemon>, beats: u64) -> usize {
    let link = Arc::new(FakeLink::default());
    for _ in 0..beats {
        link.answer(Ok(Reply::default()));
    }
    let dispatch = Arc::new(ManualDispatch::default());
    let links = Links {
        link: link.clone(),
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(
            hostkeeper_link::testing::identity("a"),
        ))),
        clock: Arc::new(FakeClock::new(1_900_000_000, 0)),
        sleeper: Box::new(ScriptedSleeper::new(beats - 1)),
        dispatch: dispatch.clone(),
        ..idle_links()
    };
    links.start(daemon);
    assert!(dispatch.run_next(), "the enrollment loop");
    assert!(dispatch.run_next(), "the heartbeat loop");
    link.calls().len()
}

/// Proves: a start whose job is marked disabled runs `print-disabled` and then `enable` once, and
/// the boot report says the job was enabled again.
///
/// Catches: a Mac that starts with its job disabled and says nothing, and comes back from the
/// next reboot with no daemon.
#[test]
fn a_start_with_the_job_disabled_enables_it_once() {
    let rig = Rig::new("own-job-start-disabled");
    let fake = launchctl(&[DISABLED]);
    let mut ports = rig.ports();
    ports.own_job = Some(Arc::new(OwnJob::new(Arc::new(fake.clone()))));
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rig.settings(), ports, OWN, &*notifier)
        .expect("the daemon starts");
    assert_eq!(
        started.boot_report().own_job,
        Some(OwnJobCheck::EnabledAgain)
    );
    assert_eq!(calls(&fake), [PRINT, ENABLE]);
    assert_eq!(notifier.sent(), 1, "the start went on to its readiness");
}

/// Proves: a start whose job is enabled, or not listed in the database, runs `print-disabled`
/// alone: no `enable`.
#[test]
fn a_start_with_the_job_enabled_or_not_listed_only_reads_the_database() {
    for printed in [ENABLED, NOT_LISTED] {
        let rig = Rig::new("own-job-start-enabled");
        let fake = launchctl(&[printed]);
        let mut ports = rig.ports();
        ports.own_job = Some(Arc::new(OwnJob::new(Arc::new(fake.clone()))));
        let notifier = FakeNotifier::new(Log::default());
        let started = start::startup_notifying(&rig.settings(), ports, OWN, &*notifier)
            .expect("the daemon starts");
        assert_eq!(started.boot_report().own_job, Some(OwnJobCheck::Enabled));
        assert_eq!(calls(&fake), [PRINT], "{printed}");
    }
}

/// Proves: a start on a host with no check (Linux) records none in its boot report.
#[test]
fn a_start_without_the_check_records_none() {
    let rig = Rig::new("own-job-start-none");
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rig.settings(), rig.ports(), OWN, &*notifier)
        .expect("the daemon starts");
    assert_eq!(started.boot_report().own_job, None);
}

/// Proves: a start whose database cannot be read goes on to its readiness, the failure in its
/// boot report.
#[test]
fn a_start_whose_database_cannot_be_read_still_starts() {
    let rig = Rig::new("own-job-start-unread");
    let fake = FakeLaunchctl::new();
    fake.always(
        &print_disabled(),
        Ok(LaunchctlOutput::exit(1, "Operation not permitted")),
    );
    let mut ports = rig.ports();
    ports.own_job = Some(Arc::new(OwnJob::new(Arc::new(fake.clone()))));
    let notifier = FakeNotifier::new(Log::default());
    let started = start::startup_notifying(&rig.settings(), ports, OWN, &*notifier)
        .expect("the daemon starts");
    assert_eq!(
        started.boot_report().own_job,
        Some(OwnJobCheck::NotRead(
            "launchctl print-disabled exited with 1: Operation not permitted".to_owned()
        ))
    );
    assert_eq!(notifier.sent(), 1);
    assert_eq!(calls(&fake), [PRINT]);
}

/// Proves: with the job enabled, each of three heartbeats reads the database and runs nothing
/// else, and nothing is alerted.
#[test]
fn a_heartbeat_with_the_job_enabled_only_reads_the_database() {
    let rig = Rig::new("own-job-beat-enabled");
    let fake = launchctl(&[ENABLED, ENABLED, NOT_LISTED]);
    let (daemon, lines) = daemon(&rig, &fake);
    assert_eq!(beat(&daemon, 3), 3, "three heartbeats went out");
    assert_eq!(calls(&fake), [PRINT, PRINT, PRINT]);
    assert_eq!(said(&lines), Vec::<String>::new());
}

/// Proves: a job disabled from outside between two heartbeats is enabled again after the
/// second, with one alert naming the job; the third heartbeat finds it enabled and says nothing
/// more.
///
/// Catches: a check that runs at the start alone, which leaves a job disabled after the start
/// disabled until the next restart; and one that alerts at every heartbeat once it was disabled.
#[test]
fn a_job_disabled_after_the_start_is_enabled_again_and_alerted_once() {
    let rig = Rig::new("own-job-beat-disabled");
    let fake = launchctl(&[ENABLED, DISABLED, ENABLED]);
    let (daemon, lines) = daemon(&rig, &fake);
    assert_eq!(beat(&daemon, 3), 3);
    assert_eq!(calls(&fake), [PRINT, PRINT, ENABLE, PRINT]);
    assert_eq!(
        said(&lines),
        [format!(
            "hostkeeper daemon: level=error alert={ALERT_OWN_JOB_DISABLED} the launchd job \
             dev.hostkeeper.hostkeeper was marked disabled, so it would not have started after a \
             reboot; it was enabled again"
        )]
    );
}

/// Proves: a database that cannot be read is alerted at each heartbeat that fails, with the
/// failure, and every heartbeat still goes out: a `launchctl` that exits non-zero with text, one
/// that exits non-zero with none, one ended by a signal with text and one with none, one that
/// cannot start, one that times out, and output that is not UTF-8.
#[test]
fn a_database_that_cannot_be_read_is_alerted_and_the_beats_go_on() {
    let rig = Rig::new("own-job-beat-unread");
    let fake = FakeLaunchctl::new();
    let answers: [Result<LaunchctlOutput, LaunchctlError>; 7] = [
        Ok(LaunchctlOutput::exit(1, "Operation not permitted\n")),
        Ok(LaunchctlOutput::exit(2, "")),
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: b"killed".to_vec(),
        }),
        Ok(LaunchctlOutput {
            code: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }),
        Err(LaunchctlError::Spawn {
            args: PRINT.to_owned(),
            why: "no such file".to_owned(),
        }),
        Err(LaunchctlError::TimedOut {
            args: PRINT.to_owned(),
        }),
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: vec![0xff, 0xfe],
            stderr: Vec::new(),
        }),
    ];
    for answer in answers {
        fake.then(&print_disabled(), answer);
    }
    let (daemon, lines) = daemon(&rig, &fake);
    assert_eq!(beat(&daemon, 7), 7, "every heartbeat went out");
    assert_eq!(calls(&fake), [PRINT; 7]);
    let prefix = format!(
        "hostkeeper daemon: level=error alert={ALERT_OWN_JOB_CHECK} launchd's override database \
         could not be read, so it is not known whether the job dev.hostkeeper.hostkeeper is \
         disabled: "
    );
    let reasons = [
        "launchctl print-disabled exited with 1: Operation not permitted",
        "launchctl print-disabled exited with 2",
        "launchctl print-disabled was ended by a signal: killed",
        "launchctl print-disabled was ended by a signal",
        "cannot run launchctl print-disabled system: no such file",
        "launchctl print-disabled system did not finish within its deadline and was killed",
        "launchctl print-disabled wrote output that is not UTF-8",
    ];
    let expected: Vec<String> = reasons.iter().map(|why| format!("{prefix}{why}")).collect();
    assert_eq!(said(&lines), expected);
}

/// Proves: an `enable` that fails (exit 1 with text, then a `launchctl` that cannot start) is
/// alerted with the failure at the heartbeat that found the job disabled, the job is tried again
/// at the next heartbeat, and every heartbeat goes out.
#[test]
fn an_enable_that_fails_is_alerted_and_tried_again_at_the_next_heartbeat() {
    let rig = Rig::new("own-job-beat-enable-fails");
    let fake = FakeLaunchctl::new();
    fake.always(&print_disabled(), Ok(LaunchctlOutput::success(DISABLED)));
    fake.then(
        &enable(),
        Ok(LaunchctlOutput::exit(5, "Input/output error")),
    );
    fake.then(
        &enable(),
        Err(LaunchctlError::Spawn {
            args: ENABLE.to_owned(),
            why: "no such file".to_owned(),
        }),
    );
    fake.then(&enable(), Ok(LaunchctlOutput::success("")));
    let (daemon, lines) = daemon(&rig, &fake);
    assert_eq!(beat(&daemon, 3), 3, "every heartbeat went out");
    assert_eq!(calls(&fake), [PRINT, ENABLE, PRINT, ENABLE, PRINT, ENABLE]);
    let failed = |why: &str| {
        format!(
            "hostkeeper daemon: level=error alert={ALERT_OWN_JOB_DISABLED} the launchd job \
             dev.hostkeeper.hostkeeper is marked disabled, so it will not start after a reboot, \
             and it could not be enabled: {why}"
        )
    };
    assert_eq!(
        said(&lines),
        [
            failed("launchctl enable exited with 5: Input/output error"),
            failed("cannot run launchctl enable system/dev.hostkeeper.hostkeeper: no such file"),
            "hostkeeper daemon: level=error alert=own_job_disabled the launchd job \
             dev.hostkeeper.hostkeeper was marked disabled, so it would not have started after a \
             reboot; it was enabled again"
                .to_owned(),
        ]
    );
}

/// Proves: a daemon whose ports have no check (Linux) runs the heartbeat loop with no check:
/// `check_own_job` is `None`, and nothing is alerted.
#[test]
fn a_daemon_with_no_check_runs_no_command_and_alerts_nothing() {
    let rig = Rig::new("own-job-none");
    let (sink, lines) = alerts();
    let daemon = Arc::new(rig.daemon().with_alerts(sink));
    assert_eq!(beat(&daemon, 2), 2);
    assert_eq!(daemon.ctx().check_own_job(), None);
    assert_eq!(said(&lines), Vec::<String>::new());
}
