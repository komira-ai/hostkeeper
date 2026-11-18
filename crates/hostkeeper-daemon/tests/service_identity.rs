//! The daemon's service identities: which services want one, the worker that keeps them over
//! injected ports, what the heartbeat reports of them, and the uninstall that removes a service's
//! identity directory. The coordinator is a counted fake; the identity directory is the rig's, in
//! a scratch directory owned by the test's own user.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::service_account::ServiceAccounts;
use hostkeeper_daemon::service_identity::{
    IdentityLoop, MakePorts, NO_IDENTITY, identity_loop, identity_ports, wanted,
};
use hostkeeper_daemon::{Daemon, Dirs, Refusal, Reply};
use hostkeeper_enroll::{
    EntropyError, HostId, HostTrust, IdentityFile, IdentityFiles, IdentityPorts, IssueAnswer,
    Owner, Reason, ServiceCertIssuer, Site, TransportError, Trust, WallClock, Wanted,
};
use hostkeeper_hook::Verdict;
use hostkeeper_link::testing::{FakeCerts, FakeLink, ManualDispatch, RecordingLog, identity};
use hostkeeper_link::{CertState, Reply as LinkReply, ServiceIdentityReport, StopFlag};
use hostkeeper_proto::hostkeeper::release::v1::{Hook, Manifest, StepBounds, Target};
use hostkeeper_steps::{BootId, BootIdError, Clock};
use support::{OWN, Rig, config, idle_links, my_uid, uninstall};

fn me() -> Owner {
    Owner {
        uid: my_uid(),
        gid: rustix::process::getegid().as_raw(),
    }
}

/// The accounts a test host has: a name is its owner, or the host has none.
struct Accounts(BTreeMap<&'static str, Owner>);

impl ServiceAccounts for Accounts {
    fn lookup(&self, name: &str) -> Result<Owner, String> {
        self.0
            .get(name)
            .copied()
            .ok_or_else(|| format!("the host has no account named {name:?}"))
    }
}

fn accounts() -> Accounts {
    Accounts(BTreeMap::from([
        ("root", Owner { uid: 0, gid: 0 }),
        ("svc", Owner { uid: 998, gid: 997 }),
    ]))
}

/// An entry whose manifest every rule accepts and that asks for an identity as `identity` and
/// `account` say.
fn entry(identity: Option<bool>, account: Option<&str>) -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: "ab".repeat(32),
        manifest: Some(Manifest {
            package: Some("web".to_owned()),
            hook: Some(Hook {
                timeout_secs: Some(StepBounds {
                    prepare: 60,
                    cutover: 61,
                    verify: 62,
                    rollback: 63,
                    cleanup: 64,
                    uninstall: Some(65),
                }),
                verify_deadline_secs: 300,
                ..Hook::default()
            }),
            run: vec!["web.service".to_owned()],
            min_hostkeeper: "0.1.0".to_owned(),
            identity,
            identity_account: account.map(str::to_owned),
            ..Manifest::default()
        }),
        repository: Some("main".to_owned()),
        package_version: Some("1.0.0-1".to_owned()),
        ..Target::default()
    }
}

/// `service` runs 1.0.0, sealed with `entry`.
fn installed(rig: &Rig, service: &str, entry: &Target) {
    rig.store.add_entry(service, "1.0.0", entry, false);
    rig.store.link(service, "current", "1.0.0");
}

fn name(service: &str) -> ServiceName {
    ServiceName::new(service).expect("a service name")
}

/// Catches a service wanted that does not ask for an identity, or left out that does: of services
/// that ask, do not, ask with `identity` false, are staged and not running, are tombstoned, or
/// have no sealed entry, exactly the running ones that ask are listed, with the account of the
/// manifest, `root` when it names none, and an account the host does not have as that service's
/// error.
#[test]
fn the_services_wanted_are_the_running_ones_whose_entry_asks_for_an_identity() {
    let rig = Rig::new("identity-wanted");
    installed(&rig, "alpha", &entry(Some(true), Some("svc")));
    installed(&rig, "beta", &entry(Some(true), None));
    installed(&rig, "gamma", &entry(None, None));
    installed(&rig, "delta", &entry(Some(false), Some("svc")));
    installed(&rig, "theta", &entry(Some(true), Some("ghost")));
    installed(&rig, "zeta", &entry(Some(true), Some("svc")));
    rig.store
        .add_entry("epsilon", "1.0.0", &entry(Some(true), Some("svc")), false);
    fs::create_dir_all(rig.packages.join("eta").join("1.0.0")).expect("a version directory");
    rig.store.link("eta", "current", "1.0.0");
    let mut record = rig.placed_record();
    record
        .uninstalling
        .insert("zeta".to_owned(), hostkeeper_placed::Tombstone::default());
    rig.save_placed(&record);

    let daemon = rig.daemon();
    assert_eq!(
        wanted(daemon.ctx(), &accounts()),
        vec![
            Wanted {
                service: name("alpha"),
                account: Ok(Owner { uid: 998, gid: 997 }),
            },
            Wanted {
                service: name("beta"),
                account: Ok(Owner { uid: 0, gid: 0 }),
            },
            Wanted {
                service: name("theta"),
                account: Err("the host has no account named \"ghost\"".to_owned()),
            },
        ]
    );
}

