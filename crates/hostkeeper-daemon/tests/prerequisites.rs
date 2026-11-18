//! The host prerequisites the coordinator sends, through the daemon's heartbeat loop
//! (`hostkeeper_daemon::links`, `hostkeeper_daemon::prerequisites`): a daemon whose links keep
//! prerequisites announces `prerequisites-v1`, reports what it holds in every heartbeat, and makes
//! sure of the set of a reply that advertises the feature (here over scripted programs, never the
//! host's) before the beat that reports it.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::executor::Daemon;
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::prerequisites::Prerequisites;
use hostkeeper_daemon::xcode_clt::{CommandLineTools, DEVELOPER_DIR};
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, FakeLink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    BeatOutcome, CertState, DEFAULT_HEARTBEAT_SECS, Dispatch, Feature, Heartbeater, Job,
    PrerequisiteStatus, PrerequisitesRefusal, PrerequisitesReport, PrerequisitesSink,
    PushedPrerequisites, Reply, StopFlag,
};
use hostkeeper_wire::prerequisites::digest;
use support::{OWN, Rig, config};

const NAME: &str = "xcode-command-line-tools";
const VERSION: &str = "15.3.0.0.1.1708646388";

/// A dispatcher that runs each job where it is started.
struct Inline;

impl Dispatch for Inline {
    fn start(&self, job: Job) {
        job();
    }
}

fn set(names: &[&str]) -> PushedPrerequisites {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    PushedPrerequisites {
        digest: digest(&names),
        names,
    }
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// A Mac that has the tools, over scripted programs, and the fake behind `pkgutil`.
fn mac(marker: PathBuf) -> (Arc<Prerequisites>, FakeLaunchctl) {
    let pkgutil = FakeLaunchctl::new();
    pkgutil.always(
        &words(&["--pkg-info=com.apple.pkg.CLTools_Executables"]),
        Ok(LaunchctlOutput::success(&format!("version: {VERSION}\n"))),
    );
    let select = FakeLaunchctl::new();
    select.always(&words(&["-p"]), Ok(LaunchctlOutput::success(DEVELOPER_DIR)));
    let none = FakeLaunchctl::new();
    let tools = CommandLineTools::new(
        Box::new(pkgutil.clone()),
        Box::new(select),
        Box::new(none.clone()),
        Box::new(none),
        marker,
    );
    let sink = Prerequisites::new(
        Some(tools),
        Arc::new(FakeClock::new(2_000_000_000, 7)),
        Arc::new(Inline),
        Arc::new(RecordingLog::default()),
    );
    (Arc::new(sink), pkgutil)
}

/// A heartbeat loop over a daemon on `rig` whose links hold `prerequisites`, and its link.
fn host(
    rig: &Rig,
    prerequisites: Option<Arc<dyn PrerequisitesSink>>,
) -> (Heartbeater, Arc<FakeLink>) {
    let daemon = Arc::new(Daemon::new(config(), rig.dirs(), rig.ports(), OWN));
    let link = Arc::new(FakeLink::default());
    let links = Links {
        enroller: Box::new(|| Err("no enrollment here".to_owned())),
        link: link.clone(),
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("host-1")))),
        clock: Arc::new(FakeClock::new(2_000_000_000, 7)),
        log: Arc::new(RecordingLog::default()),
        sleeper: Box::new(ScriptedSleeper::new(0)),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: Arc::new(ManualDispatch::default()),
        stop: StopFlag::default(),
        enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
        renewal: hostkeeper_enroll::RenewalState::default(),
        watchdog: Box::new(hostkeeper_daemon::NoWatchdog),
        console: None,
        remote_login_probe: None,
        operator_keys: None,
        prerequisites,
        identity: None,
    };
    (links.heartbeater(&daemon), link)
}

fn answer_with(link: &FakeLink, held: &PushedPrerequisites, beats: usize) {
    for _ in 0..beats {
        link.answer(Ok(Reply {
            features: vec![Feature::PrerequisitesV1],
            prerequisites: Some(held.clone()),
            ..Reply::default()
        }));
    }
}

/// Proves a daemon whose links keep prerequisites announces `prerequisites-v1` and reports an
/// empty report before it was sent a set, makes sure of the set of a reply that advertises the
/// feature, and reports the digest and the tools present, with their version, in the next
/// heartbeats.
#[test]
fn a_host_makes_sure_of_the_set_of_a_reply_and_reports_it() {
    let rig = Rig::new("prerequisites-daemon");
    let (sink, pkgutil) = mac(rig.scratch.path().join("marker"));
    let (mut heartbeater, link) = host(&rig, Some(sink));
    let held = set(&[NAME]);
    answer_with(&link, &held, 2);
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    let beats = link.beats();
    assert!(beats[0].features.contains(&Feature::PrerequisitesV1));
    assert_eq!(beats[0].prerequisites, Some(PrerequisitesReport::default()));
    for beat in &beats[1..] {
        let report = beat.prerequisites.as_ref().expect("a report");
        assert_eq!(report.digest, held.digest);
        assert_eq!(report.states.len(), 1);
        assert_eq!(report.states[0].name, NAME);
        assert_eq!(report.states[0].status, PrerequisiteStatus::Present);
        assert_eq!(report.states[0].version, VERSION);
    }
    assert_eq!(pkgutil.calls().len(), 1);
}

/// Proves a Linux host's sink announces the feature, holds the set and reports each name not
/// applicable, and a set it refuses is reported with the digest and the reason in the next beat.
#[test]
fn a_linux_host_reports_not_applicable_and_refuses_a_broken_set() {
    let rig = Rig::new("prerequisites-linux");
    let sink = Prerequisites::new(
        None,
        Arc::new(FakeClock::new(2_000_000_000, 7)),
        Arc::new(Inline),
        Arc::new(RecordingLog::default()),
    );
    let (mut heartbeater, link) = host(&rig, Some(Arc::new(sink)));
    let held = set(&[NAME]);
    answer_with(&link, &held, 1);
    let bad = PushedPrerequisites {
        digest: "f".repeat(64),
        names: vec!["only".to_owned()],
    };
    link.answer(Ok(Reply {
        features: vec![Feature::PrerequisitesV1],
        prerequisites: Some(bad.clone()),
        ..Reply::default()
    }));
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    let beats = link.beats();
    let report = beats[1].prerequisites.as_ref().expect("a report");
    assert_eq!(report.digest, held.digest);
    assert_eq!(report.states[0].status, PrerequisiteStatus::NotApplicable);
    let report = beats[2].prerequisites.as_ref().expect("a report");
    assert_eq!(report.digest, held.digest);
    assert_eq!(
        report.refused,
        Some(PrerequisitesRefusal {
            digest: bad.digest,
            reason: "the coordinator's digest is not the digest of its names".to_owned(),
        })
    );
}

/// Proves a daemon whose links keep no prerequisites does not announce the feature and reports
/// none, and a set it is sent is held nowhere.
#[test]
fn a_host_with_no_prerequisites_announces_nothing_of_them() {
    let rig = Rig::new("prerequisites-none");
    let (mut heartbeater, link) = host(&rig, None);
    answer_with(&link, &set(&[NAME]), 1);
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let beats = link.beats();
    assert!(!beats[0].features.contains(&Feature::PrerequisitesV1));
    assert_eq!(beats[0].prerequisites, None);
}
