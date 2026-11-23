//! Tests of the launchd jobs `activate` enables and disables: the labels of the order's `run`
//! are enabled, and the labels the replaced version's `run` named that the order's does not are
//! disabled; a job no `run` named is never touched.

use super::*;

/// A version of the service that ships the plist of the job `svc` and names no `run`: the shape
/// of hostkeeper's own package.
fn own_shape(version: &str) -> FakePkg {
    FakePkg::standard(SERVICE, IDENTIFIER, version)
}

/// Sets a `disable` override on `label`, as an earlier release's `activate` left it.
fn override_disabled(rig: &Rig, label: &str) {
    rig.mac
        .run(
            Tool::Launchctl,
            &["disable".to_owned(), format!("system/{label}")],
            Duration::from_secs(1),
        )
        .unwrap();
    rig.mac.clear_calls();
}

/// Proves that an update over a kept version whose manifest named no `run`, both versions
/// shipping the job's plist, runs no `launchctl disable` and leaves the job's override as it
/// was. Catches: an `activate` that disables every plist of the kept tree instead of the labels
/// the kept version ran, which leaves a job disabled after a reboot.
#[test]
fn an_update_disables_no_job_the_previous_version_did_not_run() {
    let rig = Rig::new("jobs-own-shape");
    rig.mac.add(own_shape("1.0"));
    let order1 = rig.order_run("1.0", &[]);
    let dir1 = rig.stage(&order1, "svc-1.0");
    rig.activate(&order1, &dir1, None).unwrap();
    override_disabled(&rig, SERVICE);
    rig.mac.add(own_shape("2.0"));
    let order2 = rig.order_run("2.0", &[]);
    let dir2 = rig.stage(&order2, "svc-2.0");
    rig.activate(&order2, &dir2, replaces(&dir1, &order1))
        .unwrap();
    assert!(rig.mac.calls_of(Tool::Launchctl).is_empty());
    assert_eq!(rig.mac.enabled(SERVICE), Some(false), "the override stays");
    assert_eq!(rig.read_text(&home("VERSION")), "2.0");
}

/// Proves that a rollback of the same shape disables nothing either.
#[test]
fn a_rollback_disables_no_job_the_rolled_back_from_version_did_not_run() {
    let rig = Rig::new("jobs-own-shape-rollback");
    rig.mac.add(own_shape("1.0"));
    let order1 = rig.order_run("1.0", &[]);
    let dir1 = rig.stage(&order1, "svc-1.0");
    rig.activate(&order1, &dir1, None).unwrap();
    rig.mac.add(own_shape("2.0"));
    let order2 = rig.order_run("2.0", &[]);
    let dir2 = rig.stage(&order2, "svc-2.0");
    rig.activate(&order2, &dir2, replaces(&dir1, &order1))
        .unwrap();
    rig.mac.clear_calls();
    rig.activate(&order1, &dir1, replaces(&dir2, &order2))
        .unwrap();
    assert!(rig.mac.calls_of(Tool::Launchctl).is_empty());
    assert_eq!(rig.read_text(&home("VERSION")), "1.0");
}

/// Proves that a label the previous version ran and the new one does not is never disabled while
/// the new version still ships its plist: only `svc` is enabled, `svc.worker` keeps the override
/// the previous activate gave it (packages-repositories.md section 4.7).
///
/// Catches: a job disabled because a `run` list left it out although its plist is still
/// installed, the shape of the daemon's own job, which its manifest never runs.
#[test]
fn a_label_whose_plist_the_new_version_still_ships_is_never_disabled() {
    let rig = Rig::new("jobs-still-shipped");
    rig.mac.add(v1());
    let order1 = rig.order_run("1.0", &BOTH);
    let dir1 = rig.stage(&order1, "svc-1.0");
    rig.activate(&order1, &dir1, None).unwrap();
    rig.mac.add(v2().label("svc.worker"));
    let order2 = rig.order("2.0");
    let dir2 = rig.stage(&order2, "svc-2.0");
    rig.mac.clear_calls();
    rig.activate(&order2, &dir2, replaces(&dir1, &order1))
        .unwrap();
    assert!(rig.exists("/Library/LaunchDaemons/svc.worker.plist"));
    assert_eq!(
        rig.mac.calls_of(Tool::Launchctl),
        [["enable", "system/svc"]]
    );
    assert_eq!(rig.mac.enabled("svc.worker"), Some(true));
}

