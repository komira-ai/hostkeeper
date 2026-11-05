//! The reload a repaired link owes: `begin` on a service already recorded whose link on disk is
//! missing or wrong records the debt in one write of `placed.json` before the link is repaired;
//! `relink_to` and `reconcile` then reload, and a reload that failed stays owed. Also what
//! `Store::save` refuses before it writes: the lock of another record, and a record that breaks
//! the rules `load` holds.

mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::Class;
use hostkeeper_placed::testing::{FakeUnitControl, FsWrite, UnitCall};
use hostkeeper_placed::{
    BootView, Entry, PathKind, PathState, PlacedError, PlacedRecord, Placer, ReconcileReport,
    Store, Tombstone,
};
use support::{Spec, World, install};

const STATE: &str = "/var/lib/hostkeeper";
const PLACED: &str = "/var/lib/hostkeeper/placed.json";
const LOCK: &str = "/var/lib/hostkeeper/placed.lock";
const TMP: &str = "/var/lib/hostkeeper/.hk~placed.json~tmp";
const UNIT: &str = "/etc/systemd/system/tierstore@.service";
const OTHER: &str = "/var/lib/other";

/// The four writes of one save of `placed.json`.
fn save_sequence() -> Vec<FsWrite> {
    vec![
        FsWrite::Write(PathBuf::from(TMP)),
        FsWrite::SyncFile(PathBuf::from(TMP)),
        FsWrite::Rename {
            from: PathBuf::from(TMP),
            to: PathBuf::from(PLACED),
        },
        FsWrite::SyncDir(PathBuf::from(STATE)),
    ]
}

fn view() -> BootView {
    BootView {
        intents: vec![],
        running: support::RUNNING,
    }
}

/// tierstore 1.4.2 installed and committed, with the reload its run unit's `WantedBy=` link owes
/// (`install` enables after its reload) run, and the world's log cleared.
fn installed(name: &str) -> World {
    let world = World::new(name);
    install(&world, "tierstore", "1.4.2", &Spec::tierstore());
    world.placer().reload().expect("the reload the enable owes");
    assert!(!world.record().reload_pending);
    world.log.clear();
    world
}

/// The empty record, changed by `edit`.
fn changed(edit: impl FnOnce(&mut PlacedRecord)) -> PlacedRecord {
    let mut record = PlacedRecord::default();
    edit(&mut record);
    record
}

fn entry() -> Entry {
    Entry::new(
        "x",
        "1.0.0",
        PathKind::File,
        PathState::Placed,
        Path::new("/var/lib/hostkeeper/packages/x/current/files/x.conf"),
    )
}

/// Catches a repair of a deleted unit link that owes no reload: with the unit's link deleted,
/// `relink_to` of the same manifest writes the record before the link, links the unit again,
/// runs exactly one `daemon-reload`, and leaves no reload owed.
#[test]
fn relink_to_repairs_a_deleted_unit_link_and_reloads() {
    let world = installed("repair-relink");
    let manifest = world.load("tierstore", "1.4.2");
    let target = world.record().paths[Path::new(UNIT)].link_target.clone();
    fs::remove_file(world.host(UNIT)).expect("delete the unit link");

    assert!(world.placer().relink_to(&manifest).expect("relink"));

    assert_eq!(world.read_link(UNIT), Some(target));
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert!(!world.record().reload_pending);
    let writes = world.writes();
    assert_eq!(writes[..4], save_sequence());
    let linked = writes
        .iter()
        .position(|write| matches!(write, FsWrite::Symlink { link, .. } if link == Path::new(UNIT)))
        .expect("the unit link is made");
    assert!(linked > 4, "the debt is recorded before the repair");
}

/// Catches a failed reload after a repair being forgotten: `reconcile` with a failing service
/// manager is an error, the link is back and the reload is owed on disk; the next `reconcile`
/// with a working one reloads exactly once and clears it, and a third writes and calls nothing.
#[test]
fn a_repair_whose_reload_fails_stays_owed_until_the_next_start() {
    let world = installed("repair-owed");
    fs::remove_file(world.host(UNIT)).expect("delete the unit link");

    let failing = FakeUnitControl::failing(world.log.clone());
    let broken = Placer::new(world.roots.clone(), &world.fs, &failing);
    let refusal = broken.reconcile(&view()).expect_err("the reload fails");
    assert!(matches!(refusal, PlacedError::Units(_)));
    assert!(world.read_link(UNIT).is_some(), "the link is repaired");
    assert!(world.record().reload_pending, "the reload is owed");
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);

    world.log.clear();
    let retry = world.placer().reconcile(&view()).expect("the next start");
    assert!(retry.reloaded && !retry.changed());
    assert_eq!(world.unit_calls(), [UnitCall::DaemonReload]);
    assert!(!world.record().reload_pending);

    world.log.clear();
    let quiet = world.placer().reconcile(&view()).expect("a third start");
    assert_eq!(quiet, ReconcileReport::default());
    assert!(world.writes().is_empty() && world.unit_calls().is_empty());
}

