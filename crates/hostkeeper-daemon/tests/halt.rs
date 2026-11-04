//! The held halt generation: persisted, never lowered, never read as 0 when it is not a number,
//! and written so that a crash leaves it absent or whole.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::WriteStep;
use hostkeeper_daemon::halt::{HELD_HALT_FILE, HaltError, HaltStore};
use support::Scratch;

fn web() -> ServiceName {
    ServiceName::new("web").expect("a service name")
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
}

/// Catches a halt that a restart lifts, a generation that goes down, or a file that does not
/// hold a number read as 0 (which would lift the halt): a missing file reads 0; a raise is read
/// back by a new store on the same directory; a lower raise is refused and changes nothing; the
/// same one again changes nothing; a file of garbage is an error naming the service.
#[test]
fn the_held_halt_generation_survives_a_restart_and_is_never_lowered() {
    let scratch = Scratch::new("halt-keep");
    let state = scratch.path().join("state");
    let store = HaltStore::new(&state);
    assert_eq!(store.get(&web()).expect("missing reads 0"), 0);
    store.raise(&web(), 5).expect("raise");
    let restarted = HaltStore::new(&state);
    assert_eq!(restarted.get(&web()).expect("read"), 5);
    let lower = restarted.raise(&web(), 3).expect_err("lower");
    assert_eq!(
        lower.to_string(),
        "the held halt generation of web is 5; it is never lowered to 3"
    );
    assert!(matches!(
        lower,
        HaltError::Lower {
            held: 5,
            asked: 3,
            ..
        }
    ));
    restarted.raise(&web(), 5).expect("the same again");
    assert_eq!(restarted.get(&web()).expect("read"), 5);
    let file = state.join("web").join(HELD_HALT_FILE);
    assert_eq!(fs::read_to_string(&file).expect("the file"), "5\n");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&state.join("web")), 0o700);
    for garbage in ["", "\n", "x\n", "5", "-1\n", "18446744073709551616\n"] {
        fs::write(&file, garbage).expect("write garbage");
        let error = restarted.get(&web()).expect_err(garbage);
        assert_eq!(
            error.to_string(),
            format!("the held halt generation of web is not a number: {garbage:?}")
        );
        assert!(restarted.raise(&web(), 9).is_err(), "{garbage:?}");
    }
}

/// Catches a held halt file that a crash leaves half written: stopped before the rename, the old
/// value stands and the file is whole; stopped after it, the new value is there, whole.
#[test]
fn held_halt_is_written_with_temp_fsync_rename() {
    let scratch = Scratch::new("halt-crash");
    let state = scratch.path().join("state");
    HaltStore::new(&state).raise(&web(), 2).expect("raise");
    let file = state.join("web").join(HELD_HALT_FILE);
    let before = HaltStore::new(&state)
        .with_gate(Arc::new(|step: WriteStep| step == WriteStep::BeforeRename));
    let crashed = before
        .raise(&web(), 7)
        .expect_err("stopped before the rename");
    assert!(crashed.to_string().contains("BeforeRename"), "{crashed}");
    assert_eq!(fs::read_to_string(&file).expect("the file"), "2\n");
    assert_eq!(HaltStore::new(&state).get(&web()).expect("read"), 2);
    let after = HaltStore::new(&state)
        .with_gate(Arc::new(|step: WriteStep| step == WriteStep::AfterRename));
    after
        .raise(&web(), 7)
        .expect_err("stopped after the rename");
    assert_eq!(fs::read_to_string(&file).expect("the file"), "7\n");
    assert_eq!(HaltStore::new(&state).get(&web()).expect("read"), 7);
    let passing = HaltStore::new(&state).with_gate(Arc::new(|_: WriteStep| false));
    passing.raise(&web(), 8).expect("not stopped");
    assert_eq!(HaltStore::new(&state).get(&web()).expect("read"), 8);
}

/// Catches an unreadable file read as 0, and a write where the service's directory cannot be
/// made (a dangling link is in its place) reported as done.
#[test]
fn a_held_halt_that_cannot_be_read_or_written_is_an_error() {
    let scratch = Scratch::new("halt-io");
    let state = scratch.path().join("state");
    fs::create_dir_all(state.join("web").join(HELD_HALT_FILE)).expect("a directory in the way");
    let unreadable = HaltStore::new(&state).get(&web()).expect_err("a directory");
    assert!(matches!(unreadable, HaltError::Io { .. }), "{unreadable}");
    assert!(
        unreadable
            .to_string()
            .starts_with("the held halt generation of web cannot be read or written: "),
        "{unreadable}"
    );
    let linked = scratch.path().join("linked");
    fs::create_dir_all(&linked).expect("the state dir");
    std::os::unix::fs::symlink("nowhere", linked.join("web")).expect("a dangling link");
    let error = HaltStore::new(&linked)
        .raise(&web(), 1)
        .expect_err("not a directory");
    assert!(matches!(error, HaltError::Io { .. }), "{error}");
}
