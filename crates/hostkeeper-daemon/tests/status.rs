//! What `Daemon::status` holds: each service's running version, last result, held halt and
//! record, how it is observed, and the
//! error of a status that cannot list the services.

mod support;
mod support_apt;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::executor::RecordState;
use hostkeeper_daemon::halt::HaltStore;
use hostkeeper_daemon::testing::spawn;
use hostkeeper_daemon::{Daemon, Dirs};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_observe::{HealthView, Observation, ServiceState as Observed, VersionView};
use hostkeeper_steps::record::RECORD_FILE;
use support::{OWN, Rig, config, update};
use support_apt::AptRig;
use support_apt::services;
use support_apt::with_entry;

/// Catches a field of the status dropped or read from the wrong service: a service whose order
/// ended, one with a step running, one halted with a held generation, one whose record does not
/// read and one a newer daemon wrote, each with what it holds.
#[test]
fn status_reports_running_version_last_result_and_held_halt() {
    let apt = AptRig::new("status-fields");
    let rig = &apt.rig;
    services::installed(&apt, "done", "2.0.0");
    rig.store
        .add("idle", "1.0.0", &services::manifest_text("idle", "1.0.0"));
    rig.store.link("idle", "current", ".hidden");
    rig.launcher.exits("halted", Step::Prepare, &[76]);
    let gate = rig.launcher.hold("busy", Step::Cutover);
    let daemon = Arc::new(apt.daemon());
    let done_entry = services::publish(&apt, "done", "2.1.0");
    let done_order = with_entry(update("done", "2.1.0", &done_entry.digest, 3), &done_entry);
    assert!(matches!(
        daemon.order(&done_order),
        hostkeeper_daemon::Reply::Finished { .. }
    ));
    daemon.order(&services::update_of(&apt, "halted", "2.0.0", 1));
    HaltStore::new(&rig.state)
        .raise(&ServiceName::new("halted").expect("a name"), 3)
        .expect("raise");
    for (service, text) in [("broken", "{"), ("newer", r#"{"schema": 9}"#)] {
        fs::create_dir_all(rig.state.join(service)).expect("dir");
        fs::write(rig.state.join(service).join(RECORD_FILE), text).expect("record");
    }
    let busy_order = services::update_of(&apt, "busy", "2.0.0", 1);
    let running = {
        let daemon = Arc::clone(&daemon);
        spawn("the order", move || daemon.order(&busy_order))
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
        (Some(3), "2.1.0", done_entry.digest.as_str())
    );
    assert_eq!(halted.last_result().map(|f| f.verdict), Some(Verdict::Halt));
    assert_eq!(halted.held_halt, Some(3));
    assert!(matches!(broken.record, RecordState::Unreadable(_)));
    assert!(!broken.step_running());
    assert_eq!(newer.record, RecordState::Newer(9));
    // The service runs `done.service`, which the rig's service manager does not list.
    assert_eq!(
        done.observation,
        Some(Observation {
            service: "done".to_owned(),
            state: Observed::Absent,
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
