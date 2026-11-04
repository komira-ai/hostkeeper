//! Start-up reconciliation of placed paths (`Placer::reconcile`; the crash sweeps are in
//! `reconcile_crash.rs`): a hand-made switch of `current` is followed, an occupied
//! pending path is reported and left alone, the temporary names are removed, `daemon-reload`
//! runs once and only for a change, and a service whose version cannot be read is reported and
//! left as it is.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::{Expected, Manifest};
use hostkeeper_placed::placer::is_temporary;
use hostkeeper_placed::testing::{FakeUnitControl, UnitCall};
use hostkeeper_placed::{
    BootView, Conflict, Entry, Owner, PathKind, PathState, PlacedError, Placer, ReconcileReport,
    Side, Store, SwitchIntent, Tombstone, Unresolved,
};
use support::{Spec, World, install, service, version};

const PKG: &str = "/var/lib/hostkeeper/packages/tierstore";
const A: &str = "/etc/tierstore/a.conf";
const B: &str = "/etc/tierstore/b.conf";
const C: &str = "/etc/tierstore/c.conf";
const UNIT: &str = "/etc/systemd/system/tierstore@.service";
const RUN_UNIT: &str = "tierstore@9301.service";

fn want() -> String {
    format!("{}/{RUN_UNIT}", support::WANTS)
}

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

fn paths(list: &[&str]) -> BTreeSet<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

/// Creates the lock file, so that a reconciliation that touches nothing leaves the tree as it was.
fn touch_lock(world: &World) {
    fs::write(world.host("/var/lib/hostkeeper/placed.lock"), b"").expect("create the lock file");
}

/// Adds an entry to the record on disk.
fn put(world: &World, path: &str, owner: &str, state: PathState) {
    let mut record = world.record();
    let target = PathBuf::from(format!(
        "/var/lib/hostkeeper/packages/{owner}/current/files/x"
    ));
    record.paths.insert(
        PathBuf::from(path),
        Entry::new(owner, "1.0.0", PathKind::File, state, &target),
    );
    save(world, &record);
}

fn save(world: &World, record: &hostkeeper_placed::PlacedRecord) {
    let lock = world.placer().lock().expect("take the lock");
    Store::new(&world.fs, world.roots.state_dir())
        .save(record, &lock)
        .expect("save placed.json");
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

/// Asserts the host is settled on `chosen` of tierstore: every path its manifest places resolves
/// into it, `current` names it, the record lists exactly those paths all placed, every placed link
/// is listed, and no temporary name remains.
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
    for link in world.placed_links() {
        assert!(record.paths.contains_key(&link), "{link:?} is not listed");
    }
    for path in world.snapshot().keys() {
        let name = path.file_name().expect("a name").to_string_lossy();
        assert!(!is_temporary(&name), "{path:?} remains");
    }
}

/// 1.4.2 and 1.5.0 installed, then `current` pointed back at 1.4.2 by hand.
fn hand_switched(name: &str) -> World {
    let world = World::new(name);
    install(&world, "tierstore", "1.4.2", &old_spec());
    install(&world, "tierstore", "1.5.0", &new_spec());
    world.set_link("tierstore", "current", "1.4.2");
    world
}

/// Catches a reconciliation that follows the record or the old version instead of `current`: with
/// `current` switched by hand, the links of the version it names exist and resolve into it, the
/// links only the other version listed are removed (the run instance stopped), and a second run
/// changes nothing.
#[test]
fn reconcile_relinks_every_placed_link_that_differs_from_current() {
    let world = hand_switched("relink");
    world.log.clear();
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert_resolves_into(&world, Path::new(A), "1.4.2");
    assert_resolves_into(&world, Path::new(B), "1.4.2");
    let want = want();
    let removed: BTreeSet<PathBuf> = report.removed.iter().cloned().collect();
    assert_eq!(removed, paths(&[C, UNIT, want.as_str()]));
    assert_eq!(report.stopped, [RUN_UNIT]);
    assert!(report.linked.is_empty() && report.switched.is_empty());
    assert!(report.reloaded);
    let snapshot = world.snapshot();
    for gone in [C, UNIT, want.as_str()] {
        assert!(!snapshot.contains_key(Path::new(gone)), "{gone} remains");
    }
    assert_eq!(
        world
            .record()
            .paths
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        paths(&[A, B])
    );
    settled(&world, "1.4.2");
    world.log.clear();
    let again = world
        .placer()
        .reconcile(&view(vec![]))
        .expect("reconcile again");
    assert_eq!(again, ReconcileReport::default());
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}

