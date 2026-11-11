//! `export` of the MDM gate's device records: the rows of `mdm_devices` of a database (migration
//! `0005`) written as the files `<--enrollment-dir>/mdm-devices/<enrollment id>.pb` of a file
//! store.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_mdm_gate::{DeviceRecord, DeviceStore, EnrollmentId, FileDeviceStore, Phase};

use super::TransferError;
use super::export;
use super::tests::{Store, clock, export_flags, seeded};
use crate::lock::FileLock;
use crate::mdm::DEVICES_DIR;
use crate::store::postgres::tests::TestDatabase;

/// The records of two devices, one enrolled and supervised, one checked out.
fn devices() -> Vec<DeviceRecord> {
    let id = |text: &str| EnrollmentId::parse(text).expect("an enrollment id");
    let mut enrolled = DeviceRecord::new(id("A1B2-C3"), 100);
    enrolled.serial = Some("C02SERIAL".to_owned());
    enrolled.phase = Phase::Enrolled;
    enrolled.supervised = Some(true);
    let mut gone = DeviceRecord::new(id("D4E5-F6"), 200);
    gone.phase = Phase::CheckedOut;
    gone.checked_out_unix = Some(300);
    vec![enrolled, gone]
}

/// The directory of the device records of the enrollment store of `store`.
fn devices_dir(store: &Store) -> std::path::PathBuf {
    store
        .paths
        .enrollment_dir
        .as_ref()
        .expect("an enrollment store")
        .join(DEVICES_DIR)
}

/// The device records `FileDeviceStore` reads in `dir`, in the order of their enrollment ids.
fn files_of(dir: &Path) -> Vec<DeviceRecord> {
    let mut records = FileDeviceStore::open(Arc::new(RealFs), dir)
        .expect("the store")
        .load()
        .expect("the records read");
    records.sort_by(|a, b| a.enrollment_id.as_str().cmp(b.enrollment_id.as_str()));
    records
}

/// A recorded file store with an enrollment store that holds only the gate's records of
/// [`devices`], written by the gate's own file store.
fn store_with_devices(name: &str) -> Store {
    let mut store = Store::recorded(name);
    store.paths.enrollment_dir = Some(store.dir.path().join("enrollment"));
    let files = FileDeviceStore::open(Arc::new(RealFs), &devices_dir(&store)).expect("the store");
    for record in devices() {
        files.save(&record).expect("saved");
    }
    store
}

/// Proves: exporting a database that holds device records writes each back as
/// `mdm-devices/<enrollment id>.pb` of the enrollment store, byte for byte the file the gate's
/// file store wrote, so the file store of the back-out reads the records the database held; and
/// the report counts the rows. A database that holds only device records and no enrollment
/// record still needs `--enrollment-dir`, and is refused without it.
///
/// Catches: an export that drops the device records, writes them in another encoding than the
/// gate's file store reads, or loses them silently when no enrollment directory is given.
#[test]
fn export_writes_the_device_records_back_as_the_files_they_were() {
    let from = store_with_devices("transfer-mdm-export-from");
    let database = TestDatabase::new();
    seeded(&from, &database);
    let mut into = Store::new("transfer-mdm-export-into");
    into.paths.enrollment_dir = Some(into.dir.path().join("enrollment"));
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();
    assert!(report.contains("  mdm_devices: 2 rows\n"), "{report}");
    for record in devices() {
        let name = format!("{}.pb", record.enrollment_id);
        assert_eq!(
            fs::read(devices_dir(&into).join(&name)).expect("the exported file"),
            fs::read(devices_dir(&from).join(&name)).expect("the original file"),
            "{name}"
        );
    }
    assert_eq!(files_of(&devices_dir(&into)), devices());

    let without = Store::new("transfer-mdm-export-without");
    assert!(matches!(
        export(&export_flags(&database, &without), &FileLock, &clock()),
        Err(TransferError::EnrollmentDirNeeded)
    ));
    assert_eq!(fs::read_dir(without.dir.path()).expect("listed").count(), 0);
}

/// Proves: an export of a database holding a row of `mdm_devices` the gate's reader refuses is
/// refused with the gate's own reason, naming the row, and writes no file.
///
/// Catches: an export that writes a record the file store would refuse at its start (the
/// back-out would not start), or skips it and forgets the device.
#[test]
fn an_export_refuses_a_device_row_the_gate_cannot_read() {
    let from = store_with_devices("transfer-mdm-torn-from");
    let database = TestDatabase::new();
    seeded(&from, &database);
    database
        .run("INSERT INTO mdm_devices (enrollment_id, record) VALUES ('AAAA-1', '\\xffff')")
        .expect("a torn row");
    let mut into = Store::new("transfer-mdm-torn-into");
    into.paths.enrollment_dir = Some(into.dir.path().join("enrollment"));
    let refused = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect_err("a row the gate cannot read");
    assert!(matches!(refused, TransferError::Database(_)), "{refused:?}");
    assert_eq!(
        refused.to_string(),
        "database:mdm_devices/AAAA-1: the file is not a record; the gate does not guess what a \
         device's record said, so fix or remove the file to start"
    );
    assert_eq!(into.files().len(), 0);
}