/// Catches an unreadable `placed.json` or package directory listing services anyway: with no
/// tombstone to read, no order of a service runs, and none is asked an identity for.
#[test]
fn a_placed_record_or_package_directory_that_cannot_be_read_wants_nothing() {
    let rig = Rig::new("identity-unreadable");
    installed(&rig, "alpha", &entry(Some(true), None));
    assert_eq!(wanted(rig.daemon().ctx(), &accounts()).len(), 1);
    fs::write(rig.placed_record_path(), "not json").expect("a damaged record");
    assert_eq!(wanted(rig.daemon().ctx(), &accounts()), vec![]);

    let file = rig.packages.with_file_name("a-file");
    fs::write(&file, "x").expect("a file");
    let dirs = Dirs {
        packages: file,
        ..rig.dirs()
    };
    let daemon = Daemon::new(config(), dirs, rig.ports(), OWN);
    assert_eq!(wanted(daemon.ctx(), &accounts()), vec![]);
}

/// What the coordinator was asked: the service and the request.
type Asked = Vec<(String, Vec<u8>)>;

/// The coordinator of these tests: it counts each call and answers as it is told.
#[derive(Clone)]
struct Coordinator {
    calls: Arc<Mutex<Asked>>,
    answer: Result<IssueAnswer, TransportError>,
}

impl ServiceCertIssuer for Coordinator {
    fn issue(&mut self, service: &str, csr_der: &[u8]) -> Result<IssueAnswer, TransportError> {
        self.calls
            .lock()
            .expect("calls")
            .push((service.to_owned(), csr_der.to_vec()));
        self.answer.clone()
    }
}

struct FixedTrust;

impl HostTrust for FixedTrust {
    fn read(&self) -> Result<Trust, String> {
        Ok(Trust {
            host_id: HostId::new("web-1").expect("a host id"),
            ca_bundle_pem: String::new(),
        })
    }
}

struct Fixed(u64);

impl WallClock for Fixed {
    fn now_unix_secs(&self, _site: Site) -> u64 {
        self.0
    }
}

struct Draws;

impl hostkeeper_enroll::Entropy for Draws {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
        buf.fill(3);
        Ok(())
    }
}

/// A monotonic clock whose first sleep stops the loop: one pass is made.
struct StopsAtFirstSleep(StopFlag);

impl Clock for StopsAtFirstSleep {
    fn mono_secs(&self) -> u64 {
        0
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test clock", "boot")
    }

    fn sleep_secs(&self, _secs: u64) {
        self.0.stop();
    }
}

fn ports(rig: &Rig, coordinator: &Coordinator, stop: &StopFlag) -> MakePorts {
    let files = rig.identity_dir();
    let coordinator = coordinator.clone();
    let stop = stop.clone();
    Box::new(move || {
        Ok(IdentityPorts {
            issuer: Box::new(coordinator),
            files: Box::new(files),
            trust: Box::new(FixedTrust),
            entropy: Box::new(Draws),
            wall: Box::new(Fixed(1_900_000_000)),
            clock: Box::new(StopsAtFirstSleep(stop)),
        })
    })
}

/// A host with `alpha` (account `svc`... which the test host's accounts map to the test's own
/// user), and a coordinator that answers `answer`.
fn keeping(
    name: &str,
    answer: Result<IssueAnswer, TransportError>,
) -> (Rig, Arc<Daemon>, Coordinator, StopFlag) {
    let rig = Rig::new(name);
    installed(&rig, "alpha", &entry(Some(true), Some("svc")));
    let coordinator = Coordinator {
        calls: Arc::default(),
        answer,
    };
    let daemon = Arc::new(rig.daemon());
    (rig, daemon, coordinator, StopFlag::default())
}

