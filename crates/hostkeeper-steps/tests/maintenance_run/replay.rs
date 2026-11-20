//! A crash at every write of a turn, before the write touched the disk and after it was durable,
//! resumed by a new sequencer from the record on disk: the turn ends as an uncrashed one does,
//! each step launched once, in order, but for the one step the crash cut short, which is launched
//! again at once.
//!
//! The sequencer here is the least one: it reads the record, asks the machine, writes the record
//! before each launch and after each end, and gives the boot id after a start while a reboot is
//! under way. A reboot is the sequencer stopping where it waits; the host then comes back on a new
//! boot.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_steps::CrashPoint;
use hostkeeper_steps::maintenance::{
    Action, Ending, Event, Kind, Launch, MaintenanceLoaded, MaintenanceRecord,
    MaintenanceRecordError, MaintenanceStore, Outcome, next_action,
};

use crate::support::{Scratch, boot, defer, on, service};

/// The host: its boot and what was launched on it.
struct Host {
    boot: u32,
    launched: Vec<Launch>,
}

/// How each step of a scenario ends; a step of a kind that reboots records its reboot and does
/// not end (the host goes down under it).
struct Scenario {
    kinds: &'static [Kind],
    services: &'static [&'static str],
    reboots: &'static [Kind],
    outcome: fn(&Launch) -> Outcome,
}

fn all_ok(_: &Launch) -> Outcome {
    Outcome::Ok
}

fn tierstore_past_its_deadline(launch: &Launch) -> Outcome {
    match launch {
        Launch::Drain(service) if service.as_str() == "tierstore" => {
            defer("the drain deadline passed")
        }
        _ => Outcome::Ok,
    }
}

fn check_fails(launch: &Launch) -> Outcome {
    match launch {
        Launch::Check => Outcome::Failed("exit 1".to_owned()),
        _ => Outcome::Ok,
    }
}

const UPDATE: Scenario = Scenario {
    kinds: &[Kind::Linux, Kind::Reboot],
    services: &["js-agent", "tierstore"],
    reboots: &[Kind::Reboot],
    outcome: all_ok,
};

const DEFERRAL: Scenario = Scenario {
    kinds: &[Kind::Linux],
    services: &["js-agent", "tierstore", "web"],
    reboots: &[],
    outcome: tierstore_past_its_deadline,
};

const HALT: Scenario = Scenario {
    kinds: &[Kind::Linux],
    services: &["js-agent", "tierstore"],
    reboots: &[],
    outcome: check_fails,
};

/// One run of the sequencer: to the turn's end (`Some`), to a wait for a reboot (`None`), or to
/// the store's crash.
fn sequencer(
    store: &MaintenanceStore,
    scenario: &Scenario,
    host: &mut Host,
) -> Result<Option<Ending>, MaintenanceRecordError> {
    if store.load()? == MaintenanceLoaded::Absent {
        // The order is taken again: the coordinator sends it until the host reports the turn.
        let names = scenario.services.iter().map(|name| service(name)).collect();
        let new = MaintenanceRecord::new("run-1", "order-1", 1, scenario.kinds, names)
            .expect("a valid record");
        store.save(&new)?;
    }
    loop {
        let MaintenanceLoaded::Current(rec) = store.load()? else {
            panic!("the record is neither absent nor current");
        };
        match next_action(&rec) {
            Action::End(ending) => return Ok(Some(ending)),
            Action::Wait(_) => {
                let (after, action) = on(&rec, Event::BootIdRead(boot(host.boot)));
                if let Action::Wait(_) = action {
                    return Ok(None);
                }
                store.save(&after)?;
            }
            Action::Launch(_) => {
                let (started, action) = on(&rec, Event::Started);
                store.save(&started)?;
                let Action::Launch(launch) = action else {
                    panic!("Started gave {action:?}");
                };
                host.launched.push(launch.clone());
                if let Launch::Apply(kind) = &launch
                    && scenario.reboots.contains(kind)
                {
                    let (rebooting, _) = on(&started, Event::RebootUnderWay(boot(host.boot)));
                    store.save(&rebooting)?;
                    return Ok(None);
                }
                let (ended, _) = on(&started, Event::Ended((scenario.outcome)(&launch)));
                store.save(&ended)?;
            }
        }
    }
}

/// Runs the sequencer on `store` until the turn ends, rebooting the host at each wait.
fn to_the_end(
    store: &MaintenanceStore,
    scenario: &Scenario,
    host: &mut Host,
) -> Result<Ending, MaintenanceRecordError> {
    for _ in 0..8 {
        match sequencer(store, scenario, host)? {
            Some(ending) => return Ok(ending),
            None => host.boot += 1,
        }
    }
    panic!("the turn did not end in 8 boots");
}

