//! `clear_halt`: a halt lifted only by a strictly greater halt generation, the halt record
//! cleared before the held-halt file is removed, the generation a halt is held under kept across
//! a restart, nothing written for a service that holds no halt, a running step `busy`, and the
//! heartbeat's halt generations lifting a halt the same way.

mod support;
mod support_apt;
mod support_host;

use std::fs;
use std::sync::Arc;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::halt::{HELD_HALT_FILE, HaltStore};
use hostkeeper_daemon::link_adapters::DaemonHalts;
use hostkeeper_daemon::locks::Holder;
use hostkeeper_daemon::testing::spawn;
use hostkeeper_daemon::verbs::clear_halt::Lifted;
use hostkeeper_daemon::{Daemon, Refusal, Refused, Reply};
use hostkeeper_hook::{Step, Verdict};
use hostkeeper_link::HeldHalts;
use hostkeeper_proto::hostkeeper::agent::v1::{ClearHalt, Order, order};
use hostkeeper_steps::record::{RECORD_FILE, State};
use hostkeeper_steps::runner::REASON_HALT_CLEARED;
use hostkeeper_steps::{CrashPoint, RecordStore};
use support::{Rig, name};
use support_host::NativeHost;

fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// The state of a halt record cleared by `clear_halt`.
fn cleared() -> State {
    State::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(REASON_HALT_CLEARED.to_owned()),
    }
}

/// The release `web` runs before the update that halts.
const BASE: &str = "1.9.0";

/// `web` installed at [`BASE`], with 2.0.0 and 2.1.0 published to order next.
fn installed(farm: &NativeHost) -> Daemon {
    for version in [BASE, "2.0.0", "2.1.0"] {
        farm.publish("web", version);
    }
    let daemon = farm.daemon();
    assert_eq!(daemon.order(&farm.install("web", BASE, 1)), succeeded());
    daemon
}

/// `web` halted by its hook (an update to 2.0.0 whose `prepare` exits 76), held under halt
/// generation `persisted` (no file for 0), with a package 2.1.0 to order next.
fn halted(farm: &NativeHost, persisted: u64) -> Daemon {
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
    if persisted > 0 {
        HaltStore::new(&rig.state)
            .raise(&name("web"), persisted)
            .expect("held");
    }
    daemon
}

/// The bytes of a file of `web`'s state directory, `None` when it is absent.
fn bytes(rig: &Rig, file: &str) -> Option<Vec<u8>> {
    fs::read(rig.state.join("web").join(file)).ok()
}

/// A coordinator's `clear_halt` of `service` for `generation`.
fn clear_order(service: &str, generation: u64, order_try: u64) -> Order {
    Order {
        order_id: format!("clear-{service}"),
        r#try: order_try,
        verb: Some(order::Verb::ClearHalt(ClearHalt {
            service: service.to_owned(),
            halt_generation: generation,
        })),
    }
}

/// Catches a comparison of `>=`, a generation written on a refusal, a record not cleared, or a
/// held-halt file that outlives the halt: each row its own rig; a lifting row removes the file,
/// turns the halt record into the `halt_cleared` deferral and lets the next order run; a refused
/// row is
/// `stale_halt_generation` and leaves the held-halt file and the record byte for byte as they
/// were.
#[test]
fn clear_halt_lifts_only_a_strictly_greater_generation() {
    let rows: [(u64, u64, bool); 5] = [
        (0, 1, true),
        (3, 4, true),
        (3, 3, false),
        (3, 2, false),
        (0, 0, false),
    ];
    for (persisted, asked, lifts) in rows {
        let farm = NativeHost::new(&format!("clear-row-{persisted}-{asked}"));
        let rig = farm.rig();
        let daemon = halted(&farm, persisted);
        let (halt_before, record_before) = (bytes(rig, HELD_HALT_FILE), bytes(rig, RECORD_FILE));
        let answer = daemon.clear_halt(&name("web"), asked);
        let row = format!("held {persisted}, asked {asked}");
        match lifts {
            true => {
                let lifted = Lifted {
                    generation: asked,
                    cleared: true,
                };
                assert_eq!(answer, Ok(lifted), "{row}");
                assert_eq!(bytes(rig, HELD_HALT_FILE), None, "{row}");
                assert_eq!(
                    rig.record("web").expect("the record").state,
                    cleared(),
                    "{row}"
                );
                assert_eq!(
                    daemon.order(&farm.update("web", "2.1.0", 3)),
                    succeeded(),
                    "{row}"
                );
            }
            false => {
                let detail = format!(
                    "web is held under halt generation {persisted}; {asked} does not lift it"
                );
                let refused = Refused::new(Refusal::StaleHaltGeneration, detail);
                assert_eq!(answer, Err(refused), "{row}");
                assert_eq!(bytes(rig, HELD_HALT_FILE), halt_before, "{row}");
                assert_eq!(bytes(rig, RECORD_FILE), record_before, "{row}");
                let held = daemon.order(&farm.update("web", "2.1.0", 3));
                assert_eq!(held.refusal(), Some(Refusal::Halted), "{row}");
            }
        }
    }
}

