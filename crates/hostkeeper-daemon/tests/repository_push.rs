//! The repository sets the coordinator pushes, through the daemon's heartbeat loop
//! (`hostkeeper_daemon::links`): a Linux host (one whose repository ports name a directory)
//! announces `repositories-v1`, writes the set of a reply that advertises it, reports it in the
//! next heartbeat, announces `apt` from then on, and has the repository installer read the
//! entries the set made, with no restart; a host with no such directory (a Mac) announces
//! nothing of it and writes nothing.

mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_daemon::executor::{Daemon, Ports};
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::repository_push::Managed;
use hostkeeper_install::announce::Announcement;
use hostkeeper_install::repository::Repositories;
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, FakeLink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, CertState, DEFAULT_HEARTBEAT_SECS, Feature, Heartbeater, PushedSet,
    Reply, RepositoriesReport, StopFlag,
};
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::{Installer, registry};
use support::{OWN, Rig, config, my_uid};

/// The komira repository, with a one-key keyring that `seed` makes its own.
fn komira(seed: u8) -> Repository {
    let body = [
        4, 0, 0, 0, seed, 1, 0, 8, 0xAA, 0xBB, 0xCC, seed, 0xDD, 0xEE,
    ];
    let mut keyring = vec![0x99, 0, body.len() as u8];
    keyring.extend_from_slice(&body);
    let fingerprint = registry::keys(&keyring).expect("a keyring")[0]
        .fingerprint
        .clone();
    Repository {
        name: "komira".to_owned(),
        kind: "apt".to_owned(),
        uri: "https://repo.example.com/debian".to_owned(),
        suites: vec!["stable".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: vec![fingerprint],
        keyring,
    }
}

fn set(generation: u64, seed: u8) -> PushedSet {
    let entries = vec![komira(seed)];
    PushedSet {
        generation,
        digest: registry::digest(&entries),
        entries,
    }
}

fn pushing(set: PushedSet) -> Reply {
    Reply {
        features: vec![Feature::RepositoriesV1],
        repositories: Some(set),
        ..Reply::default()
    }
}

/// A heartbeat loop over a daemon on `rig` whose repository ports name `live` when it is some.
fn host(rig: &Rig, live: Option<&Path>) -> (Arc<Daemon>, Heartbeater, Arc<FakeLink>) {
    let mut repository = (*rig.repository_ports(Repositories::default())).clone();
    if let Some(dir) = live {
        repository = repository.with_managed(Managed {
            dir: dir.to_path_buf(),
            owner_uid: my_uid(),
            native: Some(Installer::Apt),
        });
    }
    let ports = Ports {
        repository: Arc::new(repository),
        announcement: Announcement::default(),
        ..rig.ports()
    };
    let daemon = Arc::new(Daemon::new(config(), rig.dirs(), ports, OWN));
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
    };
    let heartbeater = links.heartbeater(&daemon);
    (daemon, heartbeater, link)
}

/// Proves a host with a repository directory announces `repositories-v1` after the others,
/// reports nothing held before a set comes, takes the set of a reply that advertises the feature
/// (the files are in its directory), reports its generation and digest in the next heartbeat,
/// lists `apt` from then on, and has the repository installer's entries and the daemon's
/// installers follow, with no restart; a later set with another keyring is taken the same way.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn a_host_takes_the_pushed_set_reports_it_and_announces_apt_without_a_restart() {
    let rig = Rig::new("push-linux");
    let live = rig.scratch.path().join("etc/hostkeeper/repositories");
    let (daemon, mut heartbeater, link) = host(&rig, Some(&live));

    link.answer(Ok(Reply::default()));
    link.answer(Ok(pushing(set(3, 1))));
    link.answer(Ok(pushing(set(4, 2))));
    for _ in 0..4 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    let beats = link.beats();
    let mut features = ANNOUNCED.to_vec();
    features.extend([Feature::RepositoriesV1, Feature::FactsV1]);
    assert_eq!(beats[0].features, features);
    assert_eq!(beats[0].repositories, Some(RepositoriesReport::default()));
    assert!(beats[0].installers.is_empty());

    let first = set(3, 1);
    assert_eq!(
        beats[2].repositories,
        Some(RepositoriesReport {
            generation: 3,
            digest: first.digest.clone(),
            refused: None
        })
    );
    assert_eq!(beats[2].installers, ["apt"]);
    let second = set(4, 2);
    assert_eq!(
        beats[3].repositories.as_ref().map(|r| r.generation),
        Some(4)
    );
    assert_eq!(
        beats[3].repositories.as_ref().map(|r| r.digest.clone()),
        Some(second.digest.clone())
    );
    assert_eq!(
        fs::read(live.join("komira.gpg")).expect("the keyring"),
        second.entries[0].keyring
    );
    assert!(
        daemon
            .ctx()
            .installers()
            .contains(&hostkeeper_release::Installer::Apt)
    );
    let entries = daemon.ctx().repository_ports.repositories.snapshot();
    let stable = entries.get("komira-stable").expect("the entry");
    assert_eq!(stable.keyring, live.join("komira.gpg"));
    assert_eq!(stable.uri, "https://repo.example.com/debian");
}

/// Proves a set of a reply that does not advertise `repositories-v1` is not written.
#[test]
fn a_set_without_the_advertised_feature_is_not_written() {
    let rig = Rig::new("push-unadvertised");
    let live = rig.scratch.path().join("etc/hostkeeper/repositories");
    let (_daemon, mut heartbeater, link) = host(&rig, Some(&live));
    link.answer(Ok(Reply {
        repositories: Some(set(1, 1)),
        ..Reply::default()
    }));
    heartbeater.beat_once();
    heartbeater.beat_once();
    assert!(!live.exists());
    assert_eq!(
        link.beats()[1].repositories,
        Some(RepositoriesReport::default())
    );
}

/// Proves a host with no repository directory (a Mac) announces the features every daemon does
/// and `facts-v1`, not `repositories-v1`, sends no report, and writes nothing for a set that
/// arrives.
#[test]
fn a_host_with_no_repository_directory_announces_and_writes_nothing() {
    let rig = Rig::new("push-mac");
    let (daemon, mut heartbeater, link) = host(&rig, None);
    link.answer(Ok(pushing(set(1, 1))));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let beats = link.beats();
    let mut features = ANNOUNCED.to_vec();
    features.push(Feature::FactsV1);
    assert_eq!(beats[0].features, features);
    assert_eq!(beats[1].repositories, None);
    assert!(
        daemon
            .ctx()
            .repository_ports
            .repositories
            .snapshot()
            .is_empty()
    );
    assert!(!rig.scratch.path().join("etc").exists());
}
