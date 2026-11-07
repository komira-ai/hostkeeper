//! The audit file: one JSON line for each change, appended and fsynced, numbered on from the
//! larger of the ring and the file, and read back without a torn last line.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_testkit::Scratch;

use crate::audit_file::{self, AuditLine, AuditLog, AuditRead};
use crate::desired::StoreError;
use crate::ops::{AuditEntry, OpsError, OpsState, OpsStore, Write};
use crate::testutil::{Log, scratch};

const AT: u64 = 1_800_000_000;

/// The clock of the audit file: always `AT`.
fn fixed() -> audit_file::UnixClock {
    Arc::new(|| AT)
}

/// The ops file of `dir` and its audit file.
fn files(dir: &Scratch) -> (PathBuf, PathBuf) {
    let file = dir.path().join("ops.json");
    let audit = audit_file::path_of(&file);
    (file, audit)
}

/// The store over `file`, its audit file stamped `AT`.
fn open(file: &Path) -> OpsStore {
    OpsStore::open(file).expect("opened").with_unix(fixed())
}

/// One change by `by` of `action`, with no context, as numbered.
fn make(store: &OpsStore, by: &str, action: &str) -> AuditEntry {
    store
        .change(Write::BeforeMemory, AuditEntry::new(by, action), |_| {})
        .expect("a change")
}

/// The numbers of the lines of the audit file at `path`.
fn numbers(path: &Path) -> Vec<u64> {
    let found = audit_file::read(path).expect("read");
    found.lines.iter().map(|line| line.seq).collect()
}

/// Proves: a line is the five keys in order, a newline ends it, `detail` has the context the
/// entry has and no other key (`{}` for none).
///
/// Catches: a reordered or renamed key, a null for a missing context, or a line without its
/// newline.
#[test]
fn an_audit_line_is_five_keys_in_order() {
    let dir = scratch("audit-keys");
    let (file, audit) = files(&dir);
    let store = open(&file);
    let mut full = AuditEntry::new("alice", "exclude");
    full.service = Some("kbf".to_owned());
    full.node_id = Some("s2".to_owned());
    full.reason = Some("bad disk".to_owned());
    store
        .change(Write::BeforeMemory, full, |_| {})
        .expect("a change");
    let first = concat!(
        r#"{"seq":1,"at_unix":1800000000,"by":"alice","action":"exclude","detail":"#,
        r#"{"service":"kbf","node_id":"s2","reason":"bad disk"}}"#,
        "\n"
    );
    assert_eq!(fs::read_to_string(&audit).expect("read"), first);

    make(&store, "bob", "include");
    let second = r#"{"seq":2,"at_unix":1800000000,"by":"bob","action":"include","detail":{}}"#;
    let mut reason_only = AuditEntry::new("bob", "clear_halt");
    reason_only.reason = Some("replaced".to_owned());
    store
        .change(Write::BeforeMemory, reason_only, |_| {})
        .expect("a change");
    let third = concat!(
        r#"{"seq":3,"at_unix":1800000000,"by":"bob","action":"clear_halt","#,
        r#""detail":{"reason":"replaced"}}"#
    );
    assert_eq!(
        fs::read_to_string(&audit).expect("read"),
        format!("{first}{second}\n{third}\n")
    );
}

/// Proves: reopening the store appends to the file and rewrites nothing: the bytes of the first
/// run are the start of the file, the numbers go on, the mode is 0600 and the ring holds the
/// same three changes.
///
/// Catches: a file truncated or rewritten at open, numbers that start again at 1, or a file
/// other users can read.
#[test]
fn the_audit_file_is_append_only_across_restart() {
    let dir = scratch("audit-restart");
    let (file, audit) = files(&dir);
    let store = open(&file);
    make(&store, "alice", "include");
    make(&store, "alice", "include");
    let before = fs::read(&audit).expect("read");
    drop(store);

    let again = open(&file);
    make(&again, "bob", "include");
    let after = fs::read(&audit).expect("read");
    assert!(after.starts_with(&before));
    assert_eq!(numbers(&audit), [1, 2, 3]);
    assert!(after.len() > before.len());
    let mode = fs::metadata(&audit).expect("metadata").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let ring: Vec<u64> = again.state().audit.iter().map(|e| e.seq).collect();
    assert_eq!(ring, [1, 2, 3]);
}

