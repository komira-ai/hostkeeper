//! The record's file: what this version writes reads back equal, a newer schema is refused
//! untouched, a record that does not read is set aside only by `clear_record`, and a record that
//! breaks a rule is neither written nor read.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_steps::CrashPoint;
use hostkeeper_steps::maintenance::{
    Event, InvalidRecord, Kind, MAINTENANCE_CORRUPT_FILE, MAINTENANCE_RECORD_FILE,
    MAINTENANCE_SCHEMA, MaintenanceLoaded, MaintenanceRecord, MaintenanceRecordError,
    MaintenanceStore, Outcome, next_action,
};

use crate::support::{Scratch, apply, boot, defer, drain, ok_until, on, record, run_step, service};

/// A record with every key set: a kind applied, a reboot under way, services drained, a
/// deferral, a halt and the note of a Put back.
fn full_record() -> MaintenanceRecord {
    let rec = ok_until(
        record(&[Kind::Linux, Kind::Reboot], &["js-agent", "tierstore"]),
        &apply(Kind::Linux),
    );
    let (_, rec, _) = run_step(&rec, Outcome::Ok);
    let (started, _) = on(&rec, Event::Started);
    let (mut rec, _) = on(&started, Event::RebootUnderWay(boot(3)));
    rec.deferred = Some("drain tierstore: the drain deadline passed".to_owned());
    rec.halted = Some("check: exit 1".to_owned());
    rec.put_back = Some("apply linux: exit 1".to_owned());
    rec
}

fn current(store: &MaintenanceStore) -> MaintenanceRecord {
    match store.load().expect("the record loads") {
        MaintenanceLoaded::Current(rec) => *rec,
        other => panic!("not a current record: {other:?}"),
    }
}

fn write_file(dir: &Path, text: &str) {
    fs::write(dir.join(MAINTENANCE_RECORD_FILE), text).expect("write the record file");
}

fn read_file(dir: &Path, name: &str) -> Vec<u8> {
    fs::read(dir.join(name)).expect("read the file")
}

fn corrupt_why(store: &MaintenanceStore) -> String {
    match store.load() {
        Err(MaintenanceRecordError::Corrupt(why)) => why,
        other => panic!("not corrupt: {other:?}"),
    }
}

/// Catches a field lost or changed on the way to disk and back, a key written for a field that
/// is not set, and a file not at its name.
#[test]
fn a_record_written_by_this_version_reads_back_equal() {
    let dir = Scratch::new("mu-roundtrip");
    let store = MaintenanceStore::new(dir.path());
    assert_eq!(store.load().expect("load"), MaintenanceLoaded::Absent);

    let full = full_record();
    assert_eq!(full.kinds[1].reboot_under_way, Some(boot(3)));
    store.save(&full).expect("save");
    assert_eq!(current(&store), full);

    let fresh = record(&[Kind::Macos], &[]);
    store.save(&fresh).expect("save");
    assert_eq!(current(&store), fresh);
    let value: serde_json::Value =
        serde_json::from_slice(&read_file(dir.path(), MAINTENANCE_RECORD_FILE)).expect("JSON");
    assert_eq!(
        value,
        serde_json::json!({
            "schema": MAINTENANCE_SCHEMA,
            "run_id": "run-1",
            "order_id": "order-1",
            "order_try": 1,
            "kinds": [{ "kind": "macos", "applied": false }],
            "services": [],
            "drained": [],
            "step": "stage",
            "running": false,
        })
    );
}

/// Catches the keys of the file renamed: a record as the module's documentation shows it
/// reads, with every key, and so does one without the keys written only when set.
#[test]
fn the_record_of_the_documented_schema_reads() {
    let dir = Scratch::new("mu-documented");
    let store = MaintenanceStore::new(dir.path());
    write_file(
        dir.path(),
        r#"{
          "schema": 1,
          "run_id": "run-7",
          "order_id": "order-12",
          "order_try": 2,
          "kinds": [
            { "kind": "linux", "applied": true },
            { "kind": "reboot", "applied": false,
              "reboot_under_way": "0b6f3f3e-2a43-4b83-9a0a-6e0f1a8d1c55" }
          ],
          "services": ["js-agent", "tierstore"],
          "drained": ["js-agent", "tierstore"],
          "step": "put_back",
          "running": true,
          "deferred": "drain tierstore: the drain deadline passed",
          "halted": "resume tierstore: exit 1",
          "put_back": "apply linux: exit 1",
          "a_key_of_a_later_release": 1
        }"#,
    );
    let rec = current(&store);
    assert_eq!(rec.run_id, "run-7");
    assert_eq!(rec.order_try, 2);
    assert_eq!(
        rec.kinds[1].reboot_under_way.as_ref().map(|id| id.as_str()),
        Some("0b6f3f3e-2a43-4b83-9a0a-6e0f1a8d1c55")
    );
    assert_eq!(rec.drained, [service("js-agent"), service("tierstore")]);
    assert_eq!(rec.halted.as_deref(), Some("resume tierstore: exit 1"));
    assert_eq!(rec.put_back.as_deref(), Some("apply linux: exit 1"));
}

