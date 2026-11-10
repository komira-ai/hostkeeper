//! The host's facts the coordinator sends, through the daemon's heartbeat loop
//! (`hostkeeper_daemon::links`): every host announces `facts-v1`, writes the facts of a reply that
//! advertises it to its facts file (`--facts-file`, here beside the rig's packages directory)
//! before it starts the order of that reply, and reports the generation it wrote in the next
//! heartbeat.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::executor::Daemon;
use hostkeeper_daemon::links::Links;
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, FakeLink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, CertState, DEFAULT_HEARTBEAT_SECS, FACTS, Feature, Heartbeater, Order,
    OrderId, PackageDigest, PushedFacts, Reply, StopFlag, Target, Verb,
};
use support::{OWN, Rig, config};

fn facts(generation: u64) -> PushedFacts {
    PushedFacts {
        generation,
        host_id: "host-1".to_owned(),
        host_group: "web".to_owned(),
        labels: BTreeMap::from([("rack".to_owned(), "r12".to_owned())]),
    }
}

/// The install of `hello` 1.0, which the loop starts on a worker.
fn install() -> Order {
    Order {
        id: OrderId::new("install-1").expect("an id"),
        order_try: 1,
        verb: Verb::Install(Target {
            service: ServiceName::new("hello").expect("a name"),
            version: Version::new("1.0").expect("a version"),
            digest: PackageDigest::new(&"0a".repeat(32)).expect("a digest"),
            index_digest: None,
            entry: None,
        }),
    }
}

/// A heartbeat loop over a daemon on `rig`, its link, and the dispatcher of its orders.
fn host(rig: &Rig) -> (Heartbeater, Arc<FakeLink>, Arc<ManualDispatch>) {
    let daemon = Arc::new(Daemon::new(config(), rig.dirs(), rig.ports(), OWN));
    let link = Arc::new(FakeLink::default());
    let dispatch = Arc::new(ManualDispatch::default());
    let links = Links {
        enroller: Box::new(|| Err("no enrollment here".to_owned())),
        link: link.clone(),
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("host-1")))),
        clock: Arc::new(FakeClock::new(2_000_000_000, 7)),
        log: Arc::new(RecordingLog::default()),
        sleeper: Box::new(ScriptedSleeper::new(0)),
        period: DEFAULT_HEARTBEAT_SECS,
        dispatch: dispatch.clone(),
        stop: StopFlag::default(),
        enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
        renewal: hostkeeper_enroll::RenewalState::default(),
        watchdog: Box::new(hostkeeper_daemon::NoWatchdog),
        console: None,
    };
    (links.heartbeater(&daemon), link, dispatch)
}

/// Proves a daemon announces `facts-v1` (a Mac daemon `private-repository-v1` after it), reports
/// no generation before its file holds facts,
/// writes the facts of a reply that advertises the feature to its facts file, as the design's
/// object with the daemon's platform, and starts the order of that reply, and reports the
/// generation it wrote in the next heartbeat; facts the coordinator sends again with the same
/// values leave the file as it is.
#[test]
fn a_host_writes_the_facts_of_a_reply_and_reports_their_generation() {
    let rig = Rig::new("facts-file-daemon");
    let (mut heartbeater, link, dispatch) = host(&rig);
    link.answer(Ok(Reply {
        features: vec![Feature::FactsV1],
        facts: Some(facts(7)),
        order: Some(install()),
        ..Reply::default()
    }));
    link.answer(Ok(Reply {
        features: vec![Feature::FactsV1],
        facts: Some(facts(8)),
        ..Reply::default()
    }));
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    let beats = link.beats();
    let mut features = ANNOUNCED.to_vec();
    features.push(FACTS);
    // A Mac daemon reads private repositories through its `coordinator` store.
    if hostkeeper_daemon::links::reads_private_repositories(hostkeeper_daemon::links::PLATFORM) {
        features.push(Feature::PrivateRepositoryV1);
    }
    assert_eq!(beats[0].features, features);
    assert_eq!(beats[0].facts_generation, None);
    assert_eq!(beats[1].facts_generation, Some(7));
    assert_eq!(beats[2].facts_generation, Some(8));

    let written = fs::read_to_string(rig.facts_file()).expect("the facts file");
    let platform = hostkeeper_daemon::links::PLATFORM.as_str();
    assert_eq!(
        written,
        format!(
            "{{\"schema\":1,\"host_id\":\"host-1\",\"host_group\":\"web\",\"platform\":\"{platform}\",\"labels\":{{\"rack\":\"r12\"}}}}\n"
        )
    );
    assert_eq!(dispatch.pending(), 1, "the order of the reply was started");
}

/// Proves facts from a coordinator that does not advertise `facts-v1` are not written, and the
/// daemon reports no generation.
#[test]
fn facts_without_the_advertised_feature_are_not_written() {
    let rig = Rig::new("facts-file-unadvertised");
    let (mut heartbeater, link, _) = host(&rig);
    link.answer(Ok(Reply {
        features: vec![Feature::HaltV1],
        facts: Some(facts(7)),
        ..Reply::default()
    }));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert_eq!(link.beats()[1].facts_generation, None);
    assert!(!rig.facts_file().exists());
}