/// Proves: opening a store creates no audit file, the first change does, and a store with no
/// file (volatile or over a writer) makes none.
///
/// Catches: an audit file created by a farm that never changes anything, or one made beside a
/// store that has no ops file.
#[test]
fn the_audit_file_is_created_by_the_first_change_and_not_by_opening() {
    let dir = scratch("audit-created");
    let (file, audit) = files(&dir);
    let store = open(&file);
    assert!(!audit.exists());
    make(&store, "alice", "include");
    assert!(audit.exists());

    let empty = scratch("audit-none");
    let volatile = OpsStore::volatile();
    make(&volatile, "alice", "include");
    let written = OpsStore::refusing(OpsState::default(), Box::new(|_| Ok(())));
    make(&written, "alice", "include");
    assert_eq!(fs::read_dir(empty.path()).expect("a directory").count(), 0);
}

/// Proves: a last line cut short is passed over and counted, the next line starts after a
/// newline of its own, takes the next number, and the line after that adds nothing else.
///
/// Catches: a new line joined to the torn one so that both are lost, a torn line that stops the
/// start, or a newline added again with every line.
#[test]
fn a_torn_last_line_is_skipped_and_the_next_line_starts_on_its_own() {
    let dir = scratch("audit-torn");
    let (file, audit) = files(&dir);
    let first = r#"{"seq":1,"at_unix":5,"by":"alice","action":"include","detail":{}}"#;
    let torn = r#"{"seq":2,"at_un"#;
    fs::write(&audit, format!("{first}\n{torn}")).expect("written");
    let store = open(&file);
    let found = audit_file::read(&audit).expect("read");
    assert_eq!(found.lines.len(), 1);
    assert_eq!(found.skipped, 1);
    assert!(found.torn);

    let entry = make(&store, "bob", "include");
    assert_eq!(entry.seq, 2);
    let second = r#"{"seq":2,"at_unix":1800000000,"by":"bob","action":"include","detail":{}}"#;
    assert_eq!(
        fs::read_to_string(&audit).expect("read"),
        format!("{first}\n{torn}\n{second}\n")
    );
    let found = audit_file::read(&audit).expect("read");
    assert_eq!(found.lines.len(), 2);
    assert_eq!(found.skipped, 1);
    assert!(!found.torn);

    make(&store, "bob", "include");
    let third = r#"{"seq":3,"at_unix":1800000000,"by":"bob","action":"include","detail":{}}"#;
    assert_eq!(
        fs::read_to_string(&audit).expect("read"),
        format!("{first}\n{torn}\n{second}\n{third}\n")
    );
}

/// Proves: an append that failed part-way while the store runs (a part of a line written, no
/// newline) does not swallow the next line. The store has not reopened the file, so it cannot
/// have learned of the cut from a read at open; the next line still starts after a newline of
/// its own and is read back whole.
///
/// Catches: the store remembering only what it found at open, so that a line cut short by a
/// failed write while it runs has the next line joined to it, and both are lost to the reader.
#[test]
fn an_append_cut_short_while_the_store_runs_does_not_swallow_the_next_line() {
    let dir = scratch("audit-cut-short");
    let (file, audit) = files(&dir);
    let store = open(&file);
    make(&store, "alice", "include");
    let first = r#"{"seq":1,"at_unix":1800000000,"by":"alice","action":"include","detail":{}}"#;
    // What a write that failed part-way leaves: the start of the line, no newline.
    let cut = r#"{"seq":2,"at_unix":1800000000,"by":"bo"#;
    {
        use std::io::Write as _;
        let mut appending = fs::OpenOptions::new()
            .append(true)
            .open(&audit)
            .expect("the audit file");
        appending
            .write_all(cut.as_bytes())
            .expect("a part of a line");
    }

    let entry = make(&store, "bob", "include");
    assert_eq!(entry.seq, 2);
    let second = r#"{"seq":2,"at_unix":1800000000,"by":"bob","action":"include","detail":{}}"#;
    assert_eq!(
        fs::read_to_string(&audit).expect("read"),
        format!("{first}\n{cut}\n{second}\n")
    );
    assert_eq!(numbers(&audit), [1, 2]);
    assert_eq!(audit_file::read(&audit).expect("read").skipped, 1);
}

