//! The cases every device store passes ([`hostkeeper_mdm_gate::conformance`]), run on the files of
//! `FileDeviceStore`, and against stores that are wrong in one way each, so the list is shown to
//! catch what each case claims. The coordinator runs the same list on its table `mdm_devices`.

mod support;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::fs::Fs;
use hostkeeper_mdm_gate::conformance::{self, Clause, Subject};
use hostkeeper_mdm_gate::{
    DeviceRecord, DeviceStore, FileDeviceStore, StoreError, decode_record, encode_record,
};
use hostkeeper_testkit::Scratch;
use support::{TestFs, scratch};

/// The files of one scratch directory, and a file system that fails the write it is told to.
struct OnFiles {
    scratch: Scratch,
    fs: Arc<TestFs>,
}

impl OnFiles {
    fn dir(&self) -> std::path::PathBuf {
        self.scratch.path().join("mdm-devices")
    }
}

impl Subject for OnFiles {
    fn open(&self) -> Box<dyn DeviceStore> {
        let fs: Arc<dyn Fs> = Arc::clone(&self.fs) as Arc<dyn Fs>;
        Box::new(FileDeviceStore::open(fs, &self.dir()).expect("the store opens"))
    }

    fn fail_next_save(&self) {
        self.fs.fail_write(self.fs.writes() + 1);
    }

    fn put_bytes(&self, key: &str, bytes: &[u8]) {
        std::fs::create_dir_all(self.dir()).expect("the directory");
        std::fs::write(self.dir().join(format!("{key}.pb")), bytes).expect("the file");
    }
}

/// Proves: the file store passes every case of the list.
#[test]
fn the_device_store_cases_pass_on_the_files() {
    let failures = conformance::run(&|| {
        Box::new(OnFiles {
            scratch: scratch("conformance-files"),
            fs: Arc::new(TestFs::default()),
        })
    });
    let failures: Vec<String> = failures.iter().map(ToString::to_string).collect();
    assert_eq!(failures, Vec::<String>::new());
}

/// One way a store can be wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Defect {
    /// None: the store is right.
    None,
    /// A new store holds a record already.
    Prefilled,
    /// Every load fails.
    LoadFails,
    /// Every save fails.
    SaveFails,
    /// A save forgets the record's last error.
    DropsAField,
    /// A save of an enrollment id already kept keeps the first record.
    KeepsTheFirst,
    /// A save told to fail writes the record and then fails.
    WritesWhenFailing,
    /// A save told to fail answers Ok, writing nothing.
    OkWhenFailing,
    /// Once a save failed, every later save fails too.
    StaysFailed,
    /// A load skips a record it cannot read.
    SkipsWhatItCannotRead,
    /// A load skips a record of another enrollment id than its key.
    SkipsOtherKeys,
    /// A load refuses a record holding a field it does not know.
    RefusesUnknownFields,
    /// A load refuses a record that is not whole with another reason than the gate's.
    OtherReason,
    /// A load refuses a record of a newer schema naming another schema.
    OtherSchema,
    /// The save after a failed one answers Ok and writes nothing.
    DropsTheSaveAfterAFailure,
}

/// A store in memory, wrong in the way `defect` says.
struct InMemory {
    defect: Defect,
    kept: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    fail: Arc<AtomicBool>,
}

