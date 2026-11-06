//! The rotation of the audit file: a full file is sealed as a numbered segment on an append
//! boundary, numbers go on across files, the oldest segments past the retained count are
//! deleted, the next number is recovered from the newest file that holds a line, and the
//! `--audit-*` flags refuse zero.

use std::fs;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};

use clap::Parser as _;

use crate::audit_file::{
    self, AuditLine, AuditLog, DEFAULT_MAX_BYTES, DEFAULT_RETAINED, DEFAULT_ROTATION, Detail,
    Rotation,
};
use crate::config::{Cli, Command};
use crate::desired::StoreError;
use crate::ops::{AuditEntry, OpsError, OpsStore, Write};
use crate::testutil::{Log, scratch};

const AT: u64 = 1_800_000_000;

/// The line numbered `seq`, the same length for every `seq` of the same number of digits.
fn line(seq: u64) -> AuditLine {
    AuditLine {
        seq,
        at_unix: AT + seq,
        by: "alice".to_owned(),
        action: "exclude".to_owned(),
        detail: Detail {
            service: Some("kbf".to_owned()),
            node_id: Some("s2".to_owned()),
            reason: None,
        },
    }
}

/// How long [`line`] `seq` is in the file, with its newline.
fn len_of(seq: u64) -> u64 {
    serde_json::to_string(&line(seq)).expect("serialised").len() as u64 + 1
}

/// A rotation of at most `max_bytes` a file, keeping `retained` segments.
fn rotation(max_bytes: u64, retained: usize) -> Rotation {
    Rotation {
        max_bytes: NonZeroU64::new(max_bytes).expect("not zero"),
        retained: NonZeroUsize::new(retained).expect("not zero"),
    }
}

/// The active audit file of a scratch directory.
fn active_of(dir: &hostkeeper_testkit::Scratch) -> PathBuf {
    dir.path().join("ops.json.audit")
}

/// The numbers of the sealed segments of `active`, oldest first.
fn numbers_of(active: &Path) -> Vec<u64> {
    audit_file::segments(active)
        .expect("listed")
        .into_iter()
        .map(|(number, _)| number)
        .collect()
}

/// The numbers of the lines of one file.
fn seqs_in(file: &Path) -> Vec<u64> {
    let read = audit_file::read(file).expect("read");
    assert_eq!((read.skipped, read.torn), (0, false), "{}", file.display());
    read.lines.iter().map(|line| line.seq).collect()
}

/// The numbers of every line kept, across the files, oldest first.
fn all_seqs(active: &Path) -> Vec<u64> {
    audit_file::read_after(active, 0)
        .expect("read")
        .lines
        .iter()
        .map(|line| line.seq)
        .collect()
}

/// Appends the lines numbered `seqs` through `log`.
fn append_all(log: &AuditLog, seqs: impl IntoIterator<Item = u64>) {
    for seq in seqs {
        log.append(&line(seq)).expect("appended");
    }
}

/// Proves: with room for two lines a file, twenty appends seal nine segments, numbered 1 to 9,
/// each holding two whole lines and no file over the maximum; the numbers read back across the
/// files are 1 to 20, each once, rising by one, and the active file holds the newest two.
///
/// Catches: a file that never rotates, a line split across two files or written twice, a
/// segment numbered out of order, or numbers that restart in a fresh file.
#[test]
fn rotation_continues_seq_across_files() {
    let dir = scratch("audit-rotation-seq");
    let active = active_of(&dir);
    let max = 2 * len_of(10);
    let log = AuditLog::open(active.clone(), rotation(max, 100)).expect("opened");
    append_all(&log, 10..30);

    assert_eq!(numbers_of(&active), (1..=9).collect::<Vec<_>>());
    let mut joined = Vec::new();
    for (number, segment) in audit_file::segments(&active).expect("listed") {
        let held = seqs_in(&segment);
        assert_eq!(held.len(), 2, "segment {number} holds two whole lines");
        assert!(fs::metadata(&segment).expect("stat").len() <= max);
        joined.extend(held);
    }
    assert_eq!(seqs_in(&active), [28, 29]);
    joined.extend([28, 29]);
    assert_eq!(joined, (10..30).collect::<Vec<_>>());
    assert_eq!(all_seqs(&active), joined);
    assert_eq!(log.last_seq(), 29);
}

