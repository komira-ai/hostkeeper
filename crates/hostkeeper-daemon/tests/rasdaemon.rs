//! rasdaemon's database read into hardware event records (`hostkeeper_daemon::rasdaemon`), over
//! a fixture database SQLite makes in a scratch directory with rasdaemon's own tables
//! (`fixtures/rasdaemon/schema.sql`): each table's rows normalized and compared whole, corrected
//! errors and failed disk requests counted an hour at a time, new rows only by the mark, a
//! database made again, the bounds of one poll, the source states (no file, no table, an empty
//! table, WAL, a write in progress, not SQLite), the history of a first read through the
//! daemon's `HealthKeeper`, the worker's loop and the host's worker started by the links. No real
//! `/var/lib`, no real clock.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::host_health::{BootFacts, HISTORY_PREFIX, HealthKeeper};
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::rasdaemon::{
    ABSENT, MAX_PENDING, MAX_ROWS, Mark, NO_TABLES, POLL_SECS, Pending, Rasdaemon, Reading, SOURCE,
    at_host, parse_time, read,
};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::{FakeClock, ManualDispatch, RecordingLog, ScriptedSleeper};
use hostkeeper_proto::health::check_health;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use pb::HardwareComponent::{Cpu, Disk, Memory, Pcie};
use pb::HardwareSeverity::{Critical, Info, Warning};
use rusqlite::Connection;
use support::{Rig, Scratch, idle_links};

/// rasdaemon's tables.
const SCHEMA: &str = include_str!("fixtures/rasdaemon/schema.sql");

/// The second the tests read at: 2026-11-26 03:00:00 UTC.
const NOW: u64 = 1_795_662_000;
/// The hour of [`NOW`].
const HOUR_NOW: u64 = NOW / 3600;
/// When the boot began, before every row of the tests but one.
const BOOT: u64 = 1_795_000_000;

const T1: &str = "2026-11-26 01:15:00 +0000";
const T1_UNIX: u64 = 1_795_655_700;
const T2: &str = "2026-11-26 01:20:00 +0000";
const T2_UNIX: u64 = 1_795_656_000;
const T3: &str = "2026-11-26 01:59:59 +0000";
const T4: &str = "2026-11-26 02:10:00 +0000";
const T4_UNIX: u64 = 1_795_659_000;
/// In the hour of [`NOW`].
const T5: &str = "2026-11-26 03:00:12 +0000";
const T5_UNIX: u64 = 1_795_662_012;
const T6: &str = "2026-11-26 03:30:00 +0000";

/// rasdaemon's database at `dir`, its tables made, then `sql` run.
fn database(dir: &Scratch, sql: &str) -> PathBuf {
    let path = dir.path().join("ras-mc_event.db");
    let conn = Connection::open(&path).expect("open with SQLite");
    conn.execute_batch(SCHEMA).expect("rasdaemon's tables");
    conn.execute_batch(sql).expect("the rows");
    path
}

fn more(path: &Path, sql: &str) {
    Connection::open(path)
        .expect("open with SQLite")
        .execute_batch(sql)
        .expect("the rows");
}

/// The records and the mark of a read that read the database.
fn found(reading: Reading) -> (Vec<pb::HardwareEvent>, Mark, String) {
    assert_eq!(
        (reading.status, reading.reason.as_str()),
        (pb::SourceStatus::Ok, "")
    );
    let found = reading.found.expect("a read that read");
    let mark = serde_json::from_str(&found.mark).expect("the mark is JSON");
    (found.events, mark, found.mark)
}

fn first_read(path: &Path) -> (Vec<pb::HardwareEvent>, Mark, String) {
    found(read(path, None, BOOT, NOW))
}

/// A record of the row `rowid` of `table` at `time`.
fn record(
    (table, rowid, time): (&str, i64, u64),
    (component, severity, code): (pb::HardwareComponent, pb::HardwareSeverity, &str),
    subject: &str,
    message: &str,
    raw: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: format!("rasdaemon:{table}:{rowid}@{time}"),
        time_unix: time,
        of_previous_boot: false,
        component: component.into(),
        subject: subject.to_owned(),
        severity: severity.into(),
        state: pb::HardwareEventState::Event.into(),
        source: SOURCE.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about: String::new(),
    }
}

fn ids(events: &[pb::HardwareEvent]) -> Vec<String> {
    events.iter().map(|e| e.event_id.clone()).collect()
}

fn mc(time: &str, count: &str, kind: &str, label: &str) -> String {
    format!(
        "INSERT INTO mc_event (timestamp, err_count, err_type, err_msg, label, mc, driver_detail) \
         VALUES ('{time}', {count}, '{kind}', 'ecc', {label}, 0, NULL);"
    )
}