/// Proves: a change is numbered one past the larger of the ring's last and the audit file's
/// last, so moving the ops file aside does not repeat a number, nor does losing the audit file.
///
/// Catches: numbers taken from the ring alone, which start again at 1 when the ops file is
/// moved aside, or from the file alone.
#[test]
fn numbers_continue_from_the_larger_of_the_ring_and_the_audit_file() {
    let dir = scratch("audit-numbers");
    let (file, audit) = files(&dir);
    let store = open(&file);
    for _ in 0..3 {
        make(&store, "alice", "include");
    }
    drop(store);

    fs::remove_file(&file).expect("moved aside");
    let store = open(&file);
    assert_eq!(store.state().audit.len(), 0);
    assert_eq!(make(&store, "alice", "include").seq, 4);
    assert_eq!(numbers(&audit), [1, 2, 3, 4]);
    drop(store);

    fs::remove_file(&audit).expect("removed");
    let store = open(&file);
    assert_eq!(make(&store, "alice", "include").seq, 5);
    assert_eq!(numbers(&audit), [5]);
}

/// Proves: a line that cannot be appended is an error in the log on the audit target. A change
/// written before memory is then refused, `AuditUnavailable`, and not made: not in the ring, no
/// ops file written. A change that raises protection (written after memory) stands: it is
/// answered and in the ring, and it is pending under its marker, with no ops file written until
/// its line can be appended; then the next heartbeat's retry writes the line and the ops file
/// together and removes the marker.
///
/// Catches: an operator's change made with no audit line, a raise of protection refused for its
/// audit, a raise written to the ops file without its line, a raise dropped when its line could
/// not be appended, or a failure that goes unsaid.
#[test]
fn a_change_is_refused_when_its_audit_line_cannot_be_appended_and_a_raise_stands() {
    let dir = scratch("audit-append-fails");
    let (file, audit) = files(&dir);
    let log = Log::default();
    let _guard = log.install();
    let store = open(&file);
    fs::create_dir(&audit).expect("a directory in its place");
    let refused = store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("alice", "include"),
            |_| {},
        )
        .expect_err("refused");
    assert!(
        matches!(refused, OpsError::AuditUnavailable(_)),
        "{refused:?}"
    );
    assert_eq!(store.state().audit, []);
    assert!(!file.exists(), "no ops file is written");
    assert!(
        !store.halt_unwritten(),
        "a refused change leaves nothing pending"
    );

    let entry = store
        .change(
            Write::AfterMemory,
            AuditEntry::new("coordinator", "mark_bad"),
            |_| {},
        )
        .expect("a raise stands");
    assert_eq!(entry.seq, 1);
    assert_eq!(store.state().audit, std::slice::from_ref(&entry));
    assert!(store.halt_unwritten(), "the raise is pending");
    assert!(!file.exists(), "no ops file is written without its line");
    let marker = crate::ops_file::unfinished_path(&file);
    assert_eq!(
        fs::read_to_string(&marker).expect("the marker"),
        "{\"unwritten\":[1]}\n"
    );
    let text = log.text();
    let refusal = "ERROR hostkeeper_coordinator::audit: the audit file could not be appended to: the change is not written";
    let stands = "a change stands in memory and is not yet written";
    assert_eq!(text.matches(refusal).count(), 2, "{text}");
    assert_eq!(text.matches(stands).count(), 1, "{text}");

    fs::remove_dir(&audit).expect("the audit file can be appended to again");
    store.retry_unwritten(std::time::Duration::from_secs(1));
    assert!(!store.halt_unwritten());
    assert!(!marker.exists(), "the marker is removed");
    assert_eq!(numbers(&audit), [1]);
    let held = fs::read_to_string(&file).expect("read");
    assert_eq!(held.matches("\"action\": \"mark_bad\"").count(), 1);
}

