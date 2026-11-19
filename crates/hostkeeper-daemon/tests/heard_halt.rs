//! The halt generation each heartbeat reply names for a service ([`Daemon::heard_halt`]), as the
//! coordinator sends it to every host after any clear: a service that holds no halt keeps no
//! `held-halt` file, whatever it hears, and a file left with no halt is removed; a halt held
//! under no generation is held under the one the reply before named (the one heard when no reply
//! came before since the start), and lifted, its file removed, only by a greater one.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::halt::{HELD_HALT_FILE, HaltStore};
use hostkeeper_daemon::link_adapters::DaemonHalts;
use hostkeeper_daemon::locks::Holder;
use hostkeeper_daemon::verbs::clear_halt::Heard;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_link::HeldHalts;
use hostkeeper_steps::record::{RECORD_FILE, State};
use hostkeeper_steps::runner::REASON_HALT_CLEARED;
use hostkeeper_steps::{CrashPoint, RecordStore};
use support::{Rig, name};
use support_host::NativeHost;

/// The release `web` runs before the update that halts.
const BASE: &str = "1.9.0";

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// `web` installed at [`BASE`], with 2.0.0 and 2.1.0 published to order next.
fn installed(farm: &NativeHost) -> Daemon {
    for version in [BASE, "2.0.0", "2.1.0"] {
        farm.publish("web", version);
    }
    let daemon = farm.daemon();
    assert_eq!(daemon.order(&farm.install("web", BASE, 1)), succeeded());
    daemon
}

/// `web` halted by its hook (an update to 2.0.0 whose `prepare` exits 76), held under no
/// generation yet.
fn halted(farm: &NativeHost) -> Daemon {
    let daemon = installed(farm);
    farm.rig().launcher.exits("web", Step::Prepare, &[76]);
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
    daemon
}

/// The text of `service`'s `held-halt` file; `None` when there is none.
fn held_file(rig: &Rig, service: &str) -> Option<String> {
    fs::read_to_string(rig.state.join(service).join(HELD_HALT_FILE)).ok()
}

/// The state of `web`'s step record.
fn record_state(rig: &Rig) -> State {
    rig.record("web").expect("the record").state
}

/// Proves: a reply's halt generation of a service that holds no halt writes no `held-halt` file,
/// for every service the reply names, through the heartbeat loop's adapter as through the daemon.
///
/// Catches: every host writing the generation of a clear meant for another host's halt, so each
/// host is left with a `held-halt` file that outlives any halt (a clear of tierstore's halt on
/// one host wrote one on every host).
#[test]
fn a_service_that_holds_no_halt_keeps_no_held_halt_file_whatever_it_hears() {
    let farm = NativeHost::new("heard-no-halt");
    let rig = farm.rig();
    let daemon = Arc::new(installed(&farm));
    let halts = DaemonHalts::new(Arc::clone(&daemon));
    for generation in [4, 5, 11] {
        halts
            .heard(&name("web"), None, generation)
            .expect("no error");
        halts
            .heard(&name("api"), None, generation)
            .expect("no error");
    }
    assert_eq!(held_file(rig, "web"), None);
    assert_eq!(held_file(rig, "api"), None);
    assert!(!rig.state.join("api").exists());
    assert_eq!(
        daemon.heard_halt(&name("web"), None, 12),
        Ok(Heard::Unchanged)
    );
    assert_eq!(held_file(rig, "web"), None);
}

/// Proves: a `held-halt` file of a service that holds no halt is removed by the next reply that
/// names the service, whether the generation heard is below, equal to or above the file's.
///
/// Catches: a file left from a lift that did not finish, or written for no halt by an earlier
/// daemon, kept until a later halt is held under it, so that halt is held under a generation the
/// coordinator never issued.
#[test]
fn a_held_halt_file_of_no_halt_is_removed_by_the_next_reply() {
    for heard in [11, 15, 16] {
        let farm = NativeHost::new(&format!("heard-orphan-{heard}"));
        let rig = farm.rig();
        let daemon = installed(&farm);
        HaltStore::new(&rig.state)
            .raise(&name("web"), 15)
            .expect("a file of no halt");
        assert_eq!(
            daemon.heard_halt(&name("web"), None, heard),
            Ok(Heard::Removed),
            "{heard}"
        );
        assert_eq!(held_file(rig, "web"), None, "{heard}");
        assert_eq!(
            daemon.heard_halt(&name("web"), None, heard),
            Ok(Heard::Unchanged),
            "{heard}"
        );
    }
}