/// Catches a generation kept only in memory: a halt held under 3, the daemon is dropped, and a
/// new one on the same directories still refuses 3 and lifts 4, which removes the file.
#[test]
fn the_generation_a_halt_is_held_under_survives_a_restart() {
    let farm = NativeHost::new("clear-restart");
    let rig = farm.rig();
    drop(halted(&farm, 3));
    let daemon = farm.daemon();
    let refused = daemon.clear_halt(&name("web"), 3).expect_err("3 is held");
    assert_eq!(refused.refusal, Refusal::StaleHaltGeneration);
    assert_eq!(bytes(rig, HELD_HALT_FILE), Some(b"3\n".to_vec()));
    let lifted = daemon.clear_halt(&name("web"), 4).expect("4 lifts");
    assert_eq!(lifted.generation, 4);
    assert_eq!(bytes(rig, HELD_HALT_FILE), None);
}

/// Catches the two writes in the other order: a daemon that dies once the record's write is
/// durable (the record store's crash gate) and before the held-halt file is removed keeps the
/// file and no halt, so the same `clear_halt` sent to the next daemon removes it; with the file
/// removed first, the halt would stand held under no generation.
#[test]
fn a_crash_between_clearing_the_record_and_writing_the_generation_is_lifted_by_the_resend() {
    let farm = NativeHost::new("clear-crash");
    let rig = farm.rig();
    drop(halted(&farm, 3));
    let dies = farm
        .daemon()
        .with_stores(Arc::new(|_: &ServiceName, store: RecordStore| {
            let after_a_write = |point: CrashPoint| matches!(point, CrashPoint::AfterWrite(_));
            store.with_crash_gate(Arc::new(after_a_write))
        }));
    let died = dies
        .clear_halt(&name("web"), 4)
        .expect_err("the daemon died");
    assert_eq!(died.refusal, Refusal::StoreFailed);
    assert_eq!(bytes(rig, HELD_HALT_FILE), Some(b"3\n".to_vec()));
    drop(dies);

    let daemon = farm.daemon();
    assert_eq!(rig.record("web").expect("the record").state, cleared());
    let lifted = daemon
        .clear_halt(&name("web"), 4)
        .expect("the resend lifts");
    let expected = Lifted {
        generation: 4,
        cleared: false,
    };
    assert_eq!(lifted, expected);
    assert_eq!(bytes(rig, HELD_HALT_FILE), None);
    assert_eq!(rig.record("web").expect("the record").state, cleared());
    assert_eq!(daemon.order(&farm.update("web", "2.1.0", 3)), succeeded());
}

/// Catches `clear_halt` that ignores the service's lock: while an order of the service runs a
/// step, it is `busy` and nothing is written.
#[test]
fn clear_halt_on_a_service_with_a_running_step_is_busy() {
    let farm = NativeHost::new("clear-busy");
    let rig = farm.rig();
    farm.publish("web", BASE);
    farm.publish("web", "2.0.0");
    let daemon = Arc::new(farm.daemon());
    assert_eq!(daemon.order(&farm.install("web", BASE, 1)), succeeded());
    let gate = rig.launcher.hold("web", Step::Prepare);
    let update = farm.update("web", "2.0.0", 2);
    let order = {
        let daemon = Arc::clone(&daemon);
        spawn("the order", move || daemon.order(&update))
    };
    gate.entered(&order).expect("the held step is entered");
    let record_before = bytes(rig, RECORD_FILE);
    let refused = daemon.clear_halt(&name("web"), 1).expect_err("busy");
    let detail = "web is locked by an order to update it to 2.0.0";
    assert_eq!(refused, Refused::new(Refusal::Busy, detail));
    assert_eq!(bytes(rig, HELD_HALT_FILE), None);
    assert_eq!(bytes(rig, RECORD_FILE), record_before);
    gate.release();
    assert_eq!(order.join().expect("the order"), succeeded());
}

/// Catches a held-halt file that is not a number read as 0: `halt_unreadable`, naming the file's
/// text, and neither the file nor the halt record changes.
#[test]
fn clear_halt_with_a_garbage_held_halt_file_names_the_error_and_lifts_nothing() {
    let farm = NativeHost::new("clear-garbage");
    let rig = farm.rig();
    let daemon = halted(&farm, 0);
    fs::write(rig.state.join("web").join(HELD_HALT_FILE), "seven\n").expect("garbage");
    let record_before = bytes(rig, RECORD_FILE);
    let refused = daemon.clear_halt(&name("web"), 9).expect_err("garbage");
    let detail = "the held halt generation of web is not a number: \"seven\\n\"";
    assert_eq!(refused, Refused::new(Refusal::HaltUnreadable, detail));
    assert_eq!(bytes(rig, HELD_HALT_FILE), Some(b"seven\n".to_vec()));
    assert_eq!(bytes(rig, RECORD_FILE), record_before);
}

