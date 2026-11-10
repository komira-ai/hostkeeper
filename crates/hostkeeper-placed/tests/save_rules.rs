//! What `Store::save` refuses before it writes: a record whose known fields break the rules a
//! load holds them to.

mod support;

use std::path::PathBuf;

use hostkeeper_placed::{InstalledEntry, PlacedError, PlacedRecord, Tombstone};
use support::World;

const PLACED: &str = "/var/lib/hostkeeper/placed.json";

fn good() -> PlacedRecord {
    let mut record = PlacedRecord::default();
    record
        .installed
        .insert("x".to_owned(), InstalledEntry::new("apt"));
    record
}

/// Catches a save that writes a record no load would accept: a schema that is not 1, an installed
/// service that is not a service name and a tombstone for a non-service are each `Corrupt` naming
/// `placed.json` and the rule, with nothing written and the bytes of the old record as they were.
#[test]
fn save_refuses_a_record_that_breaks_its_rules_before_writing() {
    let mut schema = good();
    schema.schema = 0;
    let mut service = good();
    service
        .installed
        .insert("bad name".to_owned(), InstalledEntry::new("apt"));
    let mut tombstone = good();
    tombstone
        .uninstalling
        .insert("a/b".to_owned(), Tombstone::default());
    let cases = [
        (schema, "its schema is 0"),
        (
            service,
            "the installed service \"bad name\" is not a service name",
        ),
        (tombstone, "the tombstone \"a/b\" is not a service name"),
    ];
    for (n, (record, fragment)) in cases.into_iter().enumerate() {
        let world = World::new(&format!("save-rules-{n}"));
        let placer = world.placer();
        let lock = placer.lock().expect("the lock is free");
        placer.store().save(&good(), &lock).expect("the old record");
        let old = world.placed_bytes();
        assert!(old.is_some());
        world.log.clear();

        match placer.store().save(&record, &lock) {
            Err(PlacedError::Corrupt { path, why }) => {
                assert_eq!(path, PathBuf::from(PLACED), "{fragment}");
                assert!(why.contains(fragment), "{fragment}: {why}");
            }
            other => panic!("{fragment}: expected Corrupt, got {other:?}"),
        }
        assert!(world.writes().is_empty(), "{fragment}");
        assert_eq!(world.placed_bytes(), old, "{fragment}");
        assert_eq!(world.record(), good());
    }
}