/// Proves: a halt held under no generation, heard after a reply that named the same generation
/// (4), is held under it (its file written), and stands through that generation again and a
/// lower one, refusing orders `halted`; a greater one lifts it: the record becomes the
/// `halt_cleared` deferral, the file is removed, and the next order runs; a generation heard
/// after that writes nothing.
///
/// Catches: a halt lifted by the generation it came under, and a lift that leaves its
/// `held-halt` file behind.
#[test]
fn a_halt_is_held_under_the_generation_before_it_and_its_lift_removes_the_file() {
    let farm = NativeHost::new("heard-halt");
    let rig = farm.rig();
    let daemon = halted(&farm);
    assert_eq!(held_file(rig, "web"), None);
    assert_eq!(daemon.heard_halt(&name("web"), Some(4), 4), Ok(Heard::Held));
    assert_eq!(held_file(rig, "web").as_deref(), Some("4\n"));
    for again in [4, 3] {
        assert_eq!(
            daemon.heard_halt(&name("web"), Some(4), again),
            Ok(Heard::Unchanged)
        );
    }
    assert_eq!(held_file(rig, "web").as_deref(), Some("4\n"));
    let refused = daemon.order(&farm.update("web", "2.1.0", 3));
    assert_eq!(refused.refusal(), Some(Refusal::Halted), "{refused:?}");

    assert_eq!(
        daemon.heard_halt(&name("web"), Some(4), 5),
        Ok(Heard::Lifted)
    );
    let cleared = State::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(REASON_HALT_CLEARED.to_owned()),
    };
    assert_eq!(record_state(rig), cleared);
    assert_eq!(held_file(rig, "web"), None);
    assert_eq!(daemon.order(&farm.update("web", "2.1.0", 4)), succeeded());
    assert_eq!(
        daemon.heard_halt(&name("web"), Some(5), 6),
        Ok(Heard::Unchanged)
    );
    assert_eq!(held_file(rig, "web"), None);
}

/// Proves: a halt that came while the service had no generation (0, which a reply leaves out)
/// is lifted by the first reply that names generation 1, the first clear of the service: no file
/// is written, and the next order runs.
///
/// Catches: the first clear of a service taken as the generation a halt is held under, so the
/// halt stands until a second clear.
#[test]
fn a_halt_before_any_clear_is_lifted_by_the_first_clear() {
    let farm = NativeHost::new("heard-first-clear");
    let rig = farm.rig();
    let daemon = halted(&farm);
    assert_eq!(
        daemon.heard_halt(&name("web"), Some(0), 1),
        Ok(Heard::Lifted)
    );
    assert_eq!(held_file(rig, "web"), None);
    assert_eq!(daemon.order(&farm.update("web", "2.1.0", 3)), succeeded());
}

/// Proves: a halt heard with no reply before it since the daemon started is held under the
/// generation heard (5), not lifted by it; the next one (6) lifts it.
///
/// Catches: a halt lifted at a daemon's start by a clear issued before it was known, or held for
/// good.
#[test]
fn a_halt_heard_first_after_a_start_is_held_under_that_generation() {
    let farm = NativeHost::new("heard-start");
    let rig = farm.rig();
    let daemon = halted(&farm);
    assert_eq!(daemon.heard_halt(&name("web"), None, 5), Ok(Heard::Held));
    assert_eq!(held_file(rig, "web").as_deref(), Some("5\n"));
    assert_eq!(
        daemon.heard_halt(&name("web"), Some(5), 6),
        Ok(Heard::Lifted)
    );
    assert_eq!(held_file(rig, "web"), None);
}

/// Proves: a halt heard as generation 0 after a reply of 0 is held under 0 (its file reads 0),
/// and generation 1 lifts it.
///
/// Catches: a halt held under 0 written as no halt, or lifted by 0.
#[test]
fn a_halt_held_under_zero_is_lifted_by_generation_one() {
    let farm = NativeHost::new("heard-zero");
    let rig = farm.rig();
    let daemon = halted(&farm);
    assert_eq!(daemon.heard_halt(&name("web"), Some(0), 0), Ok(Heard::Held));
    assert_eq!(held_file(rig, "web").as_deref(), Some("0\n"));
    assert_eq!(
        daemon.heard_halt(&name("web"), Some(0), 0),
        Ok(Heard::Unchanged)
    );
    assert_eq!(
        daemon.heard_halt(&name("web"), Some(0), 1),
        Ok(Heard::Lifted)
    );
    assert_eq!(held_file(rig, "web"), None);
}

