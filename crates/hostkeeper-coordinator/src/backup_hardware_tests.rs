//! `backup` and `restore` over the hardware event log a coordinator keeps beside its state file
//! (`<state-file>.hardware-events`).

use crate::backup::{StatePaths, backup, restore};
use crate::backup_tests::{backup_flags, files_under, restore_flags, targets_in};
use crate::hardware_files::{FileLog, hardware_events_path};
use crate::hardware_log::tests::{mixed, record};
use crate::hardware_log::{HardwareLog, Query};
use crate::lock::FileLock;
use crate::testutil::scratch;

/// Proves: a backup of a state that holds a hardware event log copies its file into the snapshot
/// as `state-hardware-events`, byte for byte, and a restore into empty targets puts it back beside
/// the restored state file, where the log over it answers every record, and a restored log takes
/// a new record.
///
/// Catches: the log left out of a backup, so a restored coordinator would lose every host's
/// hardware history, which the log keeps for good.
#[test]
fn a_backup_copies_the_hardware_event_log_and_a_restore_puts_it_back() {
    let dir = scratch("backup-hardware");
    let state = dir.path().join("state").join("desired.json");
    std::fs::create_dir_all(state.parent().expect("a parent")).expect("a directory");
    FileLog::new(hardware_events_path(&state))
        .write(&mixed())
        .expect("written");
    let paths = StatePaths {
        state_file: state.clone(),
        ops_file: None,
        enrollment_dir: None,
    };
    let written = std::fs::read(hardware_events_path(&state)).expect("the log's file");

    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    assert_eq!(
        files_under(&snapshot).get("state-hardware-events"),
        Some(&written)
    );

    let target = scratch("backup-hardware-target");
    let restored = targets_in(&target);
    restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");
    let path = hardware_events_path(&restored.state_file);
    assert_eq!(std::fs::read(&path).expect("the restored file"), written);
    let log = FileLog::new(path);
    let all = Query {
        limit: 100,
        ..Query::default()
    };
    assert_eq!(log.page(&all), Ok(all.page_of(mixed().iter())));
    log.write(&[record("host-d", "e9", 1, "power", "warning")])
        .expect("written after the restore");
    assert_eq!(log.page(&all).map(|page| page.len()), Ok(6));
}
