//! What the maintenance tests share: names, boot ids, records, and one step of a turn.

use std::path::Path;

use hostkeeper_config::names::ServiceName;
use hostkeeper_steps::BootId;
use hostkeeper_steps::maintenance::{
    Action, Event, Kind, Launch, MaintenanceRecord, Outcome, transition,
};

/// A directory under the target directory, unique to the test, removed when it is dropped.
pub struct Scratch(hostkeeper_testkit::Scratch);

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        Scratch(
            hostkeeper_testkit::Scratch::under(base, name).expect("create the scratch directory"),
        )
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

pub fn service(name: &str) -> ServiceName {
    ServiceName::new(name).expect("a service name")
}

pub fn boot(n: u32) -> BootId {
    BootId::new("a test", &format!("boot-{n}")).expect("a boot id")
}

/// A new turn of `run-1`, order `order-1`, try 1.
pub fn record(kinds: &[Kind], services: &[&str]) -> MaintenanceRecord {
    MaintenanceRecord::new(
        "run-1",
        "order-1",
        1,
        kinds,
        services.iter().map(|name| service(name)).collect(),
    )
    .expect("a valid record")
}

/// `event` on `rec`, which must fit.
pub fn on(rec: &MaintenanceRecord, event: Event) -> (MaintenanceRecord, Action) {
    transition(rec, event).expect("the event fits the record")
}

/// Launches the step `rec` stands at and ends it with `outcome`: the step launched, the record
/// after it and the next action.
pub fn run_step(rec: &MaintenanceRecord, outcome: Outcome) -> (Launch, MaintenanceRecord, Action) {
    let (started, action) = on(rec, Event::Started);
    let Action::Launch(launch) = action else {
        panic!("Started gave {action:?}, not a launch");
    };
    let (ended, next) = on(&started, Event::Ended(outcome));
    (launch, ended, next)
}

/// Runs every step `rec` stands at with `Ok` until the record stands at `at`, which must come.
pub fn ok_until(mut rec: MaintenanceRecord, at: &Action) -> MaintenanceRecord {
    for _ in 0..32 {
        let (_, after, next) = run_step(&rec, Outcome::Ok);
        rec = after;
        if next == *at {
            return rec;
        }
    }
    panic!("the turn never stood at {at:?}");
}

pub fn fail(detail: &str) -> Outcome {
    Outcome::Failed(detail.to_owned())
}

pub fn defer(detail: &str) -> Outcome {
    Outcome::Deferred(detail.to_owned())
}

pub fn drain(name: &str) -> Action {
    Action::Launch(Launch::Drain(service(name)))
}

pub fn resume(name: &str) -> Action {
    Action::Launch(Launch::Resume(service(name)))
}

pub fn apply(kind: Kind) -> Action {
    Action::Launch(Launch::Apply(kind))
}
