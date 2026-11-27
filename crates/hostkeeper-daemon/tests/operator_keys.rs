//! The operator keys the coordinator sends, through the daemon's heartbeat loop
//! (`hostkeeper_daemon::links`, `hostkeeper_daemon::operator_keys`): a daemon whose links keep
//! operator keys announces `operator-keys-v1`, reports what its managed block holds in every
//! heartbeat, and writes the set of a reply that advertises the feature into the login account's
//! `authorized_keys` (here a scratch home, never the host's) before the beat that reports it.

mod support;

use std::fs;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_daemon::authorized_keys::{BEGIN, END};
use hostkeeper_daemon::executor::Daemon;
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::login_account::{Account, AccountError, AccountSource};
use hostkeeper_daemon::operator_keys::OperatorKeys;
use hostkeeper_link::testing::{
    FakeCerts, FakeClock, FakeLink, ManualDispatch, RecordingLog, ScriptedSleeper, identity,
};
use hostkeeper_link::{
    BeatOutcome, CertState, DEFAULT_HEARTBEAT_SECS, Feature, Heartbeater, KeysReport,
    OperatorKeysSink, PushedKeys, Reply, StopFlag,
};
use hostkeeper_wire::operator_keys::digest;
use support::{OWN, Rig, config};

/// An ed25519 public key whose blob is filled with `fill`.
fn key(fill: u8) -> String {
    let mut bytes = vec![0, 0, 0, 11];
    bytes.extend_from_slice(b"ssh-ed25519");
    bytes.extend_from_slice(&[0, 0, 0, 32]);
    bytes.extend_from_slice(&[fill; 32]);
    format!("ssh-ed25519 {} ops laptop", STANDARD.encode(bytes))
}

fn set(fills: &[u8]) -> PushedKeys {
    let keys: Vec<String> = fills.iter().map(|fill| key(*fill)).collect();
    PushedKeys {
        digest: digest(&keys),
        keys,
    }
}

/// The login account of the test: the home it is given and the test's own user.
struct Fixed(Account);

impl AccountSource for Fixed {
    fn login_account(&self) -> Result<Account, AccountError> {
        Ok(self.0.clone())
    }
}

/// A heartbeat loop over a daemon on `rig` whose links hold `keys`, and its link.
fn host(rig: &Rig, keys: Option<Arc<dyn OperatorKeysSink>>) -> (Heartbeater, Arc<FakeLink>) {
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
        identity: None,
        remote_login_probe: None,
        full_disk_access: None,
        operator_keys: keys,
        prerequisites: None,
        maintenance: None,
        bmc: None,
        serial_console: None,
        health: None,
        health_reads: Vec::new(),
    };
    (links.heartbeater(&daemon), link)
}

/// Proves a daemon whose links keep operator keys announces `operator-keys-v1` and reports an
/// empty digest before it was sent a set, writes the set of a reply that advertises the feature
/// into the login account's file as the managed block, and reports its digest in the next
/// heartbeat; a set sent again with nothing changed leaves the file as it is.
#[test]
fn a_host_writes_the_operator_keys_of_a_reply_and_reports_their_digest() {
    let rig = Rig::new("operator-keys-daemon");
    let home = rig.scratch.path().join("login-home");
    fs::create_dir(&home).expect("the home");
    let account = Account {
        name: "ops".to_owned(),
        uid: support::my_uid(),
        gid: rustix::process::getegid().as_raw(),
        home: home.clone(),
    };
    let log = Arc::new(RecordingLog::default());
    let sink = OperatorKeys::new(Box::new(Fixed(account)), None, log.clone());
    let (mut heartbeater, link) = host(&rig, Some(Arc::new(sink)));
    let keys = set(&[1, 2]);
    for _ in 0..2 {
        link.answer(Ok(Reply {
            features: vec![Feature::OperatorKeysV1],
            operator_keys: Some(keys.clone()),
            ..Reply::default()
        }));
    }
    for _ in 0..3 {
        assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    }
    let beats = link.beats();
    assert!(beats[0].features.contains(&Feature::OperatorKeysV1));
    assert_eq!(beats[0].operator_keys, Some(KeysReport::default()));
    assert_eq!(
        beats[1]
            .operator_keys
            .as_ref()
            .map(|report| report.digest.clone()),
        Some(keys.digest.clone())
    );
    assert_eq!(
        beats[2]
            .operator_keys
            .as_ref()
            .map(|report| report.digest.clone()),
        Some(keys.digest.clone())
    );
    assert_eq!(
        fs::read_to_string(home.join(".ssh/authorized_keys")).expect("the file"),
        format!("{BEGIN}\n{}\n{}\n{END}\n", keys.keys[0], keys.keys[1])
    );
    assert_eq!(
        log.lines(),
        [
            format!(
                "operator keys {}: 2 keys in force in the login account's authorized_keys",
                &keys.digest[..12]
            ),
            format!(
                "operator keys {}: 2 keys already in force in the login account's authorized_keys",
                &keys.digest[..12]
            ),
        ]
    );
}

/// Proves a daemon whose links keep no operator keys does not announce the feature and reports
/// none, and a set it is sent is not written anywhere.
#[test]
fn a_host_with_no_operator_keys_announces_nothing_of_them() {
    let rig = Rig::new("operator-keys-none");
    let (mut heartbeater, link) = host(&rig, None);
    link.answer(Ok(Reply {
        features: vec![Feature::OperatorKeysV1],
        operator_keys: Some(set(&[1])),
        ..Reply::default()
    }));
    assert_eq!(heartbeater.beat_once(), BeatOutcome::Sent);
    let beats = link.beats();
    assert!(!beats[0].features.contains(&Feature::OperatorKeysV1));
    assert_eq!(beats[0].operator_keys, None);
}

/// Proves a set the daemon refuses is reported as refused, with its reason and the digest, in the
/// next heartbeat, and the sink the loop was given is the one that judged it.
#[test]
fn a_refused_set_is_reported_in_the_next_heartbeat() {
    struct Refusing;
    impl AccountSource for Refusing {
        fn login_account(&self) -> Result<Account, AccountError> {
            Err(AccountError::Refused(
                "no local administrator account".to_owned(),
            ))
        }
    }
    let rig = Rig::new("operator-keys-refused");
    let sink = OperatorKeys::new(Box::new(Refusing), None, Arc::new(RecordingLog::default()));
    let (mut heartbeater, link) = host(&rig, Some(Arc::new(sink)));
    let keys = set(&[1]);
    link.answer(Ok(Reply {
        features: vec![Feature::OperatorKeysV1],
        operator_keys: Some(keys.clone()),
        ..Reply::default()
    }));
    heartbeater.beat_once();
    heartbeater.beat_once();
    let report = link.beats()[1].operator_keys.clone().expect("a report");
    assert_eq!(report.digest, "");
    let refused = report.refused.expect("refused");
    assert_eq!(refused.digest, keys.digest);
    assert_eq!(refused.reason, "no local administrator account");
}
