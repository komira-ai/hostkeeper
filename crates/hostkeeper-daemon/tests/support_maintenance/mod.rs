//! The fakes of the maintenance hook's tests: a fake kind per kind, a run record in memory, a
//! rebooter and a boot id the test sets, all writing what they are asked to one shared log, so that
//! a test sees the order of the effects; and the report lines a step ends with.
#![allow(dead_code, reason = "each test binary uses part of the shared support")]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::maintenance_hook::{
    Cutover, HookPorts, HookStep, Kind, KindError, KindState, MaintainParams, MaintenanceKind,
    REASON_REBOOT_UNDER_WAY, RebootError, Rebooter, RecordError, Registry, RunRecord, run_step,
};
use hostkeeper_hook::{Class, Outcome, Report};
use hostkeeper_proto::hostkeeper::agent::v1::{
    Maintain, MaintainAction, MaintenanceKind as WireKind,
};
use hostkeeper_steps::{BootId, BootIdError, BootIdSource};

/// The boot before a reboot.
pub const BOOT_A: &str = "0f6b1c3e-5a2d-4c8e-9b7f-1d2e3f4a5b6c";
/// The boot after it.
pub const BOOT_B: &str = "7c1e2d3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f";
/// The run of the tests.
pub const RUN: &str = "run-7";

/// What the fakes were asked, in order.
pub type Effects = Arc<Mutex<Vec<String>>>;

pub fn push(effects: &Effects, effect: String) {
    effects.lock().expect("effects").push(effect);
}

pub fn taken(effects: &Effects) -> Vec<String> {
    std::mem::take(&mut *effects.lock().expect("effects"))
}

/// A kind whose steps answer what the test set, each logging `<kind> <step>`.
#[derive(Clone)]
pub struct FakeKind {
    pub kind: Kind,
    pub effects: Effects,
    pub prepare: Result<(), KindError>,
    pub cutover: Result<Cutover, KindError>,
    pub after_reboot: Result<String, KindError>,
    pub verify: Result<(), KindError>,
}

impl FakeKind {
    /// A kind whose every step succeeds, applying `version` without a reboot.
    pub fn applying(kind: Kind, version: &str, effects: &Effects) -> FakeKind {
        FakeKind {
            kind,
            effects: effects.clone(),
            prepare: Ok(()),
            cutover: Ok(Cutover::Applied {
                version: version.to_owned(),
            }),
            after_reboot: Ok(version.to_owned()),
            verify: Ok(()),
        }
    }

    /// A kind that installs and needs a reboot, the host then running `version` of it.
    pub fn rebooting(kind: Kind, version: &str, effects: &Effects) -> FakeKind {
        FakeKind {
            cutover: Ok(Cutover::NeedsReboot),
            ..FakeKind::applying(kind, version, effects)
        }
    }

    pub fn log(&self, step: &str) {
        push(&self.effects, format!("{} {step}", self.kind));
    }
}

impl MaintenanceKind for FakeKind {
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        assert_eq!(params.run_id(), RUN);
        self.log("prepare");
        self.prepare.clone()
    }

    fn cutover(&self, _params: &MaintainParams) -> Result<Cutover, KindError> {
        self.log("cutover");
        self.cutover.clone()
    }

    fn after_reboot(&self, _params: &MaintainParams) -> Result<String, KindError> {
        self.log("after_reboot");
        self.after_reboot.clone()
    }

    fn verify(&self, _params: &MaintainParams) -> Result<(), KindError> {
        self.log("verify");
        self.verify.clone()
    }
}

/// A run record in memory: each kind's state, every write logged as
/// `record <run> <kind> <what>`; `fail` fails every read and write.
#[derive(Default)]
pub struct FakeRecord {
    pub effects: Effects,
    pub states: Mutex<BTreeMap<Kind, KindState>>,
    pub fail: Option<RecordError>,
    /// How many more writes succeed; `None` for no bound. A write past it fails.
    pub writes_left: Mutex<Option<usize>>,
}

impl FakeRecord {
    pub fn new(effects: &Effects) -> FakeRecord {
        FakeRecord {
            effects: effects.clone(),
            ..FakeRecord::default()
        }
    }

    pub fn set(&self, kind: Kind, state: KindState) {
        self.states.lock().expect("states").insert(kind, state);
    }

    pub fn checked(&self) -> Result<(), RecordError> {
        self.fail.clone().map_or(Ok(()), Err)
    }

    /// Lets `writes` more writes succeed, and fails every write after them.
    pub fn allow_writes(&self, writes: usize) {
        *self.writes_left.lock().expect("writes") = Some(writes);
    }

    fn writable(&self) -> Result<(), RecordError> {
        self.checked()?;
        let mut left = self.writes_left.lock().expect("writes");
        match *left {
            Some(0) => Err(RecordError::Failed("the disk is full".to_owned())),
            Some(more) => {
                *left = Some(more - 1);
                Ok(())
            }
            None => Ok(()),
        }
    }
}

