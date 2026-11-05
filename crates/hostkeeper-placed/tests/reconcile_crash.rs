//! Start-up reconciliation of placed paths after a crash or a refusal (`Placer::reconcile`): a
//! crash between any two writes of an update leaves only listed links and is finished or undone
//! at start, a reload owed in the record is run once, a failed reload stays owed, a switch is
//! made only after everything is judged, contradictory intents are refused before a write, and a
//! read that fails once is an error and never a missed path.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_placed::placer::is_temporary;
use hostkeeper_placed::testing::{FakeUnitControl, FaultFs, Log, UnitCall};
use hostkeeper_placed::{
    BootView, Conflict, Owner, PathState, PlacedError, Placer, ReconcileReport, Side, SwitchIntent,
    Unresolved,
};
use support::{Spec, World, install, service, version};

const PKG: &str = "/var/lib/hostkeeper/packages/tierstore";
const A: &str = "/etc/tierstore/a.conf";
const B: &str = "/etc/tierstore/b.conf";
const C: &str = "/etc/tierstore/c.conf";
const RUN_UNIT: &str = "tierstore@9301.service";

/// 1.4.2: the files a.conf and b.conf.
fn old_spec() -> Spec {
    Spec::new().file("files/a.conf", A).file("files/b.conf", B)
}

/// 1.5.0: the files a.conf and c.conf, and a unit with a run instance.
fn new_spec() -> Spec {
    Spec::new()
        .unit(
            "tierstore@.service",
            &support::unit_text(
                "/var/lib/hostkeeper/packages/tierstore/current/bin/tierstore serve --port %i",
            ),
        )
        .run(RUN_UNIT)
        .file("files/a.conf", A)
        .file("files/c.conf", C)
}

fn view(intents: Vec<SwitchIntent>) -> BootView {
    BootView {
        intents,
        running: support::RUNNING,
    }
}

fn intent(from: Option<&str>, target: &str, side: Side) -> SwitchIntent {
    SwitchIntent {
        service: service("tierstore"),
        from: from.map(version),
        target: version(target),
        side,
    }
}

/// Creates the lock file, so that a reconciliation that touches nothing leaves the tree as it was.
fn touch_lock(world: &World) {
    fs::write(world.host("/var/lib/hostkeeper/placed.lock"), b"").expect("create the lock file");
}

fn assert_resolves_into(world: &World, path: &Path, ver: &str) {
    let at = world
        .resolve(path)
        .unwrap_or_else(|| panic!("{path:?} does not resolve"));
    assert!(
        at.starts_with(format!("{PKG}/{ver}")),
        "{path:?} resolves to {at:?}, not into {ver}"
    );
}

/// Asserts that `placed.json` parses and that every link outside the package directories is
/// listed in it, with the target the link has: whichever write a crash fell after, no link is
/// unlisted. A temporary name is not a placed link.
fn assert_links_listed(world: &World) {
    let record = world.record();
    for link in world.placed_links() {
        let name = link.file_name().expect("a name").to_string_lossy();
        if is_temporary(&name) {
            continue;
        }
        let entry = record
            .paths
            .get(&link)
            .unwrap_or_else(|| panic!("{link:?} is a link the record does not list"));
        assert_eq!(
            world.read_link(&link),
            Some(entry.link_target.clone()),
            "{link:?} has a target other than the recorded one"
        );
    }
}

/// Asserts the host is settled on `chosen` of tierstore: every path its manifest places resolves
/// into it, `current` names it, the record lists exactly those paths all placed with no reload
/// owed, every placed link is listed, and no temporary name remains.
fn settled(world: &World, chosen: &str) {
    let placer = world.placer();
    let manifest = world.load("tierstore", chosen);
    let desired = placer.desired(&manifest).expect("desired paths");
    for claim in desired.iter() {
        assert_resolves_into(world, &claim.path, chosen);
    }
    assert_eq!(
        placer.current("tierstore").expect("current"),
        Some(chosen.to_owned())
    );
    let record = world.record();
    assert_eq!(
        record.paths.keys().cloned().collect::<BTreeSet<_>>(),
        desired.paths()
    );
    assert!(
        record
            .paths
            .values()
            .all(|entry| entry.state == PathState::Placed),
        "a pending entry remains: {record:?}"
    );
    assert!(!record.reload_pending, "a reload is still owed");
    assert_links_listed(world);
    for path in world.snapshot().keys() {
        let name = path.file_name().expect("a name").to_string_lossy();
        assert!(!is_temporary(&name), "{path:?} remains");
    }
}