/// Proves: with two segments retained, only the active file and the two newest segments are
/// left after many rotations, and the lines they hold are the newest, in order, ending at the
/// last appended.
///
/// Catches: a retention that keeps every segment, deletes the newest instead of the oldest,
/// or counts the active file as a segment.
#[test]
fn rotation_retention_drops_oldest_segments() {
    let dir = scratch("audit-rotation-retention");
    let active = active_of(&dir);
    let log = AuditLog::open(active.clone(), rotation(2 * len_of(10), 2)).expect("opened");
    append_all(&log, 10..30);

    assert_eq!(numbers_of(&active), [8, 9]);
    let mut files: Vec<String> = fs::read_dir(dir.path())
        .expect("listed")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("UTF-8")
        })
        .collect();
    files.sort();
    assert_eq!(
        files,
        ["ops.json.audit", "ops.json.audit.8", "ops.json.audit.9"]
    );
    assert_eq!(all_seqs(&active), (24..30).collect::<Vec<_>>());
}

/// Proves: a line longer than the maximum is written whole, alone in a fresh file, and the
/// next line rotates past it; an empty or missing file takes a line of any length without a
/// rotation.
///
/// Catches: a line split to fit the maximum, a line refused for its length, or a rotation of a
/// file that holds nothing (an empty segment).
#[test]
fn a_line_longer_than_the_maximum_is_written_whole_in_a_file_of_its_own() {
    let dir = scratch("audit-rotation-long-line");
    let active = active_of(&dir);
    let log = AuditLog::open(active.clone(), rotation(8, 10)).expect("opened");
    append_all(&log, [10]);
    assert_eq!(numbers_of(&active), Vec::<u64>::new());
    assert_eq!(seqs_in(&active), [10]);
    append_all(&log, [11, 12]);
    assert_eq!(numbers_of(&active), [1, 2]);
    assert_eq!(seqs_in(&audit_file::segment_path(&active, 1)), [10]);
    assert_eq!(seqs_in(&active), [12]);
}

/// Proves: an active file whose last line was cut short is sealed as it is when it is full,
/// and the fresh file starts with the next line, with no newline before it.
///
/// Catches: a torn line carried into the fresh file, or the fresh file starting blank.
#[test]
fn a_torn_active_file_is_sealed_as_it_is_and_the_fresh_one_starts_clean() {
    let dir = scratch("audit-rotation-torn");
    let active = active_of(&dir);
    let whole = serde_json::to_string(&line(10)).expect("serialised");
    fs::write(&active, format!("{whole}\n{{\"seq\":11")).expect("written");
    let log = AuditLog::open(active.clone(), rotation(len_of(10) + 12, 10)).expect("opened");
    assert_eq!(log.last_seq(), 10);
    append_all(&log, [12]);
    let sealed = audit_file::read(&audit_file::segment_path(&active, 1)).expect("read");
    assert_eq!(
        (sealed.lines.len(), sealed.skipped, sealed.torn),
        (1, 1, true)
    );
    let fresh = fs::read_to_string(&active).expect("read");
    assert_eq!(
        fresh,
        format!("{}\n", serde_json::to_string(&line(12)).expect("line"))
    );
}

/// Proves: a reopened log takes its last number from the newest file that holds a line: the
/// active file, else (missing, empty, or holding only a torn line right after a rotation) the
/// newest segment that holds one; with no file at all it is 0. The next line appended after the
/// reopen goes on from it.
///
/// Catches: numbers recovered from the oldest segment, from the active file only (0 right after
/// a rotation), or a reopen that restarts the numbers.
#[test]
fn restart_recovers_next_seq_from_newest_file() {
    let dir = scratch("audit-rotation-restart");
    let active = active_of(&dir);
    let tiny = rotation(2 * len_of(10), 10);
    assert_eq!(
        AuditLog::open(active.clone(), tiny)
            .expect("opened")
            .last_seq(),
        0
    );
    let log = AuditLog::open(active.clone(), tiny).expect("opened");
    append_all(&log, 10..15);
    drop(log);
    assert_eq!(
        AuditLog::open(active.clone(), tiny)
            .expect("opened")
            .last_seq(),
        14
    );

    // Right after a rotation: the active file sealed, the fresh one not yet made.
    fs::rename(&active, audit_file::segment_path(&active, 3)).expect("sealed");
    let reopened = AuditLog::open(active.clone(), tiny).expect("opened");
    assert_eq!(reopened.last_seq(), 14);
    append_all(&reopened, [reopened.last_seq() + 1]);
    assert_eq!(seqs_in(&active), [15]);
    drop(reopened);

    // An empty active file, then one that holds only a cut line, after a newest segment that
    // holds no line either: the number comes from the newest segment that does.
    fs::rename(&active, audit_file::segment_path(&active, 4)).expect("sealed");
    fs::write(audit_file::segment_path(&active, 5), "not a line\n").expect("written");
    fs::write(&active, "").expect("emptied");
    assert_eq!(
        AuditLog::open(active.clone(), tiny)
            .expect("opened")
            .last_seq(),
        15
    );
    fs::write(&active, "{\"seq\":99").expect("torn");
    assert_eq!(
        AuditLog::open(active.clone(), tiny)
            .expect("opened")
            .last_seq(),
        15
    );
}

