//! `backup` and `restore` of the sealed segments of the audit file (`<ops-file>.audit.<n>`): a
//! backup names each one in its manifest as `ops-audit.<n>`, a restore writes each back under
//! the number it had, a target that holds a segment holds state, and a manifest that names a
//! segment the snapshot lacks, or a name that is no segment's, is refused before anything is
//! written.

use std::fs;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::audit_file::{self, AuditLine, AuditLog, Detail, Rotation};
use crate::backup::{BackupError, MANIFEST, StatePaths, backup, restore};
use crate::backup_tests::{backup_flags, files_under, restore_flags, targets_in};
use crate::lock::FileLock;
use crate::testutil::scratch;

const AT: u64 = 1_800_000_000;

/// The line numbered `seq`: every `seq` of one digit makes a line of the same length.
fn line(seq: u64) -> AuditLine {
    AuditLine {
        seq,
        at_unix: AT + seq,
        by: "alice".to_owned(),
        action: "exclude".to_owned(),
        detail: Detail {
            host_group: None,
            subject: None,
            source: None,
            service: Some("kbf".to_owned()),
            node_id: Some(format!("s{seq}")),
            reason: None,
        },
    }
}

/// A rotation with room for two of [`line`]'s lines a file, keeping `retained` segments.
fn two_lines_a_file(retained: usize) -> Rotation {
    let one = serde_json::to_string(&line(1)).expect("serialised").len() as u64 + 1;
    Rotation {
        max_bytes: NonZeroU64::new(2 * one).expect("not zero"),
        retained: NonZeroUsize::new(retained).expect("not zero"),
    }
}

/// The state of a coordinator in `dir` whose audit file was written through [`AuditLog`] with
/// the lines numbered 1 to `last`, rotated by `rotation`: a state file and the audit file with
/// its sealed segments.
fn rotated_state(dir: &Path, last: u64, rotation: Rotation) -> StatePaths {
    let state = dir.join("desired.json");
    fs::write(&state, b"{\"services\":{}}\n").expect("a state file");
    let paths = StatePaths {
        state_file: state,
        ops_file: None,
        enrollment_dir: None,
    };
    let log = AuditLog::open(audit_file::path_of(&paths.ops_file()), rotation).expect("opened");
    for seq in 1..=last {
        log.append(&line(seq)).expect("appended");
    }
    paths
}

/// The audit file of the ops file `paths` names.
fn audit_of(paths: &StatePaths) -> PathBuf {
    audit_file::path_of(&paths.ops_file())
}

/// The numbers of the sealed segments of the audit file of `paths`, oldest first.
fn numbers_of(paths: &StatePaths) -> Vec<u64> {
    audit_file::segments(&audit_of(paths))
        .expect("listed")
        .into_iter()
        .map(|(number, _)| number)
        .collect()
}

/// Every line of the audit file of `paths`, read across its files as `Audit.List` reads them.
fn lines_of(paths: &StatePaths) -> Vec<AuditLine> {
    let read = audit_file::read_after(&audit_of(paths), 0).expect("read");
    assert_eq!((read.skipped, read.torn), (0, false));
    read.lines
}