/// Proves: an audit file that exists and cannot be read stops the open, naming the file.
///
/// Catches: a store that starts and forgets the numbers of the file it could not read.
#[test]
fn an_audit_file_that_cannot_be_read_stops_the_open() {
    let dir = scratch("audit-unreadable");
    let (file, audit) = files(&dir);
    fs::create_dir(&audit).expect("a directory in its place");
    let refused = OpsStore::open(&file).err().expect("refused");
    assert!(matches!(refused, OpsError::Io(StoreError::Io { .. })));
    assert_eq!(refused.to_string().matches("ops.json.audit").count(), 1);
}

/// Proves: reading passes over an empty line silently and counts a line that is not JSON, one
/// without the keys and a cut one; the open warns of them and takes the number from the lines
/// that parse; a missing file reads as nothing.
///
/// Catches: a stray line stopping the start, a skipped line not counted, or a missing file read
/// as an error.
#[test]
fn reading_passes_over_what_is_not_a_line_and_says_so() {
    let dir = scratch("audit-passed-over");
    let (_, audit) = files(&dir);
    let missing = audit_file::read(&audit).expect("read");
    assert_eq!(
        missing,
        AuditRead {
            lines: vec![],
            skipped: 0,
            torn: false
        }
    );

    let valid = r#"{"seq":5,"at_unix":5,"by":"alice","action":"include","detail":{}}"#;
    fs::write(&audit, format!("\n\nnot json\n{valid}\n{{}}\n{{\"seq\":6")).expect("written");
    let found = audit_file::read(&audit).expect("read");
    assert_eq!(found.lines.len(), 1);
    assert_eq!(found.skipped, 3);
    assert!(found.torn);

    let log = Log::default();
    let _guard = log.install();
    let opened = AuditLog::open(audit, audit_file::DEFAULT_ROTATION).expect("opened");
    assert_eq!(opened.last_seq(), 5);
    assert_eq!(log.text().matches("passed over").count(), 1);
}

/// Proves: a line reads back equal to the entry it was made from, and a key a later build adds
/// does not make a line unreadable.
///
/// Catches: a line that does not round-trip, or a reader that refuses a newer file.
#[test]
fn an_audit_line_is_read_back_whole() {
    let mut entry = AuditEntry::new("alice", "exclude");
    entry.seq = 7;
    entry.service = Some("kbf".to_owned());
    entry.reason = Some("bad disk".to_owned());
    let line = AuditLine::of(&entry, AT);
    let text = serde_json::to_string(&line).expect("serialised");
    let back: AuditLine = serde_json::from_str(&text).expect("read back");
    assert_eq!(back, line);

    let newer = r#"{"seq":1,"at_unix":1,"by":"a","action":"x","detail":{},"extra":true}"#;
    let read: AuditLine = serde_json::from_str(newer).expect("read");
    assert_eq!(read.seq, 1);
    assert_eq!(read.by, "a");
}

/// Proves: a store opened without a clock of its own stamps a line with the wall clock, in
/// seconds since the epoch: the line is read back with its number and a time after the epoch.
/// Catches: a store whose audit lines are stamped by nothing, or by a clock that is not set.
#[test]
fn a_store_stamps_its_audit_lines_with_the_wall_clock() {
    let dir = scratch("audit-wall-clock");
    let (file, audit) = files(&dir);
    let store = OpsStore::open(&file).expect("opened");
    make(&store, "alice", "exclude");
    let read = audit_file::read(&audit).expect("read");
    assert_eq!(read.lines.len(), 1);
    assert_eq!(read.lines[0].seq, 1);
    assert!(read.lines[0].at_unix > 0, "{:?}", read.lines[0]);
}