/// Proves: a store over an ops file rotates its audit file by its rotation; moved aside, the ops
/// file leaves the numbering to the audit files, and the first change after the reopen is
/// numbered one past the last line of the newest file.
///
/// Catches: a store that ignores its rotation, or one whose numbers restart when the ring is
/// gone and the active file was just sealed.
#[test]
fn a_reopened_store_numbers_on_from_the_newest_audit_file() {
    let dir = scratch("audit-rotation-store");
    let file = dir.path().join("ops.json");
    let active = audit_file::path_of(&file);
    let store = OpsStore::open_with(&file, rotation(1, 10)).expect("opened");
    for _ in 0..3 {
        store
            .change(
                Write::BeforeMemory,
                AuditEntry::new("alice", "include"),
                |_| {},
            )
            .expect("a change");
    }
    drop(store);
    assert_eq!(numbers_of(&active), [1, 2]);
    fs::rename(&active, audit_file::segment_path(&active, 3)).expect("sealed");
    fs::remove_file(&file).expect("the ops file moved aside");
    let store = OpsStore::open(&file).expect("reopened");
    let next = store
        .change(
            Write::BeforeMemory,
            AuditEntry::new("alice", "include"),
            |_| {},
        )
        .expect("a change");
    assert_eq!(next.seq, 4);
    assert_eq!(all_seqs(&active), [1, 2, 3, 4]);
}

/// Proves: only `<name>.<n>` with `n` a number with no leading zero is a segment: a file with
/// another suffix, a leading zero, an empty number, a number past `u64`, or of another audit
/// file is not, and is neither read nor deleted.
///
/// Catches: a stray file beside the audit read as audit lines or deleted by the retention, or a
/// segment read twice under two spellings of its number.
#[test]
fn only_numbered_files_of_the_audit_file_are_its_segments() {
    let dir = scratch("audit-rotation-names");
    let active = active_of(&dir);
    let strays = [
        "ops.json.audit.01",
        "ops.json.audit.tmp",
        "ops.json.audit.",
        "ops.json.audit.1x",
        "ops.json.audit.99999999999999999999",
        "other.json.audit.1",
        "ops.json.auditx1",
    ];
    for stray in strays {
        fs::write(dir.path().join(stray), "{\"seq\":1}\n").expect("written");
    }
    let log = AuditLog::open(active.clone(), rotation(1, 1)).expect("opened");
    append_all(&log, 10..14);
    assert_eq!(numbers_of(&active), [3]);
    assert_eq!(all_seqs(&active), [12, 13]);
    for stray in strays {
        assert!(dir.path().join(stray).exists(), "{stray} is left");
    }
}

/// Proves: a segment the retention cannot delete is logged as a warning and left, the append
/// that rotated still lands, and the next rotation deletes what it can.
///
/// Catches: a retention failure that fails the change it rotated for, or one that goes unsaid.
#[test]
fn a_segment_that_cannot_be_deleted_is_logged_and_left() {
    let dir = scratch("audit-rotation-undeletable");
    let active = active_of(&dir);
    let log_text = Log::default();
    let _guard = log_text.install();
    let log = AuditLog::open(active.clone(), rotation(1, 1)).expect("opened");
    let undeletable = audit_file::segment_path(&active, 1);
    fs::create_dir(&undeletable).expect("a directory in its place");
    fs::write(undeletable.join("keep"), "").expect("not empty");
    fs::write(audit_file::segment_path(&active, 2), "").expect("a segment");
    append_all(&log, [10, 11]);
    assert!(undeletable.is_dir());
    assert_eq!(seqs_in(&active), [11]);
    assert_eq!(numbers_of(&active), [1, 3]);
    let warning = "WARN hostkeeper_coordinator::audit: a sealed segment of the audit file past the retained count could not be deleted";
    assert_eq!(log_text.text().matches(warning).count(), 1);
}

/// Proves: a log whose directory cannot be listed (its path is a file) does not open, and
/// neither reading nor `pruned_before` answers, each naming the directory; a log whose
/// directory is not there opens with nothing in it.
///
/// Catches: an unlistable directory taken for an empty one, which would number from 0 and
/// could seal over an existing segment.
#[test]
fn a_directory_that_cannot_be_listed_stops_the_open_and_the_reads() {
    let dir = scratch("audit-rotation-unlistable");
    let not_dir = dir.path().join("a-file");
    fs::write(&not_dir, "").expect("written");
    let active = not_dir.join("ops.json.audit");
    let refusals = [
        AuditLog::open(active.clone(), DEFAULT_ROTATION).err(),
        audit_file::read_after(&active, 0).err(),
        audit_file::pruned_before(&active).err(),
    ];
    for refused in refusals {
        let refused = refused.expect("refused");
        assert!(matches!(refused, OpsError::Io(StoreError::Io { .. })));
        assert!(
            refused
                .to_string()
                .starts_with(&format!("{}: ", not_dir.display()))
        );
    }

    let gone = dir.path().join("gone").join("ops.json.audit");
    assert_eq!(
        AuditLog::open(gone.clone(), DEFAULT_ROTATION)
            .expect("opened")
            .last_seq(),
        0
    );
    assert_eq!(numbers_of(&gone), Vec::<u64>::new());
}