/// Catches a link of the current version that is missing and not made again, or a reload that
/// does not follow it: the missing link is linked, listed in `linked`, and one reload runs.
#[test]
fn reconcile_makes_a_missing_link_of_the_current_version() {
    let world = World::new("missing");
    install(&world, "tierstore", "1.4.2", &old_spec());
    fs::remove_file(world.host(B)).expect("remove a link");
    world.log.clear();
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert_eq!(report.linked, [PathBuf::from(B)]);
    assert!(report.reloaded && report.changed());
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    settled(&world, "1.4.2");
}

/// Catches a pending path that holds something else being overwritten, or a conflict that does
/// not stop the service's reconciliation: whatever holds the path (a file, a directory, a link
/// elsewhere or into another service), the conflict names it, and the tree, the record and the
/// units are as they were.
#[test]
fn reconcile_reports_an_occupied_pending_path_as_path_conflict_and_touches_nothing() {
    let path = "/etc/tierstore/tierstore.conf";
    for case in 0..4 {
        let world = World::new(&format!("occupied-{case}"));
        fs::create_dir_all(world.host("/etc/tierstore")).expect("create a directory");
        put(&world, path, "tierstore", PathState::Pending);
        let at = world.host(path);
        let owner = match case {
            0 => {
                fs::write(at.as_path(), "mine").expect("write a file");
                Owner::File
            }
            1 => {
                fs::create_dir(at.as_path()).expect("create a directory");
                Owner::Directory
            }
            2 => {
                symlink("/elsewhere", at.as_path()).expect("make a link");
                Owner::Link(PathBuf::from("/elsewhere"))
            }
            _ => {
                let other = "/var/lib/hostkeeper/packages/other/current/files/x";
                symlink(other, at.as_path()).expect("make a link");
                Owner::Link(PathBuf::from(other))
            }
        };
        touch_lock(&world);
        let before = world.snapshot();
        let record = world.placed_bytes();
        world.log.clear();
        let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
        assert_eq!(
            report,
            ReconcileReport {
                conflicts: vec![Conflict {
                    service: "tierstore".to_owned(),
                    path: PathBuf::from(path),
                    owner,
                }],
                ..ReconcileReport::default()
            }
        );
        assert_eq!(world.snapshot(), before);
        assert_eq!(world.placed_bytes(), record);
        assert!(world.writes().is_empty() && world.unit_calls().is_empty());
    }
}

/// Catches a pending path that holds the recorded link, or a link into the service's package
/// directory, being reported as a conflict: neither is, the link is made right and the entries
/// are marked placed.
#[test]
fn a_pending_path_holding_a_link_into_the_package_is_not_a_conflict() {
    let world = World::new("own-links");
    install(&world, "tierstore", "1.4.2", &old_spec());
    let mut record = world.record();
    for entry in record.paths.values_mut() {
        entry.state = PathState::Pending;
    }
    save(&world, &record);
    fs::remove_file(world.host(B)).expect("remove a link");
    symlink(format!("{PKG}/1.4.2/files/b.conf"), world.host(B)).expect("make a link");
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert!(report.conflicts.is_empty());
    assert_eq!(report.linked, [PathBuf::from(B)]);
    assert_eq!(
        world.read_link(B),
        Some(PathBuf::from(format!("{PKG}/current/files/b.conf")))
    );
    settled(&world, "1.4.2");
}