/// Proves each row of the tables that is a record of its own is normalized to exactly its
/// record, compared whole: memory errors uncorrected, fatal, deferred, of type `Info` and of a
/// type not known before (by DIMM label, or by memory controller with none), machine checks with
/// the uncorrected bit and with no status (by socket), fatal, non-fatal and unknown PCIe errors
/// (by device), and a memory failure; the table the daemon does not read is passed over.
///
/// Catches: a severity, code, subject or message of the wrong table or type, a `raw` that is not
/// the row, an `event_id` that is not the table, rowid and time.
#[test]
fn each_row_is_normalized_to_its_record() {
    let dir = Scratch::new("ras-rows");
    let path = database(
        &dir,
        &format!(
            "INSERT INTO mc_event (timestamp, err_count, err_type, err_msg, label, mc, driver_detail) VALUES \
             ('{T1}', 2, 'Uncorrected', 'read error', 'DIMM_A1', 0, 'ch 0'), \
             ('{T1}', 1, 'Fatal', 'read error', 'DIMM_A1', 0, NULL), \
             ('{T1}', 1, 'Deferred', 'poison', 'DIMM_B2', 0, NULL), \
             ('{T1}', 1, 'Info', 'scrub done', 'DIMM_B2', 0, NULL), \
             ('{T1}', 0, 'Weird', NULL, NULL, 1, NULL); \
             INSERT INTO mce_record (timestamp, status, cpu, socketid, bank, bank_name, error_msg, mcistatus_msg, mc_location) VALUES \
             ('{T1}', -4755801206494854144, 12, 1, 4, 'Intel MCA', 'memory read error', 'Uncorrected_error', NULL), \
             ('{T1}', NULL, NULL, 0, 2, NULL, NULL, NULL, NULL); \
             INSERT INTO aer_event (timestamp, dev_name, err_type, err_msg) VALUES \
             ('{T1}', '0000:3b:00.0', 'Uncorrected (Fatal)', 'Completion Timeout'), \
             ('{T1}', '0000:3b:00.0', 'Uncorrected (Non-Fatal)', 'Poisoned TLP'), \
             ('{T1}', '0000:5e:00.0', 'Odd', 'x'); \
             INSERT INTO mf_event (timestamp, pfn, page_type, action_result) VALUES \
             ('{T1}', '0x1a2b3c', 'dirty LRU page', 'Recovered'); \
             INSERT INTO devlink_event (timestamp, msg) VALUES ('{T1}', 'not read');"
        ),
    );
    let (events, mark, _) = first_read(&path);
    let mc = |rowid| ("mc_event", rowid, T1_UNIX);
    let mce = |rowid| ("mce_record", rowid, T1_UNIX);
    let aer = |rowid| ("aer_event", rowid, T1_UNIX);
    assert_eq!(
        events,
        [
            record(
                mc(1),
                (Memory, Critical, "memory_uncorrectable"),
                "DIMM_A1",
                "Uncorrectable memory error on DIMM_A1: 2 reported",
                &format!("{T1} | Uncorrected | count 2 | read error | DIMM_A1 | ch 0"),
            ),
            record(
                mc(2),
                (Memory, Critical, "memory_uncorrectable"),
                "DIMM_A1",
                "Uncorrectable memory error on DIMM_A1: 1 reported",
                &format!("{T1} | Fatal | count 1 | read error | DIMM_A1 | "),
            ),
            record(
                mc(3),
                (Memory, Warning, "memory_deferred"),
                "DIMM_B2",
                "Deferred memory error on DIMM_B2: 1 reported",
                &format!("{T1} | Deferred | count 1 | poison | DIMM_B2 | "),
            ),
            record(
                mc(4),
                (Memory, Info, "memory_info"),
                "DIMM_B2",
                "Memory event on DIMM_B2: scrub done",
                &format!("{T1} | Info | count 1 | scrub done | DIMM_B2 | "),
            ),
            record(
                mc(5),
                (Memory, Warning, "memory_error"),
                "mc1",
                "Memory error (Weird) on mc1: 1 reported",
                &format!("{T1} | Weird | count 1 |  |  | "),
            ),
            record(
                mce(1),
                (Cpu, Critical, "cpu_machine_check"),
                "CPU 1",
                "Uncorrected machine check on CPU 1, bank 4",
                &format!(
                    "{T1} | cpu 12 | bank 4 Intel MCA | status 0xbe00000000800400 | \
                     memory read error | Uncorrected_error | "
                ),
            ),
            record(
                mce(2),
                (Cpu, Warning, "cpu_machine_check"),
                "CPU 0",
                "Machine check on CPU 0, bank 2, with no status",
                &format!("{T1} | cpu 0 | bank 2  | status none |  |  | "),
            ),
            record(
                aer(1),
                (Pcie, Critical, "pcie_fatal"),
                "0000:3b:00.0",
                "Fatal PCIe error on 0000:3b:00.0",
                &format!("{T1} | 0000:3b:00.0 | Uncorrected (Fatal) | Completion Timeout"),
            ),
            record(
                aer(2),
                (Pcie, Critical, "pcie_uncorrected"),
                "0000:3b:00.0",
                "Uncorrected PCIe error on 0000:3b:00.0",
                &format!("{T1} | 0000:3b:00.0 | Uncorrected (Non-Fatal) | Poisoned TLP"),
            ),
            record(
                aer(3),
                (Pcie, Warning, "pcie_error"),
                "0000:5e:00.0",
                "PCIe error (Odd) on 0000:5e:00.0",
                &format!("{T1} | 0000:5e:00.0 | Odd | x"),
            ),
            record(
                ("mf_event", 1, T1_UNIX),
                (Memory, Critical, "memory_failure"),
                "memory",
                "Memory failure at page 0x1a2b3c (dirty LRU page): Recovered",
                &format!("{T1} | pfn 0x1a2b3c | dirty LRU page | Recovered"),
            ),
        ]
    );
    assert_eq!(
        mark.ids.into_iter().collect::<Vec<_>>(),
        [
            ("aer_event".to_owned(), 3),
            ("disk_errors".to_owned(), 0),
            ("mc_event".to_owned(), 5),
            ("mce_record".to_owned(), 2),
            ("mf_event".to_owned(), 1),
        ]
    );
    assert!(mark.pending.is_empty());
}