/// Proves: `pruned_before` is `None` while segment 1 is kept (or there is none), and once it is
/// deleted, the time of the oldest line kept; `None` again when nothing kept is a line.
///
/// Catches: approvals audited again at every start once their entries were deleted, or a
/// deletion taken for a lost line.
#[test]
fn pruned_before_is_the_time_of_the_oldest_line_once_segments_are_deleted() {
    let dir = scratch("audit-rotation-pruned");
    let active = active_of(&dir);
    assert_eq!(audit_file::pruned_before(&active).expect("read"), None);
    let log = AuditLog::open(active.clone(), rotation(1, 2)).expect("opened");
    append_all(&log, 10..12);
    assert_eq!(audit_file::pruned_before(&active).expect("read"), None);
    append_all(&log, 12..15);
    assert_eq!(numbers_of(&active), [3, 4]);
    assert_eq!(
        audit_file::pruned_before(&active).expect("read"),
        Some(AT + 12)
    );

    for (_, segment) in audit_file::segments(&active).expect("listed") {
        fs::write(segment, "not a line\n").expect("written");
    }
    fs::write(&active, "").expect("emptied");
    assert_eq!(audit_file::pruned_before(&active).expect("read"), None);
}

/// Proves: reading after a number reads the newest files only as far back as the first that
/// holds a line at or before it, and answers the lines after it, oldest first, across files;
/// a number in a deleted segment answers from the oldest line kept.
///
/// Catches: lines at or before the number answered, lines of an older segment dropped, or a
/// cursor past the kept range answering nothing.
#[test]
fn reading_after_a_number_answers_across_files_from_the_oldest_kept() {
    let dir = scratch("audit-rotation-after");
    let active = active_of(&dir);
    let log = AuditLog::open(active.clone(), rotation(2 * len_of(10), 3)).expect("opened");
    append_all(&log, 10..30);
    let after = |since| -> Vec<u64> {
        let read = audit_file::read_after(&active, since).expect("read");
        read.lines.iter().map(|line| line.seq).collect()
    };
    assert_eq!(after(23), (24..30).collect::<Vec<_>>());
    assert_eq!(after(24), (25..30).collect::<Vec<_>>());
    assert_eq!(after(29), Vec::<u64>::new());
    assert_eq!(after(11), (22..30).collect::<Vec<_>>());
    assert_eq!(after(0), (22..30).collect::<Vec<_>>());
}

/// The flags of `serve` with the paths and addresses it requires, and `extra`.
fn serve_args(extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "hostkeeper-coordinator",
        "serve",
        "--heartbeat-listen",
        "127.0.0.1:0",
        "--private-listen",
        "127.0.0.1:0",
        "--tls-cert",
        "cert.pem",
        "--tls-key",
        "key.pem",
        "--client-ca",
        "ca.pem",
        "--state-file",
        "desired.json",
        "--oidc-config",
        "oidc.toml",
        "--rollout-file",
        "rollout.toml",
    ]
    .map(str::to_owned)
    .into();
    args.extend(extra.iter().map(|arg| (*arg).to_owned()));
    args
}

/// Proves: `--audit-max-bytes 0` and `--audit-retained-segments 0` are refused when the flags
/// are read, naming the flag; a value of 1 is taken; left out, they are 16 MiB and 8.
///
/// Catches: a zero that would rotate before every line or delete every segment, or defaults
/// that never rotate.
#[test]
fn config_rejects_zero_rotation_values() {
    for flag in ["--audit-max-bytes", "--audit-retained-segments"] {
        let refused = Cli::try_parse_from(serve_args(&[flag, "0"])).expect_err("refused");
        assert_eq!(refused.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(refused.to_string().contains(flag), "{refused}");
    }
    let parsed = |extra: &[&str]| {
        let Command::Serve(flags) = Cli::try_parse_from(serve_args(extra))
            .expect("flags")
            .command;
        (
            flags.audit_max_bytes.get(),
            flags.audit_retained_segments.get(),
        )
    };
    assert_eq!(parsed(&[]), (16 << 20, 8));
    assert_eq!(
        (DEFAULT_MAX_BYTES.get(), DEFAULT_RETAINED.get()),
        (16 << 20, 8)
    );
    let ones = ["--audit-max-bytes", "1", "--audit-retained-segments", "1"];
    assert_eq!(parsed(&ones), (1, 1));
}
