//! Where each service's update plan stands, and what moves it (`docs/design/rollout-update-plans.md`):
//! the coordinator entering a stage by itself ([`advance`]), and the operator's
//! `Rollouts.Advance`, `Rollouts.Pause` and `Rollouts.Resume`.
//!
//! The position of a plan ([`Rollout`]) is kept per service in the ops store and written in the
//! halts file beside the ops file ([`crate::ops_file`]), whose reader skips a key it does not
//! know, so the release before still reads both files. It is the desired version it is for, how
//! many stages of the rollout file the plan has entered, the hosts recorded done when it entered
//! the last of them, and the operator's pause. A position for another version than the desired
//! one counts for nothing but its pause: a new desired version starts the plan over
//! ([`crate::stages::View::of`]).
//!
//! The coordinator writes a position only when the plan enters a stage, with one audit entry
//! (`rollout_advance`); a plan that never goes past its first stage writes nothing. An
//! operator's call is one audit entry under the token's name: `rollout_advance`,
//! `rollout_pause` (written memory first: a pause raises protection and stands without its
//! audit line), `rollout_resume`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::PoisonError;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::version::Version;
use serde::{Deserialize, Serialize};

use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::holds::marks_of;
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{OpsError, OpsState, OpsStore, Write};
use crate::reconcile::{COORDINATOR, entry};
use crate::service::Coordinator;
use crate::services::ServiceDef;
use crate::stages::{Advance, HostState, PlanStage, View, Wait, elsewhere, first_entered, reach};

/// Where the update plan of a service stands, as the ops store keeps it. Every field is left out
/// when it holds nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollout {
    /// The desired version the position is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<Version>,
    /// How many stages of the rollout file the plan has entered.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub entered: u32,
    /// The hosts, by node id, that had rolled when the plan entered its last stage.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub done: BTreeSet<String>,
    /// The operator's pause, if one stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<Pause>,
}