/// Proves a value SQLite kept in another type than its column's is read as rasdaemon meant it: a
/// count that is no number (1), a label kept as a blob, a message written as a real (text by the
/// column's affinity), a status written as text (an integer by the column's, its uncorrected bit
/// set), a CPU kept as a real and a bank as a blob (neither a number: 0), a sector kept as a
/// real; and a time that is not one is 0, in the record's id too.
///
/// Catches: a row whose values are not of their declared type dropped, or read as zero where
/// it holds the value.
#[test]
fn values_of_other_types_are_read_as_meant() {
    let dir = Scratch::new("ras-types");
    let path = database(
        &dir,
        "INSERT INTO mc_event (timestamp, err_count, err_type, err_msg, label) VALUES \
         ('2026-11-26 01:15:00 +0000', '3x', 'Uncorrected', 2.5, X'44494d4d5f4331'); \
         INSERT INTO mce_record (timestamp, status, cpu, socketid, bank) VALUES \
         ('garbage', '2305843009213693952', 1.5, NULL, X'01'); \
         INSERT INTO disk_errors (timestamp, dev, sector, nr_sector, error, rwbs, cmd) VALUES \
         ('2026-11-26 01:15:00 +0000', 'sdb', 1.5, 8, 'e', 'R', 'c');",
    );
    let (events, _, _) = first_read(&path);
    assert_eq!(
        events,
        [
            record(
                ("mc_event", 1, T1_UNIX),
                (Memory, Critical, "memory_uncorrectable"),
                "DIMM_C1",
                "Uncorrectable memory error on DIMM_C1: 1 reported",
                &format!("{T1} | Uncorrected | count 1 | 2.5 | DIMM_C1 | "),
            ),
            record(
                ("mce_record", 1, 0),
                (Cpu, Critical, "cpu_machine_check"),
                "CPU 0",
                "Uncorrected machine check on CPU 0, bank 0",
                "garbage | cpu 0 | bank 0  | status 0x2000000000000000 |  |  | ",
            ),
            record(
                ("disk_errors", 1, T1_UNIX),
                (Disk, Warning, "io_error"),
                "sdb",
                "1 I/O errors on sdb in the hour",
                &format!("{T1} | sdb | sector 1.5 | 8 sectors | e | R | c"),
            ),
        ]
    );
}

