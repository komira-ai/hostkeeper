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
use crate::write_record::{WriteAsk, WriteRecord};

/// Who sets the desired state.
pub(crate) enum Setter<'a> {
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
    pub(crate) async fn set_desired_verified(
        &self,
        verified: &Verified,
        body: &[u8],
    ) -> Result<Set, SetError> {
        self.set_desired_by(&Setter::Ci(verified), body, &WriteAsk::default())
            .await
            .map(|(set, _)| set)
    }

    /// Sets the desired state from `body` for `setter`. CI's set is [`Self::set_desired_verified`];
    /// an operator's is the same with no allow-list (the operator may set any service the
    /// coordinator manages), with the same refusals in the same order (the release channel's
    /// among them), and nothing changes when one is made. Parse, admit the service, ask the
    /// release channel (with `--release-channel`), then under the fleet's lock refuse, audit (an
    /// operator's), record, and end a force and lift a halt. Before an operator's state is
    /// recorded, one audit entry `set_desired` by the operator is written (the service, and as its
    /// reason the version, followed by `with package digest <digest>` when the body named one, or
    /// `with release index <digest>` when it named an index); then the state is recorded, a force
    /// of another version ends and a halt the updater raised is lifted, both audited by the
    /// operator.
    ///
    /// `ask` is what `Releases.SetDesired` asks of the store beside the state ([`WriteAsk`]); the
    /// answer carries the service's write record after the set, none over a store that keeps no
    /// records.
    ///
    /// # Errors
    /// [`SetError`], other than the refusals of a token. [`SetError::Ops`] of
    /// [`crate::ops::OpsError::AuditUnavailable`] from the `set_desired` entry means nothing is
    /// set. Any later [`SetError::Ops`] means the desired state is set and the end of a force or
    /// the lift of a halt could not be written; setting the same state again is safe and tries
    /// them again. Two refusals are of `ask`, each with nothing changed:
    /// [`DesiredError::Unrecorded`] first of all, when `ask` asks for anything and the store keeps
    /// no records (the desired-state file); and [`DesiredError::GenerationConflict`] after the
    /// refusals of the state itself and before the operator's audit entry, when its
    /// `if_generation` is not the service's generation.
    pub(crate) async fn set_desired_by(
        &self,
        setter: &Setter<'_>,
        body: &[u8],
        ask: &WriteAsk,
    ) -> Result<(Set, Option<WriteRecord>), SetError> {
        if ask.needs_records() && !self.desired.keeps_write_records() {
            return Err(SetError::Invalid(DesiredError::Unrecorded));
        }
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
        // The release channel before the fleet's lock, which is never held across a wait on the
        // network; a refusal here changes nothing.
        if let Some(channel) = &self.channel {
            channel.check(&service, &desired).await?;
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
        let record = {
            let _fleet = self.lock_fleet_for_set();
            let def = self
                .services
                .get(&service)
                .ok_or(SetError::Invalid(DesiredError::UnknownService))?;
            self.refuse(def, &desired)?;
            // The precondition before the audit entry: a set refused for it writes nothing.
            self.desired.admits(&service, ask)?;
            let marks = self.ops.marks(&service);
            // An operator's set is audited before the desired state is written: a set whose
            // audit line cannot be appended is refused (`audit_unavailable`) with nothing
            // written.
            if let Setter::Operator(name) = setter {
                let mut entry = AuditEntry::new(name, "set_desired");
                entry.service = Some(service.clone());
                entry.reason = Some(match (&desired.release_digest, &desired.release_index) {
                    (Some(digest), _) => {
                        format!("{} with package digest {digest}", desired.version)
                    }
                    (None, Some(index)) => {
                        format!("{} with release index {}", desired.version, index.digest)
                    }
                    (None, None) => desired.version.to_string(),
                });
                self.ops.change(Write::BeforeMemory, entry, |_| {})?;
            }
            let record =
                self.desired
                    .record_as(&service, desired.clone(), |v| kept(&marks, v), ask)?;
            // A set of another version ends a force that stands ([`crate::force`]), before
            // the lift: a lift that fails then leaves no force ordering the old version.
            end_by_setter(&self.ops, &service, &desired.version, actor, &set_by)?;
            lift_by_setter(&self.ops, &service, &desired.version, actor, &set_by)?;
            record
        };
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
        Ok((
            Set {
                desired,
                dropped_roles,
            },
            record,
        ))
    }

    /// How the writes of `service` are recorded now, as `Releases.Converge` shows them: none
    /// when the store keeps no records ([`crate::desired::DesiredStore::write_record`]).
    pub(crate) fn write_record(&self, service: &str) -> Option<WriteRecord> {
        self.desired.write_record(service)
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