/// Catches a `begin` that writes when nothing is wrong: every link right and every entry
/// recorded, it writes nothing, owes nothing and leaves `placed.json` as it was.
#[test]
fn begin_with_every_link_right_owes_nothing() {
    let world = installed("begin-right");
    let placer = world.placer();
    let desired = placer
        .desired(&world.load("tierstore", "1.4.2"))
        .expect("desired");
    let before = world.placed_bytes();

    placer.begin(&desired).expect("begin");

    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), before);
    assert!(!world.record().reload_pending);
}

/// Catches a `begin` that leaves a missing or wrong link without a debt: for each, `begin`
/// writes `placed.json` once (temporary file, fsync, rename, directory fsync), sets the reload
/// owed, and does not touch the link.
#[test]
fn begin_owes_a_reload_for_a_missing_or_a_wrong_link_in_one_write() {
    for wrong in [false, true] {
        let world = installed(&format!("begin-repair-{wrong}"));
        let placer = world.placer();
        let desired = placer
            .desired(&world.load("tierstore", "1.4.2"))
            .expect("desired");
        fs::remove_file(world.host(UNIT)).expect("delete the unit link");
        if wrong {
            symlink("/old/target", world.host(UNIT)).expect("make a wrong link");
        }
        world.log.clear();

        placer.begin(&desired).expect("begin");

        assert_eq!(world.writes(), save_sequence(), "wrong: {wrong}");
        assert!(world.record().reload_pending, "wrong: {wrong}");
        assert_eq!(
            world.read_link(UNIT).is_some(),
            wrong,
            "begin repairs nothing"
        );
        assert!(
            world
                .record()
                .paths
                .values()
                .all(|entry| entry.state == PathState::Placed)
        );
    }
}

/// Catches a repair that writes the record again when the debt is already there: with the reload
/// already owed and a link missing, `begin` writes nothing and `placed.json` keeps its bytes.
#[test]
fn begin_does_not_write_again_when_the_reload_is_already_owed() {
    let world = installed("begin-already-owed");
    let placer = world.placer();
    let desired = placer
        .desired(&world.load("tierstore", "1.4.2"))
        .expect("desired");
    let mut record = world.record();
    record.reload_pending = true;
    {
        let lock = placer.lock().expect("take the lock");
        placer.store().save(&record, &lock).expect("owe a reload");
    }
    fs::remove_file(world.host(UNIT)).expect("delete the unit link");
    let before = world.placed_bytes();
    world.log.clear();

    placer.begin(&desired).expect("begin");

    assert!(world.writes().is_empty());
    assert_eq!(world.placed_bytes(), before);
    assert!(world.record().reload_pending);
}

/// Catches a lock that does not say which lock file it holds: `PlacedLock::path` is the lock
/// file of the state directory on disk, and a save under it is accepted.
#[test]
fn a_placed_lock_names_its_lock_file_and_a_save_under_it_is_accepted() {
    let world = World::new("lock-path");
    let placer = world.placer();
    let lock = placer.lock().expect("take the lock");

    assert_eq!(lock.path(), world.host(LOCK));
    assert!(lock.path().is_file());
    placer
        .store()
        .save(&PlacedRecord::default(), &lock)
        .expect("the lock of this record");
    assert_eq!(world.record(), PlacedRecord::default());
}