/// Proves corrected errors and failed disk requests are counted an hour at a time, one record
/// per subject and hour: a DIMM's corrected errors by their `err_count` (a null one 1), a CPU's
/// corrected machine checks, a device's corrected PCIe errors (`info`), a disk's failed requests
/// (`warning`, a disk with no name `disk`); each record has the hour's first row's id and time
/// and its last row's `raw`; the hour not over yet is counted in the mark, then made one record,
/// with the rows that came in it since, once it is over.
///
/// Catches: a record per corrected row (24 a day becomes thousands), a count that drops rows, an
/// hour's record sent before the hour is over, or twice.
#[test]
fn corrected_errors_are_counted_an_hour_at_a_time() {
    let dir = Scratch::new("ras-counted");
    let path = database(
        &dir,
        &format!(
            "{}{}{}{}{}{} \
             INSERT INTO mce_record (timestamp, status, socketid, bank) VALUES \
             ('{T1}', -7205759403792727755, 0, 7), ('{T2}', -7205759403792727755, 0, 7); \
             INSERT INTO aer_event (timestamp, dev_name, err_type, err_msg) VALUES \
             ('{T4}', '0000:3b:00.0', 'Corrected', 'Bad TLP'); \
             INSERT INTO disk_errors (timestamp, dev, sector, nr_sector, error, rwbs, cmd) VALUES \
             ('{T1}', 'sda', 2048, 8, 'I/O error', 'R', ''), \
             ('{T2}', 'sda', 4096, 8, 'I/O error', 'W', ''), \
             ('{T2}', NULL, 0, 0, 'timeout', 'R', NULL);",
            mc(T1, "5", "Corrected", "'DIMM_D1'"),
            mc(T2, "'3'", "Corrected", "'DIMM_D1'"),
            mc(T3, "NULL", "Corrected", "'DIMM_D1'"),
            mc(T4, "1", "Corrected", "'DIMM_D1'"),
            mc(T5, "1", "Corrected", "'DIMM_D1'"),
            mc(T2, "2", "Corrected", "'DIMM_D2'"),
        ),
    );
    let (events, mark, text) = first_read(&path);
    let row =
        |t: &str, n: u64, label: &str| format!("{t} | Corrected | count {n} | ecc | {label} | ");
    assert_eq!(
        events,
        [
            record(
                ("mc_event", 1, T1_UNIX),
                (Memory, Info, "memory_corrected"),
                "DIMM_D1",
                "9 corrected errors on DIMM_D1 in the hour",
                &row(T3, 1, "DIMM_D1"),
            ),
            record(
                ("mc_event", 4, T4_UNIX),
                (Memory, Info, "memory_corrected"),
                "DIMM_D1",
                "1 corrected errors on DIMM_D1 in the hour",
                &row(T4, 1, "DIMM_D1"),
            ),
            record(
                ("mc_event", 6, T2_UNIX),
                (Memory, Info, "memory_corrected"),
                "DIMM_D2",
                "2 corrected errors on DIMM_D2 in the hour",
                &row(T2, 2, "DIMM_D2"),
            ),
            record(
                ("mce_record", 1, T1_UNIX),
                (Cpu, Info, "cpu_corrected"),
                "CPU 0",
                "2 corrected machine checks on CPU 0 in the hour",
                &format!("{T2} | cpu 0 | bank 7  | status 0x9c00000000010135 |  |  | "),
            ),
            record(
                ("aer_event", 1, T4_UNIX),
                (Pcie, Info, "pcie_corrected"),
                "0000:3b:00.0",
                "1 corrected PCIe errors on 0000:3b:00.0 in the hour",
                &format!("{T4} | 0000:3b:00.0 | Corrected | Bad TLP"),
            ),
            record(
                ("disk_errors", 1, T1_UNIX),
                (Disk, Warning, "io_error"),
                "sda",
                "2 I/O errors on sda in the hour",
                &format!("{T2} | sda | sector 4096 | 8 sectors | I/O error | W | "),
            ),
            record(
                ("disk_errors", 3, T2_UNIX),
                (Disk, Warning, "io_error"),
                "disk",
                "1 I/O errors on disk in the hour",
                &format!("{T2} | disk | sector 0 | 0 sectors | timeout | R | "),
            ),
        ]
    );
    assert_eq!(
        mark.pending,
        [Pending {
            table: "mc_event".to_owned(),
            subject: "DIMM_D1".to_owned(),
            hour: HOUR_NOW,
            first_id: 5,
            first_unix: T5_UNIX,
            count: 1,
            raw: row(T5, 1, "DIMM_D1"),
        }]
    );
    let (again, _, _) = found(read(&path, Some(&text), BOOT, NOW + 59));
    assert!(again.is_empty(), "the hour is not over: {again:?}");

    more(&path, &mc(T6, "2", "Corrected", "'DIMM_D1'"));
    let (later, mark, _) = found(read(&path, Some(&text), BOOT, NOW + 3600));
    assert_eq!(
        later,
        [record(
            ("mc_event", 5, T5_UNIX),
            (Memory, Info, "memory_corrected"),
            "DIMM_D1",
            "3 corrected errors on DIMM_D1 in the hour",
            &row(T6, 2, "DIMM_D1"),
        )]
    );
    assert!(mark.pending.is_empty());
    assert_eq!(mark.ids["mc_event"], 7);
}

