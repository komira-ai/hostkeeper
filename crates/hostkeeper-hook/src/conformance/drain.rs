//! Check 9 of the harness, for a hook with `drain` and `resume` steps
//! ([`Conformance::with_drain`]): each run twice ends the same and changes nothing the second
//! time; `resume` with no `drain` before it succeeds and changes nothing; `drain` then `resume`
//! puts the service back; and a `drain` whose deadline has passed while work is in flight exits
//! `75`, cuts no work and leaves the service resumed.
//!
//! The deadline is a time the harness writes into the params file, never a wait: it is
//! one second after the Unix epoch, a time the host's clock read long ago, so the deadline has passed
//! whenever the hook reads its clock. The work is what the fake service puts in the world in
//! [`FakeService::start_work`]; the harness reads it as the paths that call added or changed.
//!
//! [`FakeService::start_work`]: super::FakeService::start_work

use std::fs;

use super::{Conformance, Violation, changes, require};
use crate::{DRAIN_DEADLINE_KEY, Outcome, RUN_ID_KEY, SERVICE_KEY, Scalar, Step};

/// The name of the params file of `drain` and `resume`, beside the update's.
pub(super) const DRAIN_PARAMS: &str = "drain.toml";

/// The service the harness names in the params file of `drain` and `resume`.
const SERVICE: &str = "conformance";

/// The maintenance run the harness names in the params file of `drain` and `resume`.
const RUN_ID: &str = "conformance-run";

/// The deadline of a `drain` that has passed: one second after the Unix epoch.
const PASSED_DEADLINE: i64 = 1;

/// The scenario of the passed deadline, as [`super::Summary`] lists it.
const DEADLINE_SCENARIO: &str = "drain=deadline";

/// The scenario of a `drain` and a `resume` with no work in flight.
const DRAIN_SCENARIO: &str = "drain,resume";

impl Conformance<'_> {
    /// The params file of `drain` and `resume`: the fake's own table, the service and the run,
    /// and the deadline if there is one.
    fn write_drain_params(
        &self,
        sandbox: &super::Sandbox,
        deadline: Option<i64>,
    ) -> Result<(), Violation> {
        let mut params = self.fake.params();
        params.insert(SERVICE_KEY, Scalar::Text(SERVICE.to_owned()));
        params.insert(RUN_ID_KEY, Scalar::Text(RUN_ID.to_owned()));
        if let Some(secs) = deadline {
            params.insert(DRAIN_DEADLINE_KEY, Scalar::Integer(secs));
        }
        fs::write(sandbox.control().join(DRAIN_PARAMS), params.to_toml())?;
        Ok(())
    }

    /// Check 9: `resume` alone, `drain` and `resume` each twice, and, if the fake can put work
    /// in flight, a `drain` past its deadline. Returns the scenarios run and those the fake
    /// cannot make.
    pub fn drain_resume(&mut self) -> Result<(Vec<String>, Vec<String>), Violation> {
        self.resume_without_drain()?;
        self.drain_then_resume()?;
        let mut ran = vec![DRAIN_SCENARIO.to_owned()];
        let mut skipped = Vec::new();
        if self.drain_past_deadline()? {
            ran.push(DEADLINE_SCENARIO.to_owned());
        } else {
            skipped.push(DEADLINE_SCENARIO.to_owned());
        }
        Ok((ran, skipped))
    }

    /// `resume` with no `drain` before it succeeds and changes neither the world nor the
    /// record: the updater runs it for every service it may have drained, and a service it
    /// never drained is left as it was.
    pub fn resume_without_drain(&mut self) -> Result<(), Violation> {
        let sandbox = self.fresh()?;
        self.write_drain_params(&sandbox, None)?;
        let start = Self::state(&sandbox)?;
        self.exec_ok(&sandbox, Step::Resume)?;
        let moved = changes(&start, &Self::state(&sandbox)?);
        if !moved.is_empty() {
            return Err(Violation::ResumeWithoutDrainChanged { changes: moved });
        }
        Ok(())
    }

    /// `drain` twice, then `resume` twice, with no work in flight and no deadline: each second
    /// run ends as the first and changes nothing, and after the `resume` the world is as it
    /// was before the `drain`.
    pub fn drain_then_resume(&mut self) -> Result<(), Violation> {
        let sandbox = self.fresh()?;
        self.write_drain_params(&sandbox, None)?;
        let start = Self::world(&sandbox)?;
        self.twice(&sandbox, Step::Drain)?;
        self.twice(&sandbox, Step::Resume)?;
        let moved = changes(&start, &Self::world(&sandbox)?);
        if !moved.is_empty() {
            return Err(Violation::ResumeLeftDrained { changes: moved });
        }
        Ok(())
    }

    /// With work in flight that does not end and a deadline that has passed, `drain` exits
    /// `75`, leaves the work as it was and the service resumed (the world as before it), and
    /// run again ends the same and changes nothing; a `resume` after it succeeds and changes
    /// nothing in the world. `Ok(false)` if the fake cannot put work in flight.
    pub fn drain_past_deadline(&mut self) -> Result<bool, Violation> {
        let sandbox = self.fresh()?;
        let idle = Self::world(&sandbox)?;
        if !self.fake.start_work(&sandbox)? {
            return Ok(false);
        }
        let busy = Self::world(&sandbox)?;
        self.write_drain_params(&sandbox, Some(PASSED_DEADLINE))?;
        let first = self.exec(&sandbox, Step::Drain)?;
        let after = Self::world(&sandbox)?;
        // The work: what `start_work` added or changed. Each piece must be as it was.
        let cut: Vec<String> = busy
            .iter()
            .filter(|(path, entry)| idle.get(*path) != Some(*entry))
            .filter(|(path, entry)| after.get(*path) != Some(*entry))
            .map(|(path, _)| path.display().to_string())
            .collect();
        if !cut.is_empty() {
            return Err(Violation::DrainCutWork { changes: cut });
        }
        if first.outcome == Outcome::Success {
            return Err(Violation::DrainMissedDeadline);
        }
        require(Step::Drain, Outcome::Deferred, first.outcome)?;
        let left = changes(&busy, &after);
        if !left.is_empty() {
            return Err(Violation::DeferredDrainNotResumed { changes: left });
        }
        self.again(&sandbox, Step::Drain, first)?;
        self.exec_ok(&sandbox, Step::Resume)?;
        let moved = changes(&busy, &Self::world(&sandbox)?);
        if !moved.is_empty() {
            return Err(Violation::ResumeWithoutDrainChanged { changes: moved });
        }
        Ok(true)
    }
}
