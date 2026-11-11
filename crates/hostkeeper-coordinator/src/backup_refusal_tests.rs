//! What `backup` and `restore` refuse, and what a refused or failed one leaves: a restore over
//! targets that hold state or under a held lock, a snapshot changed after the backup or holding
//! what its manifest does not name, a backup over one already there, the marker of a
//! `host-groups migrate --apply` of release 0.1.253 that stopped partway; each leaves the targets
//! (or the place of the snapshot) as they were.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::backup::{
    BackupError, MANIFEST, StatePaths, backup, migrate_marker, ops_path, parent_of, restore,
    restore_marker,
};
use crate::backup_tests::{
    Unopenable, backup_flags, files_under, restore_flags, small_snapshot, small_state, targets_in,
};
use crate::jti_file::jti_path;
use crate::lock::{FileLock, StateLock, lock_path};
use crate::testutil::scratch;

/// Every entry under `dir`, files and directories, by its path under it.
fn entries_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in fs::read_dir(&next).expect("a directory") {
            let path = entry.expect("an entry").path();
            found.push(
                path.strip_prefix(dir)
                    .expect("under it")
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() && !path.is_symlink() {
                todo.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The manifest of `snapshot`.
fn manifest(snapshot: &Path) -> Value {
    serde_json::from_slice(&fs::read(snapshot.join(MANIFEST)).expect("a manifest")).expect("JSON")
}

/// Replaces the manifest of `snapshot` with `value`.
fn rewrite_manifest(snapshot: &Path, value: &Value) {
    fs::write(snapshot.join(MANIFEST), value.to_string()).expect("written");
}

/// Catches: a restore that writes over what a target holds, or beside it: for each file of the
/// state, and for an enrollment directory with anything in it, a target that holds it is
/// refused by name, and the target directory holds afterwards exactly what it held before,
/// byte for byte. An enrollment directory that is there and empty is a target that holds no
/// state.
#[test]
fn restore_refuses_targets_that_hold_state_and_leaves_them_as_they_were() {
    let (_dir, snapshot) = small_snapshot("restore-busy");
    let target = scratch("restore-busy-target");
    let paths = targets_in(&target);
    let ops = paths.ops_file();
    let state = paths.state_file.clone();
    let mut held: Vec<PathBuf> = [".jti", ".verdicts", ".accounts"]
        .iter()
        .map(|suffix| PathBuf::from(format!("{}{suffix}", state.display())))
        .collect();
    held.insert(0, state.clone());
    held.push(ops.clone());
    for suffix in [".audit", ".halts", ".halts.next", ".halts.unfinished"] {
        held.push(PathBuf::from(format!("{}{suffix}", ops.display())));
    }
    let store = paths.enrollment_dir.clone().expect("a store");
    held.push(store.join("serial.pb"));
    for there in held {
        fs::create_dir_all(there.parent().expect("a parent")).expect("a directory");
        fs::write(&there, b"what was there").expect("a file");
        let mut before = files_under(target.path());
        before.retain(|name, _| !name.ends_with(".lock"));
        let refused = restore(&restore_flags(&snapshot, &paths), &FileLock).expect_err("not empty");
        assert_eq!(
            refused.to_string(),
            format!(
                "{} is already there; restore writes only into targets that hold no state, never over a file",
                there.display()
            )
        );
        let mut after = files_under(target.path());
        after.retain(|name, _| !name.ends_with(".lock"));
        assert_eq!(after, before, "{}", there.display());
        fs::remove_file(&there).expect("removed");
    }
    fs::create_dir_all(&store).expect("an empty store directory");
    restore(&restore_flags(&snapshot, &paths), &FileLock).expect("an empty dir holds no state");
    assert!(store.join("hosts").join("s1.pb").exists());
}

/// Catches: a restore that writes under a running coordinator: with the lock held (here by the
/// test) it is refused with the lock's path, and no file of the state is written; the directory
/// it made for the lock's file is the only thing it leaves.
#[test]
fn restore_is_refused_while_the_lock_is_held() {
    let (_dir, snapshot) = small_snapshot("restore-locked");
    let target = scratch("restore-locked-target");
    let paths = targets_in(&target);
    fs::create_dir_all(paths.state_file.parent().expect("a parent")).expect("a directory");
    let held = FileLock
        .acquire(&lock_path(&paths.state_file))
        .expect("the test holds the lock");
    let refused = restore(&restore_flags(&snapshot, &paths), &FileLock).expect_err("held");
    assert!(
        matches!(&refused, BackupError::Locked { path } if *path == lock_path(&paths.state_file)),
        "{refused:?}"
    );
    assert_eq!(
        entries_under(target.path()),
        ["state", "state/desired.json.lock"]
    );
    drop(held);
    let refused = restore(
        &restore_flags(&snapshot, &scratch_targets("restore-lock-io")),
        &Unopenable,
    )
    .expect_err("cannot be taken");
    assert!(matches!(refused, BackupError::Lock { .. }), "{refused:?}");
}

/// Targets in a new scratch directory, which is leaked so the test may name it after.
fn scratch_targets(test: &str) -> StatePaths {
    let dir = scratch(test);
    let paths = targets_in(&dir);
    std::mem::forget(dir);
    paths
}

/// What a test does to a snapshot after the backup, named.
type Alteration = (&'static str, fn(&Path));

/// Catches: a snapshot restored though a file changed after the backup (same size or not), or
/// though it holds a file its manifest does not name, or lacks one it does: each is refused,
/// naming the file, before anything is written: the targets' directory stays empty.
#[test]
fn restore_refuses_a_snapshot_that_changed_after_the_backup_and_writes_nothing() {
    let target = scratch("restore-changed-target");
    let paths = targets_in(&target);
    let altered: [Alteration; 5] = [
        ("same size", |s| {
            fs::write(s.join("state"), b"{\"services\":{]}\n").expect("written");
        }),
        ("longer", |s| {
            fs::write(s.join("state-jti"), b"spent 1900000000\nmore 1\n").expect("written");
        }),
        ("an extra file", |s| {
            fs::write(s.join("enrollment").join("hosts").join("s9.pb"), b"").expect("written");
        }),
        ("a missing file", |s| {
            fs::remove_file(s.join("enrollment").join("serial.pb")).expect("removed");
        }),
        ("a link in place of a file", |s| {
            fs::remove_file(s.join("state")).expect("removed");
            std::os::unix::fs::symlink("state-jti", s.join("state")).expect("a link");
        }),
    ];
    for (n, (what, alter)) in altered.iter().enumerate() {
        let (_dir, snapshot) = small_snapshot(&format!("restore-changed-{n}"));
        alter(&snapshot);
        let refused = restore(&restore_flags(&snapshot, &paths), &FileLock).expect_err(what);
        let text = refused.to_string();
        match n {
            0 => assert!(
                text.starts_with(&format!(
                    "{}: 16 bytes with SHA-256 ",
                    snapshot.join("state").display()
                )) && text
                    .ends_with("; the snapshot changed after the backup, so nothing was restored"),
                "{text}"
            ),
            1 => assert!(
                matches!(
                    &refused,
                    BackupError::Mismatch {
                        bytes: 24,
                        want_bytes: 17,
                        ..
                    }
                ),
                "{refused:?}"
            ),
            2 => assert_eq!(
                text,
                format!(
                    "{}: the snapshot holds this, and its manifest does not name it; nothing was restored",
                    snapshot.join("enrollment/hosts/s9.pb").display()
                )
            ),
            _ => assert!(
                matches!(refused, BackupError::Io { .. }),
                "{what}: {refused:?}"
            ),
        }
        assert!(entries_under(target.path()).is_empty(), "{what}");
    }
}

/// Catches: a manifest read leniently into a restore of the wrong files: one that is not JSON,
/// of another schema, naming a file twice, naming a path that is no file of a snapshot (outside
/// the enrollment store, or not a `.pb` file of it), or naming the enrollment store when no
/// `--enrollment-dir` is given, is refused by what is wrong, and nothing is written.
#[test]
fn restore_refuses_a_manifest_it_cannot_follow() {
    let target = scratch("restore-manifest-target");
    let paths = targets_in(&target);
    let (_dir, snapshot) = small_snapshot("restore-manifest");
    let good = manifest(&snapshot);
    let with_name = |name: &str| {
        let mut value = good.clone();
        value["files"][0]["name"] = json!(name);
        value
    };
    let mut twice = good.clone();
    let first = twice["files"][0].clone();
    twice["files"].as_array_mut().expect("files").push(first);
    let cases = [
        (json!("not a manifest"), "the manifest cannot be read"),
        (
            {
                let mut value = good.clone();
                value["schema"] = json!(2);
                value
            },
            "the manifest is of schema 2; this build restores schema 1",
        ),
        (twice, "the manifest names state twice"),
        (
            with_name("../desired.json"),
            "the manifest names ../desired.json, which is no file of a snapshot",
        ),
        (
            with_name("enrollment/../x.pb"),
            "the manifest names enrollment/../x.pb, which is no file of a snapshot",
        ),
        (
            with_name("enrollment/hosts//s1.pb"),
            "the manifest names enrollment/hosts//s1.pb, which is no file of a snapshot",
        ),
        (
            with_name("enrollment/./serial.pb"),
            "the manifest names enrollment/./serial.pb, which is no file of a snapshot",
        ),
        (
            json!({"schema": 2, "entries": []}),
            "the manifest is of schema 2; this build restores schema 1",
        ),
        (
            with_name("enrollment/ca.key"),
            "the manifest names enrollment/ca.key, which is no file of a snapshot",
        ),
        (
            with_name("enrollmentx/s1.pb"),
            "the manifest names enrollmentx/s1.pb, which is no file of a snapshot",
        ),
    ];
    for (value, says) in cases {
        rewrite_manifest(&snapshot, &value);
        let refused = restore(&restore_flags(&snapshot, &paths), &FileLock).expect_err(says);
        assert!(refused.to_string().contains(says), "{refused}");
        assert!(entries_under(target.path()).is_empty(), "{says}");
    }
    rewrite_manifest(&snapshot, &good);
    let without = StatePaths {
        enrollment_dir: None,
        ..paths.clone()
    };
    let refused = restore(&restore_flags(&snapshot, &without), &FileLock).expect_err("no dir");
    assert_eq!(
        refused.to_string(),
        "the snapshot holds the enrollment store and --enrollment-dir is not given: restoring without it would lose the enrolled hosts"
    );
    let missing = restore(
        &restore_flags(&target.path().join("none"), &paths),
        &FileLock,
    )
    .expect_err("no snapshot");
    assert!(matches!(missing, BackupError::Io { .. }), "{missing:?}");
    assert!(entries_under(target.path()).is_empty());
}

/// Catches: a restore that fails partway and leaves half a state that `serve` would start on:
/// with the ops file named the same as the state file the second write meets the first, the
/// restore fails naming it, and removes every file and directory it had made but the state
/// file's directory with the lock's file.
#[test]
fn a_restore_that_fails_partway_removes_what_it_wrote() {
    let (_dir, snapshot) = small_snapshot("restore-partway");
    let target = scratch("restore-partway-target");
    let paths = targets_in(&target);
    let clash = StatePaths {
        ops_file: Some(paths.state_file.clone()),
        ..paths.clone()
    };
    let mut value = manifest(&snapshot);
    let state = value["files"][0].clone();
    let mut ops = state.clone();
    ops["name"] = json!("ops");
    value["files"].as_array_mut().expect("files").insert(1, ops);
    rewrite_manifest(&snapshot, &value);
    fs::copy(snapshot.join("state"), snapshot.join("ops")).expect("a copy");
    let refused = restore(&restore_flags(&snapshot, &clash), &FileLock).expect_err("a clash");
    assert_eq!(
        refused.to_string(),
        format!(
            "{}: File exists (os error 17); the restore removed what it had written",
            paths.state_file.display()
        )
    );
    assert_eq!(
        entries_under(target.path()),
        ["state", "state/desired.json.lock"]
    );
}

/// Catches: a backup that replaces a snapshot, or the partial directory of one being made, or
/// that writes a snapshot of nothing because a flag names the wrong file: each is refused by
/// name, and nothing is made.
#[test]
fn a_backup_never_replaces_a_snapshot_and_refuses_an_empty_state() {
    let dir = scratch("backup-refusals");
    let paths = small_state(&dir);
    let out = dir.path().join("snapshot");
    fs::create_dir(&out).expect("a snapshot there");
    let refused = backup(&backup_flags(&paths, &out), &FileLock).expect_err("there");
    assert_eq!(
        refused.to_string(),
        format!(
            "{} is already there; a backup never replaces one: name another --out",
            out.display()
        )
    );
    let partial = dir.path().join("other.partial");
    fs::create_dir(&partial).expect("a partial one there");
    let refused =
        backup(&backup_flags(&paths, &dir.path().join("other")), &FileLock).expect_err("partial");
    assert!(
        matches!(&refused, BackupError::Exists { path } if *path == partial),
        "{refused:?}"
    );

    let wrong = StatePaths {
        state_file: dir.path().join("no-such.json"),
        ops_file: None,
        enrollment_dir: None,
    };
    let refused =
        backup(&backup_flags(&wrong, &dir.path().join("third")), &FileLock).expect_err("none");
    assert_eq!(
        refused.to_string(),
        format!(
            "{}: none of the coordinator's state files is there; check --state-file and --ops-file",
            dir.path().join("no-such.json").display()
        )
    );
    let no_dir = StatePaths {
        state_file: dir.path().join("no-dir").join("desired.json"),
        ops_file: None,
        enrollment_dir: None,
    };
    let refused =
        backup(&backup_flags(&no_dir, &dir.path().join("fifth")), &FileLock).expect_err("none");
    assert!(
        matches!(refused, BackupError::NothingThere { .. }),
        "{refused:?}"
    );
    assert!(
        !dir.path().join("no-dir").exists(),
        "no lock file made for a wrong flag"
    );
    let refused = backup(
        &backup_flags(&paths, &dir.path().join("no-parent").join("snapshot")),
        &FileLock,
    )
    .expect_err("no parent");
    assert!(
        matches!(&refused, BackupError::Io { path, .. } if *path == dir.path().join("no-parent/snapshot.partial")),
        "{refused:?}"
    );
    let no_store = StatePaths {
        enrollment_dir: Some(dir.path().join("no-store")),
        ..paths.clone()
    };
    let refused = backup(
        &backup_flags(&no_store, &dir.path().join("fourth")),
        &FileLock,
    )
    .expect_err("dir");
    assert_eq!(
        refused.to_string(),
        format!(
            "--enrollment-dir: {} is no directory",
            dir.path().join("no-store").display()
        )
    );
    let inside_a_file = jti_path(&paths.state_file).join("snapshot");
    let refused =
        backup(&backup_flags(&paths, &inside_a_file), &FileLock).expect_err("not a directory");
    assert!(matches!(refused, BackupError::Io { .. }), "{refused:?}");
    assert!(!dir.path().join("third").exists() && !dir.path().join("fourth").exists());
}

/// Catches: a backup that fails while copying and leaves a partial directory that refuses the
/// next backup: the state file is a directory, so reading it fails; the backup fails naming it
/// and removes its partial directory.
#[test]
fn a_backup_that_fails_while_copying_leaves_no_partial_directory() {
    let dir = scratch("backup-fails");
    let state = dir.path().join("desired.json");
    fs::create_dir(&state).expect("a directory where the state file goes");
    let paths = StatePaths {
        state_file: state.clone(),
        ops_file: None,
        enrollment_dir: None,
    };
    let out = dir.path().join("snapshot");
    let refused = backup(&backup_flags(&paths, &out), &FileLock).expect_err("unreadable");
    assert!(
        matches!(&refused, BackupError::Io { path, .. } if *path == state),
        "{refused:?}"
    );
    assert!(!out.exists());
    assert!(!dir.path().join("snapshot.partial").exists());
}

/// Catches: what a store does not read copied (a link, a leftover `.tmp`, a name that is not
/// UTF-8, the store's lock), a store file in a deeper directory left out, or a state file that
/// was not there left out without a word (a wrong `--ops-file`): only the `.pb` files are in the
/// snapshot, each other entry is named as passed over (the lock is not), and each file of the
/// state that is not there is named.
#[test]
fn a_backup_copies_the_store_files_alone_and_names_what_it_passed_over() {
    let dir = scratch("backup-passed");
    let paths = small_state(&dir);
    let store = paths.enrollment_dir.clone().expect("a store");
    fs::write(store.join("lock"), b"").expect("the store's lock");
    fs::write(store.join("hosts").join("s2.pb.tmp"), b"half").expect("a leftover");
    fs::create_dir_all(store.join("deep").join("er")).expect("a deeper directory");
    fs::write(store.join("deep").join("er").join("x.pb"), b"\x01").expect("a deep file");
    std::os::unix::fs::symlink("serial.pb", store.join("link.pb")).expect("a link");
    let odd = store.join(OsStr::from_bytes(b"odd\xff.pb"));
    fs::write(&odd, b"\x02").expect("a name that is not UTF-8");
    let out = dir.path().join("snapshot");
    let report = backup(&backup_flags(&paths, &out), &FileLock).expect("a backup");
    assert_eq!(
        files_under(&out).keys().collect::<Vec<_>>(),
        [
            "enrollment/deep/er/x.pb",
            "enrollment/hosts/s1.pb",
            "enrollment/serial.pb",
            MANIFEST,
            "state",
            "state-jti",
        ]
    );
    let absent: Vec<String> = report
        .to_string()
        .lines()
        .filter_map(|line| line.strip_prefix("not there, so not in this backup: "))
        .map(str::to_owned)
        .collect();
    let state = paths.state_file.display().to_string();
    let ops = format!("{state}.ops");
    assert_eq!(
        absent,
        [
            format!("{state}.releases"),
            format!("{state}.repositories"),
            format!("{state}.verdicts"),
            format!("{state}.accounts"),
            ops.clone(),
            format!("{ops}.audit"),
            format!("{ops}.halts"),
            format!("{ops}.halts.next"),
            format!("{ops}.halts.unfinished"),
        ]
    );
    let passed: Vec<String> = report
        .to_string()
        .lines()
        .filter_map(|line| line.strip_prefix("not copied, no file of the enrollment store: "))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        passed,
        [
            store.join("hosts").join("s2.pb.tmp").display().to_string(),
            store.join("link.pb").display().to_string(),
            odd.display().to_string(),
        ]
    );
}

/// Catches: a marker of an unfinished halt dropped by the round trip (the restored coordinator
/// would start over a halt that may be lost) or restored without a word: the marker is copied
/// and restored, and the restore says `serve` refuses to start until it is settled.
#[test]
fn the_marker_of_an_unfinished_halt_is_restored_and_said() {
    let dir = scratch("backup-unfinished");
    let paths = small_state(&dir);
    let ops = ops_path(&paths.state_file, None);
    fs::write(format!("{}.halts.unfinished", ops.display()), b"").expect("a marker");
    let out = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &out), &FileLock).expect("a backup");
    let target = scratch("backup-unfinished-target");
    let restored = targets_in(&target);
    let report = restore(&restore_flags(&out, &restored), &FileLock).expect("restored");
    assert!(
        PathBuf::from(format!(
            "{}.halts.unfinished",
            restored.ops_file().display()
        ))
        .exists()
    );
    assert_eq!(
        report.to_string().lines().last(),
        Some(
            "the snapshot holds the marker of changes that were not yet written: serve refuses to start until they are settled by hand and the marker removed, unless the ops file holds every change it lists, as it did before the backup"
        )
    );
}

/// Catches: the paths of a snapshot's files drifting from the ones `serve` opens: the ops file
/// defaults to the state file's name with `.ops` added, and `--ops-file` overrides it.
#[test]
fn the_ops_file_defaults_as_serve_defaults_it() {
    assert_eq!(
        ops_path(Path::new("/srv/c/desired.json"), None),
        Path::new("/srv/c/desired.json.ops")
    );
    assert_eq!(
        ops_path(Path::new("/srv/c/desired.json"), Some(Path::new("/srv/o"))),
        Path::new("/srv/o")
    );
}

/// Catches: a failed restore that says it removed what it wrote when it could not: the error
/// names each file it left, for the operator to remove before restoring again.
#[test]
fn a_failed_restore_names_what_it_could_not_remove() {
    let failed = BackupError::Unfinished {
        path: PathBuf::from("/srv/c/desired.json.ops"),
        source: std::io::Error::from(std::io::ErrorKind::StorageFull),
        left: vec![
            PathBuf::from("/srv/c/desired.json"),
            PathBuf::from("/srv/c"),
        ],
    };
    assert_eq!(
        failed.to_string(),
        "/srv/c/desired.json.ops: no storage space; the restore could not remove what it had written of: /srv/c/desired.json, /srv/c; remove those before restoring again"
    );
}

/// Catches: a bare file name with no directory to sync or make: its directory is `.`, as it
/// is for the root.
#[test]
fn the_directory_of_a_bare_name_is_the_current_one() {
    assert_eq!(parent_of(Path::new("snapshot")), Path::new("."));
    assert_eq!(parent_of(Path::new("/")), Path::new("."));
    assert_eq!(parent_of(Path::new("/srv/snapshot")), Path::new("/srv"));
}

/// Catches: a restore that stopped partway (killed, or the machine lost) leaving files `serve`
/// starts on as if they were the whole snapshot: a restore that finished leaves no marker, a
/// marker left over refuses a second restore into the same targets, and `serve`'s start refuses
/// it by name.
#[test]
fn a_restore_that_did_not_finish_is_refused_by_restore_and_serve() {
    let (_dir, snapshot) = small_snapshot("restore-marker");
    let target = scratch("restore-marker-target");
    let paths = targets_in(&target);
    restore(&restore_flags(&snapshot, &paths), &FileLock).expect("restored");
    let marker = restore_marker(&paths.state_file);
    assert!(!marker.exists());
    assert_eq!(
        crate::config::refuse_unfinished_restore(&paths.state_file).ok(),
        Some(())
    );

    let other = scratch("restore-marker-other");
    let again = targets_in(&other);
    let marker = restore_marker(&again.state_file);
    fs::create_dir_all(marker.parent().expect("a parent")).expect("a directory");
    fs::write(&marker, b"").expect("a marker left over");
    let refused = restore(&restore_flags(&snapshot, &again), &FileLock).expect_err("a marker");
    assert!(
        matches!(&refused, BackupError::NotEmpty { path } if *path == marker),
        "{refused:?}"
    );
    let start = crate::config::refuse_unfinished_restore(&again.state_file).expect_err("marker");
    assert_eq!(
        start.to_string(),
        format!(
            "{}: a restore into these state files stopped before it finished, so they may hold part of a snapshot; remove them and this file, and restore again",
            marker.display()
        )
    );
}

/// Proves: the marker of a `host-groups migrate --apply` of release 0.1.253 that stopped partway
/// (`<state-file>.migrate-unfinished`, which this release never writes) refuses a backup of those
/// files, with the text that names it and says to restore the backup taken before the migration,
/// and makes no snapshot; `serve`'s start refuses it with the same text; and `restore` takes such
/// a marker in its targets for state, and writes nothing there.
///
/// Catches: a backup or a start over state files that hold part of a migration, because the verb
/// that wrote the marker is gone.
#[test]
fn the_marker_of_a_migration_the_release_before_stopped_refuses_backup_restore_and_serve() {
    let dir = scratch("migrate-marker");
    let paths = small_state(&dir);
    let marker = migrate_marker(&paths.state_file);
    fs::write(&marker, b"").expect("the marker");
    let text = format!(
        "{}: a host-groups migrate --apply of release 0.1.253 stopped partway; restore the backup \
         taken before it",
        marker.display()
    );
    let out = scratch("migrate-marker-out");
    let snapshot = out.path().join("snapshot");
    let refused = backup(&backup_flags(&paths, &snapshot), &FileLock).expect_err("refused");
    assert!(
        matches!(&refused, BackupError::MigrateUnfinished { path } if *path == marker),
        "{refused:?}"
    );
    assert_eq!(refused.to_string(), text);
    assert!(!snapshot.exists(), "no snapshot is made");
    let start = crate::config::refuse_unfinished_migrate(&paths.state_file).expect_err("marker");
    assert_eq!(start.to_string(), text);
    assert!(marker.exists(), "the marker stays for the operator");

    let (_from, made) = small_snapshot("migrate-marker-snapshot");
    let target = scratch("migrate-marker-target");
    let into = targets_in(&target);
    let in_target = migrate_marker(&into.state_file);
    fs::create_dir_all(in_target.parent().expect("a parent")).expect("a directory");
    fs::write(&in_target, b"").expect("a marker in the target");
    let refused = restore(&restore_flags(&made, &into), &FileLock).expect_err("state");
    assert!(
        matches!(&refused, BackupError::NotEmpty { path } if *path == in_target),
        "{refused:?}"
    );
    assert!(!into.state_file.exists(), "nothing is restored");
}