/// Proves the hours counted in the mark are bounded: [`MAX_PENDING`] subjects of the hour not over
/// stay counted, and one more makes them all records at once.
///
/// Catches: a mark that grows with every device that ever had a corrected error.
#[test]
fn the_counted_hours_are_bounded() {
    let dir = Scratch::new("ras-pending");
    let rows: Vec<String> = (0..MAX_PENDING)
        .map(|i| format!("('{T5}', '0000:00:{i:02x}.0', 'Corrected', 'x')"))
        .collect();
    let path = database(
        &dir,
        &format!(
            "INSERT INTO aer_event (timestamp, dev_name, err_type, err_msg) VALUES {};",
            rows.join(", ")
        ),
    );
    let (events, mark, text) = first_read(&path);
    assert!(events.is_empty());
    assert_eq!(mark.pending.len(), MAX_PENDING);
    more(
        &path,
        &format!(
            "INSERT INTO aer_event (timestamp, dev_name, err_type, err_msg) VALUES \
             ('{T5}', '0000:01:00.0', 'Corrected', 'x');"
        ),
    );
    let (events, mark, _) = found(read(&path, Some(&text), BOOT, NOW));
    assert_eq!(events.len(), MAX_PENDING + 1);
    assert!(events.iter().all(|e| e.code == "pcie_corrected"));
    assert!(mark.pending.is_empty());
}

/// Proves a read takes only the rows after the mark: a second read of the same database gives
/// nothing and leaves the mark as it was; rows added are read once. A database made again, whose
/// table's last id is below the mark, is read from its first row, its record ids new by their
/// time, and the log says so.
///
/// Catches: rows read again at every poll, or the rows of a database made again never read
/// because their ids are below the mark.
#[test]
fn only_new_rows_are_read_and_a_database_made_again_is_read_whole() {
    let dir = Scratch::new("ras-mark");
    let uncorrected = |t: &str| mc(t, "1", "Uncorrected", "'DIMM_A1'");
    let path = database(&dir, &uncorrected(T1).repeat(3));
    let (events, _, text) = first_read(&path);
    assert_eq!(events.len(), 3);
    let same = read(&path, Some(&text), BOOT, NOW);
    assert!(same.notes.is_empty());
    let (none, _, kept) = found(same);
    assert!(none.is_empty());
    assert_eq!(kept, text);
    more(&path, &uncorrected(T2).repeat(2));
    let (events, _, text) = found(read(&path, Some(&text), BOOT, NOW));
    assert_eq!(
        ids(&events),
        [
            format!("rasdaemon:mc_event:4@{T2_UNIX}"),
            format!("rasdaemon:mc_event:5@{T2_UNIX}"),
        ]
    );

    fs::remove_file(&path).expect("rasdaemon reinstalled");
    let path = database(&dir, &uncorrected(T4));
    let again = read(&path, Some(&text), BOOT, NOW);
    assert_eq!(
        again.notes,
        [
            "rasdaemon: mc_event has no row after 5, its last is 1: the database was made again; \
          read from its first row"
        ]
    );
    let (events, mark, _) = found(again);
    assert_eq!(ids(&events), [format!("rasdaemon:mc_event:1@{T4_UNIX}")]);
    assert_eq!(mark.ids["mc_event"], 1);
}

/// Proves a mark that does not parse is said in the log and the database read from its first
/// rows; a mark of a later release, with a field and a table this one does not know, reads, and
/// its count of the unknown table is dropped once its hour is over.
///
/// Catches: a broken mark that stops the source for good, or a later release's mark refused.
#[test]
fn a_mark_that_does_not_parse_or_is_of_a_later_release_reads() {
    let dir = Scratch::new("ras-bad-mark");
    let path = database(&dir, &mc(T1, "1", "Uncorrected", "'DIMM_A1'"));
    let broken = read(&path, Some("not json"), BOOT, NOW);
    assert_eq!(broken.notes.len(), 1);
    assert!(
        broken.notes[0].starts_with("rasdaemon: its mark does not parse (")
            && broken.notes[0].ends_with("); read from the first rows"),
        "{:?}",
        broken.notes
    );
    assert_eq!(found(broken).0.len(), 1);
    let later = r#"{"ids":{"mc_event":1},"pending":[{"table":"cxl_event","subject":"x",
        "hour":1,"first_id":1,"first_unix":3600,"count":2,"raw":"r","kind":"new"}],"since":3}"#;
    let (events, mark, _) = found(read(&path, Some(later), BOOT, NOW));
    assert!(events.is_empty());
    assert!(mark.pending.is_empty());
}

