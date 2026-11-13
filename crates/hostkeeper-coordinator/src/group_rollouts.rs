//! `Rollouts.Advance`, `Pause` and `Resume` once the coordinator holds a host group
//! (`docs/design/hostgroups.md`, section 4): the plan they act on is a (group, package)'s, kept in
//! the group's marks ([`crate::host_groups::GroupMarks::rollout`]).
//!
//! A call names its group in `host_group`. `Pause` with none pauses the plan of every group that
//! lists the package, in one change, and skips a group already paused (the incident's stop is
//! not refused for one plan that stood already; `already_paused` is the answer when none was
//! left to pause). `Advance` and `Resume` with none act on the only group that lists the package
//! (`ambiguous_host_group` when more do). A group that does not exist, or does not list the
//! package, or a package no group lists, is `not_in_host_group`.
//!
//! Each plan paused, resumed or advanced is one audit entry naming its group. The answer holds
//! the first entry, and the plan of its group after the change.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::{Pause, View, Wait, entered, kept};
use crate::fleet::Fleet;
use crate::group_reach::Reached;
use crate::group_scope::group_services;
use crate::holds::marks_of;
use crate::operator::{entry_proto, valid_reason};
use crate::ops::Write;
use crate::reconcile::entry;
use crate::service::Coordinator;
use crate::stages::reach;

type Refusal = proto::RolloutsRefusal;

impl Coordinator {
    /// The plan of the package of `one` in its group as it stands now, for an answer; none when
    /// the group has no desired version of it.
    fn group_plan(&self, one: &Reached, fleet: &Fleet) -> Option<proto::RolloutPlan> {
        let ops = self.ops.state();
        let services = group_services(
            &one.name,
            &one.listed,
            &self.desired.host_groups(),
            &self.services,
            &ops,
            &[],
        );
        let want = self.desired.group_desired(&one.name, &one.def.name);
        let all = self.desired.group_desireds(&one.name);
        self.plan_in(
            &one.def,
            &services,
            want.as_ref(),
            &all,
            &ops,
            fleet,
            (self.clock)(),
        )
    }

    /// `Rollouts.Advance` for a coordinator that holds a host group.
    ///
    /// # Errors
    /// As [`Self::rollout_advance`], and `ambiguous_host_group`, `not_in_host_group`.
    pub(super) fn advance_in_groups(
        &self,
        by: &str,
        request: proto::AdvanceRequest,
    ) -> Result<proto::AdvanceResponse, Refusal> {
        let service = self
            .package_def(&request.service)
            .ok_or(Refusal::UnknownService)?
            .name
            .clone();
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        // The fleet's lock first, as for a plan of the rollout file.
        let mut fleet = self.lock_fleet();
        let groups = self.desired.host_groups();
        let state = self.ops.state();
        let one = self.reach_one(&groups, &state, &service, &request.host_group)?;
        let want = self
            .desired
            .group_desired(&one.name, &service)
            .ok_or(Refusal::NoDesired)?;
        let now = (self.clock)();
        let group = Some(&one.name);
        let marks = state.marks_in(group, &service);
        let kept_now = state.rollout_in(group, &service);
        let view = View::effective(&one.def, &want, kept_now.as_ref(), &fleet, now);
        let stopped = marks.halt.is_some() || marks.bad.contains(&want.version);
        let waiting = view.wait(&one.def, &want, &fleet, now) == Some(Wait::ManualAdvance);
        let ready = !stopped && !fleet.is_quiet(now) && waiting;
        let view = ready
            .then(|| reach(&one.def, &want, view, true, &fleet, now))
            .flatten()
            .ok_or(Refusal::NotWaiting)?;
        let record = kept(&view);
        let mut audit = entry(
            by,
            "rollout_advance",
            &service,
            format!("{}: {why}", entered(&service, &view)),
        );
        audit.host_group = Some(one.name.to_string());
        let done = self.ops.try_change(Write::BeforeMemory, audit, |ops| {
            ops.set_rollout_in(group, &service, record);
            Ok::<(), Refusal>(())
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::AdvanceResponse {
            entry: Some(entry_proto(&done)),
            plan: self.group_plan(&one, &fleet),
        })
    }

    /// `Rollouts.Pause` for a coordinator that holds a host group.
    ///
    /// # Errors
    /// As [`Self::rollout_pause`], and `not_in_host_group`.
    pub(super) fn pause_in_groups(
        &self,
        by: &str,
        request: proto::PauseRequest,
    ) -> Result<proto::PauseResponse, Refusal> {
        let service = self
            .package_def(&request.service)
            .ok_or(Refusal::UnknownService)?
            .name
            .clone();
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let mut fleet = self.lock_fleet();
        let groups = self.desired.host_groups();
        let state = self.ops.state();
        let reached = self.reach(&groups, &state, &service, &request.host_group)?;
        let pause = Pause {
            by: by.to_owned(),
            reason: why.clone(),
        };
        let done = self.ops.decide_changes(Write::AfterMemory, |ops| {
            let mut entries = Vec::new();
            for one in &reached {
                let group = Some(&one.name);
                let mut rollout = ops.rollout_in(group, &service).unwrap_or_default();
                if rollout.paused.is_some() {
                    continue;
                }
                rollout.paused = Some(pause.clone());
                ops.set_rollout_in(group, &service, rollout);
                let mut audit = entry(by, "rollout_pause", &service, why.clone());
                audit.host_group = Some(one.name.to_string());
                entries.push(audit);
            }
            if entries.is_empty() {
                return Err(Refusal::AlreadyPaused);
            }
            Ok(entries)
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        let first = done.first().and_then(|first| {
            reached
                .iter()
                .find(|one| first.host_group.as_deref() == Some(one.name.as_str()))
        });
        Ok(proto::PauseResponse {
            entry: done.first().map(entry_proto),
            plan: first.and_then(|one| self.group_plan(one, &fleet)),
        })
    }

    /// `Rollouts.Resume` for a coordinator that holds a host group.
    ///
    /// # Errors
    /// As [`Self::rollout_resume`], and `ambiguous_host_group`, `not_in_host_group`.
    pub(super) fn resume_in_groups(
        &self,
        by: &str,
        request: proto::ResumeRequest,
    ) -> Result<proto::ResumeResponse, Refusal> {
        let service = self
            .package_def(&request.service)
            .ok_or(Refusal::UnknownService)?
            .name
            .clone();
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let mut fleet = self.lock_fleet();
        let groups = self.desired.host_groups();
        let state = self.ops.state();
        let one = self.reach_one(&groups, &state, &service, &request.host_group)?;
        let group = Some(&one.name);
        let mut audit = entry(by, "rollout_resume", &service, why);
        audit.host_group = Some(one.name.to_string());
        let done = self.ops.try_change(Write::BeforeMemory, audit, |ops| {
            let mut rollout = ops
                .rollout_in(group, &service)
                .filter(|rollout| rollout.paused.is_some())
                .ok_or(Refusal::NotPaused)?;
            rollout.paused = None;
            ops.set_rollout_in(group, &service, rollout);
            Ok::<(), Refusal>(())
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::ResumeResponse {
            entry: Some(entry_proto(&done)),
            plan: self.group_plan(&one, &fleet),
        })
    }
}