impl InMemory {
    fn new(defect: Defect) -> InMemory {
        let mut kept = BTreeMap::new();
        if defect == Defect::Prefilled {
            let record = DeviceRecord::new(support::id("z-9"), 1);
            kept.insert("z-9".to_owned(), encode_record(&record));
        }
        InMemory {
            defect,
            kept: Arc::new(Mutex::new(kept)),
            fail: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct MemoryStore {
    defect: Defect,
    kept: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    fail: Arc<AtomicBool>,
    /// Whether the last save was told to fail ([`Defect::DropsTheSaveAfterAFailure`]).
    dropping: AtomicBool,
}

fn refused() -> StoreError {
    StoreError::Io {
        path: "memory".into(),
        source: std::io::Error::other("refused"),
    }
}

impl DeviceStore for MemoryStore {
    fn load(&self) -> Result<Vec<DeviceRecord>, StoreError> {
        if self.defect == Defect::LoadFails {
            return Err(refused());
        }
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let mut records = Vec::new();
        for (key, bytes) in kept.iter() {
            match (decode_record(Path::new("memory"), key, bytes), self.defect) {
                (Err(_), Defect::SkipsWhatItCannotRead) => {}
                (Err(StoreError::Corrupt { reason, .. }), Defect::SkipsOtherKeys)
                    if reason == "the record's enrollment id is not its key" => {}
                (Ok(record), Defect::RefusesUnknownFields) if encode_record(&record) != *bytes => {
                    return Err(refused());
                }
                (Err(StoreError::Corrupt { path, .. }), Defect::OtherReason) => {
                    return Err(StoreError::Corrupt {
                        path,
                        reason: "another reason".to_owned(),
                    });
                }
                (Err(StoreError::NewerSchema { path, schema }), Defect::OtherSchema) => {
                    return Err(StoreError::NewerSchema {
                        path,
                        schema: schema - 1,
                    });
                }
                (outcome, _) => records.push(outcome?),
            }
        }
        Ok(records)
    }

    fn save(&self, record: &DeviceRecord) -> Result<(), StoreError> {
        let failing = match self.defect {
            Defect::StaysFailed => self.fail.load(Ordering::SeqCst),
            _ => self.fail.swap(false, Ordering::SeqCst),
        };
        if self.defect == Defect::DropsTheSaveAfterAFailure
            && self.dropping.swap(failing, Ordering::SeqCst)
        {
            return Ok(());
        }
        match (self.defect, failing) {
            (Defect::SaveFails, _) => return Err(refused()),
            (Defect::OkWhenFailing, true) => return Ok(()),
            (Defect::WritesWhenFailing, _) | (_, false) => {}
            (_, true) => return Err(refused()),
        }
        let mut record = record.clone();
        if self.defect == Defect::DropsAField {
            record.last_error = None;
        }
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let key = record.enrollment_id.as_str().to_owned();
        if self.defect == Defect::KeepsTheFirst && kept.contains_key(&key) {
            return Ok(());
        }
        kept.insert(key, encode_record(&record));
        if failing {
            // Defect::WritesWhenFailing: written, and then the error.
            return Err(refused());
        }
        Ok(())
    }
}

impl Subject for InMemory {
    fn open(&self) -> Box<dyn DeviceStore> {
        Box::new(MemoryStore {
            defect: self.defect,
            kept: Arc::clone(&self.kept),
            fail: Arc::clone(&self.fail),
            dropping: AtomicBool::new(false),
        })
    }

    fn fail_next_save(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }

    fn put_bytes(&self, key: &str, bytes: &[u8]) {
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), bytes.to_vec());
    }
}

/// The cases a store wrong by `defect` fails.
fn failed_by(defect: Defect) -> Vec<Clause> {
    conformance::run(&|| Box::new(InMemory::new(defect)))
        .into_iter()
        .map(|failure| failure.clause)
        .collect()
}

/// Proves: the list catches each way a store can be wrong in the case that claims it, and a store
/// in memory that is right passes it whole (so each failure below is the defect's).
#[test]
fn the_cases_fail_against_a_store_that_breaks_each_one() {
    assert_eq!(failed_by(Defect::None), Vec::<Clause>::new());
    let all = vec![
        Clause::Empty,
        Clause::RoundTrip,
        Clause::Replace,
        Clause::FailedSave,
        Clause::NotWhole,
        Clause::NewerSchema,
        Clause::OtherKey,
        Clause::UnknownField,
    ];
    let cases = [
        (
            Defect::Prefilled,
            vec![
                Clause::Empty,
                Clause::RoundTrip,
                Clause::Replace,
                Clause::FailedSave,
                Clause::UnknownField,
            ],
        ),
        (Defect::LoadFails, all.clone()),
        (
            Defect::SaveFails,
            vec![Clause::RoundTrip, Clause::Replace, Clause::FailedSave],
        ),
        (
            Defect::DropsAField,
            vec![Clause::RoundTrip, Clause::Replace, Clause::FailedSave],
        ),
        (
            Defect::KeepsTheFirst,
            vec![Clause::Replace, Clause::FailedSave],
        ),
        (Defect::WritesWhenFailing, vec![Clause::FailedSave]),
        (Defect::OkWhenFailing, vec![Clause::FailedSave]),
        (Defect::StaysFailed, vec![Clause::FailedSave]),
        (
            Defect::SkipsWhatItCannotRead,
            vec![Clause::NotWhole, Clause::NewerSchema, Clause::OtherKey],
        ),
        (Defect::SkipsOtherKeys, vec![Clause::OtherKey]),
        (Defect::RefusesUnknownFields, vec![Clause::UnknownField]),
        (Defect::OtherReason, vec![Clause::NotWhole]),
        (Defect::OtherSchema, vec![Clause::NewerSchema]),
        (Defect::DropsTheSaveAfterAFailure, vec![Clause::FailedSave]),
    ];
    for (defect, clauses) in cases {
        assert_eq!(failed_by(defect), clauses, "{defect:?}");
    }
    // Every failure names its case and says what the store did.
    let failure = &conformance::run(&|| Box::new(InMemory::new(Defect::OkWhenFailing)))[0];
    assert_eq!(
        failure.to_string(),
        "FailedSave: a save told to fail returned Ok"
    );
}