/// Proves one poll reads at most [`MAX_ROWS`] rows of a table, the rest in the polls after,
/// none twice and none lost, but the source's first read on a host, which reads every row so
/// that its history is judged at once.
///
/// Catches: a poll whose read is unbounded after a burst, or a first read cut short whose older
/// rows then count as news.
#[test]
fn a_poll_reads_a_bounded_number_of_rows_but_the_first() {
    let dir = Scratch::new("ras-bound");
    let rows = |n: usize, t: &str| {
        format!(
            "INSERT INTO mc_event (timestamp, err_count, err_type, err_msg, label) \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {n}) \
             SELECT '{t}', 1, 'Uncorrected', 'x', 'DIMM_A1' FROM n;"
        )
    };
    let path = database(&dir, &rows(5000, T1));
    let (events, _, text) = first_read(&path);
    assert_eq!(events.len(), 5000);
    more(&path, &rows(5000, T2));
    let (events, _, text) = found(read(&path, Some(&text), BOOT, NOW));
    assert_eq!(events.len(), MAX_ROWS);
    assert_eq!(
        events[0].event_id,
        format!("rasdaemon:mc_event:5001@{T2_UNIX}")
    );
    let (events, mark, text) = found(read(&path, Some(&text), BOOT, NOW));
    assert_eq!(events.len(), 5000 - MAX_ROWS);
    assert_eq!(mark.ids["mc_event"], 10_000);
    let (events, _, _) = found(read(&path, Some(&text), BOOT, NOW));
    assert!(events.is_empty());
}

/// Proves a record says it happened before the boot when its time is before the boot's start,
/// and not when it is after, or when the boot's start or the row's time is not known.
///
/// Catches: every record of a database kept across reboots taken for one of this boot, or the
/// reverse.
#[test]
fn a_row_before_the_boot_is_of_the_previous_boot() {
    let dir = Scratch::new("ras-boot");
    let path = database(
        &dir,
        &format!(
            "{}{}{}",
            mc(T1, "1", "Uncorrected", "'DIMM_A1'"),
            mc(T4, "1", "Uncorrected", "'DIMM_A1'"),
            mc("no time", "1", "Uncorrected", "'DIMM_A1'"),
        ),
    );
    let previous = |boot: u64| -> Vec<bool> {
        found(read(&path, None, boot, NOW))
            .0
            .iter()
            .map(|e| e.of_previous_boot)
            .collect()
    };
    assert_eq!(previous(T2_UNIX), [true, false, false]);
    assert_eq!(previous(0), [false, false, false]);
}

/// Proves rasdaemon's times read in any zone, with no zone as UTC, a leap second, a date in
/// January; and that what is not one, or is before 1970, is none.
///
/// Catches: a zone's sign or minutes dropped, the months of a year counted from January, a
/// malformed time read as some other second.
#[test]
fn rasdaemons_times_read_in_their_zone() {
    let cases: [(&str, Option<u64>); 16] = [
        (T1, Some(T1_UNIX)),
        ("2026-11-26 08:30:12 +0530", Some(T5_UNIX)),
        ("2026-11-25 19:00:12 -0800", Some(T5_UNIX)),
        ("2026-11-26 03:00:12", Some(T5_UNIX)),
        ("2027-01-15 12:00:00 +0000", Some(1_800_014_400)),
        ("2028-02-29 23:59:60 +0000", Some(1_835_481_600)),
        ("2026-13-01 00:00:00 +0000", None),
        ("2026-11-26 24:00:00 +0000", None),
        ("2026-11-26 03:00:12 Z", None),
        ("2026-11-26 03:00:12 +05", None),
        ("2026-11-26 03:00:12 +0a00", None),
        ("2026-11-26", None),
        ("", None),
        ("2026-11-26-01 03:00:00 +0000", None),
        ("2026-11-xx 03:00:00 +0000", None),
        ("2026-11-26 03:00 +0000", None),
    ];
    for (text, unix) in cases {
        assert_eq!(parse_time(text), unix, "{text:?}");
    }
    // The epoch itself, and an hour before it by its zone (the date built, not written).
    let epoch = format!("{}-01-01 00:00:00", 1969 + 1);
    assert_eq!(parse_time(&format!("{epoch} +0000")), Some(0));
    assert_eq!(parse_time(&format!("{epoch} +0100")), None);
}