/// The names the manifest of `snapshot` lists, in its order.
fn names_in(snapshot: &Path) -> Vec<String> {
    let value: Value =
        serde_json::from_slice(&fs::read(snapshot.join(MANIFEST)).expect("a manifest"))
            .expect("JSON");
    value["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|entry| entry["name"].as_str().expect("a name").to_owned())
        .collect()
}

/// Proves: after two rotations (lines 1 and 2 in segment 1, 3 and 4 in segment 2, 5 in the
/// active file) the manifest names `ops-audit`, `ops-audit.1` and `ops-audit.2`, the restore
/// into empty targets writes both segments under their numbers, byte for byte, and every line
/// reads back across the restored files, 1 to 5 in order, as it read before; the report names
/// each segment it wrote.
///
/// Catches: a backup that copies the active audit file alone and so drops every line sealed
/// before it, or a restore that writes a segment under another number or not at all.
#[test]
fn a_backup_after_two_rotations_restores_every_audit_line_in_order() {
    let dir = scratch("backup-segments-round");
    let paths = rotated_state(dir.path(), 5, two_lines_a_file(100));
    assert_eq!(numbers_of(&paths), [1, 2]);
    let snapshot = dir.path().join("snapshot");
    let report = backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    assert_eq!(
        names_in(&snapshot),
        ["state", "ops-audit", "ops-audit.1", "ops-audit.2"]
    );
    let audit = audit_of(&paths);
    for number in [1, 2] {
        let source = audit_file::segment_path(&audit, number);
        assert!(
            report
                .to_string()
                .lines()
                .any(|l| l.starts_with(&format!("  {} ", source.display()))),
            "{report}"
        );
    }

    let target = scratch("backup-segments-round-target");
    let restored = targets_in(&target);
    let report = restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");
    assert_eq!(numbers_of(&restored), [1, 2]);
    let back = audit_of(&restored);
    for number in [1, 2] {
        assert_eq!(
            fs::read(audit_file::segment_path(&back, number)).expect("restored"),
            fs::read(audit_file::segment_path(&audit, number)).expect("the original"),
        );
        let written = audit_file::segment_path(&back, number);
        assert!(
            report
                .to_string()
                .contains(&format!("  {} ", written.display())),
            "{report}"
        );
    }
    let seqs: Vec<u64> = lines_of(&restored).iter().map(|l| l.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
    assert_eq!(lines_of(&restored), lines_of(&paths));
}

/// Proves: when rotation has deleted the oldest segments (two kept: 2 and 3, of lines 3 to 6),
/// the snapshot and the restore keep their numbers, 2 and 3, so the restored audit still says
/// lines were deleted before line 3 (`pruned_before`) and reads 3 to 7; restoring into an
/// `--ops-file` in another directory than the state file puts the segments beside that ops
/// file.
///
/// Catches: segments renumbered from 1 by the round trip, which would read as an audit that
/// lost nothing, or segments written beside the state file instead of the ops file.
#[test]
fn a_restore_keeps_the_numbers_of_segments_left_after_deletion() {
    let dir = scratch("backup-segments-pruned");
    let paths = rotated_state(dir.path(), 7, two_lines_a_file(2));
    assert_eq!(numbers_of(&paths), [2, 3]);
    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    assert_eq!(
        names_in(&snapshot),
        ["state", "ops-audit", "ops-audit.2", "ops-audit.3"]
    );

    let target = scratch("backup-segments-pruned-target");
    let restored = StatePaths {
        ops_file: Some(target.path().join("ops").join("ops.json")),
        ..targets_in(&target)
    };
    restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");
    assert_eq!(numbers_of(&restored), [2, 3]);
    assert!(
        audit_of(&restored).starts_with(target.path().join("ops")),
        "{}",
        audit_of(&restored).display()
    );
    let seqs: Vec<u64> = lines_of(&restored).iter().map(|l| l.seq).collect();
    assert_eq!(seqs, [3, 4, 5, 6, 7]);
    let pruned = |p: &StatePaths| audit_file::pruned_before(&audit_of(p)).expect("read");
    assert_eq!(pruned(&restored), Some(AT + 3));
    assert_eq!(pruned(&restored), pruned(&paths));
}

/// Proves: a target whose only file of the state is a sealed audit segment (here number 7,
/// left from an older coordinator) holds state: the restore is refused naming that segment,
/// and the targets hold afterwards what they held before (with the lock's file); once the
/// segment is removed the same restore is made.
///
/// Catches: a restore that checks the fixed files alone and writes a fresh audit beside a
/// stale segment, which `Audit.List` would then read as the oldest part of the restored audit.
#[test]
fn restore_refuses_a_target_that_holds_only_a_stale_segment() {
    let dir = scratch("backup-segments-stale");
    let paths = rotated_state(dir.path(), 3, two_lines_a_file(100));
    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");

    let target = scratch("backup-segments-stale-target");
    let restored = targets_in(&target);
    let stale = audit_file::segment_path(&audit_of(&restored), 7);
    fs::create_dir_all(stale.parent().expect("a parent")).expect("a directory");
    fs::write(&stale, b"what was there\n").expect("a stale segment");
    let refused = restore(&restore_flags(&snapshot, &restored), &FileLock).expect_err("stale");
    assert_eq!(
        refused.to_string(),
        format!(
            "{} is already there; restore writes only into targets that hold no state, never over a file",
            stale.display()
        )
    );
    let mut after = files_under(target.path());
    after.retain(|name, _| !name.ends_with(".lock"));
    assert_eq!(
        after.into_iter().collect::<Vec<_>>(),
        [(
            "state/desired.json.ops.audit.7".to_owned(),
            b"what was there\n".to_vec()
        )]
    );

    fs::remove_file(&stale).expect("removed");
    restore(&restore_flags(&snapshot, &restored), &FileLock).expect("no state left");
    assert_eq!(numbers_of(&restored), [1]);
}

/// Proves: a manifest that names `ops-audit.1` when the snapshot does not hold it is refused,
/// naming the file it lacks, and nothing is written into the targets.
///
/// Catches: a restore that passes over a segment it cannot find and so restores an audit with
/// a gap, without a word.
#[test]
fn restore_refuses_a_manifest_that_names_a_missing_segment() {
    let dir = scratch("backup-segments-missing");
    let paths = rotated_state(dir.path(), 3, two_lines_a_file(100));
    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    let lacking = snapshot.join("ops-audit.1");
    fs::remove_file(&lacking).expect("removed from the snapshot");

    let target = scratch("backup-segments-missing-target");
    let refused =
        restore(&restore_flags(&snapshot, &targets_in(&target)), &FileLock).expect_err("lacks it");
    assert!(
        matches!(&refused, BackupError::Io { path, .. } if *path == lacking),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        format!(
            "{}: No such file or directory (os error 2)",
            lacking.display()
        )
    );
    assert!(files_under(target.path()).is_empty());
    assert_eq!(fs::read_dir(target.path()).expect("listed").count(), 0);
}

/// Proves: a manifest that names `ops-audit.1` twice is refused naming it, before anything is
/// written.
///
/// Catches: a segment restored twice over one target, which the second write would meet.
#[test]
fn restore_refuses_a_segment_named_twice() {
    let dir = scratch("backup-segments-twice");
    let paths = rotated_state(dir.path(), 3, two_lines_a_file(100));
    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    let path = snapshot.join(MANIFEST);
    let mut value: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("JSON");
    let files = value["files"].as_array_mut().expect("files");
    let segment = files
        .iter()
        .find(|entry| entry["name"] == json!("ops-audit.1"))
        .expect("the segment")
        .clone();
    files.push(segment);
    fs::write(&path, value.to_string()).expect("written");
    let target = scratch("backup-segments-twice-target");
    let refused =
        restore(&restore_flags(&snapshot, &targets_in(&target)), &FileLock).expect_err("twice");
    assert_eq!(refused.to_string(), "the manifest names ops-audit.1 twice");
    assert_eq!(fs::read_dir(target.path()).expect("listed").count(), 0);
}

/// Proves: a manifest name that only looks like a segment's (number 0, a leading zero, no
/// number, a sign, a letter, a number past `u64`, another file's name with a number) is
/// refused as no file of a snapshot, before anything is written.
///
/// Catches: a name read loosely as a segment, which would restore it under a name `serve`
/// never reads (`.audit.01`) or a number it did not have.
#[test]
fn restore_refuses_names_that_are_no_segments() {
    let dir = scratch("backup-segments-names");
    let paths = rotated_state(dir.path(), 3, two_lines_a_file(100));
    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    let path = snapshot.join(MANIFEST);
    let good: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("JSON");
    let target = scratch("backup-segments-names-target");
    for name in [
        "ops-audit.0",
        "ops-audit.01",
        "ops-audit.",
        "ops-audit.+1",
        "ops-audit.1x",
        "ops-audit.99999999999999999999",
        "ops-halts.1",
        "ops-audit-1",
    ] {
        let mut value = good.clone();
        let entry = value["files"]
            .as_array_mut()
            .expect("files")
            .iter_mut()
            .find(|entry| entry["name"] == json!("ops-audit.1"))
            .expect("the segment");
        entry["name"] = json!(name);
        fs::write(&path, value.to_string()).expect("written");
        let refused =
            restore(&restore_flags(&snapshot, &targets_in(&target)), &FileLock).expect_err(name);
        assert_eq!(
            refused.to_string(),
            format!("the manifest names {name}, which is no file of a snapshot")
        );
        assert_eq!(fs::read_dir(target.path()).expect("listed").count(), 0);
    }
}
