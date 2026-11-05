//! An `update` places its package at the points of design sections 4.7 and 5.2: the paths are
//! checked before any step, written `pending` and linked, with `current` switched, immediately
//! before `cutover`, committed and the paths the release no longer lists removed after `cleanup`,
//! and put back on the release replaced immediately before `rollback`.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::place::hooked_switch;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Params, Step, Verdict};
use hostkeeper_placed::testing::{Event, FaultFs, FsWrite, UnitCall};
use hostkeeper_placed::{PathState, PlacedRecord};
use hostkeeper_steps::{Bounds, Staged, Toward, UpdateOrder};
use support::{OWN, Rig, Tap, config, d, install, roots, update};

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// `web` at 1.0.0, placing `web-old.service`, installed; and 2.0.0, which places
/// `web-new.service` and not `web-old.service`. The daemon's launcher marks each step in the
/// placed log, which is empty once 1.0.0 is installed.
fn installed(rig: &Rig) -> Daemon {
    rig.add_units(
        "web",
        "1.0.0",
        &["web.service", "web-old.service"],
        &["web.service"],
        &[],
    );
    rig.add_units(
        "web",
        "2.0.0",
        &["web.service", "web-new.service"],
        &["web.service"],
        &[],
    );
    let ports = hostkeeper_daemon::Ports {
        launcher: Tap::new(rig.launcher.clone(), rig.placed_log.clone()),
        ..rig.ports()
    };
    let daemon = Daemon::new(config(), rig.dirs(), ports, OWN);
    assert_eq!(daemon.order(&install("web", "1.0.0", &d(), 1)), succeeded());
    rig.placed_log.clear();
    daemon
}