/// Proves an empty memory table is a source that is read, not one that is not monitored: a
/// database with every table and no row is `ok`, with no record.
///
/// Catches: a host whose firmware records memory errors only in the BMC's event log reported
/// as not monitored.
#[test]
fn an_empty_memory_table_is_a_source_that_is_read() {
    let dir = Scratch::new("ras-empty");
    let path = database(&dir, "");
    let (events, mark, _) = first_read(&path);
    assert!(events.is_empty());
    assert_eq!(mark.ids.values().copied().collect::<Vec<_>>(), [0; 5]);
}

fn shut(reading: &Reading) -> (pb::SourceStatus, &str, bool) {
    (
        reading.status,
        reading.reason.as_str(),
        reading.found.is_some(),
    )
}

/// Proves the source's states: no database file and a database with none of the tables are
/// `unsupported`; a database with only some of them is read; a database in WAL mode, one being
/// written at every attempt and a file that is not SQLite are `unreadable` with the reason; none
/// of these makes a record or a mark.
///
/// Catches: a host with no rasdaemon reported as healthy, a database that cannot be read
/// reported as one with no error, or a missing table taken for a broken database.
#[test]
fn a_database_that_is_not_there_or_cannot_be_read_says_why() {
    let dir = Scratch::new("ras-states");
    let missing = dir.path().join("none.db");
    use pb::SourceStatus::{Unreadable, Unsupported};
    assert_eq!(
        shut(&read(&missing, None, BOOT, NOW)),
        (Unsupported, ABSENT, false)
    );

    let some = Scratch::new("ras-some");
    let path = database(
        &some,
        &format!(
            "DROP TABLE aer_event; DROP TABLE mce_record; DROP TABLE mf_event; \
             DROP TABLE disk_errors; {}",
            mc(T1, "1", "Uncorrected", "'DIMM_A1'")
        ),
    );
    let (events, mark, _) = first_read(&path);
    assert_eq!(events.len(), 1);
    assert_eq!(mark.ids.keys().collect::<Vec<_>>(), ["mc_event"]);
    more(&path, "DROP TABLE mc_event;");
    assert_eq!(
        shut(&read(&path, None, BOOT, NOW)),
        (Unsupported, NO_TABLES, false)
    );

    let wal = Scratch::new("ras-wal");
    let path = database(&wal, "PRAGMA journal_mode = WAL;");
    assert_eq!(
        shut(&read(&path, None, BOOT, NOW)),
        (Unreadable, "the database is in WAL mode", false)
    );

    let busy = Scratch::new("ras-busy");
    let path = database(&busy, "");
    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    fs::write(
        &journal,
        [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7, 0, 0, 0, 1],
    )
    .expect("a hot journal");
    assert_eq!(
        shut(&read(&path, None, BOOT, NOW)),
        (Unreadable, "the database was being written", false)
    );

    let other = dir.path().join("other.db");
    fs::write(&other, vec![b'x'; 200]).expect("write");
    assert_eq!(
        shut(&read(&other, None, BOOT, NOW)),
        (Unreadable, "not a SQLite database", false)
    );
}

/// A probe that says the system is not going down.
struct Staying;

impl ShutdownProbe for Staying {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("the test's system stays up".to_owned())
    }
}

fn keeper(dir: &Scratch) -> (Arc<HealthKeeper>, Arc<RecordingLog>) {
    let state = dir.path().join("state");
    fs::create_dir_all(&state).expect("the state directory");
    let log = Arc::new(RecordingLog::default());
    let boot = BootFacts {
        boot_id: Some("boot-1".to_owned()),
        boot_unix: BOOT,
        machine_id: "m-1".to_owned(),
    };
    let keeper = HealthKeeper::open(&state, boot, Box::new(Staying), log.clone());
    (Arc::new(keeper), log)
}

fn state(status: pb::SourceStatus, reason: &str) -> pb::SourceState {
    pb::SourceState {
        source: SOURCE.to_owned(),
        status: status.into(),
        reason: reason.to_owned(),
    }
}

