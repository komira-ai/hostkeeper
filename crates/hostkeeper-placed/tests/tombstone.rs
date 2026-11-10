//! The fields an uninstall writes into its tombstone (`hook_done`, `halted`), read across a
//! release boundary in both directions (AGENTS.md B8); the removal of an empty directory through
//! the filesystem port; and the fakes a caller's tests record an uninstall with.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_placed::fs::{Fs, RealFs};
use hostkeeper_placed::testing::{Event, FakeUnitControl, FaultFs, FsWrite, Log, UnitCall};
use hostkeeper_placed::{PlacedRecord, Store, Tombstone, UnitControl};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use support::World;

const PLACED: &str = "/var/lib/hostkeeper/placed.json";
const STATE: &str = "/var/lib/hostkeeper";

/// A tombstone as the release before these fields reads it: nothing but the keys it keeps.
#[derive(Debug, Deserialize, Serialize)]
struct PreviousTombstone {
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// `placed.json` as the release before these fields reads and writes it: its schema, its paths,
/// its tombstones, its owed reload and the keys of a newer writer, each kept.
#[derive(Debug, Deserialize, Serialize)]
struct PreviousRecord {
    schema: u64,
    #[serde(default)]
    paths: BTreeMap<PathBuf, Value>,
    #[serde(default)]
    uninstalling: BTreeMap<String, PreviousTombstone>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reload_pending: bool,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// A tombstone with these fields, read as this release reads one.
fn tombstone(hook_done: bool, halted: bool) -> Tombstone {
    let fields = json!({"hook_done": hook_done, "halted": halted});
    serde_json::from_value(fields).expect("a tombstone")
}

fn save(world: &World, record: &PlacedRecord) {
    let lock = world.placer().lock().expect("the lock is free");
    Store::new(&world.fs, Path::new(STATE))
        .save(record, &lock)
        .expect("saves");
}

/// Catches a tombstone field that a reader of the release before refuses, drops when it writes
/// the record again, or that this release misreads in a record that reader wrote: tombstones
/// with `hook_done` and `halted` written by this release are read by the previous release's
/// reader as tombstones, kept byte for byte when it rewrites the record, and read back by this
/// release as written; a tombstone the previous release wrote (`{}`, the fixture of
/// `the_tombstone_round_trip`) reads as an uninstall that has not run its hook and is not
/// halted, and is written back as `{}`.
#[test]
fn placed_json_with_a_tombstone_is_read_by_the_previous_release_reader() {
    let world = World::new("tombstone-previous-reader");
    let mut record = PlacedRecord::default();
    record
        .uninstalling
        .insert("web".to_owned(), tombstone(true, false));
    record
        .uninstalling
        .insert("api".to_owned(), tombstone(false, true));
    record
        .uninstalling
        .insert("db".to_owned(), Tombstone::default());
    save(&world, &record);
    let written = fs::read(world.host(PLACED)).expect("written");
    let value: Value = serde_json::from_slice(&written).expect("json");
    assert_eq!(
        value["uninstalling"],
        json!({"api": {"halted": true}, "db": {}, "web": {"hook_done": true}})
    );

    // The previous release reads every tombstone, and writes the record back with every key.
    let previous: PreviousRecord = serde_json::from_slice(&written).expect("it reads");
    assert_eq!(
        previous.uninstalling.keys().collect::<Vec<_>>(),
        ["api", "db", "web"]
    );
    let rewritten = serde_json::to_vec_pretty(&previous).expect("it writes");
    let again: Value = serde_json::from_slice(&rewritten).expect("json");
    // The previous release owns a `paths` key this release no longer writes: it writes it empty.
    assert_eq!(again["uninstalling"], value["uninstalling"]);
    assert_eq!(again["paths"], json!({}));
    fs::write(world.host(PLACED), &rewritten).expect("the previous release's write");
    assert_eq!(world.record().uninstalling, record.uninstalling);

    // What the previous release wrote, read by this one.
    let fixture = json!({"schema": 1, "paths": {}, "uninstalling": {"x": {}}});
    fs::write(world.host(PLACED), fixture.to_string()).expect("the fixture");
    let read = world.record();
    assert_eq!(read.uninstalling["x"], tombstone(false, false));
    save(&world, &read);
    let value: Value =
        serde_json::from_slice(&fs::read(world.host(PLACED)).expect("written")).expect("json");
    assert_eq!(value["uninstalling"], json!({"x": {}}));
}

/// Catches a removal of a directory that is not one, that removes what is in it, or that the
/// recording filesystem does not log or does not fail: `remove_dir` removes an empty directory
/// under the root, refuses one that is not empty and leaves it, is logged as `RemoveDir` when it
/// happened, and fails without touching the disk from the write `failing_at` names on.
#[test]
fn an_empty_directory_is_removed_through_the_port() {
    let world = World::new("tombstone-remove-dir");
    let empty = Path::new("/var/lib/hostkeeper/packages/web/1.0.0");
    let full = Path::new("/var/lib/hostkeeper/packages/api/1.0.0");
    fs::create_dir_all(world.host(empty)).expect("an empty directory");
    fs::create_dir_all(world.host(full)).expect("a full directory");
    fs::write(world.host(full).join("manifest.toml"), "x").expect("a file in it");

    let real = RealFs::new(world.root());
    real.remove_dir(full).expect_err("not empty");
    assert!(world.host(full).join("manifest.toml").exists());

    world.fs.remove_dir(empty).expect("removed");
    assert!(!world.host(empty).exists());
    assert_eq!(
        world.log.events(),
        [Event::Fs(FsWrite::RemoveDir(empty.to_path_buf()))]
    );

    fs::create_dir_all(world.host(empty)).expect("made again");
    let failing = FaultFs::failing_at(world.root(), 1, world.log.clone());
    failing
        .remove_dir(empty)
        .expect_err("the first write fails");
    assert!(world.host(empty).exists());
    assert_eq!(world.log.events().len(), 1);
}

/// Catches a fake that cannot stand for a unit that will not stop, or that stops failing other
/// calls: after `fail_stop`, the stop of that one unit is recorded and fails, the stop of every
/// other unit and `daemon-reload` pass; a mark a test writes lands in its place among them.
#[test]
fn the_fake_unit_control_can_refuse_to_stop_one_unit() {
    let log = Log::new();
    let units = FakeUnitControl::new(log.clone());
    units.fail_stop("web.service");
    units.stop_unit("api.service").expect("another unit stops");
    log.push(Event::Mark("hook".to_owned()));
    let refused = units
        .stop_unit("web.service")
        .expect_err("it will not stop");
    assert_eq!(refused.why, "the fake fails this call");
    units.daemon_reload().expect("a reload goes through");
    assert_eq!(
        log.events(),
        [
            Event::Unit(UnitCall::Stop("api.service".to_owned())),
            Event::Mark("hook".to_owned()),
            Event::Unit(UnitCall::Stop("web.service".to_owned())),
            Event::Unit(UnitCall::DaemonReload),
        ]
    );
}
