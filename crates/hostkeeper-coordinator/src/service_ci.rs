//! The calls of [`Coordinator`] that act for a caller already checked: the transports check
//! the token first and then call these with what the check returned. A CI token is checked
//! by [`Coordinator::ci`], which spends it, so a token is checked once; an operator's token
//! is resolved to its name by [`Coordinator::caller`] and spends nothing. CI and an operator
//! both set the desired state, and only CI reads the convergence here. They differ in what may
//! be set (CI: the services its allow-list entry names; an operator: any service the coordinator
//! manages) and in who the audit names.

use std::sync::Arc;

use hostkeeper_oidc::Verified;
use hostkeeper_release::registry;
use hostkeeper_release::target::is_repository_entry;
use hostkeeper_wire::version::Version;

use super::{ConvergenceError, Coordinator, Set, SetError, kept};
use crate::converge::Convergence;
use crate::desired::{Desired, DesiredError};
use crate::force::end_by_setter;
use crate::halt::lift_by_setter;
use crate::ops::{AuditEntry, Write};
use crate::releases::{self, Release, Targets};
use crate::services::ServiceDef;
use crate::status::convergence_of;
use crate::write_record::{WriteAsk, WriteRecord};

/// Why `Releases.GetRelease` answers no release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReleaseError {
    /// The coordinator manages no such service.
    UnknownService,
    /// The version was never set with targets, or its release was dropped at the cap.
    Unknown,
    /// The host group named is not held or does not list the service.
    NotInHostGroup,
}

