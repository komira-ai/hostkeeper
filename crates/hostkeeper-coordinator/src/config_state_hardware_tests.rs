//! Where a start keeps the hardware event log: in the file beside the state file, or, with
//! neither a state file nor a database, in memory.

use super::hardware_log_of;
use crate::hardware_files::hardware_events_path;
use crate::hardware_log::Query;
use crate::hardware_log::tests::mixed;
use crate::testutil::scratch;

/// Proves: over files the log is the file beside the state file, which a log chosen again for
/// the same state file reads back; with neither a state file nor a database it is kept in memory
/// and writes no file.
///
/// Catches: a start that keeps the log in memory over files, so a restart loses it, and one that
/// writes it to a file of no state file's.
#[test]
fn over_files_the_log_is_the_file_beside_the_state_file() {
    let dir = scratch("config-hardware-log");
    let state = dir.path().join("desired.json");
    let all = Query {
        limit: 10,
        ..Query::default()
    };
    hardware_log_of(None, Some(&state))
        .write(&mixed())
        .expect("written");
    assert!(hardware_events_path(&state).exists());
    let again = hardware_log_of(None, Some(&state));
    assert_eq!(again.page(&all).map(|page| page.len()), Ok(5));

    let memory = hardware_log_of(None, None);
    memory.write(&mixed()[..1]).expect("written");
    assert_eq!(memory.page(&all).map(|page| page.len()), Ok(1));
    assert_eq!(
        std::fs::read_dir(dir.path())
            .expect("the directory")
            .count(),
        1,
        "only the file beside the state file"
    );
}