/// Catches a record of a newer schema read as this one, written over by a save, or set aside by
/// `clear_record`: each is refused and the file keeps its bytes.
#[test]
fn a_newer_schema_record_is_refused_and_left_untouched() {
    let dir = Scratch::new("mu-newer");
    let store = MaintenanceStore::new(dir.path());
    let text = r#"{"schema": 2, "run_id": "run-9", "steps_of_a_later_release": []}"#;
    write_file(dir.path(), text);

    assert_eq!(
        store.load().expect("load"),
        MaintenanceLoaded::Newer { schema: 2 }
    );
    let saved = store.save(&record(&[Kind::Linux], &[]));
    assert!(
        matches!(saved, Err(MaintenanceRecordError::Newer { schema: 2 })),
        "{saved:?}"
    );
    let cleared = store
        .clear_record()
        .expect_err("a newer record is not cleared");
    assert_eq!(
        cleared.to_string(),
        "the maintenance record has schema 2, of a newer hostkeeper; it is left as it is"
    );
    assert_eq!(
        read_file(dir.path(), MAINTENANCE_RECORD_FILE),
        text.as_bytes()
    );
    assert!(!dir.path().join(MAINTENANCE_CORRUPT_FILE).exists());
}

/// Catches a record that does not read written over by a save, so that what broke is lost:
/// only `clear_record` sets it aside, keeping its bytes, and a new turn can then be written.
#[test]
fn a_corrupt_record_is_set_aside_only_by_clear_record() {
    let dir = Scratch::new("mu-corrupt");
    let store = MaintenanceStore::new(dir.path());
    let text = "{\"schema\": 1, \"run_id\": ";
    write_file(dir.path(), text);

    let broke = corrupt_why(&store);
    let saved = store
        .save(&record(&[Kind::Linux], &[]))
        .expect_err("a corrupt record is not written over");
    assert!(
        matches!(saved, MaintenanceRecordError::Corrupt(_)),
        "{saved:?}"
    );
    assert_eq!(
        saved.to_string(),
        format!("the maintenance record is not readable: {broke}")
    );
    assert_eq!(
        read_file(dir.path(), MAINTENANCE_RECORD_FILE),
        text.as_bytes()
    );

    let why = store.clear_record().expect("cleared");
    assert!(why.contains("EOF"), "{why}");
    assert!(!dir.path().join(MAINTENANCE_RECORD_FILE).exists());
    assert_eq!(
        read_file(dir.path(), MAINTENANCE_CORRUPT_FILE),
        text.as_bytes()
    );
    assert_eq!(store.load().expect("load"), MaintenanceLoaded::Absent);

    let fresh = record(&[Kind::Linux], &[]);
    store.save(&fresh).expect("save after the clear");
    assert_eq!(current(&store), fresh);
}

/// Catches `clear_record` touching a record that is absent or reads: each is refused by name.
#[test]
fn clear_record_refuses_an_absent_record_and_one_that_reads() {
    let dir = Scratch::new("mu-clear-refused");
    let store = MaintenanceStore::new(dir.path());
    let absent = store.clear_record().expect_err("absent");
    assert_eq!(absent.to_string(), "there is no maintenance record");

    let rec = record(&[Kind::Linux], &["js-agent"]);
    store.save(&rec).expect("save");
    let reads = store.clear_record().expect_err("reads");
    assert_eq!(
        reads.to_string(),
        "the maintenance record reads; only one that does not is set aside"
    );
    assert_eq!(current(&store), rec);
    assert!(!dir.path().join(MAINTENANCE_CORRUPT_FILE).exists());
}

