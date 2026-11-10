//! The unit paths an archive version left (docs/design/packages-native.md, section 10.6): the
//! native `activate` of a moved service takes them away, reloads systemd and checks that systemd
//! loads the package's own unit; the start removes a dangling unit link an earlier move left.
//! Against the fake host, whose `systemctl show` reads the rig's unit directory `/units` with a
//! link's semantics (a dangling link there is no unit; a file or a live link hides the package's).

mod support;
mod support_apt;
mod support_native;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_daemon::boot::{ALERT_START, alert_line};
use hostkeeper_daemon::own_package::OwnActivate;
use hostkeeper_daemon::start::startup;
use hostkeeper_daemon::unit_links::repair;
use hostkeeper_hook::Class;
use hostkeeper_install::repository::ToolOutput;
use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, Log as PlacedLog, UnitCall};
use hostkeeper_steps::Settled;
use hostkeeper_steps::record::Activated;
use serde_json::{Value, json};
use support::selfupdate::{HK, awaiting};
use support::{alerting, lines};
use support_apt::seen::install_of;
use support_apt::{AptRig, SERVICE, package};
use support_native::{NativeRig, V1, V2, install_hk, pv};

const HK_UNIT: &str = "/units/hostkeeper.service";
const HK_WANTS: &str = "/units/multi-user.target.wants/hostkeeper.service";

/// The archive's link target of `service`'s unit `file`, under the rig's packages directory.
fn archive_target(service: &str, file: &str) -> String {
    format!("/packages/{service}/current/units/{file}")
}

/// Adds `paths`, as the release before the native installers wrote them, to `placed.json`.
fn seed_paths(rig: &support::Rig, paths: Value) {
    let path = rig.placed_record_path();
    let mut record: Value = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).expect("json"),
        Err(_) => json!({"schema": 1, "uninstalling": {}}),
    };
    record["paths"] = paths;
    fs::create_dir_all(path.parent().expect("a parent")).expect("the state dir");
    fs::write(&path, serde_json::to_vec_pretty(&record).expect("json")).expect("placed.json");
}

fn entry(service: &str, kind: &str, link_target: &str) -> Value {
    json!({"service": service, "version": "0.1.240", "kind": kind, "state": "placed", "link_target": link_target})
}

/// The `paths` of `placed.json`.
fn paths_of(rig: &support::Rig) -> Value {
    let bytes = fs::read(rig.placed_record_path()).expect("placed.json");
    serde_json::from_slice::<Value>(&bytes).expect("json")["paths"].clone()
}

/// Makes a link at the rig's host path `at` to `target`.
fn link(rig: &support::Rig, target: &str, at: &str) {
    let disk = rig.host(at);
    fs::create_dir_all(disk.parent().expect("a parent")).expect("the directory");
    symlink(target, disk).expect("the link");
}

/// A host that ran hostkeeper [`V1`] from its archive and whose new binary [`V2`], installed
/// from its `.deb`, confirmed itself and owes the `activate` of its package; `placed.json` still
/// lists the archive's unit and `WantedBy=` link, and the unit link dangles, as the move left it.
fn moved_hostkeeper(name: &str) -> NativeRig {
    let nr = NativeRig::new(name);
    nr.host.set_installed(HK, &pv(V1), 'i', false);
    drop(nr.start(V1));
    let pin = nr.publish(V2);
    assert_eq!(
        nr.daemon_at(V1).order(&install_hk(V2, &pin, 1)),
        awaiting(false)
    );
    assert_eq!(
        nr.daemon_at(V2).settle_self_update().expect("settles"),
        Settled::Confirmed
    );
    let target = archive_target(HK, "hostkeeper.service");
    seed_paths(
        &nr.rig,
        json!({
            HK_UNIT: entry(HK, "unit", &target),
            HK_WANTS: entry(HK, "wants_link", HK_UNIT),
        }),
    );
    link(&nr.rig, &target, HK_UNIT);
    link(&nr.rig, HK_UNIT, HK_WANTS);
    nr
}