/// Catches a desired path that is held by a file the record does not list being linked over, and
/// a path another service owns being taken: the conflict names the holder and nothing of the
/// service changes, whatever `current` names.
#[test]
fn reconcile_reports_a_desired_path_held_by_something_unlisted() {
    for owned_by_other in [false, true] {
        let world = World::new(&format!("desired-{owned_by_other}"));
        install(&world, "tierstore", "1.4.2", &old_spec());
        world.package("tierstore", "1.5.0", &new_spec());
        world.set_link("tierstore", "current", "1.5.0");
        let owner = if owned_by_other {
            put(&world, C, "other", PathState::Placed);
            Owner::Service("other".to_owned())
        } else {
            fs::write(world.host(C), "mine").expect("write a file");
            Owner::File
        };
        touch_lock(&world);
        let before = world.snapshot();
        let record = world.placed_bytes();
        world.log.clear();
        let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
        assert_eq!(
            report.conflicts,
            [Conflict {
                service: "tierstore".to_owned(),
                path: PathBuf::from(C),
                owner,
            }]
        );
        assert!(!report.changed() && !report.reloaded);
        assert_eq!(world.snapshot(), before);
        assert_eq!(world.placed_bytes(), record);
        assert!(world.writes().is_empty() && world.unit_calls().is_empty());
    }
}

/// Catches a stale temporary link or file surviving a start, or a name that merely resembles one
/// being deleted: the `.hk~<owner>~tmp` names in the state, unit, files and package directories go
/// and are listed; `keep.tmp`, `.hk~keep`, `x~tmp` and the old `.hk-*.tmp` spelling stay.
#[test]
fn reconcile_removes_stale_temporary_link_names() {
    let world = World::new("temporaries");
    install(&world, "tierstore", "1.4.2", &old_spec());
    let stale = [
        "/etc/systemd/system/.hk~x~tmp",
        "/etc/tierstore/.hk~y~tmp",
        "/var/lib/hostkeeper/packages/tierstore/.hk~tierstore~tmp",
        "/var/lib/hostkeeper/.hk~placed.json~tmp",
    ];
    symlink("/nowhere", world.host(stale[0])).expect("make a link");
    fs::write(world.host(stale[1]), "half").expect("write a file");
    symlink("1.5.0", world.host(stale[2])).expect("make a link");
    fs::write(world.host(stale[3]), "{").expect("write a file");
    let keep = [
        "/etc/tierstore/keep.tmp",
        "/etc/tierstore/.hk~keep",
        "/etc/tierstore/x~tmp",
        "/etc/tierstore/.hk-old.tmp",
    ];
    for name in keep {
        fs::write(world.host(name), "mine").expect("write a file");
    }
    world.log.clear();
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    let removed: BTreeSet<PathBuf> = report.temporaries_removed.iter().cloned().collect();
    assert_eq!(removed, paths(&stale));
    assert_eq!(report.temporaries_removed.len(), stale.len());
    assert!(!report.changed() && !report.reloaded);
    let snapshot = world.snapshot();
    for name in stale {
        assert!(!snapshot.contains_key(Path::new(name)), "{name} remains");
    }
    for name in keep {
        assert!(snapshot.contains_key(Path::new(name)), "{name} was removed");
    }
}

/// Catches a missing directory aborting the start: a recorded path whose directory does not exist
/// (and a service with no package directory) is skipped, not an error.
#[test]
fn reconcile_skips_a_directory_that_does_not_exist() {
    let world = World::new("no-dir");
    put(
        &world,
        "/etc/ghostdir/ghost.conf",
        "ghost",
        PathState::Placed,
    );
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert!(report.temporaries_removed.is_empty());
}

/// Catches a directory that cannot be listed being skipped as if it were absent: a recorded path
/// below a regular file fails the start with the listing named.
#[test]
fn reconcile_fails_when_a_directory_cannot_be_listed() {
    let world = World::new("list-error");
    fs::write(world.host("/etc/afile"), "not a directory").expect("write a file");
    put(&world, "/etc/afile/x.conf", "tierstore", PathState::Placed);
    match world.placer().reconcile(&view(vec![])) {
        Err(PlacedError::Io {
            op: "list", path, ..
        }) => assert_eq!(path, Path::new("/etc/afile")),
        other => panic!("expected a listing failure, got {other:?}"),
    }
}