fn final_record(store: &MaintenanceStore) -> MaintenanceRecord {
    match store.load().expect("load") {
        MaintenanceLoaded::Current(rec) => *rec,
        other => panic!("no record at the end: {other:?}"),
    }
}

/// The scenario run with no crash: its ending, its last record, its launches and how many
/// writes it made.
fn uncrashed(scenario: &Scenario) -> (Ending, MaintenanceRecord, Vec<Launch>, u64) {
    let dir = Scratch::new("mu-replay-clean");
    let writes = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&writes);
    let store = MaintenanceStore::new(dir.path()).with_crash_gate(Arc::new(move |point| {
        if let CrashPoint::AfterWrite(_) = point {
            counted.fetch_add(1, Ordering::SeqCst);
        }
        false
    }));
    let mut host = Host {
        boot: 1,
        launched: Vec::new(),
    };
    let ending = to_the_end(&store, scenario, &mut host).expect("no crash");
    let rec = final_record(&store);
    (ending, rec, host.launched, writes.load(Ordering::SeqCst))
}

fn replay_every_crash_point(name: &str, scenario: &Scenario) {
    let (ending, last, launched, writes) = uncrashed(scenario);
    assert!(writes > 10, "{name}: {writes} writes");
    let points = (1..=writes).flat_map(|n| [CrashPoint::BeforeWrite(n), CrashPoint::AfterWrite(n)]);
    for point in points {
        let dir = Scratch::new("mu-replay");
        let mut host = Host {
            boot: 1,
            launched: Vec::new(),
        };
        let gated =
            MaintenanceStore::new(dir.path()).with_crash_gate(Arc::new(move |at| at == point));
        let crashed = to_the_end(&gated, scenario, &mut host);
        assert!(
            matches!(crashed, Err(MaintenanceRecordError::Crashed(at)) if at == point),
            "{name} {point:?}: {crashed:?}"
        );
        let resumed = MaintenanceStore::new(dir.path());
        let ended = to_the_end(&resumed, scenario, &mut host).expect("no crash on resume");

        assert_eq!(ended, ending, "{name} {point:?}");
        assert_eq!(final_record(&resumed), last, "{name} {point:?}");
        let mut once = host.launched.clone();
        once.dedup();
        assert_eq!(once, launched, "{name} {point:?}: {:?}", host.launched);
        assert!(
            host.launched.len() <= launched.len() + 1,
            "{name} {point:?}: {:?}",
            host.launched
        );
    }
}

/// Catches a crash, at any write of an update with a reboot, that resumes at the wrong step:
/// a step skipped (an apply on a host not drained, a service never put back), a finished step
/// run again, or a reboot waited for that already happened.
#[test]
fn a_crash_at_every_write_of_an_update_with_a_reboot_resumes_where_it_was() {
    let (ending, _, launched, _) = uncrashed(&UPDATE);
    assert_eq!(ending, Ending::Done);
    assert_eq!(
        launched,
        [
            Launch::Stage,
            Launch::Drain(service("js-agent")),
            Launch::Drain(service("tierstore")),
            Launch::Apply(Kind::Linux),
            Launch::Apply(Kind::Reboot),
            Launch::Check,
            Launch::Resume(service("tierstore")),
            Launch::Resume(service("js-agent")),
        ]
    );
    replay_every_crash_point("update", &UPDATE);
}

/// Catches a crash, at any write of a deferred turn, that loses the deferral (and so applies on
/// a host whose drain did not end) or leaves a drained service out of the put back.
#[test]
fn a_crash_at_every_write_of_a_deferral_resumes_its_put_back() {
    let (ending, _, launched, _) = uncrashed(&DEFERRAL);
    assert_eq!(
        ending,
        Ending::Deferred("drain tierstore: the drain deadline passed".to_owned())
    );
    assert_eq!(
        launched,
        [
            Launch::Stage,
            Launch::Drain(service("js-agent")),
            Launch::Drain(service("tierstore")),
            Launch::Resume(service("tierstore")),
            Launch::Resume(service("js-agent")),
        ]
    );
    replay_every_crash_point("deferral", &DEFERRAL);
}

/// Catches a crash, at any write of a turn whose check fails, that loses the halt, so that the
/// resumed turn puts the services back or runs anything after the failed check: the turn ends
/// halted with both services still drained, and no `resume` is ever launched.
#[test]
fn a_crash_at_every_write_of_a_halted_turn_keeps_it_halted() {
    let (ending, last, launched, _) = uncrashed(&HALT);
    assert_eq!(ending, Ending::Halted("check: exit 1".to_owned()));
    assert_eq!(last.drained, [service("js-agent"), service("tierstore")]);
    assert_eq!(
        launched,
        [
            Launch::Stage,
            Launch::Drain(service("js-agent")),
            Launch::Drain(service("tierstore")),
            Launch::Apply(Kind::Linux),
            Launch::Check,
        ]
    );
    replay_every_crash_point("halt", &HALT);
}
