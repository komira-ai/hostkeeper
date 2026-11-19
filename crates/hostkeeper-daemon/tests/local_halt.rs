//! The halt the daemon holds of a service, as the heartbeat loop reads it
//! ([`HeldHalts::holding`] of [`DaemonHalts`]): the service's step record ended in a halt, under
//! the generation of its `held-halt` file and the time that file was written. A generation held
//! from another coordinator (one above any this coordinator issued) is reported like any other and
//! lifted only by one past it; a current one stands until a newer generation comes.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use hostkeeper_daemon::halt::{HELD_HALT_FILE, HaltStore};
use hostkeeper_daemon::link_adapters::DaemonHalts;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_link::{HeldHalt, HeldHalts};
use hostkeeper_steps::RecordStore;
use hostkeeper_steps::record::{RECORD_FILE, State};
use support::{Rig, name};
use support_host::NativeHost;

/// The release `web` runs before the update that halts.
const BASE: &str = "1.9.0";

/// When the tests stamp a `held-halt` file as written, in unix seconds.
const WRITTEN: u64 = 1_700_000_000;

/// `web` installed at [`BASE`], with 2.0.0 and 2.1.0 published to order next.
fn installed(farm: &NativeHost) -> Daemon {
    for version in [BASE, "2.0.0", "2.1.0"] {
        farm.publish("web", version);
    }
    let daemon = farm.daemon();
    let installed = daemon.order(&farm.install("web", BASE, 1));
    assert!(
        matches!(
            installed,
            Reply::Finished {
                verdict: Verdict::Succeeded { .. },
                ..
            }
        ),
        "{installed:?}"
    );
    daemon
}

/// `web` halted by its hook (an update to 2.0.0 whose `prepare` exits 76), then held under halt
/// generation `held` (no file for 0), its file stamped [`WRITTEN`]: as a host left with the
/// `held-halt` of the coordinator it heard before the one it hears now.
fn halted(farm: &NativeHost, held: u64) -> Daemon {
    let rig = farm.rig();
    let daemon = installed(farm);
    rig.launcher.exits("web", Step::Prepare, &[76]);
    let halt = daemon.order(&farm.update("web", "2.0.0", 2));
    assert!(
        matches!(
            halt,
            Reply::Finished {
                verdict: Verdict::Halt,
                ..
            }
        ),
        "{halt:?}"
    );
    if held > 0 {
        HaltStore::new(&rig.state)
            .raise(&name("web"), held)
            .expect("held");
        stamp(rig, WRITTEN);
    }
    daemon
}

/// Sets the modification time of `web`'s `held-halt` file to `unix` seconds.
fn stamp(rig: &Rig, unix: u64) {
    fs::File::options()
        .write(true)
        .open(rig.state.join("web").join(HELD_HALT_FILE))
        .and_then(|file| file.set_modified(UNIX_EPOCH + Duration::from_secs(unix)))
        .expect("stamped");
}

/// The halt the heartbeat loop reads for `web`.
fn holding(daemon: &Arc<Daemon>) -> Option<HeldHalt> {
    DaemonHalts::new(Arc::clone(daemon)).holding(&name("web"))
}

/// The reason of the halt record `web`'s hook left, as the loop reads it.
fn reason_of(rig: &Rig) -> Option<String> {
    match rig.record("web").expect("the record").state {
        State::Finished { reason, .. } => reason,
        other => panic!("not finished: {other:?}"),
    }
}

/// Proves: a halt held under generation 15, above the 3 the coordinator has issued, is reported
/// with the record's reason, the generation and the time its file was written; the coordinator's
/// generation 3 lifts nothing, no error, and every order of the service is still refused
/// `halted`, while the report stands.
///
/// Catches: a hold that refuses every order and is never reported, so that the coordinator can
/// only show the host as `failed_here`.
#[test]
fn a_halt_held_above_the_coordinators_generation_is_reported_and_its_generation_lifts_nothing() {
    let farm = NativeHost::new("local-halt-stale");
    let rig = farm.rig();
    let daemon = Arc::new(halted(&farm, 15));
    let expected = HeldHalt {
        reason: reason_of(rig).map(|r| hostkeeper_link::Reason::new(&r).expect("a token")),
        generation: 15,
        written_unix: Some(WRITTEN),
    };
    assert_eq!(holding(&daemon), Some(expected.clone()));
    DaemonHalts::new(Arc::clone(&daemon))
        .raise(&name("web"), 3)
        .expect("a lower generation is no error");
    assert_eq!(holding(&daemon), Some(expected));
    let refused = daemon.order(&farm.update("web", "2.1.0", 3));
    assert_eq!(refused.refusal(), Some(Refusal::Halted), "{refused:?}");
}

/// Proves: the generation a coordinator's clear issues past the held one (16 over 15) lifts the
/// halt: the loop reads no halt any more, the generation held is 16, and the next order runs.
///
/// Catches: a hold whose report outlives the halt, which would keep the coordinator holding a
/// host that takes orders again.
#[test]
fn a_generation_past_the_held_one_lifts_the_halt_and_its_report() {
    let farm = NativeHost::new("local-halt-lifted");
    let daemon = Arc::new(halted(&farm, 15));
    let halts = DaemonHalts::new(Arc::clone(&daemon));
    halts.raise(&name("web"), 16).expect("raised");
    assert_eq!(holding(&daemon), None);
    assert_eq!(halts.held(&name("web")), 16);
    let ran = daemon.order(&farm.update("web", "2.1.0", 3));
    assert!(
        matches!(
            ran,
            Reply::Finished {
                verdict: Verdict::Succeeded { .. },
                ..
            }
        ),
        "{ran:?}"
    );
}

