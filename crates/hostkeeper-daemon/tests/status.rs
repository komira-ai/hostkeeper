//! What `Daemon::status` holds: each service's running version, last result, held halt and
//! record, how it is observed, the Sigstore trusted root the package store reports, and the
//! error of a status that cannot list the services.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::executor::RecordState;
use hostkeeper_daemon::halt::HaltStore;
use hostkeeper_daemon::packages::{Entry, PackageError, Packages, VersionDir};
use hostkeeper_daemon::testing::spawn;
use hostkeeper_daemon::trusted_root::{RootProblem, RootReport};
use hostkeeper_daemon::verb::PackageDigest;
use hostkeeper_daemon::{Daemon, Dirs};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_observe::{HealthView, Observation, ServiceState as Observed, VersionView};
use hostkeeper_steps::record::RECORD_FILE;
use support::{OWN, Rig, config, d, update};

/// Catches a field of the status dropped or read from the wrong service: a service whose order
/// ended, one with a step running, one halted with a held generation, one whose record does not
/// read and one a newer daemon wrote, each with what it holds.
#[test]
fn status_reports_running_version_last_result_and_held_halt() {
    let rig = Rig::new("status-fields");
    rig.running("done", "2.0.0");
    rig.add("done", "2.1.0");
    rig.add("busy", "2.0.0");
    rig.add("halted", "2.0.0");
    rig.add("idle", "1.0.0");
    rig.store.link("idle", "current", ".hidden");
    rig.launcher.exits("halted", Step::Prepare, &[76]);
    let gate = rig.launcher.hold("busy", Step::Cutover);
    let daemon = Arc::new(rig.daemon());
    assert!(matches!(
        daemon.order(&update("done", "2.1.0", &d(), 3)),
        hostkeeper_daemon::Reply::Finished { .. }
    ));
    daemon.order(&update("halted", "2.0.0", &d(), 1));
    HaltStore::new(&rig.state)
        .raise(&ServiceName::new("halted").expect("a name"), 3)
        .expect("raise");
    for (service, text) in [("broken", "{"), ("newer", r#"{"schema": 9}"#)] {
        fs::create_dir_all(rig.state.join(service)).expect("dir");
        fs::write(rig.state.join(service).join(RECORD_FILE), text).expect("record");
    }
    let running = {
        let daemon = Arc::clone(&daemon);
        spawn("the order", move || {
            daemon.order(&update("busy", "2.0.0", &d(), 1))
        })
    };
    gate.entered(&running).expect("the held step is entered");

    let status = daemon.status().expect("status");
    assert_eq!(status.version, OWN);
    let names: Vec<_> = status
        .services
        .iter()
        .map(|s| s.service.to_string())
        .collect();
    assert_eq!(names, ["broken", "busy", "done", "halted", "idle", "newer"]);
    let [broken, busy, done, halted, idle, newer] = &status.services[..] else {
        panic!("six services");
    };
    assert_eq!(idle.record, RecordState::Absent);
    assert_eq!(
        (idle.running_version.clone(), idle.step_running()),
        (None, false)
    );
    assert_eq!(
        done.running_version.as_ref().map(|v| v.as_str()),
        Some("2.1.0")
    );
    assert_eq!(
        done.last_result().map(|f| f.verdict),
        Some(Verdict::Succeeded {
            promotion_pending: false
        })
    );
    assert_eq!((done.held_halt, done.step_running()), (Some(0), false));
    assert!(busy.step_running() & busy.locked);
    assert_eq!(busy.last_result(), None);
    assert_eq!(
        busy.running_version.as_ref().map(|v| v.as_str()),
        Some("2.0.0"),
        "the switch to the target comes before the cutover that is held"
    );
    let RecordState::Unfinished(current) = &busy.record else {
        panic!("the running order is unfinished: {:?}", busy.record)
    };
    assert_eq!(
        (
            current.step,
            current.attempt,
            current.order_try,
            current.target_version.as_str()
        ),
        (Step::Cutover, 1, Some(1), "2.0.0")
    );
    let RecordState::Finished(last) = &done.record else {
        panic!("the finished order: {:?}", done.record)
    };
    assert_eq!(
        (
            last.order_try,
            last.target_version.as_str(),
            last.tarball_digest.as_str()
        ),
        (Some(3), "2.1.0", d().as_str())
    );
    assert_eq!(halted.last_result().map(|f| f.verdict), Some(Verdict::Halt));
    assert_eq!(halted.held_halt, Some(3));
    assert!(matches!(broken.record, RecordState::Unreadable(_)));
    assert!(!broken.step_running());
    assert_eq!(newer.record, RecordState::Newer(9));
    assert_eq!(
        done.observation,
        Some(Observation {
            service: "done".to_owned(),
            state: Observed::Unmonitored,
            version: VersionView::NotLive,
            health: HealthView::NotProbed,
            recorded: Some("2.1.0".to_owned()),
        })
    );
    for unobserved in [idle, broken, newer] {
        assert_eq!(unobserved.observation, None, "{}", unobserved.service);
    }

    gate.release();
    running.join().expect("the running order");
}

/// Catches a status that cannot list the services reported as an empty host: it is an error, and
/// it names the directory it could not list.
#[test]
fn a_status_that_cannot_list_the_services_is_an_error_naming_the_directory() {
    let rig = Rig::new("status-fail");
    let file = rig.scratch.path().join("not-a-dir");
    fs::write(&file, "x").expect("a file");
    let dirs = Dirs {
        state: rig.state.clone(),
        packages: file.clone(),
        facts: file.with_file_name("facts.json"),
    };
    let error = Daemon::new(config(), dirs, rig.ports(), OWN)
        .status()
        .expect_err("a file where the packages are");
    assert!(
        error.to_string().starts_with(&format!(
            "cannot list the services under {}: ",
            file.display()
        )),
        "{error}"
    );

    let dirs = Dirs {
        state: file.clone(),
        packages: rig.packages.clone(),
        facts: rig.facts_file(),
    };
    let error = Daemon::new(config(), dirs, rig.ports(), OWN)
        .status()
        .expect_err("a file where the state is");
    assert!(
        error.to_string().starts_with(&format!(
            "cannot list the services under {}: ",
            file.display()
        )),
        "{error}"
    );
}

/// A package store that reports `report` as its trusted root, and otherwise is `inner`.
struct ReportingRoot {
    inner: Arc<hostkeeper_daemon::testing::FakePackages>,
    report: Option<RootReport>,
}

impl Packages for ReportingRoot {
    fn version_dir(
        &self,
        service: &ServiceName,
        version: &hostkeeper_config::names::Version,
        digest: &PackageDigest,
        index_digest: Option<&PackageDigest>,
        entry: Option<&Entry>,
    ) -> Result<VersionDir, PackageError> {
        self.inner
            .version_dir(service, version, digest, index_digest, entry)
    }

    fn trusted_root(&self) -> Option<RootReport> {
        self.report.clone()
    }
}

/// Catches the trusted root's age or reason dropped between the store and the status: the status
/// holds the store's report as it is, age and problem, for a fine root, a stale one, a missing
/// one and an unparseable one, and none for a store that reports none.
#[test]
fn status_holds_the_trusted_roots_age_and_reason() {
    let rig = Rig::new("status-trusted-root");
    let path = rig.scratch.path().join("trusted_root.json");
    let cases = [
        (None, None),
        (Some(3600), None),
        (Some(91 * 86_400), Some(RootProblem::Stale)),
        (None, Some(RootProblem::Missing)),
        (Some(60), Some(RootProblem::Unparseable)),
    ];
    for (age_secs, problem) in cases {
        let report = RootReport {
            path: path.clone(),
            age_secs,
            problem,
        };
        let packages = ReportingRoot {
            inner: Arc::clone(&rig.store),
            report: Some(report.clone()),
        };
        let ports = hostkeeper_daemon::Ports {
            packages: Arc::new(packages),
            ..rig.ports()
        };
        let daemon = Daemon::new(config(), rig.dirs(), ports, OWN);
        assert_eq!(
            daemon.status().expect("the status").trusted_root,
            Some(report),
            "{problem:?}"
        );
    }
    assert_eq!(
        rig.daemon().status().expect("the status").trusted_root,
        None
    );
}
