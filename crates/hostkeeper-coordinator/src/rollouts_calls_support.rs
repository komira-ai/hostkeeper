//! Builders for the tests of the operator's calls on an update plan
//! ([`crate::rollouts_calls_tests`], [`crate::rollouts_refusal_tests`]): a farm of eight daemons of
//! the service `web` on a real coordinator over scratch files, the three calls as the operator
//! makes them, and what a test compares before and after a call.

use std::collections::BTreeMap;
use std::fs;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::wire::{Heartbeat, ServiceReply};
use serde_json::Value;

use crate::assign_support::why_of;
use crate::decide::{Gate, Why};
use crate::marks_support::{NEW_DAEMON, WEB, ask, beat_of, operator, runs, set, start};
use crate::operator::Caller;
use crate::ops::{AuditEntry, OpsState, OpsStore};
use crate::rollouts::Rollout;
use crate::testutil::{Rig, ops_of};

/// The role of every host of these tests.
pub(crate) const DAEMON: &str = "daemon";
/// How many hosts the farm has: `d1` to `d8`.
pub(crate) const HOSTS: usize = 8;
/// A reason an operator gives.
pub(crate) const REASON: &str = "stage looks healthy";

/// 25 percent (2 hosts of 8), then 50 percent (4) entered by an operator, then all 8.
pub(crate) const STAGES: &str =
    "stages = [{ percent = 25 }, { percent = 50, advance = \"manual\" }, 100]";
/// 50 percent (4 hosts of 8) entered by an operator, then all 8.
pub(crate) const MANUAL_FIRST: &str = "stages = [{ percent = 50, advance = \"manual\" }, 100]";

/// The files a refused call must leave as they were, by name.
pub(crate) const FILES: [&str; 4] = [
    "desired.json",
    "desired.json.ops",
    "desired.json.ops.halts",
    "desired.json.ops.audit",
];

// ---- the farm ---------------------------------------------------------------------------

/// The eight daemons as they run: each reports the version it runs, and runs the version it was
/// last told to update to from its next heartbeat on.
pub(crate) struct Farm {
    pub(crate) running: BTreeMap<String, String>,
}

impl Farm {
    pub(crate) fn new() -> Self {
        Self {
            running: (1..=HOSTS)
                .map(|n| (format!("d{n}"), "1.0".to_owned()))
                .collect(),
        }
    }

    /// One heartbeat of `host` at the version it runs; the reply.
    pub(crate) fn beat(&mut self, rig: &Rig, host: &str) -> ServiceReply {
        let running = self.running[host].clone();
        let reply = ask(rig, host, DAEMON, beat_at(&running));
        if let ServiceReply::Update { version, .. } = &reply {
            self.running.insert(host.to_owned(), version.to_string());
        }
        reply
    }

    /// One round: every host beats, in node-id order. The hosts that were ordered an update.
    pub(crate) fn round(&mut self, rig: &Rig) -> Vec<String> {
        let hosts: Vec<String> = self.running.keys().cloned().collect();
        hosts
            .into_iter()
            .filter(|host| matches!(self.beat(rig, host), ServiceReply::Update { .. }))
            .collect()
    }

    /// Rounds until a round orders no host and every host runs `version`, at most `limit`.
    pub(crate) fn finish(&mut self, rig: &Rig, version: &str, limit: usize) {
        for _ in 0..limit {
            self.round(rig);
            if self.running.values().all(|running| running == version) {
                // One more, so that the last host is heard at the version.
                self.round(rig);
                return;
            }
        }
        panic!("the farm did not reach {version}: {:?}", self.running);
    }
}

/// The heartbeat of a daemon that runs `web` at `running`.
pub(crate) fn beat_at(running: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, running)])
}

/// A rig of the plan `extra` whose coordinator keeps its ops file through `ops`, a farm that has
/// been heard at 1.0, and 2.0 set as desired.
pub(crate) async fn farm_over(
    test: &str,
    extra: &str,
    ops: impl FnOnce(&Scratch) -> OpsStore,
) -> (Rig, Farm) {
    let mut rig = Rig::new(test);
    let store = ops(&rig.dir);
    rig.coordinator = start(&rig.dir, &rig.clock, extra).with_ops(store);
    rig.at(2_000);
    set(&rig, WEB, "1.0", 1).await;
    let mut farm = Farm::new();
    assert_eq!(farm.round(&rig), Vec::<String>::new(), "up to date");
    set(&rig, WEB, "2.0", 2).await;
    (rig, farm)
}

/// [`farm_over`] the files of the rig's own directory.
pub(crate) async fn farm(test: &str, extra: &str) -> (Rig, Farm) {
    farm_over(test, extra, ops_of).await
}

/// The farm of [`STAGES`] at the moment its first stage is done and the second waits for an
/// operator: `d1` and `d2` run 2.0, and no other host is ordered.
pub(crate) async fn waiting(test: &str) -> (Rig, Farm) {
    let (rig, mut farm) = farm(test, STAGES).await;
    assert_eq!(farm.round(&rig), ["d1", "d2"], "the first stage");
    assert_eq!(farm.round(&rig), Vec::<String>::new(), "d1 and d2 baked");
    assert_eq!(why_of(&rig, WEB, "d3"), Some(Why::Gate(Gate::AdvanceWait)));
    (rig, farm)
}