/// The steps of a hooked update to `manifest`, up to the first error.
fn update(placer: &Placer<'_>, manifest: &Manifest) -> Result<(), PlacedError> {
    let claims = placer.desired(manifest)?;
    let linking = placer.begin(&claims)?;
    placer.link(&linking)?;
    placer.switch_current(&manifest.name, &manifest.version)?;
    placer.reload()?;
    placer.commit(linking)?;
    Ok(())
}

/// 1.4.2 and 1.5.0 installed, then `current` pointed back at 1.4.2 by hand.
fn hand_switched(name: &str) -> World {
    let world = World::new(name);
    install(&world, "tierstore", "1.4.2", &old_spec());
    install(&world, "tierstore", "1.5.0", &new_spec());
    world.set_link("tierstore", "current", "1.4.2");
    world
}

/// 1.4.2 installed, then 1.5.0 with the same paths begun, linked and switched to, and the crash
/// before the reload and the commit: every link is as it was, the entries are 1.5.0 pending, and
/// a reload is owed in the record. The unit-control log is cleared.
fn crashed_before_reload(name: &str) -> World {
    let world = World::new(name);
    install(&world, "tierstore", "1.4.2", &old_spec());
    let manifest = world.package("tierstore", "1.5.0", &old_spec());
    let placer = world.placer();
    let claims = placer.desired(&manifest).expect("desired");
    let linking = placer.begin(&claims).expect("begin");
    placer.link(&linking).expect("link");
    placer
        .switch_current(&manifest.name, &manifest.version)
        .expect("switch");
    world.log.clear();
    world
}

/// The refusal the loader gives for `version` of tierstore.
fn refusal(world: &World, ver: &str) -> String {
    let expected = Expected {
        name: service("tierstore"),
        version: version(ver),
        platform: hostkeeper_config::platform::Platform::current(),
    };
    Manifest::load(
        &world.version_dir("tierstore", ver),
        &expected,
        &support::RUNNING,
    )
    .expect_err("the manifest does not load")
    .to_string()
}

/// Catches a write that is not ahead of its link, a link that is not whole, or a start-up
/// reconciliation that leaves a pending entry, an unlisted link, an owed reload or a temporary
/// name, or does not settle to the one version the side names, whichever write of the update the
/// crash fell after; and a crash that leaves a link `placed.json` does not list.
#[test]
fn a_crash_between_two_links_is_finished_or_undone_at_start() {
    for side in [Side::Forward, Side::Back] {
        let chosen = match side {
            Side::Forward => "1.5.0",
            Side::Back => "1.4.2",
        };
        let mut completed = false;
        for n in 1..200_u64 {
            let world = World::new(&format!("crash-{side:?}-{n}"));
            install(&world, "tierstore", "1.4.2", &old_spec());
            let target = world.package("tierstore", "1.5.0", &new_spec());
            let faulty = FaultFs::failing_at(world.root(), n, Log::new());
            let crashed = update(&world.placer_on(&faulty), &target);
            if crashed.is_ok() {
                assert!(faulty.writes() < n, "the update completed past write {n}");
                completed = true;
                break;
            }
            assert_links_listed(&world);
            let boot = view(vec![intent(Some("1.4.2"), "1.5.0", side)]);
            let placer = world.placer();
            placer
                .reconcile(&boot)
                .unwrap_or_else(|e| panic!("{side:?} after write {n}: {e}"));
            settled(&world, chosen);
            world.log.clear();
            let again = placer.reconcile(&boot).expect("a second reconcile");
            assert!(!again.changed(), "{side:?} after write {n}: {again:?}");
            assert!(!again.reloaded);
            assert!(world.writes().is_empty(), "{:?}", world.writes());
        }
        assert!(completed, "the update never completed for {side:?}");
    }
}

/// Catches a reconciliation that is not restartable: whichever write fails, the start fails with
/// the I/O error, every link on disk is still listed, and a second start over a healthy
/// filesystem settles the host.
#[test]
fn reconcile_stops_at_the_first_failed_write_and_a_rerun_finishes() {
    let mut completed = false;
    for n in 1..200_u64 {
        let world = hand_switched(&format!("sweep-{n}"));
        let faulty = world.failing_at(n);
        match world.placer_on(&faulty).reconcile(&view(vec![])) {
            Ok(_) => {
                completed = true;
                settled(&world, "1.4.2");
                break;
            }
            Err(err) => assert!(matches!(err, PlacedError::Io { .. }), "write {n}: {err}"),
        }
        assert_links_listed(&world);
        world.placer().reconcile(&view(vec![])).expect("rerun");
        settled(&world, "1.4.2");
    }
    assert!(completed, "the reconciliation never completed");
}