/// The release a set of `service` whose state is `desired` names with `targets`, judged by
/// [`releases::checked`] with the digest of the release index `desired` holds, if it has one.
pub(super) fn release_of(
    service: &str,
    desired: &Desired,
    targets: Targets,
) -> Result<Release, SetError> {
    releases::checked(
        service,
        targets,
        desired.release_digest.is_some(),
        desired.release_index.as_ref().map(|record| &record.digest),
    )
}

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
    /// coordinator manages), with the same refusals in the same order, and nothing changes when
    /// one is made. Parse, admit the service, then under the fleet's lock refuse, audit (an
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
        self.set_desired_with(setter, body, ask, None).await
    }

    /// [`Self::set_desired_by`] with the release `targets` of the version, when the set has
    /// them (docs/design/packages.md, sections 3.4 to 3.6). After the body is read, the targets
    /// are judged ([`releases::checked`]: `index_invalid` beside a package digest,
    /// `target_invalid`), and the state is stored with neither a package digest nor an index, the
    /// release kept beside it with the digest of the index set with it, whose text is not read.
    /// Under the fleet's lock, after the refusals of the state, a version set
    /// before as another release is refused (`release_exists`, [`Self::refuse_release`]); then
    /// the precondition of `ask`; then one audit entry `set_desired` by the setter, CI's as
    /// `ci`, whose reason is [`releases::audit_reason`]; then the state and the release are
    /// recorded in one commit.
    ///
    /// # Errors
    /// [`SetError`], as [`Self::set_desired_by`] and above.
    pub(crate) async fn set_desired_with(
        &self,
        setter: &Setter<'_>,
        body: &[u8],
        ask: &WriteAsk,
        targets: Option<Targets>,
    ) -> Result<(Set, Option<WriteRecord>), SetError> {
        self.set_desired_in(setter, body, ask, targets, "").await
    }

    /// [`Self::set_desired_with`] for a set that names the host group `host_group` (none: empty).
    /// A coordinator that holds a host group sets the desired version of the package in the group,
    /// or in each group that lists it ([`Self::set_desired_by_group`]); one that holds none
    /// refuses a set that names a group `not_in_host_group`.
    ///
    /// # Errors
    /// [`SetError`], as [`Self::set_desired_with`].
    pub(crate) async fn set_desired_in(
        &self,
        setter: &Setter<'_>,
        body: &[u8],
        ask: &WriteAsk,
        targets: Option<Targets>,
        host_group: &str,
    ) -> Result<(Set, Option<WriteRecord>), SetError> {
        if self.desired.in_group_mode() {
            return self
                .set_desired_by_group(setter, body, ask, targets, host_group)
                .await;
        }
        if !host_group.is_empty() {
            return Err(SetError::NotInHostGroup);
        }
        if ask.needs_records() && !self.desired.keeps_write_records() {
            return Err(SetError::Invalid(DesiredError::Unrecorded));
        }
        let (service, mut desired, dropped_roles) =
            Desired::parse_request_noting(body, &self.services, self.legacy.as_deref())?;
        // The state as the set named its release, which a set without targets is refused by.
        let named = desired.clone();
        let release = match targets {
            None => None,
            Some(targets) => {
                let release = Arc::new(release_of(&service, &desired, targets)?);
                desired.set_release(None);
                Some(release)
            }
        };
        if let Setter::Ci(verified) = setter {
            self.ci_admits(verified, &service)?;
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
            self.refuse_release(
                &service,
                &desired.version,
                release.as_deref(),
                named.release().is_some(),
            )?;
            self.refuse_repository(def, release.as_deref())?;
            // The precondition before the audit entry: a set refused for it writes nothing.
            self.desired.admits(&service, ask)?;
            let marks = self.ops.marks(&service);
            // An operator's set, and any set with targets, is audited before the desired state
            // is written: a set whose audit line cannot be appended is refused
            // (`audit_unavailable`) with nothing written.
            let reason = match (setter, &release) {
                (Setter::Ci(verified), Some(release)) => Some(releases::audit_reason(
                    &desired.version,
                    release,
                    Some(&verified.job_workflow_ref),
                )),
                (Setter::Operator(_), Some(release)) => {
                    Some(releases::audit_reason(&desired.version, release, None))
                }
                (Setter::Operator(_), None) => {
                    Some(match (&desired.release_digest, &desired.release_index) {
                        (Some(digest), _) => {
                            format!("{} with package digest {digest}", desired.version)
                        }
                        (None, Some(index)) => {
                            format!("{} with release index {}", desired.version, index.digest)
                        }
                        (None, None) => desired.version.to_string(),
                    })
                }
                (Setter::Ci(_), None) => None,
            };
            if let Some(reason) = reason {
                let mut entry = AuditEntry::new(actor, "set_desired");
                entry.service = Some(service.clone());
                entry.reason = Some(reason);
                self.ops.change(Write::BeforeMemory, entry, |_| {})?;
            }
            let record = self.desired.record_as(
                &service,
                desired.clone(),
                |v| kept(&marks, v),
                ask,
                release,
            )?;
            // A set of another version ends a force that stands ([`crate::force`]), before
            // the lift: a lift that fails then leaves no force ordering the old version.
            end_by_setter(&self.ops, &service, &desired.version, actor, &set_by)?;
            lift_by_setter(&self.ops, None, &service, &desired.version, actor, &set_by)?;
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

    /// Whether the allow-list entry that admitted `verified` names `service`: an entry that names
    /// no services stands for the legacy service.
    ///
    /// # Errors
    /// [`SetError::Forbidden`].
    pub(super) fn ci_admits(&self, verified: &Verified, service: &str) -> Result<(), SetError> {
        let admitted = match &verified.services {
            Some(names) => names.iter().any(|name| name == service),
            None => self.legacy.as_deref() == Some(service),
        };
        if !admitted {
            tracing::warn!(
                workflow = %verified.job_workflow_ref,
                %service,
                "refused a desired state: the allow-list entry does not name the service"
            );
            return Err(SetError::Forbidden);
        }
        Ok(())
    }

    /// Why CI may not set `desired` for `service`, if it may not: the refusals of
    /// [`Coordinator::set_desired`], in that order.
    pub(super) fn refuse(&self, def: &ServiceDef, desired: &Desired) -> Result<(), SetError> {
        let (service, version) = (def.name.as_str(), &desired.version);
        if !def.version_scheme.parses(version) {
            return Err(SetError::UnparseableVersion);
        }
        if def
            .min_version
            .as_ref()
            .is_some_and(|floor| !version.at_least(floor))
        {
            return Err(SetError::BelowMinVersion);
        }
        if self.ops.marks(service).bad.contains(version) {
            return Err(SetError::BadVersion);
        }
        let registered = self.desired.digests(service).get(version).cloned();
        if let (Some(before), Some(now)) = (registered, desired.release())
            && before != now
        {
            return Err(SetError::DigestConflict);
        }
        Ok(())
    }

    /// Refuses a set of `version` of `service` as another release than the version was set
    /// as (`release_exists`): with targets (`release`), when the version's release is another, or
    /// when it has none and the registry names the version (set before with a digest or an
    /// index); without targets, when the set names a digest or an index (`names_release`) and the
    /// version has a release. A set with no digest, no index and no targets of a version with a
    /// release changes its limits and keeps the release.
    pub(super) fn refuse_release(
        &self,
        service: &str,
        version: &Version,
        release: Option<&Release>,
        names_release: bool,
    ) -> Result<(), SetError> {
        let kept = self.desired.release(service, version.as_str());
        let exists = match (release, kept) {
            (Some(new), Some(old)) => *old != *new,
            (Some(_), None) => self.desired.digests(service).get(version).is_some(),
            (None, kept) => names_release && kept.is_some(),
        };
        if exists {
            return Err(SetError::ReleaseExists);
        }
        Ok(())
    }

    /// Refuses a set whose `release` holds a repository entry (`repository_unknown`) when the
    /// registry does not hold the repository `def` names, or when an entry's `repository` is not
    /// `<name>-<suite>` of that repository's name and one of its suites
    /// ([`registry::channels`]). A set without targets, or with artifact entries alone, is not
    /// checked.
    pub(super) fn refuse_repository(
        &self,
        def: &ServiceDef,
        release: Option<&Release>,
    ) -> Result<(), SetError> {
        let named: Vec<&str> = release
            .into_iter()
            .flat_map(|release| release.targets().values())
            .flat_map(|list| &list.entries)
            .filter(|entry| is_repository_entry(entry))
            .map(|entry| entry.repository.as_deref().unwrap_or_default())
            .collect();
        if named.is_empty() {
            return Ok(());
        }
        let channels = self.desired.with_repositories(|registry| {
            registry
                .get(&def.repository)
                .map(|kept| registry::channels(&kept.repository))
        });
        match channels {
            Some(channels) if named.iter().all(|name| channels.iter().any(|c| c == name)) => Ok(()),
            _ => Err(SetError::RepositoryUnknown),
        }
    }

    /// The release `version` of `service` was set with ([`crate::releases`]).
    ///
    /// # Errors
    /// [`ReleaseError::UnknownService`]: the coordinator manages no such service.
    /// [`ReleaseError::Unknown`]: the version was never set with targets, or its release was
    /// dropped at the cap.
    pub(crate) fn release_verified(
        &self,
        service: &str,
        version: &str,
    ) -> Result<Arc<Release>, ReleaseError> {
        self.services
            .get(service)
            .ok_or(ReleaseError::UnknownService)?;
        self.desired
            .release(service, version)
            .ok_or(ReleaseError::Unknown)
    }

    /// Where the hosts of `service` stand against its desired state, as
    /// [`Self::convergence`] answers once the CI token is checked: with host groups, over every
    /// group that lists the service and has a desired version of it ([`Self::convergence_in`]).
    ///
    /// # Errors
    /// [`ConvergenceError::UnknownService`]: the coordinator manages no such service.
    pub(crate) fn convergence_verified(
        &self,
        service: &str,
    ) -> Result<Convergence, ConvergenceError> {
        self.convergence_in("", service)
    }

    /// Where the hosts of the service `def` defines stand against its desired state, now.
    pub(crate) fn convergence_of_def(&self, def: &ServiceDef) -> Convergence {
        let now = (self.clock)();
        let fleet = self.lock_fleet();
        let desired = self.desired.get(&def.name);
        let ops = self.ops.state();
        convergence_of(def, desired.as_ref(), &fleet, &ops, now)
    }

    /// Where the hosts of `service` stand against its desired state, if `bearer` is a
    /// GitHub Actions OIDC token the same allow-list accepts as for [`Self::set_desired`]
    /// (any service may be read with any accepted token). The operator token is no CI token
    /// and is refused. Read-only, and each token is good once.
    ///
    /// # Errors
    /// [`ConvergenceError`].
    pub async fn convergence(
        &self,
        bearer: Option<&str>,
        service: &str,
    ) -> Result<Convergence, ConvergenceError> {
        self.ci(bearer).await?;
        self.convergence_verified(service)
    }
}