/// Catches a failed removal of a temporary name being ignored: the start fails with the removal
/// named.
#[test]
fn reconcile_fails_when_a_temporary_name_cannot_be_removed() {
    let world = World::new("remove-error");
    fs::write(world.host("/etc/systemd/system/.hk~x~tmp"), "half").expect("write a file");
    let faulty = world.failing_at(1);
    match world.placer_on(&faulty).reconcile(&view(vec![])) {
        Err(PlacedError::Io {
            op: "remove", path, ..
        }) => {
            assert_eq!(path, Path::new("/etc/systemd/system/.hk~x~tmp"));
        }
        other => panic!("expected a removal failure, got {other:?}"),
    }
}

/// Catches a reload that runs on every start, or not when a link changed: a reconcile that
/// changes nothing records no reload; one that made a link records exactly one.
#[test]
fn daemon_reload_runs_once_per_reconcile_only_when_something_changed() {
    let world = World::new("reload");
    install(&world, "tierstore", "1.4.2", &old_spec());
    world.log.clear();
    let quiet = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert!(!quiet.reloaded && !quiet.changed());
    assert!(world.unit_calls().is_empty());
    fs::remove_file(world.host(A)).expect("remove a link");
    fs::remove_file(world.host(B)).expect("remove a link");
    let busy = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert_eq!(busy.linked.len(), 2);
    assert!(busy.reloaded);
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
}

/// Catches a failed `daemon-reload` or a failed stop being swallowed: each fails the start with
/// the unit-control error, and a failed stop leaves the links the stop was to precede.
#[test]
fn reconcile_fails_when_the_service_manager_fails() {
    let world = World::new("units-fail");
    install(&world, "tierstore", "1.4.2", &old_spec());
    fs::remove_file(world.host(B)).expect("remove a link");
    let failing = FakeUnitControl::failing(world.log.clone());
    let placer = Placer::new(world.roots.clone(), &world.fs, &failing);
    let err = placer.reconcile(&view(vec![])).expect_err("reload fails");
    assert!(matches!(err, PlacedError::Units(_)));
    assert_eq!(err.reason(), "daemon_reload_failed");

    let world = hand_switched("stop-fails");
    let failing = FakeUnitControl::failing(world.log.clone());
    let placer = Placer::new(world.roots.clone(), &world.fs, &failing);
    let err = placer.reconcile(&view(vec![])).expect_err("stop fails");
    assert!(matches!(err, PlacedError::Units(_)));
    assert!(world.snapshot().contains_key(Path::new(C)));
}

/// Catches a forward intent that does not finish the switch of a first install: with no record
/// and no `current`, the intent switches `current` to the target and links every path of it.
#[test]
fn a_forward_intent_finishes_a_first_install() {
    let world = World::new("forward-first");
    let manifest = world.package("tierstore", "1.5.0", &new_spec());
    let report = world
        .placer()
        .reconcile(&view(vec![intent(None, "1.5.0", Side::Forward)]))
        .expect("reconcile");
    assert_eq!(
        report.switched,
        [("tierstore".to_owned(), "1.5.0".to_owned())]
    );
    let desired = world.placer().desired(&manifest).expect("desired");
    let linked: BTreeSet<PathBuf> = report.linked.iter().cloned().collect();
    assert_eq!(linked, desired.paths());
    assert!(report.reloaded);
    settled(&world, "1.5.0");
}

/// Catches a back intent with a version to return to that leaves `current` on the target: the
/// switch is undone, and the paths only the target listed are removed.
#[test]
fn a_back_intent_with_a_version_undoes_the_switch() {
    let world = World::new("back-from");
    install(&world, "tierstore", "1.4.2", &old_spec());
    install(&world, "tierstore", "1.5.0", &new_spec());
    let report = world
        .placer()
        .reconcile(&view(vec![intent(Some("1.4.2"), "1.5.0", Side::Back)]))
        .expect("reconcile");
    assert_eq!(
        report.switched,
        [("tierstore".to_owned(), "1.4.2".to_owned())]
    );
    settled(&world, "1.4.2");
}

/// Catches a switch that is repeated, or links rewritten, when the intent was already carried out:
/// the report is empty and nothing is written.
#[test]
fn an_intent_already_carried_out_changes_nothing() {
    let world = World::new("carried-out");
    install(&world, "tierstore", "1.4.2", &old_spec());
    let manifest = install(&world, "tierstore", "1.5.0", &new_spec());
    world
        .placer()
        .unlink_unlisted(&manifest)
        .expect("drop the paths only 1.4.2 listed");
    touch_lock(&world);
    world.log.clear();
    let report = world
        .placer()
        .reconcile(&view(vec![intent(Some("1.4.2"), "1.5.0", Side::Forward)]))
        .expect("reconcile");
    assert_eq!(report, ReconcileReport::default());
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}

