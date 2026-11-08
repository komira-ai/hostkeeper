//! `import` and `export` of the MDM gate's device records: the files
//! `<--enrollment-dir>/mdm-devices/<enrollment id>.pb` of a file store and the rows of
//! `mdm_devices` of a database (migration `0005`).

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_mdm_gate::{DeviceRecord, DeviceStore, EnrollmentId, FileDeviceStore, Phase};

use super::TransferError;
use super::export;
use super::tests::{Store, clock, count, export_flags, imported};
use crate::lock::FileLock;
use crate::mdm::DEVICES_DIR;
use crate::store::postgres::PgDevices;
use crate::store::postgres::tests::TestDatabase;
use crate::store::postgres::transfer::MoveError;

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

/// The records the database's device store reads.
fn rows_of(database: &TestDatabase) -> Vec<DeviceRecord> {
    let (store, _) = database.open("coordinator").expect("the store opens");
    let records = PgDevices::over(&store).load().expect("the rows read");
    store.stop();
    records
}

/// Proves: importing a file store whose enrollment store holds device records writes each as a
/// row of `mdm_devices` (the report counts them), which the database's device store reads as the
/// records the files hold; the files are left as they were.
///
/// Catches: an import that leaves the device records behind (the gate on the database would
/// forget every enrolled Mac), or one that refuses them as it did before the table.
#[test]
fn import_writes_each_device_record_file_as_a_row_of_mdm_devices() {
    let store = store_with_devices("transfer-mdm-import");
    let before = store.files();
    let database = TestDatabase::new();
    let report = imported(&store, &database).expect("imported");
    assert!(report.contains("  mdm_devices: 2 rows\n"), "{report}");
    assert_eq!(count(&database, "mdm_devices"), 2);
    assert_eq!(rows_of(&database), devices());
    assert_eq!(store.files(), before);
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
    imported(&from, &database).expect("imported");
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

/// Proves: an import whose read-back of `mdm_devices` differs from the files (a trigger the test
/// installs drops one row) is rolled back whole, naming `mdm_devices`, and the database holds
/// none of it.
///
/// Catches: an import that commits without comparing the device records it wrote.
#[test]
fn an_import_whose_device_rows_read_back_otherwise_is_rolled_back() {
    let store = store_with_devices("transfer-mdm-differs");
    let database = TestDatabase::new();
    let (opened, _) = database.open("coordinator").expect("the store opens");
    opened.stop();
    database
        .run(
            "CREATE FUNCTION skewed() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
             IF NEW.enrollment_id = 'D4E5-F6' THEN RETURN NULL; END IF; RETURN NEW; END $$; \
             CREATE TRIGGER skewed BEFORE INSERT ON mdm_devices FOR EACH ROW \
             EXECUTE FUNCTION skewed()",
        )
        .expect("the trigger");
    match imported(&store, &database) {
        Err(TransferError::Database(MoveError::Differs { part })) => {
            assert_eq!(part, "mdm_devices");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(count(&database, "mdm_devices"), 0);
}
