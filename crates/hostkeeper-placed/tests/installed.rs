//! The `installed` key of `placed.json` (AGENTS.md B8): read across a release boundary in both
//! directions, validated, and read back by service.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_placed::{InstalledEntry, PlacedError, PlacedRecord, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use support::World;

const PLACED: &str = "/var/lib/hostkeeper/placed.json";
const STATE: &str = "/var/lib/hostkeeper";

/// `placed.json` as the release before these keys reads and writes it: the keys it does not
/// know are kept in `extra`.
#[derive(Debug, Deserialize, Serialize)]
struct PreviousRecord {
    schema: u64,
    #[serde(default)]
    paths: BTreeMap<PathBuf, Value>,
    #[serde(default)]
    uninstalling: BTreeMap<String, Map<String, Value>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reload_pending: bool,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn save(world: &World, record: &PlacedRecord) -> Result<(), PlacedError> {
    let lock = world.placer().lock().expect("the lock is free");
    Store::new(&world.fs, Path::new(STATE)).save(record, &lock)
}

/// A record with the key, and with no `paths` key, which this release no longer writes, is read
/// by the previous reader, which writes it back with the key as it was and `paths` empty; this
/// release reads it back as saved.
#[test]
fn the_previous_reader_keeps_installed() {
    let world = World::new("installed-previous");
    let mut record = PlacedRecord::default();
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("apt"));
    save(&world, &record).expect("saves");
    let written = fs::read(world.host(PLACED)).expect("written");
    let value: Value = serde_json::from_slice(&written).expect("json");
    assert_eq!(value["installed"], json!({"web": {"installer": "apt"}}));

    let previous: PreviousRecord = serde_json::from_slice(&written).expect("it reads");
    assert!(previous.uninstalling.is_empty());
    let rewritten = serde_json::to_vec_pretty(&previous).expect("it writes");
    let again: Value = serde_json::from_slice(&rewritten).expect("json");
    // The previous reader owns a `paths` key this release no longer writes: it writes it empty.
    assert_eq!(again["installed"], value["installed"]);
    assert_eq!(again["paths"], json!({}));
    fs::write(world.host(PLACED), &rewritten).expect("the previous release's write");
    let read = world.record();
    assert_eq!(read.installed, record.installed);
    assert_eq!(read.uninstalling, record.uninstalling);
}

/// A record the previous release wrote has no key and reads as an empty map; a record with none
/// is written without it, as before.
#[test]
fn a_record_without_the_key_reads_and_writes_as_before() {
    let world = World::new("installed-absent");
    let fixture = json!({"schema": 1, "paths": {}, "uninstalling": {"x": {}}});
    fs::write(world.host(PLACED), fixture.to_string()).expect("the fixture");
    let read = world.record();
    assert!(read.installed.is_empty());
    save(&world, &read).expect("saves");
    let value: Value =
        serde_json::from_slice(&fs::read(world.host(PLACED)).expect("written")).expect("json");
    assert_eq!(
        value,
        json!({"schema": 1, "paths": {}, "uninstalling": {"x": {}}})
    );
    assert!(value.get("installed").is_none());
}

/// Keys a newer writer put in an installed entry are kept on rewrite.
#[test]
fn an_installed_entry_keeps_the_keys_of_a_newer_writer() {
    let world = World::new("installed-extra");
    let fixture = json!({"schema": 1, "installed": {"web": {"installer": "apt", "n": 3}}});
    fs::write(world.host(PLACED), fixture.to_string()).expect("the fixture");
    let record = world.record();
    save(&world, &record).expect("saves");
    let value: Value =
        serde_json::from_slice(&fs::read(world.host(PLACED)).expect("written")).expect("json");
    assert_eq!(
        value["installed"]["web"],
        json!({"installer": "apt", "n": 3})
    );
}

/// A service key that is no service name is refused on save and on load; an installer name a
/// newer writer used, whatever its spelling, is kept as written: the record is read, and the
/// daemon refuses to act for the service instead.
#[test]
fn a_bad_service_key_is_refused_and_any_installer_name_is_kept() {
    let world = World::new("installed-invalid");
    let long = "a".repeat(33);
    for installer in ["", "Apt", "a_pt", "a pt", long.as_str()] {
        let mut record = PlacedRecord::default();
        record
            .installed
            .insert("web".to_owned(), InstalledEntry::new(installer));
        save(&world, &record).expect("saves");
        assert_eq!(world.record().installer_of("web"), Some(installer));
    }

    let mut bad_installed = PlacedRecord::default();
    bad_installed
        .installed
        .insert("bad name".to_owned(), InstalledEntry::new("apt"));
    assert!(matches!(
        save(&world, &bad_installed),
        Err(PlacedError::Corrupt { .. })
    ));
    let text = json!({"schema": 1, "installed": {"bad name": {"installer": "apt"}}});
    fs::write(world.host(PLACED), text.to_string()).expect("fixture");
    assert!(matches!(
        world.placer().store().load(),
        Err(PlacedError::Corrupt { .. })
    ));
}

/// The installer of a service is the one recorded, and none for a `hostkeeper-archive` one.
#[test]
fn installer_of_reads_the_recorded_installer() {
    let mut record = PlacedRecord::default();
    assert_eq!(record.installer_of("web"), None);
    record
        .installed
        .insert("web".to_owned(), InstalledEntry::new("apt"));
    assert_eq!(record.installer_of("web"), Some("apt"));
    assert_eq!(record.installer_of("api"), None);
}

/// A repository package's entry, with every key it can hold, is read by the previous reader,
/// which writes it back as it was; this release reads it back as saved. An entry of the
/// installer alone is written without the other keys.
#[test]
fn a_repository_entry_crosses_the_release_boundary_in_both_directions() {
    let world = World::new("installed-package");
    let mut entry = InstalledEntry::new("apt");
    entry.package = Some("web".to_owned());
    entry.version = Some("1:1.4.0-1".to_owned());
    entry.repository = Some("main".to_owned());
    entry.links = ["/etc/systemd/system/multi-user.target.wants/web.service".to_owned()].into();
    entry.pending = true;
    let mut record = PlacedRecord::default();
    record.installed.insert("web".to_owned(), entry);
    record
        .installed
        .insert("api".to_owned(), InstalledEntry::new("apt"));
    save(&world, &record).expect("saves");
    let written = fs::read(world.host(PLACED)).expect("written");
    let value: Value = serde_json::from_slice(&written).expect("json");
    assert_eq!(
        value["installed"],
        json!({
            "api": {"installer": "apt"},
            "web": {
                "installer": "apt",
                "package": "web",
                "version": "1:1.4.0-1",
                "repository": "main",
                "links": ["/etc/systemd/system/multi-user.target.wants/web.service"],
                "pending": true
            }
        })
    );

    let previous: PreviousRecord = serde_json::from_slice(&written).expect("it reads");
    let rewritten = serde_json::to_vec_pretty(&previous).expect("it writes");
    fs::write(world.host(PLACED), &rewritten).expect("the previous release's write");
    assert_eq!(world.record().installed, record.installed);
}