/// Proves that a rollback disables the labels the version rolled back from ran and the previous
/// version does not.
#[test]
fn a_rollback_disables_what_the_rolled_back_from_version_ran_and_the_previous_does_not() {
    let rig = Rig::new("jobs-rollback");
    rig.mac.add(own_shape("1.0"));
    let order1 = rig.order("1.0");
    let dir1 = rig.stage(&order1, "svc-1.0");
    rig.activate(&order1, &dir1, None).unwrap();
    rig.mac.add(own_shape("2.0").label("svc.worker"));
    let order2 = rig.order_run("2.0", &BOTH);
    let dir2 = rig.stage(&order2, "svc-2.0");
    rig.activate(&order2, &dir2, replaces(&dir1, &order1))
        .unwrap();
    assert_eq!(rig.mac.enabled("svc.worker"), Some(true));
    rig.mac.clear_calls();
    rig.activate(&order1, &dir1, replaces(&dir2, &order2))
        .unwrap();
    assert_eq!(
        rig.mac.calls_of(Tool::Launchctl),
        [["enable", "system/svc"], ["disable", "system/svc.worker"]]
    );
    assert_eq!(rig.mac.enabled("svc.worker"), Some(false));
}

/// Proves that an `activate` with nothing replaced disables nothing, though the host already
/// holds a version whose run named a label this one's does not.
#[test]
fn an_activate_with_nothing_replaced_disables_nothing() {
    let (i, dir2, order2) = Installed::updated("jobs-no-from");
    i.rig.mac.clear_calls();
    i.rig.activate(&order2, &dir2, None).unwrap();
    assert_eq!(
        i.rig.mac.calls_of(Tool::Launchctl),
        [["enable", "system/svc"]]
    );
}

/// Proves that a label the replaced version's `run` names twice is disabled once.
#[test]
fn a_label_the_previous_run_names_twice_is_disabled_once() {
    let i = installed("jobs-twice");
    let (dir2, order2) = i.stage_v2();
    let worker = Label::parse("svc.worker").unwrap();
    let ran = [worker.clone(), worker];
    i.rig.mac.clear_calls();
    i.rig
        .activate(
            &order2,
            &dir2,
            Some(Replaced {
                dir: &i.dir1,
                run: &ran,
            }),
        )
        .unwrap();
    assert_eq!(
        i.rig.mac.calls_of(Tool::Launchctl),
        [["enable", "system/svc"], ["disable", "system/svc.worker"]]
    );
}

/// Proves that `enable` runs `launchctl enable system/<label>` alone, and that a `launchctl`
/// that fails or cannot run is `installer_error` or `io_error` as in an `activate`.
#[test]
fn enable_runs_launchctl_enable_for_the_label_and_reports_a_failure() {
    let rig = Rig::new("jobs-enable");
    let label = Label::parse("svc.worker").unwrap();
    rig.pkg().enable(&label, Duration::from_secs(5)).unwrap();
    assert_eq!(
        rig.mac.calls_of(Tool::Launchctl),
        [["enable", "system/svc.worker"]]
    );
    assert_eq!(rig.mac.enabled("svc.worker"), Some(true));
    rig.mac.fault(
        Tool::Launchctl,
        &["enable"],
        Fault::Output(ToolOutput::exit(1, "no such service")),
    );
    let err = rig
        .pkg()
        .enable(&label, Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(err.reason(), Reason::InstallerError);
    let rig = Rig::new("jobs-enable-spawn");
    rig.mac.fault(Tool::Launchctl, &["enable"], Fault::Spawn);
    let err = rig
        .pkg()
        .enable(&label, Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(err.reason(), Reason::IoError);
}
