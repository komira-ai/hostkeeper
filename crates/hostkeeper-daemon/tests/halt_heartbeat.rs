//! The halt generations of a coordinator's reply, through the daemon's own heartbeat loop
//! (`hostkeeper_daemon::links`): the coordinator sends every service's generation to every host
//! after any clear, and a host that holds no halt of a service keeps no `held-halt` file of it.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::executor::Daemon;
use hostkeeper_daemon::halt::{HELD_HALT_FILE, HaltStore};
use hostkeeper_daemon::links::Links;
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, FakeLink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    BeatOutcome, CertState, DEFAULT_HEARTBEAT_SECS, Feature, Heartbeater, Reply, StopFlag,
};
use support::{OWN, Rig, config};

fn service(name: &str) -> ServiceName {
    ServiceName::new(name).expect("a name")
}

/// A heartbeat loop over a daemon on `rig`, its link, and the lines it logs.
fn host(rig: &Rig) -> (Heartbeater, Arc<FakeLink>, Arc<RecordingLog>) {
    let daemon = Arc::new(Daemon::new(config(), rig.dirs(), rig.ports(), OWN));
    let link = Arc::new(FakeLink::default());
    let log = Arc::new(RecordingLog::default());
    let links = Links {
        enroller: Box::new(|| Err("no enrollment here".to_owned())),
        link: link.clone(),
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("host-1")))),
        clock: Arc::new(FakeClock::new(2_000_000_000, 7)),
        log: log.clone(),
        sleeper: Box::new(ScriptedSleeper::new(0)),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: Arc::new(ManualDispatch::default()),
        stop: StopFlag::default(),
        enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
        renewal: hostkeeper_enroll::RenewalState::default(),
        watchdog: Box::new(hostkeeper_daemon::NoWatchdog),
        console: None,
        identity: None,
        remote_login_probe: None,
        operator_keys: None,
        prerequisites: None,
        bmc: None,
    };
    (links.heartbeater(&daemon), link, log)
}

/// A `halt-v1` reply naming each of `generations`.
fn halts(generations: &[(&str, u64)]) -> Reply {
    Reply {
        features: vec![Feature::HaltV1],
        halt_generations: generations
            .iter()
            .map(|(name, generation)| (service(name), *generation))
            .collect(),
        ..Reply::default()
    }
}

/// The text of `name`'s `held-halt` file on `rig`; `None` when there is none.
fn held_file(rig: &Rig, name: &str) -> Option<String> {
    fs::read_to_string(rig.state.join(name).join(HELD_HALT_FILE)).ok()
}

/// Proves: a host whose services hold no halt keeps no `held-halt` file when a reply names their
/// generations (tierstore 4 after a clear of another host's halt, js-agent 11), nor when a later
/// reply names greater ones, and logs nothing.
///
/// Catches: every host writing the generation of a clear meant for one host's halt, so that each
/// is left with a `held-halt` file that records no halt and outlives every halt.
#[test]
fn a_reply_after_a_clear_writes_no_held_halt_on_a_host_that_holds_no_halt() {
    let rig = Rig::new("halt-beat-none");
    rig.add("tierstore", "0.3.138");
    rig.add("js-agent", "0.1.270");
    let (mut heartbeater, link, log) = host(&rig);
    link.answer(Ok(halts(&[("js-agent", 11), ("tierstore", 4)])));
    link.answer(Ok(halts(&[("js-agent", 11), ("tierstore", 5)])));
    for _ in 0..2 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    assert_eq!(held_file(&rig, "tierstore"), None);
    assert_eq!(held_file(&rig, "js-agent"), None);
    assert_eq!(log.lines(), Vec::<String>::new());
}

/// Proves: a `held-halt` file a host was left with for a service that holds no halt (15, above
/// the 11 the coordinator sends) is removed by the next reply that names the service.
///
/// Catches: a file of no halt kept until a later halt of the service is held under it, which no
/// generation this coordinator issues would lift.
#[test]
fn a_held_halt_left_with_no_halt_is_removed_by_the_next_reply() {
    let rig = Rig::new("halt-beat-left");
    rig.add("js-agent", "0.1.270");
    HaltStore::new(&rig.state)
        .raise(&service("js-agent"), 15)
        .expect("a file left with no halt");
    let (mut heartbeater, link, _) = host(&rig);
    link.answer(Ok(halts(&[("js-agent", 11)])));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    assert_eq!(held_file(&rig, "js-agent"), None);
}