/// Catches a first install that is undone leaving its pending links, entries or `current`: the
/// links and entries go, the run instance is stopped, `current` is removed and reported, the
/// reload that follows is cleared, and a second start writes nothing.
#[test]
fn a_back_intent_with_no_version_removes_the_pending_links_and_entries() {
    let world = World::new("back-none");
    let manifest = world.package("tierstore", "1.5.0", &new_spec());
    let placer = world.placer();
    let claims = placer.desired(&manifest).expect("desired");
    let linking = placer.begin(&claims).expect("begin");
    placer.link(&linking).expect("link");
    placer
        .switch_current(&service("tierstore"), &version("1.5.0"))
        .expect("switch");
    assert!(!world.placed_links().is_empty());
    world.log.clear();
    let boot = view(vec![intent(None, "1.5.0", Side::Back)]);
    let report = placer.reconcile(&boot).expect("reconcile");
    let removed: BTreeSet<PathBuf> = report.removed.iter().cloned().collect();
    assert_eq!(removed, claims.paths());
    assert_eq!(report.stopped, [RUN_UNIT]);
    assert!(report.switched.is_empty() && report.reloaded);
    assert_eq!(report.current_removed, ["tierstore"]);
    assert_eq!(world.read_link(format!("{PKG}/current")), None);
    assert!(world.record().paths.is_empty() && !world.record().reload_pending);
    assert!(world.placed_links().is_empty());
    world.log.clear();
    let again = placer.reconcile(&boot).expect("a second reconcile");
    assert_eq!(again, ReconcileReport::default());
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}

/// Catches a path a hand or another program replaced being deleted: a recorded path that the
/// current version no longer lists, and that now holds a regular file, is left on disk, listed in
/// `left_alone`, and dropped from the record.
#[test]
fn reconcile_leaves_a_foreign_file_at_a_dropped_path_and_forgets_it() {
    let world = World::new("foreign");
    install(&world, "tierstore", "1.4.2", &old_spec());
    world.package("tierstore", "1.5.0", &Spec::new().file("files/a.conf", A));
    world.set_link("tierstore", "current", "1.5.0");
    fs::remove_file(world.host(B)).expect("remove a link");
    fs::write(world.host(B), "mine").expect("write a file");
    world.log.clear();
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert_eq!(report.left_alone, [PathBuf::from(B)]);
    assert!(report.removed.is_empty() && !report.changed());
    // The record moved to 1.5.0, so the reload begin owed runs, once, and is cleared.
    assert!(report.reloaded && !world.record().reload_pending);
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert_eq!(fs::read(world.host(B)).expect("read"), b"mine");
    assert_eq!(
        world
            .record()
            .paths
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        paths(&[A])
    );
}

/// Catches a service with no `current` link being relinked from guesswork, and a missing package
/// directory failing the start: such services are reported with the reason and nothing is touched.
#[test]
fn a_service_with_no_current_link_is_reported_unresolved_and_left_alone() {
    let world = World::new("no-current");
    install(&world, "tierstore", "1.4.2", &old_spec());
    put(
        &world,
        "/etc/ghostdir/ghost.conf",
        "ghost",
        PathState::Placed,
    );
    fs::remove_file(world.host(format!("{PKG}/current"))).expect("remove current");
    touch_lock(&world);
    let before = world.snapshot();
    let record = world.placed_bytes();
    world.log.clear();
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    let no_current = "it has no `current` link";
    assert_eq!(
        report,
        ReconcileReport {
            unresolved: vec![
                Unresolved {
                    service: "ghost".to_owned(),
                    why: no_current.to_owned(),
                },
                Unresolved {
                    service: "tierstore".to_owned(),
                    why: no_current.to_owned(),
                },
            ],
            ..ReconcileReport::default()
        }
    );
    assert_eq!(world.snapshot(), before);
    assert_eq!(world.placed_bytes(), record);
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}