/// Catches the move of hostkeeper leaving its archive's unit link in the unit directory (the
/// link dangles and hides the `.deb`'s unit: systemd finds none, the unit never loads, and the
/// daemon reports its own service absent): the `activate` the new binary owes removes the unit
/// link and the archive's `WantedBy=` link, reloads systemd, installs the package, finds
/// hostkeeper's unit loaded from the package, and drops the paths from `placed.json`.
#[test]
fn hostkeepers_move_takes_away_the_archives_unit_link_and_its_unit_loads_from_the_package() {
    let nr = moved_hostkeeper("units-own-move");
    nr.rig.placed_log.clear();
    let from = nr.calls();

    assert_eq!(
        nr.daemon_at(V2).activate_own_package(),
        Some(OwnActivate::Ended(Activated::Succeeded { reason: None }))
    );

    assert!(!nr.rig.there(HK_UNIT) & !nr.rig.there(HK_WANTS));
    assert_eq!(paths_of(&nr.rig), json!({}));
    assert_eq!(nr.linked("current").as_deref(), Some(V2));
    let labels = nr.labels_since(from);
    let install = labels.iter().position(|l| l == "apt-get install");
    let show = labels.iter().position(|l| l == "systemctl show");
    assert!(install.is_some() & (install < show), "{labels:?}");
    let shown = nr.host.calls_of("systemctl show");
    assert_eq!(shown.len(), 1);
    assert_eq!(
        shown[0].run.args.last().map(String::as_str),
        Some("hostkeeper.service")
    );
    let reloads = nr
        .rig
        .placed_log
        .events()
        .into_iter()
        .filter(|e| *e == Event::Unit(UnitCall::DaemonReload))
        .count();
    assert_eq!(reloads, 1, "the take-away's reload");
}

/// Catches an `activate` that ends `Succeeded` while systemd still does not load the package's
/// unit: it ends `Failed`, of class environment, `unit_not_loaded`; `current` is not switched,
/// and `placed.json` keeps the archive's paths, so the next `activate` checks again.
#[test]
fn a_unit_systemd_does_not_load_from_the_package_fails_the_activate_and_keeps_the_paths() {
    let nr = moved_hostkeeper("units-own-hidden");
    nr.host.answer(
        "systemctl show",
        0,
        Ok(ToolOutput {
            code: Some(0),
            stdout: b"LoadState=loaded\nFragmentPath=/run/systemd/transient/hostkeeper.service\n"
                .to_vec(),
            stderr: Vec::new(),
            cut: false,
        }),
    );

    assert_eq!(
        nr.daemon_at(V2).activate_own_package(),
        Some(OwnActivate::Ended(Activated::Failed {
            class: Class::Environment,
            reason: "unit_not_loaded".to_owned(),
        }))
    );

    assert_ne!(nr.linked("current").as_deref(), Some(V2));
    assert_eq!(paths_of(&nr.rig)[HK_UNIT]["kind"], "unit");
    assert!(!nr.rig.there(HK_UNIT));
}

/// Catches a service's native install over its archive leaving the archive's unit link (the
/// move of a service through the switch, not hostkeeper's own): the link is gone, the unit is
/// checked, and the record drops the path.
#[test]
fn a_service_installed_over_its_archive_paths_has_them_taken_away() {
    let apt = AptRig::new("units-service-move");
    let unit = "/units/web.service";
    let target = archive_target(SERVICE, "web.service");
    seed_paths(&apt.rig, json!({ unit: entry(SERVICE, "unit", &target) }));
    link(&apt.rig, &target, unit);
    let pin = apt.publish(package("2.0.0"));

    assert_eq!(
        apt.daemon().order(&install_of("2.0.0", "2.0.0", &pin, 1)),
        support_apt::seen::succeeded()
    );

    assert!(!apt.rig.there(unit));
    assert_eq!(paths_of(&apt.rig), json!({}));
    assert_eq!(apt.host.calls_of("systemctl show").len(), 1);
}