/// Proves a poll through the daemon's keeper: the first read's records are held, the one older
/// than the history window as `info` history; the heartbeat lists the source as read, within the
/// health's bounds; a poll that finds nothing new writes no mark; the source's state follows
/// the database (a write in progress, then read again, then gone), the log one line at each
/// change and none for a state that stays.
///
/// Catches: the source's state missing from the heartbeat, an old row raised as news on a
/// host's first read, or a log line every minute.
#[test]
fn a_poll_holds_records_and_keeps_the_sources_state() {
    let dir = Scratch::new("ras-keeper");
    let path = database(
        &dir,
        &format!(
            "{}{}",
            mc("2026-11-10 04:00:00 +0000", "1", "Uncorrected", "'DIMM_A1'"),
            mc(T1, "1", "Uncorrected", "'DIMM_A1'"),
        ),
    );
    let (keeper, log) = keeper(&dir);
    let source = Rasdaemon::new(
        path.clone(),
        Arc::clone(&keeper),
        Arc::new(FakeClock::new(NOW, 0)),
        log.clone(),
    );
    assert_eq!(source.poll(), 2);
    let health = keeper.health();
    check_health(&health).expect("within the health's bounds");
    assert_eq!(health.sources, [state(pb::SourceStatus::Ok, "")]);
    assert_eq!(
        health
            .events
            .iter()
            .map(|e| (e.severity, e.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                Critical.into(),
                "Uncorrectable memory error on DIMM_A1: 1 reported"
            ),
            (
                Info.into(),
                format!("{HISTORY_PREFIX}Uncorrectable memory error on DIMM_A1: 1 reported")
                    .as_str()
            ),
        ]
    );
    let mark = keeper.mark(SOURCE);
    assert_eq!(source.poll(), 0);
    assert_eq!(keeper.mark(SOURCE), mark);
    assert_eq!(log.lines(), ["hardware source rasdaemon: Ok"]);

    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    fs::write(&journal, [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]).expect("hot");
    assert_eq!(source.poll(), 0);
    assert_eq!(
        keeper.health().sources,
        [state(
            pb::SourceStatus::Unreadable,
            "the database was being written"
        )]
    );
    fs::remove_file(&journal).expect("the write ended");
    source.poll();
    fs::remove_file(&path).expect("rasdaemon removed");
    source.poll();
    source.poll();
    assert_eq!(
        keeper.health().sources,
        [state(pb::SourceStatus::Unsupported, ABSENT)]
    );
    assert_eq!(
        log.lines(),
        [
            "hardware source rasdaemon: Ok",
            "hardware source rasdaemon: Unreadable: the database was being written",
            "hardware source rasdaemon: Ok",
            "hardware source rasdaemon: Unsupported: no rasdaemon database",
        ]
    );
}

/// Proves the worker reads, then waits [`POLL_SECS`], and reads again until its wait says stop;
/// a Linux host (systemd or OpenRC) has the worker and a Mac none; and the links start the
/// worker on a worker of their own, before the two loops.
///
/// Catches: a worker that reads once and stops, one that never stops, one on a Mac, or one the
/// links never start.
#[test]
fn the_worker_polls_until_stopped_and_the_links_start_it() {
    let dir = Scratch::new("ras-worker");
    let path = database(&dir, &mc(T1, "1", "Uncorrected", "'DIMM_A1'"));
    let (held, log) = keeper(&dir);
    let clock = Arc::new(FakeClock::new(NOW, 0));
    let source = Rasdaemon::new(path.clone(), Arc::clone(&held), clock.clone(), log.clone());
    let sleeper = ScriptedSleeper::new(1);
    source.run(&sleeper);
    assert_eq!(sleeper.waits(), [POLL_SECS, POLL_SECS]);
    assert_eq!(held.health().events.len(), 1);

    let worker = |kind| {
        at_host(
            kind,
            &path,
            &held,
            clock.clone(),
            Box::new(ScriptedSleeper::new(0)),
            log.clone(),
        )
    };
    assert!(worker(PlatformKind::Macos).is_none());
    assert!(worker(PlatformKind::Openrc).is_some());

    let other = Scratch::new("ras-worker-links");
    let (fresh, _) = keeper(&other);
    let job = at_host(
        PlatformKind::Linux,
        &path,
        &fresh,
        clock,
        Box::new(ScriptedSleeper::new(0)),
        log,
    )
    .expect("a Linux host reads rasdaemon");
    let rig = Rig::new("ras-worker-daemon");
    let daemon = Arc::new(rig.daemon());
    let dispatch = Arc::new(ManualDispatch::default());
    Links {
        dispatch: dispatch.clone(),
        health_reads: vec![job],
        ..idle_links()
    }
    .start(&daemon);
    assert_eq!(dispatch.pending(), 3, "the worker and the two loops");
    assert!(fresh.health().sources.is_empty());
    assert!(dispatch.run_next(), "the worker");
    assert_eq!(fresh.health().sources, [state(pb::SourceStatus::Ok, "")]);
    assert_eq!(fresh.health().events.len(), 1);
}
