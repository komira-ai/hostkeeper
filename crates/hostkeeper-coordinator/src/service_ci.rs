//! The calls of [`Coordinator`] that act for a caller already checked: the transports check
//! the token first and then call these with what the check returned. A CI token is checked
//! by [`Coordinator::ci`], which spends it, so a token is checked once; an operator's token
//! is resolved to its name by [`Coordinator::caller`] and spends nothing. CI and an operator
//! both set the desired state, and only CI reads the convergence here. They differ in what may
//! be set (CI: the services its allow-list entry names; an operator: any service the coordinator
//! manages) and in who the audit names.

use hostkeeper_oidc::Verified;

use super::{ConvergenceError, Coordinator, Set, SetError, kept};
use crate::converge::Convergence;
use crate::desired::{Desired, DesiredError};
use crate::force::end_by_setter;
use crate::halt::lift_by_setter;
use crate::ops::{AuditEntry, Write};
use crate::status::convergence_of;

/// Who sets the desired state.
enum Setter<'a> {
    /// CI, by this verified token.
    Ci(&'a Verified),
    /// The operator token of this name.
    Operator(&'a str),
}

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
        self.set_desired_by(&Setter::Ci(verified), body)
    }

    /// Sets the desired state from `body` for the operator token named `operator`. It is
    /// [`Self::set_desired_verified`] with no allow-list: the operator may set any service the
    /// coordinator manages. The refusals are the same and in the same order, and nothing changes
    /// when one is made. Once the state is recorded, one audit entry `set_desired` by `operator`
    /// is written (the service, and as its reason the version, followed by `with package digest
    /// <digest>` when the body named one); then a force of another version ends and a halt the
    /// updater raised is lifted, both audited by `operator`.
    ///
    /// # Errors
    /// [`SetError`], other than the refusals of a token. [`SetError::Ops`] means the desired
    /// state is set and the audit entry, the end of a force or the lift of a halt could not be
    /// written; setting the same state again is safe and tries them again.
    pub(crate) fn set_desired_operator(
        &self,
        operator: &str,
        body: &[u8],
    ) -> Result<Set, SetError> {
        self.set_desired_by(&Setter::Operator(operator), body)
    }

    /// The set both callers share: parse, admit the service, then under the fleet's lock refuse,
    /// record, audit (an operator's) and end a force and lift a halt.
    fn set_desired_by(&self, setter: &Setter<'_>, body: &[u8]) -> Result<Set, SetError> {
        let (service, desired, dropped_roles) =
            Desired::parse_request_noting(body, &self.services, self.legacy.as_deref())?;
        if let Setter::Ci(verified) = setter {
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
        }
        let (actor, set_by) = match setter {
            Setter::Ci(verified) => (crate::reconcile::CI, verified.job_workflow_ref.clone()),
            Setter::Operator(name) => (*name, format!("operator token {name}")),
        };
        // The fleet's lock is held to the end of the set: a heartbeat plans a rollback and
        // writes it under that lock, so it cannot write a rollback from the state it read over
        // the version stored meanwhile, and the refusals below are decided on the marks and
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
            if let Setter::Operator(name) = setter {
                let mut entry = AuditEntry::new(name, "set_desired");
                entry.service = Some(service.clone());
                entry.reason = Some(match &desired.release_digest {
                    Some(digest) => format!("{} with package digest {digest}", desired.version),
                    None => desired.version.to_string(),
                });
                self.ops.change(Write::BeforeMemory, entry, |_| {})?;
            }
            // A set of another version ends a force that stands ([`crate::force`]), before
            // the lift: a lift that fails then leaves no force ordering the old version.
            end_by_setter(&self.ops, &service, &desired.version, actor, &set_by)?;
            lift_by_setter(&self.ops, &service, &desired.version, actor, &set_by)?;
        }
        match setter {
            Setter::Ci(verified) => tracing::info!(
                workflow = %verified.job_workflow_ref,
                sha = ?verified.sha,
                %service,
                "desired state set by CI"
            ),
            Setter::Operator(name) => tracing::info!(
                token = %name,
                %service,
                "desired state set by an operator"
            ),
        }
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