/// Catches an install with no archive paths asking systemd about its units: there is nothing to
/// check, and the record is not given a `paths` key.
#[test]
fn an_install_with_no_archive_paths_asks_systemd_nothing() {
    let apt = AptRig::new("units-no-paths");
    let pin = apt.publish(package("2.0.0"));

    assert_eq!(
        apt.daemon().order(&install_of("2.0.0", "2.0.0", &pin, 1)),
        support_apt::seen::succeeded()
    );

    assert_eq!(apt.host.calls_of("systemctl show").len(), 0);
    assert_eq!(paths_of(&apt.rig), Value::Null);
}

/// A host that runs `web` natively, installed by an order through `apt`.
fn native_web(name: &str) -> AptRig {
    let apt = AptRig::new(name);
    let pin = apt.publish(package("2.0.0"));
    assert_eq!(
        apt.daemon().order(&install_of("2.0.0", "2.0.0", &pin, 1)),
        support_apt::seen::succeeded()
    );
    apt
}

/// Catches a host an earlier release moved staying broken across restarts: the start removes
/// the dangling link into the native service's package directory before the boot pass, reloads
/// systemd and records it; a link an operator placed that points elsewhere, a link into the
/// package directory of a service with no native installer, and a link whose target is there
/// stay as they are.
#[test]
fn the_start_removes_a_dangling_unit_link_of_a_native_service_and_leaves_the_rest() {
    let apt = native_web("units-start-repair");
    let rig = &apt.rig;
    let dangling = "/units/web.service";
    let operator = "/units/custom.service";
    let other = "/units/other.service";
    let resolving = "/units/web-tools.service";
    link(rig, &archive_target(SERVICE, "web.service"), dangling);
    link(rig, "/opt/units/custom.service", operator);
    link(rig, &archive_target("other", "other.service"), other);
    let program = format!("/packages/{SERVICE}/current/tree/usr/bin/web");
    assert!(
        rig.host(&program).is_file(),
        "the native tree has the program"
    );
    link(rig, &program, resolving);
    rig.placed_log.clear();

    let started = startup(&rig.settings(), apt.ports(), support::OWN).expect("the daemon starts");

    assert_eq!(started.boot_report().unit_links, [PathBuf::from(dangling)]);
    assert!(!rig.there(dangling));
    for kept in [operator, other, resolving] {
        assert!(rig.there(kept), "{kept}");
    }
    let events = rig.placed_log.events();
    let reload = events
        .iter()
        .position(|e| *e == Event::Unit(UnitCall::DaemonReload));
    assert!(reload.is_some(), "{events:?}");
}

/// Catches the repair's one log line missing or saying something else, and a second start
/// removing or logging anything.
#[test]
fn the_repair_says_what_it_removed_in_one_line_and_a_repeat_does_nothing() {
    let apt = native_web("units-repair-log");
    link(
        &apt.rig,
        &archive_target(SERVICE, "web.service"),
        "/units/web.service",
    );
    let (daemon, log) = alerting(apt.daemon());

    assert_eq!(repair(daemon.ctx()), [PathBuf::from("/units/web.service")]);
    assert_eq!(
        lines(&log),
        [
            "hostkeeper daemon: level=info removed the dangling unit links an earlier move left, \
          then reloaded the service manager: /units/web.service"
        ]
    );

    assert_eq!(repair(daemon.ctx()), Vec::<PathBuf>::new());
    assert_eq!(lines(&log).len(), 1);
}

/// Catches a dangling link removed for a host that runs no service natively: with no native
/// installer recorded, the unit directory is not read.
#[test]
fn a_host_with_no_native_service_repairs_nothing() {
    let apt = AptRig::new("units-no-native");
    let dangling = "/units/web.service";
    link(&apt.rig, &archive_target(SERVICE, "web.service"), dangling);
    let (daemon, log) = alerting(apt.daemon());

    assert_eq!(repair(daemon.ctx()), Vec::<PathBuf>::new());

    assert!(apt.rig.there(dangling));
    assert_eq!(lines(&log), Vec::<String>::new());
}

