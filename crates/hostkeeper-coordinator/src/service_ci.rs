//! The calls of [`Coordinator`] that act for a CI token already checked: the transports check
//! the token first ([`Coordinator::ci`] spends it, so a token is checked once) and then call
//! these with what the check returned.

use hostkeeper_oidc::Verified;

use super::{ConvergenceError, Coordinator, Set, SetError, kept};
use crate::converge::Convergence;
use crate::desired::{Desired, DesiredError};
use crate::force::end_by_ci;
use crate::halt;
use crate::status::convergence_of;

impl Coordinator {
    /// Sets the desired state from `body` for the CI token `verified`, as
    /// [`Self::set_desired_noting`] does once the token is checked: everything that function
    /// says of what is refused and what is changed holds here.
    ///
    /// # Errors
    /// [`SetError`], other than the refusals of the token itself.
    pub(crate) fn set_desired_verified(
        &self,
        verified: &Verified,
        body: &[u8],
    ) -> Result<Set, SetError> {
        let (service, desired, dropped_roles) =
            Desired::parse_request_noting(body, &self.services, self.legacy.as_deref())?;
        // An allow-list entry that names no services stands for the legacy service.
        let admitted = match &verified.services {
            Some(names) => names.contains(&service),
            None => self.legacy.as_deref() == Some(service.as_str()),
        };
        if !admitted {
            tracing::warn!(
                workflow = %verified.job_workflow_ref,
                %service,
                "refused a desired state: the allow-list entry does not name the service"
            );
            return Err(SetError::Forbidden);
        }
        // The fleet's lock is held to the end of the set: a heartbeat plans a rollback and
        // writes it under that lock, so it cannot write a rollback from the state it read over
        // the version CI stores meanwhile, and the refusals below are decided on the marks and
        // the registry the set is made over. The desired state goes first and the lift of an
        // updater's halt after it: a lift that fails leaves the service halted (the safe side),
        // and the reverse would order the halted target again.
        {
            let _fleet = self.lock_fleet_for_set();
            let def = self
                .services
                .get(&service)
                .ok_or(SetError::Invalid(DesiredError::UnknownService))?;
            self.refuse(def, &desired)?;
            let marks = self.ops.marks(&service);
            self.desired
                .record(&service, desired.clone(), |v| kept(&marks, v))?;
            // CI's set of another version ends a force that stands ([`crate::force`]), before
            // the lift: a lift that fails then leaves no force ordering the old version.
            end_by_ci(
                &self.ops,
                &service,
                &desired.version,
                &verified.job_workflow_ref,
            )?;
            halt::lift_by_ci(
                &self.ops,
                &service,
                &desired.version,
                &verified.job_workflow_ref,
            )?;
        }
        tracing::info!(
            workflow = %verified.job_workflow_ref,
            sha = ?verified.sha,
            %service,
            "desired state set by CI"
        );
        Ok(Set {
            desired,
            dropped_roles,
        })
    }

    /// Where the hosts of `service` stand against its desired state, as
    /// [`Self::convergence`] answers once the CI token is checked.
    ///
    /// # Errors
    /// [`ConvergenceError::UnknownService`]: the coordinator manages no such service.
    pub(crate) fn convergence_verified(
        &self,
        service: &str,
    ) -> Result<Convergence, ConvergenceError> {
        let def = self
            .services
            .get(service)
            .ok_or(ConvergenceError::UnknownService)?;
        let now = (self.clock)();
        let fleet = self.lock_fleet();
        let desired = self.desired.get(service);
        let ops = self.ops.state();
        Ok(convergence_of(def, desired.as_ref(), &fleet, &ops, now))
    }
}