/// Catches a record that cannot be read for a reason of the directory read as corrupt (and so
/// set aside): an I/O error is refused as one.
#[test]
fn a_record_that_cannot_be_read_is_an_io_error_and_not_set_aside() {
    let dir = Scratch::new("mu-io");
    let store = MaintenanceStore::new(dir.path());
    fs::create_dir(dir.path().join(MAINTENANCE_RECORD_FILE)).expect("a directory in its place");
    let loaded = store.load().expect_err("not readable");
    assert!(
        matches!(loaded, MaintenanceRecordError::Io(_)),
        "{loaded:?}"
    );
    assert!(
        loaded
            .to_string()
            .starts_with("cannot use the maintenance directory: "),
        "{loaded}"
    );
    let cleared = store.clear_record();
    assert!(
        matches!(cleared, Err(MaintenanceRecordError::Io(_))),
        "{cleared:?}"
    );
    assert!(dir.path().join(MAINTENANCE_RECORD_FILE).is_dir());
}

/// Catches a file that breaks a rule of the record read as a record: each is corrupt, naming
/// what broke.
#[test]
fn a_file_that_breaks_a_rule_of_the_record_is_corrupt() {
    let dir = Scratch::new("mu-rules");
    let store = MaintenanceStore::new(dir.path());
    let base = serde_json::json!({
        "schema": 1, "run_id": "run-1", "order_id": "order-1", "order_try": 1,
        "kinds": [{ "kind": "linux", "applied": false }],
        "services": ["js-agent", "tierstore"], "drained": ["js-agent"],
        "step": "drain", "running": false,
    });
    let cases = [
        (
            "run_id",
            serde_json::json!(""),
            "the maintenance record's run_id is empty",
        ),
        (
            "order_id",
            serde_json::json!(""),
            "the maintenance record's order_id is empty",
        ),
        (
            "kinds",
            serde_json::json!([]),
            "the maintenance record names no kind",
        ),
        (
            "kinds",
            serde_json::json!([
                { "kind": "xcode", "applied": false },
                { "kind": "macos", "applied": false }
            ]),
            "the maintenance record's kinds are not each named once, macos first: xcode, macos",
        ),
        (
            "kinds",
            serde_json::json!([
                { "kind": "linux", "applied": false },
                { "kind": "linux", "applied": false }
            ]),
            "the maintenance record's kinds are not each named once, macos first: linux, linux",
        ),
        (
            "services",
            serde_json::json!(["js-agent", "js-agent"]),
            "the maintenance record names the service js-agent twice",
        ),
        (
            "drained",
            serde_json::json!(["tierstore"]),
            "the maintenance record's drained services are not the first of its services",
        ),
    ];
    for (key, value, why) in cases {
        let mut file = base.clone();
        file[key] = value;
        write_file(dir.path(), &file.to_string());
        assert_eq!(corrupt_why(&store), why, "{key}");
    }
    write_file(dir.path(), &base.to_string());
    current(&store);
}

/// Catches a boot id, a service name, a kind or a step that is not one read from the file, and
/// a file without its schema.
#[test]
fn a_file_with_a_value_that_is_not_one_is_corrupt() {
    let dir = Scratch::new("mu-values");
    let store = MaintenanceStore::new(dir.path());
    let base = serde_json::json!({
        "schema": 1, "run_id": "run-1", "order_id": "order-1", "order_try": 1,
        "kinds": [{ "kind": "linux", "applied": false }],
        "services": ["js-agent"], "drained": [], "step": "stage", "running": false,
    });
    let mut cases = Vec::new();
    let mut empty_boot = base.clone();
    empty_boot["kinds"][0]["reboot_under_way"] = serde_json::json!("  ");
    cases.push((
        empty_boot,
        "the boot id read from the maintenance record is empty",
    ));
    let mut bad_service = base.clone();
    bad_service["services"] = serde_json::json!(["-js-agent"]);
    cases.push((bad_service, "-js-agent"));
    let mut bad_kind = base.clone();
    bad_kind["kinds"][0]["kind"] = serde_json::json!("windows");
    cases.push((bad_kind, "windows"));
    let mut bad_step = base.clone();
    bad_step["step"] = serde_json::json!("rollback");
    cases.push((bad_step, "rollback"));
    let mut no_schema = base.clone();
    no_schema
        .as_object_mut()
        .expect("an object")
        .remove("schema");
    cases.push((no_schema, "schema"));
    for (file, named) in cases {
        write_file(dir.path(), &file.to_string());
        let why = corrupt_why(&store);
        assert!(why.contains(named), "{why} names {named}");
    }
}