/// Catches a repair whose failure stops the start or passes unsaid: a `placed.json` that cannot
/// be read, and a link that cannot be removed, are each the alert `start_incomplete`, and the
/// repair removes nothing.
#[test]
fn a_repair_that_cannot_run_is_an_alert() {
    let apt = native_web("units-repair-alerts");
    let dangling = "/units/web.service";
    link(&apt.rig, &archive_target(SERVICE, "web.service"), dangling);

    let fs = Arc::new(FaultFs::failing_at(
        apt.rig.scratch.path(),
        1,
        PlacedLog::new(),
    ));
    let units = Arc::new(FakeUnitControl::new(PlacedLog::new()));
    let (daemon, log) = alerting(apt.rig.daemon_with(fs, units));
    assert_eq!(repair(daemon.ctx()), Vec::<PathBuf>::new());
    let alerts = lines(&log);
    assert_eq!(alerts.len(), 1, "{alerts:?}");
    assert!(
        alerts[0].starts_with(&alert_line(
            ALERT_START,
            "the dangling unit links were not all removed: cannot remove /units/web.service: "
        )),
        "{alerts:?}"
    );
    assert!(apt.rig.there(dangling));

    fs::write(apt.rig.placed_record_path(), "not json").expect("a broken record");
    let (daemon, log) = alerting(apt.daemon());
    assert_eq!(repair(daemon.ctx()), Vec::<PathBuf>::new());
    let alerts = lines(&log);
    assert!(
        alerts.len() == 1
            && alerts[0].starts_with(&alert_line(
                ALERT_START,
                "placed.json cannot be read, so no dangling unit link is removed: "
            )),
        "{alerts:?}"
    );
    assert!(apt.rig.there(dangling));
}

/// The `activate` hostkeeper's new binary owes on a [`moved_hostkeeper`] host, its placement
/// through `fs`, and the writes `fs` counted.
fn activate_through(nr: &NativeRig, fs: Arc<FaultFs>) -> (Option<OwnActivate>, u64) {
    let ended = nr
        .daemon_with_fs(V2, Arc::clone(&fs))
        .activate_own_package();
    (ended, fs.writes())
}

/// A filesystem over `nr`'s root whose `n`-th write and every later one fail.
fn failing(nr: &NativeRig, n: u64) -> Arc<FaultFs> {
    Arc::new(FaultFs::failing_at(&nr.root(), n, PlacedLog::new()))
}

/// The detail of an `activate` that is tried again; panics on any other end.
fn deferred(ended: Option<OwnActivate>) -> (String, String) {
    match ended {
        Some(OwnActivate::Deferred { reason, detail }) => (reason, detail),
        other => panic!("not deferred: {other:?}"),
    }
}

/// Catches a take-away that fails read as done, or the install run after it: the `activate` is
/// tried again (`io_error`, naming the archive's paths), apt installs nothing, and the link is
/// still there.
#[test]
fn a_take_away_that_fails_installs_nothing_and_is_tried_again() {
    let nr = moved_hostkeeper("units-take-away-fails");
    let from = nr.calls();

    let (reason, detail) = deferred(activate_through(&nr, failing(&nr, 1)).0);

    assert_eq!(reason, "io_error");
    assert!(
        detail.starts_with(
            "the paths of the archive version: cannot remove /units/hostkeeper.service: "
        ),
        "{detail}"
    );
    assert!(!nr.labels_since(from).iter().any(|l| l == "apt-get install"));
    assert!(nr.rig.there(HK_UNIT));
}