impl RunRecord for FakeRecord {
    fn state(&self, run_id: &str, kind: Kind) -> Result<KindState, RecordError> {
        assert_eq!(run_id, RUN);
        self.checked()?;
        let states = self.states.lock().expect("states");
        Ok(states.get(&kind).cloned().unwrap_or(KindState::Pending))
    }

    fn reboot_under_way(&self, run_id: &str, kind: Kind, boot: &BootId) -> Result<(), RecordError> {
        self.writable()?;
        push(
            &self.effects,
            format!("record {run_id} {kind} reboot_under_way {}", boot.as_str()),
        );
        self.set(kind, KindState::RebootUnderWay { boot: boot.clone() });
        Ok(())
    }

    fn applied(&self, run_id: &str, kind: Kind, version: &str) -> Result<(), RecordError> {
        self.writable()?;
        push(
            &self.effects,
            format!("record {run_id} {kind} applied {version}"),
        );
        let state = match self.state(run_id, kind)? {
            KindState::RebootUnderWay { boot } => KindState::AppliedAfterReboot { asked_in: boot },
            _ => KindState::Applied,
        };
        self.set(kind, state);
        Ok(())
    }
}

/// A rebooter that logs `reboot`, and fails when told to.
pub struct FakeRebooter {
    pub effects: Effects,
    pub fail: bool,
}

impl Rebooter for FakeRebooter {
    fn reboot(&self) -> Result<(), RebootError> {
        push(&self.effects, "reboot".to_owned());
        match self.fail {
            true => Err(RebootError::Failed("shutdown exited 1".to_owned())),
            false => Ok(()),
        }
    }
}

/// A boot id the test sets; empty text is a boot id that cannot be read.
pub struct FakeBootId(pub Mutex<String>);

impl FakeBootId {
    pub fn set(&self, id: &str) {
        *self.0.lock().expect("boot id") = id.to_owned();
    }
}

impl BootIdSource for FakeBootId {
    fn read(&self) -> Result<BootId, BootIdError> {
        BootId::uuid("the test's boot id", &self.0.lock().expect("boot id"))
    }
}

/// A host of fakes: the registry the test builds, a record, a rebooter and the boot id, all
/// logging to `effects`.
pub struct Host {
    pub effects: Effects,
    pub kinds: Registry,
    pub record: FakeRecord,
    pub rebooter: FakeRebooter,
    pub boot: FakeBootId,
}

impl Host {
    pub fn new(effects: &Effects, kinds: Vec<FakeKind>) -> Host {
        let kinds = kinds.into_iter().fold(Registry::new(), |registry, kind| {
            registry.with(kind.kind, Box::new(kind))
        });
        Host {
            effects: effects.clone(),
            kinds,
            record: FakeRecord::new(effects),
            rebooter: FakeRebooter {
                effects: effects.clone(),
                fail: false,
            },
            boot: FakeBootId(Mutex::new(BOOT_A.to_owned())),
        }
    }

    pub fn ports(&self) -> HookPorts<'_> {
        HookPorts {
            kinds: &self.kinds,
            record: &self.record,
            rebooter: &self.rebooter,
            boot_id: &self.boot,
        }
    }

    pub fn run(&self, step: HookStep, kinds: &[WireKind]) -> Ending {
        let ending = run_step(step, &params(kinds), &self.ports());
        Ending {
            line: ending.report().to_line(),
            outcome: ending.outcome,
        }
    }

    pub fn effects(&self) -> Vec<String> {
        taken(&self.effects)
    }
}

/// A step's outcome and its report line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ending {
    pub outcome: Outcome,
    pub line: String,
}

/// The ending of a step: `outcome`, with the report of `class`, `reason` and `detail`.
pub fn ending(
    outcome: Outcome,
    class: Class,
    reason: Option<&str>,
    detail: Option<&str>,
) -> Ending {
    Ending {
        outcome,
        line: Report {
            class,
            reason: reason.map(str::to_owned),
            detail: detail.map(str::to_owned),
            ..Report::default()
        }
        .to_line(),
    }
}

pub fn done() -> Ending {
    ending(Outcome::Success, Class::Release, None, None)
}

pub fn failed(reason: &str, detail: &str) -> Ending {
    ending(Outcome::Failure, Class::Release, Some(reason), Some(detail))
}

pub fn rebooting(kind: &str) -> Ending {
    ending(
        Outcome::Success,
        Class::Release,
        Some(REASON_REBOOT_UNDER_WAY),
        Some(&format!("{kind} needs a reboot: rebooting")),
    )
}

/// A `maintain` of the run [`RUN`] with `kinds`, in the order given.
pub fn maintain(kinds: &[WireKind]) -> Maintain {
    Maintain {
        run_id: RUN.to_owned(),
        action: MaintainAction::Run.into(),
        kinds: kinds.iter().map(|kind| i32::from(*kind)).collect(),
        ..Maintain::default()
    }
}

pub fn params(kinds: &[WireKind]) -> MaintainParams {
    MaintainParams::new(maintain(kinds)).expect("valid parameters")
}