/// Catches a forward intent that links over a file the record does not list, or switches before
/// the claims are judged: a new path held by an unlisted regular file is reported as a conflict,
/// and `current`, `previous`, the tree, the record and the units are as they were.
#[test]
fn a_forward_intent_whose_new_path_is_occupied_switches_nothing() {
    let world = World::new("forward-occupied");
    install(&world, "tierstore", "1.4.2", &old_spec());
    world.package("tierstore", "1.5.0", &new_spec());
    fs::write(world.host(C), "mine").expect("write a file");
    touch_lock(&world);
    let before = world.snapshot();
    let record = world.placed_bytes();
    world.log.clear();
    let report = world
        .placer()
        .reconcile(&view(vec![intent(Some("1.4.2"), "1.5.0", Side::Forward)]))
        .expect("reconcile");
    assert_eq!(
        report,
        ReconcileReport {
            conflicts: vec![Conflict {
                service: "tierstore".to_owned(),
                path: PathBuf::from(C),
                owner: Owner::File,
            }],
            ..ReconcileReport::default()
        }
    );
    assert_eq!(
        world.read_link(format!("{PKG}/current")),
        Some(PathBuf::from("1.4.2"))
    );
    assert_eq!(world.read_link(format!("{PKG}/previous")), None);
    assert_eq!(world.snapshot(), before);
    assert_eq!(world.placed_bytes(), record);
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}

/// Catches a forward intent to a version that cannot be read switching `current` to it: whether
/// its manifest does not load or its directory does not exist, the service is reported
/// unresolved with the loader's refusal and `current`, the tree and the record are as they were.
#[test]
fn a_forward_intent_to_a_version_that_does_not_load_switches_nothing() {
    for ver in ["1.5.0", "1.6.0"] {
        let world = World::new(&format!("forward-unreadable-{ver}"));
        install(&world, "tierstore", "1.4.2", &old_spec());
        if ver == "1.5.0" {
            let dir = world.version_dir("tierstore", ver);
            fs::create_dir_all(&dir).expect("create the version directory");
            fs::write(dir.join("manifest.toml"), "this is not toml\n").expect("write");
        }
        touch_lock(&world);
        let before = world.snapshot();
        let record = world.placed_bytes();
        world.log.clear();
        let why = format!("the manifest of {ver}: {}", refusal(&world, ver));
        let report = world
            .placer()
            .reconcile(&view(vec![intent(Some("1.4.2"), ver, Side::Forward)]))
            .expect("reconcile");
        assert_eq!(
            report,
            ReconcileReport {
                unresolved: vec![Unresolved {
                    service: "tierstore".to_owned(),
                    why,
                }],
                ..ReconcileReport::default()
            }
        );
        assert_eq!(
            world.read_link(format!("{PKG}/current")),
            Some(PathBuf::from("1.4.2"))
        );
        assert_eq!(world.snapshot(), before);
        assert_eq!(world.placed_bytes(), record);
        assert!(world.writes().is_empty() && world.unit_calls().is_empty());
    }
}

/// Catches a crash between the write of the record and the reload being forgotten when every
/// path is the same in both versions: no link changed, but the reload owed in the record runs
/// exactly once at start, is cleared, and a second start runs none.
#[test]
fn a_reload_owed_by_a_crash_runs_once_at_start_though_no_link_changed() {
    let world = crashed_before_reload("owed-reload");
    assert!(world.record().reload_pending);
    let boot = view(vec![intent(Some("1.4.2"), "1.5.0", Side::Forward)]);
    let report = world.placer().reconcile(&boot).expect("reconcile");
    assert!(!report.changed() && report.reloaded);
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert!(!world.record().reload_pending);
    assert!(
        world
            .record()
            .paths
            .values()
            .all(|entry| entry.state == PathState::Placed && entry.version == "1.5.0")
    );
    settled(&world, "1.5.0");
    world.log.clear();
    let again = world.placer().reconcile(&boot).expect("a second reconcile");
    assert_eq!(again, ReconcileReport::default());
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}

/// Catches a failed `daemon-reload` at start clearing the reload owed: the start fails with the
/// unit-control error, the record on disk still owes the reload, and the next start with a
/// working service manager reloads once and clears it.
#[test]
fn a_failed_reload_at_start_stays_owed_until_the_next_start_reloads() {
    let world = crashed_before_reload("reload-fails");
    let boot = view(vec![intent(Some("1.4.2"), "1.5.0", Side::Forward)]);
    let failing = FakeUnitControl::failing(world.log.clone());
    let err = Placer::new(world.roots.clone(), &world.fs, &failing)
        .reconcile(&boot)
        .expect_err("the reload fails");
    assert!(matches!(err, PlacedError::Units(_)));
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert!(world.record().reload_pending);
    world.log.clear();
    let report = world.placer().reconcile(&boot).expect("the next start");
    assert!(report.reloaded && !report.changed());
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert!(!world.record().reload_pending);
    settled(&world, "1.5.0");
}