/// Catches a `current` naming a version whose manifest does not load being relinked: the service is
/// reported with the loader's refusal and nothing is touched.
#[test]
fn a_current_whose_manifest_does_not_load_is_reported_unresolved() {
    let world = World::new("bad-manifest");
    install(&world, "tierstore", "1.4.2", &old_spec());
    world.set_link("tierstore", "current", "9.9.9");
    touch_lock(&world);
    let before = world.snapshot();
    world.log.clear();
    let expected = Expected {
        name: service("tierstore"),
        version: version("9.9.9"),
    };
    let refusal = Manifest::load(
        &world.version_dir("tierstore", "9.9.9"),
        &expected,
        &support::RUNNING,
    )
    .expect_err("no such version directory");
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert_eq!(
        report.unresolved,
        [Unresolved {
            service: "tierstore".to_owned(),
            why: format!("the manifest of 9.9.9: {refusal}"),
        }]
    );
    assert!(!report.changed() && report.conflicts.is_empty());
    assert_eq!(world.snapshot(), before);
    assert!(world.writes().is_empty());
}

/// Catches a `current` that is not a version name being followed, and a record naming a service
/// that is not one being used: the first is reported with the link named, the second fails the
/// start as a corrupt record.
#[test]
fn a_service_or_version_that_is_not_a_name_is_reported_unresolved() {
    let world = World::new("bad-names");
    install(&world, "tierstore", "1.4.2", &old_spec());
    world.set_link("tierstore", "current", "bad version");
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    let held = Owner::Link(PathBuf::from("bad version"));
    assert_eq!(
        report.unresolved,
        [Unresolved {
            service: "tierstore".to_owned(),
            why: format!("{PKG}/current is held by {held}"),
        }]
    );

    put(&world, "/etc/badname/x.conf", "bad name", PathState::Placed);
    match world.placer().reconcile(&view(vec![])) {
        Err(PlacedError::Corrupt { .. }) => {}
        other => panic!("expected a corrupt record, got {other:?}"),
    }
}

/// Catches a version whose paths cannot be derived (a run unit with no `WantedBy=`) being linked
/// partway: the service is reported with the refusal and nothing is touched.
#[test]
fn a_version_whose_paths_cannot_be_derived_is_reported_unresolved() {
    let world = World::new("bad-desired");
    let spec = Spec::new()
        .unit(
            "tierstore@.service",
            &support::unit_without_install("/bin/false"),
        )
        .run(RUN_UNIT);
    let manifest = world.package("tierstore", "1.5.0", &spec);
    world.set_link("tierstore", "current", "1.5.0");
    put(&world, A, "tierstore", PathState::Placed);
    let why = world
        .placer()
        .desired(&manifest)
        .expect_err("no WantedBy=")
        .to_string();
    touch_lock(&world);
    let before = world.snapshot();
    world.log.clear();
    let report = world.placer().reconcile(&view(vec![])).expect("reconcile");
    assert_eq!(
        report.unresolved,
        [Unresolved {
            service: "tierstore".to_owned(),
            why,
        }]
    );
    assert_eq!(world.snapshot(), before);
    assert!(world.writes().is_empty());
}

/// Catches a service being uninstalled being relinked, switched or reported: its tombstone makes
/// reconciliation skip it, whether the record or an intent names it.
#[test]
fn a_tombstoned_service_is_skipped() {
    let world = World::new("tombstone");
    install(&world, "tierstore", "1.4.2", &old_spec());
    world.package("tierstore", "1.5.0", &new_spec());
    fs::remove_file(world.host(format!("{PKG}/current"))).expect("remove current");
    let mut record = world.record();
    record
        .uninstalling
        .insert("tierstore".to_owned(), Tombstone::default());
    save(&world, &record);
    touch_lock(&world);
    let before = world.snapshot();
    world.log.clear();
    let report = world
        .placer()
        .reconcile(&view(vec![intent(Some("1.4.2"), "1.5.0", Side::Forward)]))
        .expect("reconcile");
    assert_eq!(report, ReconcileReport::default());
    assert_eq!(world.snapshot(), before);
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}