// ---- the calls ----------------------------------------------------------------------------

pub(crate) fn advance_as(
    rig: &Rig,
    caller: &Caller,
    service: &str,
    reason: &str,
) -> Result<proto::AdvanceResponse, proto::RolloutsRefusal> {
    rig.coordinator.rollout_advance(
        caller,
        proto::AdvanceRequest {
            service: service.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

pub(crate) fn pause_as(
    rig: &Rig,
    caller: &Caller,
    service: &str,
    reason: &str,
) -> Result<proto::PauseResponse, proto::RolloutsRefusal> {
    rig.coordinator.rollout_pause(
        caller,
        proto::PauseRequest {
            service: service.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

pub(crate) fn resume_as(
    rig: &Rig,
    caller: &Caller,
    service: &str,
    reason: &str,
) -> Result<proto::ResumeResponse, proto::RolloutsRefusal> {
    rig.coordinator.rollout_resume(
        caller,
        proto::ResumeRequest {
            service: service.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// `Rollouts.Advance` of `web` by the operator with [`REASON`].
pub(crate) fn advance(rig: &Rig) -> Result<proto::AdvanceResponse, proto::RolloutsRefusal> {
    advance_as(rig, &operator(), WEB, REASON)
}

/// `Rollouts.Pause` of `web` by the operator with [`REASON`].
pub(crate) fn pause(rig: &Rig) -> Result<proto::PauseResponse, proto::RolloutsRefusal> {
    pause_as(rig, &operator(), WEB, REASON)
}

/// `Rollouts.Resume` of `web` by the operator with [`REASON`].
pub(crate) fn resume(rig: &Rig) -> Result<proto::ResumeResponse, proto::RolloutsRefusal> {
    resume_as(rig, &operator(), WEB, REASON)
}

// ---- what a test compares -----------------------------------------------------------------

/// The bytes of [`FILES`], `None` for a file that is not there, and the ops state in memory.
pub(crate) fn snapshot(rig: &Rig) -> (Vec<Option<Vec<u8>>>, OpsState) {
    let files = FILES
        .iter()
        .map(|name| fs::read(rig.dir.path().join(name)).ok())
        .collect();
    (files, rig.coordinator.ops().state())
}

/// The position the ops store keeps for `web`.
pub(crate) fn kept(rig: &Rig) -> Option<Rollout> {
    rig.coordinator
        .ops()
        .read(|state| state.rollouts.get(WEB).cloned())
}

/// The ring of the ops state, oldest first.
pub(crate) fn ring(rig: &Rig) -> Vec<AuditEntry> {
    rig.coordinator.ops().state().audit
}

/// The sequence number of the newest entry of the ring: 0 while it is empty.
pub(crate) fn last_seq(rig: &Rig) -> u64 {
    ring(rig).last().map_or(0, |entry| entry.seq)
}

/// The newest entry of the ring.
pub(crate) fn newest(rig: &Rig) -> AuditEntry {
    ring(rig).pop().expect("an entry")
}

/// The entry an operator's call answers: by the token `operator`, of `web`, under `action`.
pub(crate) fn entry_of(seq: u64, action: &str, reason: &str) -> proto::AuditEntry {
    proto::AuditEntry {
        seq,
        by: proto::Actor::Operator.into(),
        action: action.to_owned(),
        service: Some(WEB.to_owned()),
        node_id: None,
        reason: Some(reason.to_owned()),
        at_unix: None,
        by_name: Some("operator".to_owned()),
    }
}

/// The hosts of stage `number` of `plan` and their states, in the order the plan lists them.
pub(crate) fn stage_hosts(
    plan: &proto::RolloutPlan,
    number: u32,
) -> Vec<(String, proto::PlanHostState)> {
    plan.stages
        .iter()
        .find(|stage| stage.number == number)
        .expect("the stage")
        .members
        .iter()
        .map(|m| (m.node_id.clone(), m.state()))
        .collect()
}

/// `(node id, state)` of the hosts `ids` in `state`.
pub(crate) fn all_in(
    ids: &[&str],
    state: proto::PlanHostState,
) -> Vec<(String, proto::PlanHostState)> {
    ids.iter().map(|id| ((*id).to_owned(), state)).collect()
}

/// The size of each stage of `plan`.
pub(crate) fn sizes(plan: &proto::RolloutPlan) -> Vec<u32> {
    plan.stages.iter().map(|stage| stage.hosts).collect()
}

/// The halts file of the rig as JSON, `Null` if there is none.
pub(crate) fn halts_json(rig: &Rig) -> Value {
    fs::read(rig.dir.path().join("desired.json.ops.halts")).map_or(Value::Null, |bytes| {
        serde_json::from_slice(&bytes).expect("JSON")
    })
}
