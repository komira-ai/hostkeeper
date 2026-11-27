//! The hardware event log over its file: the contract every store keeps, the log kept across a
//! restart, a torn last line cut off, a malformed line refused by its number, and the lines of a
//! later release read. No clock is read.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_testkit::Scratch;

use super::{FileLog, hardware_events_path};
use crate::hardware_log::tests::{Subject, conformance, keys, mixed, record};
use crate::hardware_log::{HardwareLog, Query};
use crate::testutil::scratch;

/// The whole log, newest first.
fn every(log: &dyn HardwareLog) -> Result<Vec<crate::hardware_log::Record>, String> {
    log.page(&Query {
        limit: usize::MAX,
        ..Query::default()
    })
}

/// The log's file in a directory of its own.
fn file(name: &str) -> (Scratch, PathBuf) {
    let dir = scratch(name);
    let path = hardware_events_path(&dir.path().join("desired.json"));
    (dir, path)
}

/// The file: started again by opening it again.
struct OnFile {
    path: PathBuf,
    live: Arc<FileLog>,
    _dir: Scratch,
}

impl Subject for OnFile {
    fn live(&self) -> Arc<dyn HardwareLog> {
        self.live.clone()
    }

    fn restarted(&self) -> Arc<dyn HardwareLog> {
        Arc::new(FileLog::new(self.path.clone()))
    }
}

/// Proves: the file is the state file's name with `.hardware-events` added.
///
/// Catches: a log written over another store's file.
#[test]
fn the_file_is_beside_the_state_file() {
    assert_eq!(
        hardware_events_path(Path::new("/var/lib/hostkeeper-coordinator/state.json")),
        Path::new("/var/lib/hostkeeper-coordinator/state.json.hardware-events")
    );
}

/// Proves: the file keeps the contract every store of the log keeps, a store opened again on it
/// included: the same order, filters and pages as the database and the memory, and a record
/// written once.
///
/// Catches: a file store that orders, filters or pages otherwise than the database, or forgets
/// what it wrote when it is opened again.
#[test]
fn the_file_keeps_the_contract() {
    let (dir, path) = file("hardware-files-contract");
    conformance(&OnFile {
        live: Arc::new(FileLog::new(path.clone())),
        path,
        _dir: dir,
    });
}