fn mine() -> Arc<dyn ServiceAccounts> {
    Arc::new(Accounts(BTreeMap::from([("svc", me())])))
}

/// Catches the worker not keeping a wanted service's identity: it asks the coordinator once for
/// the service by its name with a request, writes the key before it asks (owned by the account,
/// mode 0600, in a directory of mode 0750 with the account's group), and what the coordinator
/// refused with is reported through the source the heartbeat reads; a service that does not ask
/// is not asked for.
#[test]
fn the_worker_asks_for_each_wanted_service_and_reports_the_answer() {
    let refusal = Ok(IssueAnswer::Refused(
        Reason::new("identity_not_approved").expect("a token"),
    ));
    let (rig, daemon, coordinator, stop) = keeping("identity-worker", refusal);
    installed(&rig, "plain", &entry(None, None));
    let log = Arc::new(RecordingLog::default());
    let kept = identity_loop(
        Arc::clone(&daemon),
        ports(&rig, &coordinator, &stop),
        mine(),
        log.clone(),
        stop.clone(),
    );
    assert_eq!(kept.source.reports(), vec![]);
    (kept.worker)();
    let calls = coordinator.calls.lock().expect("calls").clone();
    assert_eq!(
        calls.iter().map(|call| call.0.as_str()).collect::<Vec<_>>(),
        ["alpha"]
    );
    assert_eq!(
        kept.source.reports(),
        vec![ServiceIdentityReport::new(
            "alpha".to_owned(),
            None,
            Some("identity_not_approved".to_owned())
        )]
    );
    let root = rig.packages.with_file_name("identity");
    let key = fs::metadata(root.join("alpha").join("key.pem")).expect("the key is written");
    assert_eq!(
        (key.mode() & 0o7777, key.uid(), key.gid()),
        (0o600, me().uid, me().gid)
    );
    let dir = fs::metadata(root.join("alpha")).expect("the directory");
    assert_eq!((dir.mode() & 0o7777, dir.gid()), (0o750, me().gid));
    assert!(!root.join("plain").exists());
    assert_eq!(log.lines(), Vec::<String>::new());
}

/// Catches a worker whose ports cannot be made carrying on: it logs why, once, and ends, and its
/// source reports nothing.
#[test]
fn a_worker_whose_ports_cannot_be_made_logs_why_and_ends() {
    let rig = Rig::new("identity-no-ports");
    let log = Arc::new(RecordingLog::default());
    let kept = identity_loop(
        Arc::new(rig.daemon()),
        Box::new(|| Err("no runtime".to_owned())),
        mine(),
        log.clone(),
        StopFlag::default(),
    );
    (kept.worker)();
    assert_eq!(log.lines(), [format!("{NO_IDENTITY}no runtime")]);
    assert_eq!(kept.source.reports(), vec![]);
}

/// Catches the shipped ports being made from something else than the config and the host's
/// directories: made on the worker, they are the config's coordinator, the rig's identity
/// directory and the pki directory's current generation.
#[test]
fn the_shipped_ports_are_made_from_the_config() {
    let rig = Rig::new("identity-shipped");
    let daemon = rig.daemon();
    let made = identity_ports(&daemon, &rig.state.join("pki"));
    let ports = made().expect("ports for the config's coordinator");
    assert_eq!(
        ports
            .files
            .read("alpha", IdentityFile::Key, me())
            .expect("an absent file"),
        None
    );
}

/// Catches the third worker missing or the others disturbed: with an identity loop the links start
/// three workers (the enrollment loop, the heartbeat loop, the identity loop, in that order), and
/// without one two.
#[test]
fn the_links_start_the_identity_loop_third_when_they_have_one() {
    let rig = Rig::new("identity-links");
    let daemon = Arc::new(rig.daemon());
    let ran = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&ran);
    let kept = identity_loop(
        Arc::clone(&daemon),
        Box::new(move || {
            seen.lock().expect("ran").push("made");
            Err("stop here".to_owned())
        }),
        mine(),
        Arc::new(RecordingLog::default()),
        StopFlag::default(),
    );
    let dispatch = Arc::new(ManualDispatch::default());
    let links = Links {
        dispatch: dispatch.clone(),
        identity: Some(kept),
        ..idle_links()
    };
    links.start(&daemon);
    assert_eq!(dispatch.pending(), 3);
    assert!(dispatch.run_next());
    assert!(dispatch.run_next());
    assert_eq!(
        ran.lock().expect("ran").len(),
        0,
        "the identity loop is third"
    );
    assert!(dispatch.run_next());
    assert_eq!(ran.lock().expect("ran").as_slice(), ["made"]);

    let none = Arc::new(ManualDispatch::default());
    let links = Links {
        dispatch: none.clone(),
        ..idle_links()
    };
    links.start(&daemon);
    assert_eq!(none.pending(), 2);
}

