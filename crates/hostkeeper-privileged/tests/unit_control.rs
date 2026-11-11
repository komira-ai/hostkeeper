#![cfg(feature = "privileged")]
//! The Linux adapters' unit control as the daemon builds it ([`Adapters::for_platform`]:
//! `systemctl` as a child process of the daemon), as root against the host's systemd and a unit
//! of the hook fixture linked from the test's scratch directory: `start_unit` starts it and
//! `is_active` reports it active, `daemon_reload` makes systemd read its changed unit file, then
//! `stop_unit` stops it and `is_active` reports it not active; a start of a unit systemd does not
//! know is an error naming the command and systemd's exit 5 for a unit it has not loaded, and a
//! stop of that unit is no error.

mod support;

use hostkeeper_daemon::adapters::Adapters;
use hostkeeper_launchd::Platform;
use hostkeeper_privileged::rig::service_unit;
use hostkeeper_steps::systemd::SYSTEMCTL;
use support::Bench;

/// Catches a Linux unit control that does not reach the host's systemd (the unwired one refused
/// every call), reads another unit, or takes a failed command for done.
#[test]
fn the_linux_unit_control_reloads_starts_and_stops_a_real_unit() {
    let bench = Bench::new("unit_control");
    let rig = &bench.rig;
    let adapters = Adapters::for_platform(&Platform::linux()).expect("the Linux set");
    let units = &adapters.units;

    let unit = rig.unit();
    let service = service_unit(&unit);
    let file = rig.unit_file(&unit).expect("write the unit file");
    let path = file.to_str().expect("a UTF-8 path");
    support::run_ok(SYSTEMCTL, &["link", "--system", path]);

    assert_eq!(units.is_active(&service), Ok(false), "linked, not started");
    units.start_unit(&service).expect("start");
    assert_eq!(units.is_active(&service), Ok(true));
    assert_eq!(support::property(&unit, "ActiveState"), "active");
    // A unit that is already active starts without error.
    units.start_unit(&service).expect("start again");

    // The unit file of the running unit changes on disk; systemd reads it again only at the
    // reload. (A unit that is not running may be unloaded at any time, and is read afresh when
    // asked about, so the change is made while it runs.)
    let text = rig.unit_file_text().expect("the unit file text");
    let changed = text.replace("Description=", "Description=reloaded ");
    assert_ne!(changed, text, "the unit file has a Description= line");
    std::fs::write(&file, changed).expect("rewrite the unit file");
    assert_eq!(support::property(&unit, "NeedDaemonReload"), "yes");
    units.daemon_reload().expect("daemon-reload");
    assert_eq!(support::property(&unit, "NeedDaemonReload"), "no");
    assert!(
        support::property(&unit, "Description").starts_with("reloaded "),
        "systemd read the changed unit file"
    );
    assert_eq!(
        units.is_active(&service),
        Ok(true),
        "a reload does not stop it"
    );

    units.stop_unit(&service).expect("stop");
    assert_eq!(units.is_active(&service), Ok(false));
    assert_eq!(support::property(&unit, "ActiveState"), "inactive");
    // A unit that is not running stops without error.
    units.stop_unit(&service).expect("stop again");

    let nowhere = service_unit(&rig.unit());
    let refused = units.start_unit(&nowhere).expect_err("no such unit");
    assert_eq!(refused.action, format!("systemctl start -- {nowhere}"));
    assert_eq!(refused.why, "exit status 5", "{refused}");
    assert_eq!(units.is_active(&nowhere), Ok(false));
    // Nothing of a unit systemd has not loaded runs: the rollback of a first install whose
    // package was never installed stops it without error.
    assert_eq!(units.stop_unit(&nowhere), Ok(()));
}