/// Proves: a file that is not there is an empty log and is not made by a read; a write makes it,
/// readable by its owner alone, with one JSON line per record new to it, in the order written,
/// and a repeat appends no line.
///
/// Catches: a missing file read as an error, a file others can read, and a repeat that grows it.
#[test]
fn the_file_holds_one_line_per_record_written_once() {
    let (_dir, path) = file("hardware-files-lines");
    let log = FileLog::new(path.clone());
    assert_eq!(every(&log), Ok(Vec::new()));
    assert!(!path.exists());
    log.write(&mixed()).expect("written");
    log.write(&mixed()[..2]).expect("written again");
    let text = fs::read_to_string(&path).expect("the file");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5);
    assert!(text.ends_with('\n'));
    let first: crate::hardware_log::Record = serde_json::from_str(lines[0]).expect("a record");
    assert_eq!(first, mixed()[0]);
    let mode = fs::metadata(&path)
        .expect("its metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// Proves: a last line with no line break (a write a crash cut short) is cut off the file when
/// it is read, the records before it are read, and the next write appends after them, so the file
/// reads whole again after a restart.
///
/// Catches: a torn line that stops the coordinator's log for good, and a write appended onto the
/// torn line, which would make a line in the middle that is not a record.
#[test]
fn a_torn_last_line_is_cut_off_and_the_next_write_follows_the_whole_ones() {
    let (_dir, path) = file("hardware-files-torn");
    FileLog::new(path.clone())
        .write(&mixed()[..2])
        .expect("written");
    let mut text = fs::read(&path).expect("the file");
    let whole = text.len();
    text.extend_from_slice(b"{\"host_id\":\"host-z\",\"event_");
    fs::write(&path, &text).expect("torn");
    let log = FileLog::new(path.clone());
    assert_eq!(
        keys(&every(&log).expect("read")),
        keys(
            &Query {
                limit: 10,
                ..Query::default()
            }
            .page_of(mixed()[..2].iter())
        )
    );
    assert_eq!(fs::read(&path).expect("the file").len(), whole);
    log.write(&mixed()[2..]).expect("written after it");
    let again = FileLog::new(path.clone());
    assert_eq!(every(&again).expect("read").len(), 5);
}

/// Proves: a line in the file that is not a record fails every read and write, naming the file
/// and the line by its number, and nothing is written meanwhile.
///
/// Catches: a malformed line skipped, which would answer a log with records missing as whole.
#[test]
fn a_line_that_is_not_a_record_is_refused_by_its_number() {
    let (_dir, path) = file("hardware-files-malformed");
    FileLog::new(path.clone())
        .write(&mixed()[..1])
        .expect("written");
    let mut text = fs::read_to_string(&path).expect("the file");
    text.push_str("not a record\n");
    fs::write(&path, &text).expect("spoiled");
    let log = FileLog::new(path.clone());
    let why = every(&log).expect_err("refused");
    assert!(
        why.starts_with(&format!("{}: line 2 is not a record: ", path.display())),
        "{why}"
    );
    let refused = log.write(&mixed()[1..2]).expect_err("refused");
    assert!(refused.contains("line 2 is not a record"), "{refused}");
    assert_eq!(fs::read_to_string(&path).expect("the file"), text);
}

/// Proves: a line a later release wrote, with a field this one does not know, reads as its
/// record, and a line an earlier form wrote without the fields that default (serial, boot,
/// subject, raw, about, by) reads with those empty.
///
/// Catches: a log a later release wrote that this one refuses to read (a rollback that loses the
/// log), and a field this release added that an older line must hold.
#[test]
fn lines_of_another_release_are_read() {
    let (_dir, path) = file("hardware-files-releases");
    let mut later = serde_json::to_value(record("host-a", "e1", 5, "gpu", "info")).expect("JSON");
    later["slot_temperature"] = serde_json::json!(71);
    let earlier = serde_json::json!({
        "host_id": "host-b", "event_id": "e2", "time_unix": 4, "observed_unix": 9,
        "component": "disk", "severity": "warning", "state": "asserted", "source": "smart",
        "code": "disk_failing", "message": "nvme0n1 is failing"
    });
    fs::write(&path, format!("{later}\n{earlier}\n")).expect("written");
    let read = every(&FileLog::new(path)).expect("read");
    assert_eq!(read[0], record("host-a", "e1", 5, "gpu", "info"));
    assert_eq!(
        read[1],
        crate::hardware_log::Record {
            host_id: "host-b".to_owned(),
            event_id: "e2".to_owned(),
            time_unix: 4,
            observed_unix: 9,
            component: "disk".to_owned(),
            severity: "warning".to_owned(),
            state: "asserted".to_owned(),
            source: "smart".to_owned(),
            code: "disk_failing".to_owned(),
            message: "nvme0n1 is failing".to_owned(),
            ..crate::hardware_log::Record::default()
        }
    );
}

/// Proves: a write whose file cannot be opened (its place is a directory) fails naming the file,
/// and the records it held before are still answered by a read once the place is fixed.
///
/// Catches: a failed append answered as written.
#[test]
fn a_write_that_cannot_append_fails() {
    let (_dir, path) = file("hardware-files-unwritable");
    fs::create_dir(&path).expect("a directory in the file's place");
    let log = FileLog::new(path.clone());
    let why = log.write(&mixed()).expect_err("refused");
    assert!(why.starts_with(&path.display().to_string()), "{why}");
    fs::remove_dir(&path).expect("the place is fixed");
    assert_eq!(every(&log), Ok(Vec::new()));
}