/// Catches the heartbeat not carrying the identities: a daemon with an identity loop announces
/// `service-identity-v1` and reports each service as the keeper last said, and one without
/// announces nothing of it.
#[test]
fn the_heartbeat_announces_and_reports_the_identities_of_the_loop() {
    let refusal = Ok(IssueAnswer::Refused(
        Reason::new("denied").expect("a token"),
    ));
    let (rig, daemon, coordinator, stop) = keeping("identity-beat", refusal);
    let kept = identity_loop(
        Arc::clone(&daemon),
        ports(&rig, &coordinator, &stop),
        mine(),
        Arc::new(RecordingLog::default()),
        stop,
    );
    let source = Arc::clone(&kept.source);
    let worker = kept.worker;
    worker();
    let link = Arc::new(FakeLink::default());
    link.answer(Ok(LinkReply::default()));
    link.answer(Ok(LinkReply::default()));
    let links = Links {
        link: link.clone(),
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("a")))),
        identity: Some(IdentityLoop {
            worker: Box::new(|| {}),
            source: Arc::clone(&source),
        }),
        ..idle_links()
    };
    links.heartbeater(&daemon).beat_once();
    let beat = &link.beats()[0];
    assert!(
        beat.features
            .iter()
            .any(|feature| feature.as_str() == "service-identity-v1")
    );
    assert_eq!(beat.service_identities, source.reports());
    assert_eq!(beat.service_identities.len(), 1);

    let plain = Links {
        link: link.clone(),
        certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity("a")))),
        ..idle_links()
    };
    plain.heartbeater(&daemon).beat_once();
    let beat = &link.beats()[1];
    assert!(
        !beat
            .features
            .iter()
            .any(|feature| feature.as_str() == "service-identity-v1")
    );
    assert_eq!(beat.service_identities, []);
}

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

fn identity_files(rig: &Rig, service: &str) -> PathBuf {
    let dir = rig.identity_dir();
    dir.write(service, IdentityFile::Key, me(), b"a key")
        .expect("write");
    dir.write(service, IdentityFile::Cert, me(), b"a certificate")
        .expect("write");
    rig.packages.with_file_name("identity").join(service)
}

/// Catches an uninstall that leaves the service's identity behind, or takes another service's:
/// the service's directory and what is in it are gone with the service, the other service's stay.
#[test]
fn an_uninstall_removes_the_services_identity_directory_and_no_other() {
    let rig = Rig::new("identity-uninstall");
    rig.stage("api", "1.0.0");
    let gone = identity_files(&rig, "api");
    let kept = identity_files(&rig, "other");
    assert_eq!(rig.daemon().order(&uninstall("api", 1)), succeeded());
    assert!(
        !gone.exists(),
        "the identity directory of the service is removed"
    );
    assert!(!rig.packages.join("api").exists());
    assert!(
        kept.join("key.pem").exists(),
        "another service's identity stays"
    );
}

/// Catches an uninstall that goes on when the identity cannot be removed, or cannot be resumed:
/// a directory in the identity directory stops it, interrupted `link_failed` naming the service,
/// with the tombstone and the package left, and once that is cleared the next order finishes it.
#[test]
fn an_identity_directory_that_cannot_be_removed_stops_the_uninstall_and_it_resumes() {
    let rig = Rig::new("identity-uninstall-stuck");
    rig.stage("api", "1.0.0");
    let dir = identity_files(&rig, "api");
    let nested = dir.join("nested");
    fs::create_dir(&nested).expect("a directory");
    let reply = rig.daemon().order(&uninstall("api", 1));
    let Reply::Interrupted { refusal, detail } = reply else {
        panic!("not interrupted: {reply:?}");
    };
    assert_eq!(refusal, Refusal::LinkFailed);
    assert!(
        detail.starts_with(&format!(
            "the identity directory of api could not be removed: {}: ",
            dir.display()
        )),
        "{detail}"
    );
    assert!(rig.placed_record().uninstalling.contains_key("api"));
    assert!(rig.packages.join("api").exists());

    fs::remove_dir(&nested).expect("clear it");
    assert_eq!(rig.daemon().order(&uninstall("api", 2)), succeeded());
    assert!(!dir.exists());
    assert!(!rig.placed_record().uninstalling.contains_key("api"));
}