/// Catches a generation written for a service that holds no halt: with no record and no
/// held-halt file, generation 2 is accepted with nothing cleared, and no file is written.
#[test]
fn clear_halt_without_a_halt_record_writes_no_generation() {
    let rig = Rig::new("clear-no-halt");
    rig.add("web", "2.0.0");
    let daemon = rig.daemon();
    let lifted = daemon.clear_halt(&name("web"), 2).expect("lifted");
    let expected = Lifted {
        generation: 2,
        cleared: false,
    };
    assert_eq!(lifted, expected);
    assert_eq!(bytes(&rig, HELD_HALT_FILE), None);
    assert_eq!(bytes(&rig, RECORD_FILE), None);
}

/// Catches a record that cannot be cleared lifting the halt anyway: a record of a newer schema
/// is `record_newer`, one that does not read is `record_unreadable`, and one that cannot be read
/// at all (a directory in its place) is `store_failed`; none writes the generation.
#[test]
fn clear_halt_of_a_record_that_cannot_be_cleared_names_why_and_writes_nothing() {
    let rows: [(&str, Refusal); 3] = [
        ("newer", Refusal::RecordNewer),
        ("corrupt", Refusal::RecordUnreadable),
        ("directory", Refusal::StoreFailed),
    ];
    for (kind, refusal) in rows {
        let rig = Rig::new(&format!("clear-record-{kind}"));
        let daemon = rig.daemon();
        let dir = rig.state.join("web");
        fs::create_dir_all(&dir).expect("the service's directory");
        let record = dir.join(RECORD_FILE);
        match kind {
            "newer" => fs::write(&record, r#"{"schema": 9}"#).expect("a newer record"),
            "corrupt" => fs::write(&record, "{").expect("a corrupt record"),
            _ => fs::create_dir(&record).expect("a directory in the record's place"),
        }
        let refused = daemon.clear_halt(&name("web"), 1).expect_err(kind);
        assert_eq!(refused.refusal, refusal, "{kind}: {refused:?}");
        assert_eq!(bytes(&rig, HELD_HALT_FILE), None, "{kind}");
    }
}

/// Catches the coordinator's order not reaching `clear_halt`: the order lifts the halt and is
/// answered with the record's `halt_cleared`; on a halt held under 1, the same generation is
/// `stale_halt_generation`.
#[test]
fn a_clear_halt_order_lifts_the_halt_and_a_stale_one_is_refused() {
    let farm = NativeHost::new("clear-order");
    let daemon = halted(&farm, 0);
    // Its own host, kept for the whole test: a host dropped removes its directories, and with
    // them the held-halt file the stale order is judged against.
    let held_farm = NativeHost::new("clear-order-held");
    let held = halted(&held_farm, 1);
    let lifted = Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: None,
        },
        reason: Some(REASON_HALT_CLEARED.to_owned()),
        resent: false,
    };
    assert_eq!(daemon.order(&clear_order("web", 1, 1)), lifted);
    let stale = held.order(&clear_order("web", 1, 2));
    assert_eq!(
        stale.refusal(),
        Some(Refusal::StaleHaltGeneration),
        "{stale:?}"
    );
}

/// Catches a halt generation from the heartbeat that leaves the halt record holding every
/// order: the held-halt adapter of the heartbeat loop holds an unheld halt under the first
/// generation it hears, then lifts it with a greater one as `clear_halt` does (record cleared,
/// file removed), so the next order runs; a lower generation is not an error.
#[test]
fn a_halt_generation_from_the_heartbeat_lifts_the_halt_record() {
    let farm = NativeHost::new("clear-heartbeat");
    let rig = farm.rig();
    let daemon = Arc::new(halted(&farm, 0));
    let halts = DaemonHalts::new(Arc::clone(&daemon));
    halts.heard(&name("web"), None, 2).expect("held under 2");
    assert_eq!(bytes(rig, HELD_HALT_FILE), Some(b"2\n".to_vec()));
    halts
        .heard(&name("web"), None, 1)
        .expect("a lower generation is no error");
    halts.heard(&name("web"), None, 3).expect("lifted by 3");
    assert_eq!(rig.record("web").expect("the record").state, cleared());
    assert_eq!(bytes(rig, HELD_HALT_FILE), None);
    assert_eq!(daemon.order(&farm.update("web", "2.1.0", 3)), succeeded());
}

/// Catches a `busy` that does not say what holds the service: the lock of a `clear_halt` and of
/// a `clear_record` name themselves in a refusal's text.
#[test]
fn the_locks_of_clear_halt_and_clear_record_name_themselves() {
    assert_eq!(Holder::ClearHalt.to_string(), "a `clear_halt`");
    assert_eq!(Holder::ClearRecord.to_string(), "a `clear_record`");
}