/// Proves: a halt held under the generation the coordinator issued last (3) is honoured: the same
/// generation again lifts nothing and it is still reported; the next one (4) lifts it.
///
/// Catches: a current halt lifted by the generation it was raised under.
#[test]
fn a_current_halt_stands_until_a_newer_generation() {
    let farm = NativeHost::new("local-halt-current");
    let daemon = Arc::new(halted(&farm, 3));
    let halts = DaemonHalts::new(Arc::clone(&daemon));
    halts.raise(&name("web"), 3).expect("no error");
    assert_eq!(holding(&daemon).map(|held| held.generation), Some(3));
    let refused = daemon.order(&farm.update("web", "2.1.0", 3));
    assert_eq!(refused.refusal(), Some(Refusal::Halted), "{refused:?}");
    halts.raise(&name("web"), 4).expect("raised");
    assert_eq!(holding(&daemon), None);
}

/// Proves: a halt with no generation written is held under 0 and reports no time of a write.
///
/// Catches: a missing `held-halt` file read as no halt, or given a time.
#[test]
fn a_halt_with_no_generation_written_is_held_under_zero_with_no_time() {
    let farm = NativeHost::new("local-halt-zero");
    let daemon = Arc::new(halted(&farm, 0));
    let held = holding(&daemon).expect("a halt");
    assert_eq!((held.generation, held.written_unix), (0, None));
}

/// Proves: a `held-halt` file with no halt record holds nothing and is not reported, whatever its
/// generation, and a service with no step record at all reports none.
///
/// Catches: a leftover generation reported as a halt the host does not hold.
#[test]
fn a_held_generation_with_no_halt_record_is_no_halt() {
    let farm = NativeHost::new("local-halt-none");
    let rig = farm.rig();
    let daemon = Arc::new(installed(&farm));
    HaltStore::new(&rig.state)
        .raise(&name("web"), 15)
        .expect("held");
    assert_eq!(holding(&daemon), None);
    let halts = DaemonHalts::new(Arc::clone(&daemon));
    assert_eq!(halts.holding(&name("nothing")), None);
}

/// Proves: a halt whose `held-halt` file is no number, and one whose step record cannot be read,
/// are not reported: the executor refuses the service's orders under its own name for each
/// (`halt_unreadable`, `record_unreadable`), which the verdict reports instead.
///
/// Catches: an unreadable generation reported as 0, which a clear past it would not reach, or a
/// garbage record read as a halt.
#[test]
fn a_halt_that_cannot_be_read_is_not_reported() {
    let farm = NativeHost::new("local-halt-garbage");
    let rig = farm.rig();
    let daemon = Arc::new(halted(&farm, 15));
    fs::write(rig.state.join("web").join(HELD_HALT_FILE), "x\n").expect("garbage");
    assert_eq!(holding(&daemon), None);
    let refused = daemon.order(&farm.update("web", "2.1.0", 3));
    assert_eq!(
        refused.refusal(),
        Some(Refusal::HaltUnreadable),
        "{refused:?}"
    );

    let farm = NativeHost::new("local-halt-record-garbage");
    let rig = farm.rig();
    let daemon = Arc::new(halted(&farm, 15));
    fs::write(rig.state.join("web").join(RECORD_FILE), "{").expect("garbage");
    assert_eq!(holding(&daemon), None);
}

/// Proves: the time of a generation's write is its file's modification time, which a raise
/// sets; none when no generation is written.
///
/// Catches: a time read from anything but the file, which would not tell how long a leftover
/// generation has been there.
#[test]
fn the_time_a_generation_was_written_is_its_files() {
    let scratch = support::Scratch::new("local-halt-time");
    let store = HaltStore::new(scratch.path());
    assert_eq!(store.written_unix(&name("web")), None);
    store.raise(&name("web"), 2).expect("raised");
    fs::File::options()
        .write(true)
        .open(scratch.path().join("web").join(HELD_HALT_FILE))
        .and_then(|file| file.set_modified(UNIX_EPOCH + Duration::from_secs(WRITTEN)))
        .expect("stamped");
    assert_eq!(store.written_unix(&name("web")), Some(WRITTEN));
}

/// Proves: a halt whose record names no reason, and one whose reason is no token, are still
/// reported, with no reason, under the generation held.
///
/// Catches: a halt dropped from the report for its reason, which would leave the coordinator
/// showing the host as `failed_here` again.
#[test]
fn a_halt_with_no_usable_reason_is_reported_without_one() {
    for (case, reason) in [("none", None), ("no-token", Some("Disk Bad".to_owned()))] {
        let farm = NativeHost::new(&format!("local-halt-reason-{case}"));
        let rig = farm.rig();
        let daemon = Arc::new(halted(&farm, 15));
        let mut record = rig.record("web").expect("the record");
        record.state = State::Finished {
            verdict: Verdict::Halt,
            reason,
        };
        RecordStore::new(&rig.state.join("web"))
            .save(&record)
            .expect("saved");
        assert_eq!(
            holding(&daemon),
            Some(HeldHalt {
                reason: None,
                generation: 15,
                written_unix: Some(WRITTEN),
            }),
            "{case}"
        );
    }
}
