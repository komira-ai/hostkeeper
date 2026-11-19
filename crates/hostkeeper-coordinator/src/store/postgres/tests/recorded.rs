//! The records the coordinator writes at its start for the hosts enrolled without one
//! (`inventory_enrolled.rs`), on two coordinators over one database: each record is written once,
//! audited once, whichever coordinator writes it, however the two starts overlap.

use std::sync::Arc;

use hostkeeper_enrollment::fs::RealFs;

use super::TestDatabase;
use super::coordinator::start_on_postgres;
use crate::enrollment::tests::support::{
    Files, FixedClock, HostKey, NOW, SERIAL, approve, mac_request,
};
use crate::enrollment::{Enrollment, Setup, StoreAt, os_random, setup};
use crate::service::Coordinator;
use crate::store::postgres::PgRecords;
use crate::testutil::{Rig, scratch};

/// A coordinator on `db` named `holder`, with enrollment over the database's records and the CA
/// of `files`, as `serve` starts one; and its enrollment.
fn coordinator(db: &TestDatabase, holder: &str, files: &Files) -> (Rig, Arc<Enrollment>) {
    let (store, loaded) = db.open(holder).expect("the store opens");
    let mut rig = Rig::new(holder);
    let marker = rig.dir.path().join("halt.marker");
    let enrollment = Arc::new(
        setup(
            Arc::new(RealFs),
            &Setup {
                store: StoreAt::Records(Arc::new(PgRecords::over(&store))),
                ..files.setup(true)
            },
            FixedClock::at(NOW),
            os_random(),
        )
        .expect("enrollment starts over the database"),
    );
    rig.coordinator =
        start_on_postgres(&rig, &store, loaded, &marker).with_enrollment(Arc::clone(&enrollment));
    (rig, enrollment)
}

/// The `record_add` lines of `coordinator`'s audit, as the database holds them: who and about
/// which host.
fn record_adds(coordinator: &Coordinator) -> Vec<(String, Option<String>)> {
    coordinator
        .ops()
        .audit_log()
        .expect("the audit log")
        .into_iter()
        .filter(|entry| entry.action == "record_add")
        .map(|entry| (entry.by, entry.node_id))
        .collect()
}

/// Proves: two coordinators over one database that start at once, each finding the Mac `mac-1`
/// enrolled with no record, write its record once between them: the two counts add up to 1, the
/// database (read by a third start) holds one record of it, and the audit one `record_add` by
/// `coordinator`; a start after that writes nothing.
///
/// Catches: each coordinator writing the record from its own copy of the records (a second
/// record, or a second audit line), and a start refused or stopped by the other's record.
#[test]
fn two_coordinators_starting_at_once_record_a_host_once() {
    let db = TestDatabase::new();
    let dir = scratch("recorded-two");
    let files = Files::in_dir(&dir);
    let (first, enrollment) = coordinator(&db, "first", &files);
    let (second, _) = coordinator(&db, "second", &files);
    let key = HostKey::new();
    enrollment
        .enroll(mac_request(&key, "mac-m1", SERIAL, &[]))
        .expect("posted");
    approve(&enrollment, &key, "mac-1", false);

    let written = std::thread::scope(|scope| {
        let one = scope.spawn(|| first.coordinator.record_unrecorded());
        let two = scope.spawn(|| second.coordinator.record_unrecorded());
        one.join().expect("the first starts") + two.join().expect("the second starts")
    });

    assert_eq!(written, 1);
    let (third, _) = coordinator(&db, "third", &files);
    let held = third.coordinator.ops.read(|state| {
        state
            .inventory
            .iter()
            .map(|r| (r.host_id.clone(), r.serial.clone(), r.by.clone()))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        held,
        [(
            "mac-1".to_owned(),
            SERIAL.to_owned(),
            "coordinator".to_owned()
        )]
    );
    assert_eq!(
        record_adds(&third.coordinator),
        [("coordinator".to_owned(), Some("mac-1".to_owned()))]
    );
    assert_eq!(third.coordinator.record_unrecorded(), 0);
}