/// Catches a new record taken with no id, no kind, a kind or a service twice, and a save that
/// writes a record it could not read back.
#[test]
fn a_record_that_breaks_a_rule_is_neither_made_nor_written() {
    let names = vec![service("js-agent")];
    let new = |run: &str, order: &str, kinds: &[Kind], services: Vec<_>| {
        MaintenanceRecord::new(run, order, 1, kinds, services).expect_err("refused")
    };
    assert_eq!(
        new("", "order-1", &[Kind::Linux], names.clone()),
        InvalidRecord::EmptyId("run_id")
    );
    assert_eq!(
        new("run-1", "", &[Kind::Linux], names.clone()),
        InvalidRecord::EmptyId("order_id")
    );
    assert_eq!(
        new("run-1", "order-1", &[], names.clone()),
        InvalidRecord::NoKinds
    );
    assert_eq!(
        new(
            "run-1",
            "order-1",
            &[Kind::Reboot, Kind::Linux, Kind::Reboot],
            names.clone()
        ),
        InvalidRecord::Kinds("linux, reboot, reboot".to_owned())
    );
    assert_eq!(
        new(
            "run-1",
            "order-1",
            &[Kind::Linux],
            vec![service("a"), service("a")]
        ),
        InvalidRecord::RepeatedService("a".to_owned())
    );

    let dir = Scratch::new("mu-invalid-save");
    let store = MaintenanceStore::new(dir.path());
    let mut rec = record(&[Kind::Linux], &["js-agent"]);
    rec.drained = vec![service("tierstore")];
    let saved = store.save(&rec).expect_err("refused");
    assert_eq!(
        saved.to_string(),
        "the maintenance record is not written: the maintenance record's drained services are \
         not the first of its services"
    );
    assert_eq!(store.load().expect("load"), MaintenanceLoaded::Absent);
}

/// Catches the record's directory placed in the state directory, where a service's directory
/// of the same name could meet it, rather than beside it.
#[test]
fn the_store_of_a_state_directory_is_the_maintenance_directory_beside_it() {
    let store = MaintenanceStore::beside(Path::new("/var/lib/updater/state"));
    assert_eq!(store.dir(), Path::new("/var/lib/updater/maintenance"));
    let root = MaintenanceStore::beside(Path::new("/"));
    assert_eq!(root.dir(), Path::new("/maintenance"));
}

/// Catches a save that touches the file before its write point, or does not leave the new
/// record once past it: a crash before the write leaves the old record, one after it the new,
/// and a later write of the same store is not stopped.
#[test]
fn a_crash_before_a_write_keeps_the_old_record_and_after_it_the_new() {
    let dir = Scratch::new("mu-crash-points");
    let old = record(&[Kind::Linux], &["js-agent"]);
    MaintenanceStore::new(dir.path()).save(&old).expect("save");
    let (_, new, _) = run_step(&old, Outcome::Ok);
    assert_eq!(next_action(&new), drain("js-agent"));

    let before = MaintenanceStore::new(dir.path())
        .with_crash_gate(Arc::new(|point| point == CrashPoint::BeforeWrite(1)));
    let crashed = before.save(&new);
    assert!(
        matches!(
            crashed,
            Err(MaintenanceRecordError::Crashed(CrashPoint::BeforeWrite(1)))
        ),
        "{crashed:?}"
    );
    assert_eq!(current(&before), old);

    let after = MaintenanceStore::new(dir.path())
        .with_crash_gate(Arc::new(|point| point == CrashPoint::AfterWrite(1)));
    let crashed = after.save(&new);
    assert_eq!(
        crashed.expect_err("stopped").to_string(),
        "stopped at AfterWrite(1)"
    );
    assert_eq!(current(&after), new);
    let (_, deferred, _) = run_step(&new, defer("the drain deadline passed"));
    after
        .save(&deferred)
        .expect("the second write passes the gate");
    assert_eq!(current(&after), deferred);
}