/// An operator's pause of a plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    /// The name of the operator's token.
    pub by: String,
    /// Why.
    pub reason: String,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Rollout {
    /// How many stages of `def`'s plan this position has entered for the desired `version`: its
    /// own count when it is for that version, else the count of a plan with none kept.
    #[must_use]
    pub fn entered_for(&self, def: &crate::stages::Plan, version: &Version) -> usize {
        if self.version.as_ref() == Some(version) {
            self.entered as usize
        } else {
            first_entered(def)
        }
    }

    /// Whether it holds nothing: no entry is kept for it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The position `view` holds, to be kept.
fn kept(view: &View) -> Rollout {
    Rollout {
        version: Some(view.version.clone()),
        entered: u32::try_from(view.entered).unwrap_or(u32::MAX),
        done: view.done.clone(),
        paused: view.paused.clone(),
    }
}

/// The reason of the audit entry of entering the stage `view` is in.
fn entered(service: &str, view: &View) -> String {
    format!(
        "{service} {}: stage {} of {} ({} hosts)",
        view.version,
        view.current,
        view.stages.len(),
        view.size()
    )
}

/// Enters, for the service `def` at its desired version `want`, every stage the plan may enter by
/// itself now, after a heartbeat and under the fleet's lock: one write of the position, with one
/// audit entry by the coordinator. Nothing is entered while the service is halted, its version is
/// marked bad, the plan is paused, or the coordinator is quiet. A write that fails is logged, and
/// the next heartbeat tries again; no host of the next stage is ordered until it lands.
pub(crate) fn advance(def: &ServiceDef, want: &Desired, ops: &OpsStore, fleet: &mut Fleet, now: Duration) {
    let marks = ops.marks(&def.name);
    if marks.halt.is_some() || marks.bad.contains(&want.version) || fleet.is_quiet(now) {
        return;
    }
    let kept_now = ops.read(|state| state.rollouts.get(&def.name).cloned());
    let view = View::of(def, want, kept_now.as_ref(), fleet, now);
    if view.paused.is_some() {
        return;
    }
    let Some(view) = reach(def, want, view, false, fleet, now) else {
        return;
    };
    let record = kept(&view);
    let audit = entry(COORDINATOR, "rollout_advance", &def.name, entered(&def.name, &view));
    match ops.change(Write::BeforeMemory, audit, |state| {
        state.rollouts.insert(def.name.clone(), record);
    }) {
        Ok(_) => fleet.set_marks(ops.read(marks_of)),
        Err(why) => {
            tracing::error!(service = %def.name, %why, "the plan's next stage was not entered");
        }
    }
}

impl From<crate::operator::Denied> for proto::RolloutsRefusal {
    fn from(denied: crate::operator::Denied) -> Self {
        match denied {
            crate::operator::Denied::Unauthorized => Self::Unauthorized,
            crate::operator::Denied::Forbidden => Self::Forbidden,
        }
    }
}

impl From<OpsError> for proto::RolloutsRefusal {
    fn from(why: OpsError) -> Self {
        tracing::error!(%why, "an operator's change could not be written: nothing changed");
        match why {
            OpsError::AuditUnavailable(_) => Self::AuditUnavailable,
            _ => Self::StoreFailed,
        }
    }
}

impl Coordinator {
    /// The same coordinator letting `n` services have a live rollout at once (`--live-rollouts`;
    /// 1 by default): a service whose rollout is not live is told `rollout_elsewhere` while `n`
    /// others are ([`crate::stages`]).
    #[must_use]
    pub fn with_live_rollouts(mut self, n: usize) -> Self {
        self.fleet
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .set_live_rollouts(n);
        self
    }

    /// The same coordinator ordering nothing for `quiet` after its start instead of
    /// [`crate::fleet::QUIET_START`] (`--quiet-secs`, whose floor [`crate::config::build`]
    /// checks).
    #[must_use]
    pub fn with_quiet(mut self, quiet: Duration) -> Self {
        self.fleet
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .set_quiet(quiet);
        self
    }

    /// The plan of `def` for its desired state `want` (none without one) at `now`, as the status
    /// answers it, over every service's desired state `all` and the ops state `ops`.
    pub(crate) fn plan_in(
        &self,
        def: &ServiceDef,
        want: Option<&Desired>,
        all: &BTreeMap<String, Desired>,
        ops: &OpsState,
        fleet: &Fleet,
        now: Duration,
    ) -> Option<proto::RolloutPlan> {
        let want = want?;
        let halted = ops
            .services
            .get(&def.name)
            .is_some_and(|marks| marks.halt.is_some() || marks.bad.contains(&want.version));
        Some(plan_proto(&self.services, all, def, want, halted, fleet, now))
    }

    /// The plan of `def` as it stands now, for an answer; none when the service has no desired
    /// version.
    fn plan_now(&self, def: &ServiceDef, fleet: &Fleet) -> Option<proto::RolloutPlan> {
        let want = self.desired.get(&def.name);
        let all = self.desired.all();
        let ops = self.ops.state();
        self.plan_in(def, want.as_ref(), &all, &ops, fleet, (self.clock)())
    }

    /// `Rollouts.Advance`: enters the next stage of a plan that waits for an operator, and every
    /// stage after it the plan may then enter by itself.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the reason),
    /// `no_desired`, `not_waiting` (the plan is paused, halted, quiet, or does not wait for an
    /// Advance), `store_failed`, `audit_unavailable`.
    pub fn rollout_advance(
        &self,
        caller: &Caller,
        request: proto::AdvanceRequest,
    ) -> Result<proto::AdvanceResponse, proto::RolloutsRefusal> {
        let by = caller.operator()?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::RolloutsRefusal::UnknownService)?;
        let why = valid_reason(&request.reason).ok_or(proto::RolloutsRefusal::InvalidRequest)?;
        let want = self
            .desired
            .get(&def.name)
            .ok_or(proto::RolloutsRefusal::NoDesired)?;
        let mut fleet = self.lock_fleet();
        let now = (self.clock)();
        let marks = self.ops.marks(&def.name);
        let kept_now = self.ops.read(|state| state.rollouts.get(&def.name).cloned());
        let view = View::effective(def, &want, kept_now.as_ref(), &fleet, now);
        let stopped = marks.halt.is_some() || marks.bad.contains(&want.version);
        let waiting = view.wait(def, &want, &fleet, now) == Some(Wait::ManualAdvance);
        if stopped || fleet.is_quiet(now) || !waiting {
            return Err(proto::RolloutsRefusal::NotWaiting);
        }
        let view = reach(def, &want, view, true, &fleet, now)
            .ok_or(proto::RolloutsRefusal::NotWaiting)?;
        let record = kept(&view);
        let audit = entry(
            by,
            "rollout_advance",
            &def.name,
            format!("{}: {why}", entered(&def.name, &view)),
        );
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            state.rollouts.insert(def.name.clone(), record);
            Ok::<(), proto::RolloutsRefusal>(())
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::AdvanceResponse {
            entry: Some(entry_proto(&done)),
            plan: self.plan_now(def, &fleet),
        })
    }

    /// `Rollouts.Pause`: pauses the plan of a service, whatever its desired version: no host
    /// that has not started is ordered until `Rollouts.Resume`. Written memory first: it stands
    /// even when its audit line cannot be appended.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the reason),
    /// `already_paused`, `store_failed`.
    pub fn rollout_pause(
        &self,
        caller: &Caller,
        request: proto::PauseRequest,
    ) -> Result<proto::PauseResponse, proto::RolloutsRefusal> {
        let by = caller.operator()?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::RolloutsRefusal::UnknownService)?;
        let why = valid_reason(&request.reason).ok_or(proto::RolloutsRefusal::InvalidRequest)?;
        let mut fleet = self.lock_fleet();
        let audit = entry(by, "rollout_pause", &def.name, why.clone());
        let pause = Pause {
            by: by.to_owned(),
            reason: why,
        };
        let done = self.ops.decide_change(Write::AfterMemory, |state| {
            let rollout = state.rollouts.entry(def.name.clone()).or_default();
            if rollout.paused.is_some() {
                return Err(proto::RolloutsRefusal::AlreadyPaused);
            }
            rollout.paused = Some(pause);
            Ok(audit)
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::PauseResponse {
            entry: Some(entry_proto(&done)),
            plan: self.plan_now(def, &fleet),
        })
    }

    /// `Rollouts.Resume`: lifts the pause of a service's plan, which goes on from where it
    /// stands.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the reason),
    /// `not_paused`, `store_failed`, `audit_unavailable`.
    pub fn rollout_resume(
        &self,
        caller: &Caller,
        request: proto::ResumeRequest,
    ) -> Result<proto::ResumeResponse, proto::RolloutsRefusal> {
        let by = caller.operator()?;
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::RolloutsRefusal::UnknownService)?;
        let why = valid_reason(&request.reason).ok_or(proto::RolloutsRefusal::InvalidRequest)?;
        let mut fleet = self.lock_fleet();
        let audit = entry(by, "rollout_resume", &def.name, why);
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let rollout = state
                .rollouts
                .get_mut(&def.name)
                .filter(|rollout| rollout.paused.is_some())
                .ok_or(proto::RolloutsRefusal::NotPaused)?;
            rollout.paused = None;
            if rollout.is_empty() {
                state.rollouts.remove(&def.name);
            }
            Ok::<(), proto::RolloutsRefusal>(())
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::ResumeResponse {
            entry: Some(entry_proto(&done)),
            plan: self.plan_now(def, &fleet),
        })
    }
}