/// Proves: while another holds the service's lock, a reply's generation changes nothing and is
/// no error (`Heard::Busy`); the next reply after the lock is let go holds the halt.
///
/// Catches: a reply that writes under another's lock, or that drops the generation for good.
#[test]
fn a_generation_heard_while_the_service_is_locked_is_judged_at_the_next_reply() {
    let farm = NativeHost::new("heard-busy");
    let rig = farm.rig();
    let daemon = Arc::new(halted(&farm));
    let guard = daemon
        .ctx()
        .locks
        .take_for(&name("web"), Holder::ClearRecord)
        .expect("the lock");
    assert_eq!(daemon.heard_halt(&name("web"), None, 4), Ok(Heard::Busy));
    DaemonHalts::new(Arc::clone(&daemon))
        .heard(&name("web"), None, 4)
        .expect("busy is no error");
    assert_eq!(held_file(rig, "web"), None);
    drop(guard);
    assert_eq!(daemon.heard_halt(&name("web"), None, 4), Ok(Heard::Held));
    assert_eq!(held_file(rig, "web").as_deref(), Some("4\n"));
}

/// Proves: a `held-halt` file that cannot be written when a halt is first held is the error the
/// loop logs, with the store's text, and nothing is held; the next reply holds it once it can.
///
/// Catches: a halt held under a generation that was never written, which a restart would hold
/// under the next one heard instead.
#[test]
fn a_held_halt_that_cannot_be_written_is_written_from_the_next_reply() {
    let farm = NativeHost::new("heard-unwritable");
    let rig = farm.rig();
    let daemon = Arc::new(halted(&farm));
    let blocker = rig.state.join("web").join(format!("{HELD_HALT_FILE}.tmp"));
    fs::create_dir(&blocker).expect("a directory where the temporary file goes");
    let error = DaemonHalts::new(Arc::clone(&daemon))
        .heard(&name("web"), None, 1)
        .expect_err("not written");
    assert!(
        error
            .to_string()
            .starts_with("the held halt generation of web cannot be read or written: "),
        "{error}"
    );
    assert_eq!(held_file(rig, "web"), None);
    fs::remove_dir(&blocker).expect("removed");
    assert_eq!(daemon.heard_halt(&name("web"), None, 1), Ok(Heard::Held));
    assert_eq!(held_file(rig, "web").as_deref(), Some("1\n"));
}

/// Proves: a lift from a reply whose daemon dies once the record's write is durable and before
/// the file is removed leaves the file of no halt, which the next daemon's next reply removes.
///
/// Catches: a lift that removes the file first, which would leave a halt held under no
/// generation, or a file left for good by a lift that did not finish.
#[test]
fn a_lift_that_died_before_removing_the_file_is_finished_by_the_next_reply() {
    let farm = NativeHost::new("heard-crash");
    let rig = farm.rig();
    let daemon = halted(&farm);
    assert_eq!(daemon.heard_halt(&name("web"), None, 3), Ok(Heard::Held));
    drop(daemon);
    let dies = farm
        .daemon()
        .with_stores(Arc::new(|_: &ServiceName, store: RecordStore| {
            let after_a_write = |point: CrashPoint| matches!(point, CrashPoint::AfterWrite(_));
            store.with_crash_gate(Arc::new(after_a_write))
        }));
    let died = dies.heard_halt(&name("web"), None, 4).expect_err("died");
    assert_eq!(died.refusal, Refusal::StoreFailed);
    assert_eq!(held_file(rig, "web").as_deref(), Some("3\n"));
    drop(dies);

    let daemon = farm.daemon();
    assert_eq!(daemon.heard_halt(&name("web"), None, 4), Ok(Heard::Removed));
    assert_eq!(held_file(rig, "web"), None);
    assert_eq!(daemon.order(&farm.update("web", "2.1.0", 3)), succeeded());
}

/// Proves: a step record that cannot be read (one of a newer schema, one that does not parse)
/// leaves the `held-halt` file beside it as it is, with no error.
///
/// Catches: a record that cannot be judged read as no halt, which would remove the generation a
/// halt it may hold is held under.
#[test]
fn a_record_that_cannot_be_read_leaves_the_file_as_it_is() {
    for (kind, text) in [("newer", r#"{"schema": 9}"#), ("corrupt", "{")] {
        let rig = Rig::new(&format!("heard-record-{kind}"));
        let daemon = rig.daemon();
        HaltStore::new(&rig.state)
            .raise(&name("web"), 3)
            .expect("a held halt");
        fs::write(rig.state.join("web").join(RECORD_FILE), text).expect(kind);
        assert_eq!(
            daemon.heard_halt(&name("web"), None, 9),
            Ok(Heard::Unchanged),
            "{kind}"
        );
        assert_eq!(held_file(&rig, "web").as_deref(), Some("3\n"), "{kind}");
    }
}