/// Catches an install that defers after the take-away leaving the service with no unit systemd
/// knows: the archive's unit link and `WantedBy=` link are put back as they were, and
/// `placed.json` still lists them for the next try.
#[test]
fn an_install_that_defers_puts_the_archives_links_back() {
    let nr = moved_hostkeeper("units-install-defers");
    nr.host.edit(|s| {
        s.faults.busy.insert("install", 1000);
    });

    let (reason, _) = deferred(nr.daemon_at(V2).activate_own_package());

    assert_eq!(reason, "package_manager_busy");
    let target = |at: &str| fs::read_link(nr.rig.host(at)).ok();
    assert_eq!(
        target(HK_UNIT),
        Some(PathBuf::from(archive_target(HK, "hostkeeper.service")))
    );
    assert_eq!(target(HK_WANTS), Some(PathBuf::from(HK_UNIT)));
    assert_eq!(paths_of(&nr.rig)[HK_UNIT]["kind"], "unit");
}

/// Catches a put-back that fails passed over: the installer's reason stays, and its detail says
/// the links were not put back.
#[test]
fn a_put_back_that_fails_is_said_in_the_detail() {
    let busy = |nr: &NativeRig| {
        nr.host.edit(|s| {
            s.faults.busy.insert("install", 1000);
        });
    };
    let twin = moved_hostkeeper("units-put-back-twin");
    busy(&twin);
    let (_, writes) = activate_through(&twin, failing(&twin, u64::MAX));
    let nr = moved_hostkeeper("units-put-back-fails");
    busy(&nr);

    // The put-back writes a link and syncs its directory for each of the two links: its first
    // write is the fourth from the end.
    let (reason, detail) = deferred(activate_through(&nr, failing(&nr, writes - 3)).0);

    assert_eq!(reason, "package_manager_busy");
    assert!(
        detail.contains(
            "; the archive's links were not put back: cannot link /units/hostkeeper.service: "
        ),
        "{detail}"
    );
}

/// Catches a forget that fails read as done, or `current` switched after it: the `activate` is
/// tried again (`io_error`, naming the archive's paths), and `current` does not name the new
/// version.
#[test]
fn a_forget_that_fails_switches_nothing_and_is_tried_again() {
    let twin = moved_hostkeeper("units-forget-twin");
    let (ended, writes) = activate_through(&twin, failing(&twin, u64::MAX));
    assert_eq!(
        ended,
        Some(OwnActivate::Ended(Activated::Succeeded { reason: None }))
    );
    let nr = moved_hostkeeper("units-forget-fails");

    // After the forget (a write, its sync, a rename and the directory's sync), the switch of
    // `current` writes a link, renames it and syncs its directory: the forget's first write is
    // the seventh from the end.
    let (reason, detail) = deferred(activate_through(&nr, failing(&nr, writes - 6)).0);

    assert_eq!(reason, "io_error");
    assert!(
        detail.starts_with("the paths of the archive version: cannot write "),
        "{detail}"
    );
    assert_ne!(nr.linked("current").as_deref(), Some(V2));
}

/// Catches the start's repair reaching a service whose installer is not native: a dangling link
/// into the package directory of a service recorded with the archive installer stays, and
/// nothing reloads.
#[test]
fn the_repair_leaves_a_service_recorded_with_a_non_native_installer() {
    let apt = native_web("units-non-native");
    let path = apt.rig.placed_record_path();
    let mut record: Value =
        serde_json::from_slice(&fs::read(&path).expect("placed.json")).expect("json");
    record["installed"]["other"] = json!({"installer": "hostkeeper-archive"});
    fs::write(&path, serde_json::to_vec_pretty(&record).expect("json")).expect("placed.json");
    let dangling = "/units/other.service";
    link(
        &apt.rig,
        &archive_target("other", "other.service"),
        dangling,
    );
    apt.rig.placed_log.clear();

    assert_eq!(repair(apt.daemon().ctx()), Vec::<PathBuf>::new());

    assert!(apt.rig.there(dangling));
    assert!(
        !apt.rig
            .placed_log
            .events()
            .contains(&Event::Unit(UnitCall::DaemonReload))
    );
}