/// The plan of `def` for its desired state `want` at `now` as `hostkeeper.operator.v1` says it,
/// with why it waits: a halt or a bad version (`halted`), the pause, the quiet start and the other
/// services' rollouts first, then what the plan holds ([`View::wait`]).
pub(crate) fn plan_proto(
    services: &crate::services::Services,
    all: &BTreeMap<String, Desired>,
    def: &ServiceDef,
    want: &Desired,
    halted: bool,
    fleet: &Fleet,
    now: Duration,
) -> proto::RolloutPlan {
    let view = View::effective(def, want, fleet.rollout_of(&def.name), fleet, now);
    let waiting = if view.finished() {
        None
    } else if halted {
        Some(Wait::Halted)
    } else if view.paused.is_some() {
        Some(Wait::Paused)
    } else if fleet.is_quiet(now) {
        Some(Wait::Quiet)
    } else if elsewhere(services, all, &def.name, fleet, now) {
        Some(Wait::RolloutElsewhere)
    } else {
        view.wait(def, want, fleet, now)
    };
    let stages = view
        .stages
        .iter()
        .enumerate()
        .map(|(i, stage)| stage_proto(i + 1, stage, &view))
        .collect();
    proto::RolloutPlan {
        version: view.version.as_str().to_owned(),
        stages,
        current: u32::try_from(view.current).unwrap_or(u32::MAX),
        eligible: u32::try_from(view.members.len()).unwrap_or(u32::MAX),
        waiting: wait_proto(waiting).into(),
        paused: view.paused.is_some(),
        paused_by: view.paused.as_ref().map(|p| p.by.clone()),
        pause_reason: view.paused.as_ref().map(|p| p.reason.clone()),
        finished: view.finished(),
    }
}

fn stage_proto(number: usize, stage: &PlanStage, view: &View) -> proto::PlanStage {
    proto::PlanStage {
        number: u32::try_from(number).unwrap_or(u32::MAX),
        percent: stage.percent,
        hosts: u32::try_from(stage.hosts).unwrap_or(u32::MAX),
        advance: match stage.advance {
            Advance::Auto => proto::StageAdvance::Auto,
            Advance::Manual => proto::StageAdvance::Manual,
        }
        .into(),
        members: view
            .members
            .iter()
            .filter(|m| m.stage == number)
            .map(|m| proto::PlanHost {
                node_id: m.id.to_string(),
                role: m.role.clone(),
                state: match m.state {
                    HostState::Waiting => proto::PlanHostState::Waiting,
                    HostState::Updating => proto::PlanHostState::Updating,
                    HostState::Baking => proto::PlanHostState::Baking,
                    HostState::Done => proto::PlanHostState::Done,
                    HostState::Failed => proto::PlanHostState::Failed,
                }
                .into(),
                deferring: m.deferring,
            })
            .collect(),
    }
}

fn wait_proto(wait: Option<Wait>) -> proto::PlanWait {
    match wait {
        None => proto::PlanWait::None,
        Some(Wait::Halted) => proto::PlanWait::Halted,
        Some(Wait::Paused) => proto::PlanWait::Paused,
        Some(Wait::Quiet) => proto::PlanWait::Quiet,
        Some(Wait::RolloutElsewhere) => proto::PlanWait::RolloutElsewhere,
        Some(Wait::ManualAdvance) => proto::PlanWait::ManualAdvance,
        Some(Wait::FailedHost) => proto::PlanWait::FailedHost,
        Some(Wait::BelowMinUp) => proto::PlanWait::BelowMinUp,
        Some(Wait::Updating) => proto::PlanWait::Updating,
        Some(Wait::Deferring) => proto::PlanWait::Deferring,
        Some(Wait::Baking) => proto::PlanWait::Baking,
    }
}