/// Catches a first install's `current` surviving when the install is undone for a version that
/// `current` does not name: `current` naming another version is left, and nothing is written.
#[test]
fn undoing_a_first_install_leaves_a_current_that_names_another_version() {
    let world = World::new("back-other-current");
    world.package("tierstore", "1.4.2", &old_spec());
    world.set_link("tierstore", "current", "1.4.2");
    touch_lock(&world);
    let before = world.snapshot();
    world.log.clear();
    let report = world
        .placer()
        .reconcile(&view(vec![intent(None, "1.5.0", Side::Back)]))
        .expect("reconcile");
    assert_eq!(report, ReconcileReport::default());
    assert_eq!(world.snapshot(), before);
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}

/// Catches a `current` or `previous` that leaves the service's directory being followed: a link
/// to `../other/1.0.0` is reported unresolved naming the link, and nothing is touched.
#[test]
fn a_link_that_does_not_name_a_version_beside_it_is_unresolved_and_left_alone() {
    for name in ["current", "previous"] {
        let world = World::new(&format!("escaping-{name}"));
        install(&world, "tierstore", "1.4.2", &old_spec());
        world.set_link("tierstore", name, "../other/1.0.0");
        touch_lock(&world);
        let before = world.snapshot();
        let record = world.placed_bytes();
        world.log.clear();
        let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
        let held = Owner::Link(PathBuf::from("../other/1.0.0"));
        assert_eq!(
            report,
            ReconcileReport {
                unresolved: vec![Unresolved {
                    service: "tierstore".to_owned(),
                    why: format!("{PKG}/{name} is held by {held}"),
                }],
                ..ReconcileReport::default()
            }
        );
        assert_eq!(world.snapshot(), before);
        assert_eq!(world.placed_bytes(), record);
        assert!(world.writes().is_empty() && world.unit_calls().is_empty());
    }
}

/// Catches two different intents for one service being applied in the order given: either order
/// is refused with the service named before anything is written; an intent given twice is
/// accepted and switches once.
#[test]
fn contradictory_intents_for_one_service_are_refused_before_any_write() {
    let forward = intent(Some("1.4.2"), "1.5.0", Side::Forward);
    let back = intent(Some("1.4.2"), "1.5.0", Side::Back);
    for intents in [
        vec![forward.clone(), back.clone()],
        vec![back.clone(), forward.clone()],
    ] {
        let world = hand_switched("contradiction");
        let before = world.snapshot();
        let record = world.placed_bytes();
        world.log.clear();
        let err = world
            .placer()
            .reconcile(&view(intents))
            .expect_err("two intents for one service");
        assert!(
            matches!(&err, PlacedError::ConflictingIntents { service } if service == "tierstore"),
            "{err:?}"
        );
        assert_eq!(err.reason(), "link_failed");
        assert_eq!(world.snapshot(), before);
        assert_eq!(world.placed_bytes(), record);
        assert!(world.writes().is_empty() && world.unit_calls().is_empty());
    }

    let world = World::new("duplicate-intent");
    world.package("tierstore", "1.5.0", &new_spec());
    let twice = view(vec![
        intent(None, "1.5.0", Side::Forward),
        intent(None, "1.5.0", Side::Forward),
    ]);
    let report = world.placer().reconcile(&twice).expect("reconcile");
    assert_eq!(
        report.switched,
        [("tierstore".to_owned(), "1.5.0".to_owned())]
    );
    settled(&world, "1.5.0");
}

/// Catches a read that fails once being taken for an absent path: with the n-th read of a
/// reconcile failing, every n up to the last read the reconcile makes returns an error and never
/// a report that missed a link, and a rerun over the healthy filesystem settles the host.
#[test]
fn a_read_that_fails_once_fails_the_start_and_never_misses_a_path() {
    let mut completed = false;
    for n in 1..400_u64 {
        let world = World::new(&format!("read-fault-{n}"));
        install(&world, "tierstore", "1.4.2", &old_spec());
        fs::remove_file(world.host(B)).expect("remove a link");
        let faulty = FaultFs::new(world.root(), world.log.clone()).failing_read(n, false);
        match world.placer_on(&faulty).reconcile(&view(vec![])) {
            Ok(report) => {
                assert!(faulty.reads() < n, "read {n} failed and the start went on");
                assert_eq!(report.linked, [PathBuf::from(B)]);
                settled(&world, "1.4.2");
                completed = true;
                break;
            }
            Err(err) => {
                assert!(matches!(err, PlacedError::Io { .. }), "read {n}: {err}");
                assert!(faulty.reads() >= n);
            }
        }
        world.placer().reconcile(&view(vec![])).expect("rerun");
        settled(&world, "1.4.2");
    }
    assert!(completed, "the reconciliation never completed");
}