fn link(rig: &Rig, link: &str) -> Option<String> {
    fs::read_link(rig.packages.join("web").join(link))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

/// The version and the state of the placed path `path`, `None` when it is not recorded.
fn recorded(record: &PlacedRecord, path: &str) -> Option<(String, PathState)> {
    record
        .paths
        .get(Path::new(path))
        .map(|entry| (entry.version.clone(), entry.state))
}

fn mark(text: &str) -> impl Fn(&Event) -> bool {
    let wanted = Event::Mark(text.to_owned());
    move |event| *event == wanted
}

fn renamed_to(path: &'static str) -> impl Fn(&Event) -> bool {
    move |event| matches!(event, Event::Fs(FsWrite::Rename { to, .. }) if to.as_path() == Path::new(path))
}

fn linked(path: &'static str) -> impl Fn(&Event) -> bool {
    move |event| matches!(event, Event::Fs(FsWrite::Symlink { link, .. }) if link.as_path() == Path::new(path))
}

fn removed(path: &'static str) -> impl Fn(&Event) -> bool {
    move |event| matches!(event, Event::Fs(FsWrite::Remove(at)) if at.as_path() == Path::new(path))
}

fn reloaded(event: &Event) -> bool {
    *event == Event::Unit(UnitCall::DaemonReload)
}

/// The position of the first event from `from` on that `wanted` accepts.
fn find(events: &[Event], from: usize, wanted: impl Fn(&Event) -> bool) -> usize {
    from + events[from..]
        .iter()
        .position(wanted)
        .unwrap_or_else(|| panic!("no such event after {from} in {events:#?}"))
}

/// Catches an update that places at the wrong point: the paths are only read before `prepare`;
/// before `cutover` they are recorded `pending` and the new link made, `previous` and then
/// `current` switched and `daemon-reload` run, with the old release's unit still linked and
/// nothing committed; and only after `cleanup` are they committed and the unit the release no
/// longer lists removed.
#[test]
fn a_hooked_update_places_paths_at_the_design_points() {
    let rig = Rig::new("placed-update");
    let daemon = Arc::new(installed(&rig));
    let gate = rig.launcher.hold("web", Step::Cutover);
    let running = {
        let daemon = Arc::clone(&daemon);
        thread::spawn(move || daemon.order(&update("web", "2.0.0", &d(), 2)))
    };
    gate.entered();
    // At `cutover`: `current` and `previous` switched, the new paths pending, the old still there.
    assert_eq!(link(&rig, "current").as_deref(), Some("2.0.0"));
    assert_eq!(link(&rig, "previous").as_deref(), Some("1.0.0"));
    let record = rig.placed_record();
    let pending = Some(("2.0.0".to_owned(), PathState::Pending));
    assert_eq!(recorded(&record, "/units/web-new.service"), pending);
    assert_eq!(
        recorded(&record, "/units/web-old.service"),
        Some(("1.0.0".to_owned(), PathState::Placed))
    );
    assert!(rig.there("/units/web-new.service") & rig.there("/units/web-old.service"));
    gate.release();
    assert_eq!(running.join().expect("the order"), succeeded());

    let events = rig.placed_log.events();
    let prepare = find(&events, 0, mark("run prepare"));
    assert_eq!(prepare, 0, "nothing is written before prepare: {events:#?}");
    let cutover = find(&events, 0, mark("run cutover"));
    let cleanup = find(&events, 0, mark("run cleanup"));
    let begin = find(&events, prepare, renamed_to("/placed/placed.json"));
    let new_link = find(&events, begin, linked("/units/web-new.service"));
    let previous = find(&events, new_link, renamed_to("/packages/web/previous"));
    let current = find(&events, previous, renamed_to("/packages/web/current"));
    let reload = find(&events, current, reloaded);
    assert!((begin < cutover) & (reload < cutover), "{events:#?}");
    assert!(
        events[..cutover]
            .iter()
            .all(|event| !removed("/units/web-old.service")(event)),
        "the old unit stays linked until cleanup: {events:#?}"
    );
    assert!(
        events[cutover..cleanup]
            .iter()
            .all(|event| !renamed_to("/placed/placed.json")(event)),
        "nothing is committed before cleanup: {events:#?}"
    );
    let commit = find(&events, cleanup, renamed_to("/placed/placed.json"));
    let gone = find(&events, commit, removed("/units/web-old.service"));
    assert!((cleanup < commit) & (commit < gone), "{events:#?}");

    let record = rig.placed_record();
    assert_eq!(recorded(&record, "/units/web-old.service"), None);
    assert!(
        record
            .paths
            .values()
            .all(|entry| (entry.version == "2.0.0") & (entry.state == PathState::Placed))
    );
    assert!(!record.reload_pending);
    assert!(!rig.there("/units/web-old.service") & rig.there("/units/web-new.service"));
    assert_eq!(link(&rig, "current").as_deref(), Some("2.0.0"));
}

/// Catches a failed update left on the new release's paths: when `verify` fails, `current` is
/// switched back and the release replaced is relinked (its paths recorded again, the paths only
/// the target placed removed, `daemon-reload`) before `rollback` runs, and nothing of 2.0.0 is
/// committed.
#[test]
fn a_failed_verify_puts_the_release_replaced_back_before_rollback() {
    let rig = Rig::new("placed-rollback");
    let daemon = installed(&rig);
    rig.launcher.exits("web", Step::Verify, &[1]);
    let reply = daemon.order(&update("web", "2.0.0", &d(), 2));
    assert!(
        matches!(
            reply,
            Reply::Finished {
                verdict: Verdict::Failed { .. },
                ..
            }
        ),
        "{reply:?}"
    );

    let events = rig.placed_log.events();
    let verify = find(&events, 0, mark("run verify"));
    let rollback = find(&events, verify, mark("run rollback"));
    let back = find(&events, verify, renamed_to("/packages/web/current"));
    let begin = find(&events, back, renamed_to("/placed/placed.json"));
    let new_gone = find(&events, begin, removed("/units/web-new.service"));
    let reload = find(&events, new_gone, reloaded);
    assert!(reload < rollback, "{events:#?}");

    assert_eq!(link(&rig, "current").as_deref(), Some("1.0.0"));
    let record = rig.placed_record();
    assert!(
        record
            .paths
            .values()
            .all(|entry| (entry.version == "1.0.0") & (entry.state == PathState::Placed)),
        "{record:?}"
    );
    assert_eq!(
        recorded(&record, "/units/web-old.service"),
        Some(("1.0.0".to_owned(), PathState::Placed))
    );
    assert!(rig.there("/units/web-old.service") & !rig.there("/units/web-new.service"));
    assert!(!record.reload_pending);
}

/// Catches an update checked only when it is a first install: a unit path that something the
/// record does not list occupies is `path_conflict` for an update too, refused before any step
/// ran, with nothing written, the step record as it was and the file in the way untouched.
#[test]
fn an_update_that_would_place_over_another_file_is_refused_before_any_step() {
    let rig = Rig::new("placed-conflict");
    let daemon = installed(&rig);
    let blocker = rig.host("/units/web-new.service");
    fs::write(&blocker, "mine\n").expect("a file where the unit would go");
    let ran = rig.launcher.runs().len();
    let record = rig.record("web");
    let reply = daemon.order(&update("web", "2.0.0", &d(), 2));
    assert_eq!(reply.refusal(), Some(Refusal::PathConflict), "{reply:?}");
    assert_eq!(rig.launcher.runs().len(), ran);
    assert!(rig.placed_log.events().is_empty());
    assert_eq!(rig.record("web"), record);
    assert_eq!(fs::read_to_string(&blocker).expect("kept"), "mine\n");
    assert_eq!(link(&rig, "current").as_deref(), Some("1.0.0"));
}

/// An order for the switch to call, for `web` at `target`, replacing `previous` (a version of
/// `web` on disk) and saying `previous_version`.
fn order(
    rig: &Rig,
    target: &str,
    previous_version: Option<&str>,
    previous: Option<&str>,
) -> UpdateOrder {
    let sealed = |version: &str| {
        Staged::seal(&rig.packages.join("web").join(version)).expect("a sealed version")
    };
    UpdateOrder {
        service: "web".to_owned(),
        target_version: target.to_owned(),
        previous_version: previous_version.map(str::to_owned),
        artifact_digest: d(),
        staged: sealed("2.0.0"),
        previous: previous.map(sealed),
        hook_binary: PathBuf::from("bin/hook"),
        bounds: Bounds {
            prepare_secs: 1,
            cutover_secs: 1,
            verify_secs: 1,
            rollback_secs: 1,
            cleanup_secs: 1,
            verify_deadline_secs: 1,
            promotion_ceiling_secs: 1,
            cleanup_retries: 0,
        },
        declared_params: Params::default(),
        min_updater_protocol: None,
        order_try: None,
    }
}

/// Catches an order the switch cannot read being placed anyway: an order that names half of the
/// release it replaces, a service or version that is no name, and a version directory whose
/// manifest is not the one the order names, each stop with the reason and write nothing.
#[test]
fn the_switch_refuses_an_order_it_cannot_read_and_writes_nothing() {
    let rig = Rig::new("placed-bad-order");
    let _daemon = installed(&rig);
    let switch = hooked_switch(roots(), rig.placed_fs.clone(), rig.units.clone(), OWN);
    let toward = [Toward::Target, Toward::Back, Toward::Confirmed];

    let half = "the order for web names only half of the release it replaces";
    for (version, previous) in [(None, Some("1.0.0")), (Some("1.0.0"), None)] {
        let half_order = order(&rig, "2.0.0", version, previous);
        assert_eq!(switch(Toward::Back, &half_order), Err(half.to_owned()));
    }

    let name_error = ServiceName::new("A b").expect_err("not a name").to_string();
    let version_error = Version::new("x y").expect_err("not a version").to_string();
    for (shape, previous) in [(None, None), (Some("1.0.0"), Some("1.0.0"))] {
        let mut bad_service = order(&rig, "2.0.0", shape, previous);
        bad_service.service = "A b".to_owned();
        let bad_version = order(&rig, "x y", shape, previous);
        for toward in toward {
            assert_eq!(switch(toward, &bad_service), Err(name_error.clone()));
            assert_eq!(switch(toward, &bad_version), Err(version_error.clone()));
        }
    }

    let load_error = |dir: &str, version: &str| {
        let expected = Expected {
            name: ServiceName::new("web").expect("a name"),
            version: Version::new(version).expect("a version"),
            platform: hostkeeper_config::platform::Platform::current(),
        };
        Manifest::load(&rig.packages.join("web").join(dir), &expected, &OWN)
            .expect_err("another version")
            .to_string()
    };
    let other = order(&rig, "3.0.0", None, None);
    let wrong_target = Err(load_error("2.0.0", "3.0.0"));
    assert_eq!(switch(Toward::Target, &other), wrong_target);
    assert_eq!(switch(Toward::Confirmed, &other), wrong_target);
    let wrong_previous = order(&rig, "2.0.0", Some("9.9.9"), Some("1.0.0"));
    assert_eq!(
        switch(Toward::Back, &wrong_previous),
        Err(load_error("1.0.0", "9.9.9"))
    );
    assert!(rig.placed_log.events().is_empty());
}

/// Catches a `Back` that goes on past a previous version it cannot read: a `previous_version`
/// that is no version name stops it with the version grammar's reason, before anything is written.
#[test]
fn a_back_with_a_previous_version_that_is_no_name_writes_nothing() {
    let rig = Rig::new("placed-bad-previous");
    let _daemon = installed(&rig);
    let switch = hooked_switch(roots(), rig.placed_fs.clone(), rig.units.clone(), OWN);
    let version_error = Version::new("x y").expect_err("not a version").to_string();
    let bad = order(&rig, "2.0.0", Some("x y"), Some("1.0.0"));
    assert_eq!(switch(Toward::Back, &bad), Err(version_error));
    assert!(rig.placed_log.events().is_empty());
    assert_eq!(link(&rig, "current").as_deref(), Some("1.0.0"));
}

/// Catches a `Back` that relinks the release replaced after `current` could not be put back on
/// it: with the first write failing, the switch is an error naming the failed write, `current`
/// still names the target and `placed.json` is as it was.
#[test]
fn a_back_whose_current_cannot_be_switched_changes_nothing() {
    let rig = Rig::new("placed-back-fault");
    let _daemon = installed(&rig);
    let order = order(&rig, "2.0.0", Some("1.0.0"), Some("1.0.0"));
    let switch = hooked_switch(roots(), rig.placed_fs.clone(), rig.units.clone(), OWN);
    assert_eq!(switch(Toward::Target, &order), Ok(()));
    assert_eq!(link(&rig, "current").as_deref(), Some("2.0.0"));
    let record = rig.placed_record();

    let faulty = Arc::new(FaultFs::failing_at(
        rig.scratch.path(),
        1,
        rig.placed_log.clone(),
    ));
    let failing = hooked_switch(roots(), faulty.clone(), rig.units.clone(), OWN);
    let why = failing(Toward::Back, &order).expect_err("the first write fails");
    assert!(why.contains("injected failure of write 1"), "{why}");
    assert_eq!(faulty.writes(), 1);
    assert_eq!(link(&rig, "current").as_deref(), Some("2.0.0"));
    assert_eq!(rig.placed_record(), record);
}

/// Catches claims taken from a manifest that loads but places a unit that starts a package file
/// not through `current`: `Target` and `Confirmed` each stop with the lint's reason, before any
/// write, and `placed.json` and `current` are as they were.
#[test]
fn a_target_whose_unit_runs_a_package_file_outside_current_is_refused_unwritten() {
    let rig = Rig::new("placed-bad-claims");
    let _daemon = installed(&rig);
    let unit = rig.packages.join("web/2.0.0/units/web-new.service");
    let text = rig.unit_text("web").replace("/current/", "/2.0.0/");
    fs::write(&unit, text).expect("a unit that names its version directory");
    let record = rig.placed_record();
    let switch = hooked_switch(roots(), rig.placed_fs.clone(), rig.units.clone(), OWN);
    let order = order(&rig, "2.0.0", Some("1.0.0"), Some("1.0.0"));
    for toward in [Toward::Target, Toward::Confirmed] {
        let why = switch(toward, &order).expect_err("the unit is refused");
        assert!(
            why.starts_with(
                "units/web-new.service ExecStart= names \"/packages/web/2.0.0/bin/hook"
            ),
            "{why}"
        );
    }
    assert!(rig.placed_log.events().is_empty());
    assert_eq!(rig.placed_record(), record);
    assert_eq!(link(&rig, "current").as_deref(), Some("1.0.0"));
}