/// Catches a save under the lock of another state directory: the lock of `/var/lib/other` given
/// to the store of `/var/lib/hostkeeper` is `ForeignLock` naming both files, class Environment,
/// reason `link_failed`, and no write is made, even of a record that is itself invalid.
#[test]
fn a_lock_of_another_state_directory_is_refused_without_writes() {
    let world = World::new("foreign-dir");
    fs::create_dir_all(world.host(OTHER)).expect("create the other state directory");
    let mut record = PlacedRecord::default();
    record.paths.insert(PathBuf::from("/etc/x.conf"), entry());
    {
        let placer = world.placer();
        let lock = placer.lock().expect("take the lock");
        placer.store().save(&record, &lock).expect("save");
    }
    let before = world.placed_bytes();
    let other_roots = hostkeeper_placed::Roots::new(
        Path::new(OTHER),
        world.roots.unit_dir(),
        world.roots.packages().dir(),
    );
    let other = Placer::new(other_roots, &world.fs, &world.units);
    let foreign = other.lock().expect("take the other lock");
    world.log.clear();

    let mut invalid = record.clone();
    invalid.schema = 2;
    for attempt in [&record, &invalid] {
        let error = world
            .placer()
            .store()
            .save(attempt, &foreign)
            .expect_err("a foreign lock");
        assert_eq!(error.class(), Class::Environment);
        assert_eq!(error.reason(), "link_failed");
        match error {
            PlacedError::ForeignLock { lock, record } => {
                assert_eq!(lock, world.host("/var/lib/other/placed.lock"));
                assert_eq!(record, Path::new(PLACED));
            }
            unexpected => panic!("expected ForeignLock, got {unexpected:?}"),
        }
    }
    assert_eq!(world.placed_bytes(), before);
    assert!(world.writes().is_empty());
    assert!(!world.host(TMP).exists());
}

/// Catches a lock compared by directory name alone: the lock of the same state directory under
/// another root is refused too, and the record under the first root is as it was.
#[test]
fn a_lock_of_the_same_directory_under_another_root_is_refused() {
    let world = World::new("foreign-root-a");
    let elsewhere = World::new("foreign-root-b");
    let mut record = PlacedRecord::default();
    record.paths.insert(PathBuf::from("/etc/x.conf"), entry());
    {
        let placer = world.placer();
        let lock = placer.lock().expect("take the lock");
        placer.store().save(&record, &lock).expect("save");
    }
    let before = world.placed_bytes();
    let foreign = elsewhere.placer().lock().expect("take the lock elsewhere");
    world.log.clear();

    let error = Store::new(&world.fs, Path::new(STATE))
        .save(&PlacedRecord::default(), &foreign)
        .expect_err("a lock of another root");

    match error {
        PlacedError::ForeignLock { lock, record } => {
            assert_eq!(lock, elsewhere.host(LOCK));
            assert_eq!(record, Path::new(PLACED));
        }
        other => panic!("expected ForeignLock, got {other:?}"),
    }
    assert_eq!(world.placed_bytes(), before);
    assert!(world.writes().is_empty());
}

/// Catches a save that writes a record `load` would refuse: each record breaking a rule of the
/// known fields (schema, path grammar, link target, service name, version, tombstone key) is
/// `Corrupt` naming `placed.json` before anything is written, and the bytes on disk stay.
#[test]
fn a_record_that_load_would_refuse_is_not_saved() {
    let with = |path: &str, spoil: &dyn Fn(&mut Entry)| {
        changed(|record| {
            let mut bad = entry();
            spoil(&mut bad);
            record.paths.insert(PathBuf::from(path), bad);
        })
    };
    let schema = changed(|record| record.schema = 2);
    let tombstone = changed(|record| {
        record
            .uninstalling
            .insert("bad name".to_owned(), Tombstone::default());
    });
    let cases = [
        ("schema", schema, Some("its schema is 2, not 1")),
        ("relative path", with("etc/x.conf", &|_| {}), None),
        (
            "relative link target",
            with("/etc/x.conf", &|bad| {
                bad.link_target = PathBuf::from("rel/x")
            }),
            None,
        ),
        (
            "service name",
            with("/etc/x.conf", &|bad| bad.service = "bad name".to_owned()),
            Some("\"bad name\" is not a service name"),
        ),
        (
            "version",
            with("/etc/x.conf", &|bad| bad.version = "bad version".to_owned()),
            Some("\"bad version\" is not a version"),
        ),
        (
            "tombstone",
            tombstone,
            Some("the tombstone \"bad name\" is not a service name"),
        ),
    ];
    for (name, record, exact) in cases {
        let world = World::new(&format!("save-corrupt-{}", name.replace(' ', "-")));
        let placer = world.placer();
        let lock = placer.lock().expect("take the lock");
        placer
            .store()
            .save(&PlacedRecord::default(), &lock)
            .expect("save");
        let before = world.placed_bytes();
        world.log.clear();

        let error = placer.store().save(&record, &lock).expect_err(name);

        match error {
            PlacedError::Corrupt { path, why } => {
                assert_eq!(path, Path::new(PLACED), "{name}");
                if let Some(exact) = exact {
                    assert_eq!(why, exact, "{name}");
                }
            }
            other => panic!("{name}: expected Corrupt, got {other:?}"),
        }
        assert!(world.writes().is_empty(), "{name}");
        assert_eq!(world.placed_bytes(), before, "{name}");
        assert!(!world.host(TMP).exists(), "{name}");
    }
}
