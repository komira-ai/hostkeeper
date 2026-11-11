//! Which repository of the registry a package comes from, and `Services.SetRepository`, the call
//! that sets it.
//!
//! A package's repository is coordinator state, held in the ops store with the package's other
//! marks ([`crate::ops::ServiceOps::repository`]) and changed only by this call. The repository
//! a package comes from is the one set there; else the one the rollout file's definition of it
//! names (a coordinator built with a file); else [`DEFAULT_REPOSITORY`]. Every reader of a
//! package's repository asks [`OpsState::repository_of`] or [`Coordinator::repository_of`].
//!
//! The call takes the operator token only. Its refusals are `unauthorized`, `forbidden`,
//! `invalid_request` (the reason, or a repository name that breaks the registry's grammar),
//! `unknown_service` (no host group lists the package and the rollout file does not define it),
//! `repository_unknown` (the registry does not hold the repository), `store_failed` and
//! `audit_unavailable`. An empty repository clears the setting. A call that sets what is already
//! set writes its audit entry all the same.

use std::collections::BTreeSet;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_release::grammar::is_repository_name;

use crate::group_reach::lists;
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, OpsState, ServiceOps, Write};
use crate::service::Coordinator;
use crate::services::{DEFAULT_REPOSITORY, Services};

type Refusal = proto::ServicesRefusal;

impl OpsState {
    /// The repository `package` comes from: the one `Services.SetRepository` set, else the one
    /// the rollout file `file` defines it with, else [`DEFAULT_REPOSITORY`].
    pub(crate) fn repository_of(&self, file: &Services, package: &str) -> String {
        self.services
            .get(package)
            .and_then(|marks| marks.repository.clone())
            .or_else(|| file.get(package).map(|def| def.repository.clone()))
            .unwrap_or_else(|| DEFAULT_REPOSITORY.to_owned())
    }
}

impl Coordinator {
    /// [`OpsState::repository_of`] of `package` over the state now.
    pub(crate) fn repository_of(&self, package: &str) -> String {
        self.ops
            .read(|state| state.repository_of(&self.services, package))
    }

    /// Every package whose repository can be other than the default: those the rollout file
    /// defines, those a host group lists and those with a repository set, in name order.
    pub(crate) fn packages_with_repositories(&self, state: &OpsState) -> BTreeSet<String> {
        let mut packages: BTreeSet<String> = self.services.names().map(str::to_owned).collect();
        packages.extend(
            self.desired
                .host_groups()
                .0
                .values()
                .flat_map(|group| group.packages.iter().cloned()),
        );
        packages.extend(
            state
                .services
                .iter()
                .filter(|(_, marks)| marks.repository.is_some())
                .map(|(package, _)| package.clone()),
        );
        packages
    }

    /// Whether some host group lists `package`.
    fn listed_by_a_group(&self, package: &str) -> bool {
        self.desired
            .host_groups()
            .0
            .values()
            .any(|group| lists(group, package))
    }

    /// `Services.SetRepository`: sets the repository of the registry `package` comes from, or,
    /// for an empty `repository`, goes back to the default. Audited as `set_repository`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_service`, `repository_unknown`,
    /// `store_failed`, `audit_unavailable`.
    pub fn set_repository(
        &self,
        caller: &Caller,
        request: proto::SetRepositoryRequest,
    ) -> Result<proto::SetRepositoryResponse, Refusal> {
        let by = caller.operator()?;
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        // A package with a repository set stays one the call takes, so a repository left on a
        // package no group lists any more can be cleared.
        let set_before = self
            .ops
            .state()
            .services
            .get(&request.service)
            .is_some_and(|marks| marks.repository.is_some());
        if self.services.get(&request.service).is_none()
            && !self.listed_by_a_group(&request.service)
            && !set_before
        {
            return Err(Refusal::UnknownService);
        }
        let repository = match request.repository.as_str() {
            "" => None,
            name if is_repository_name(name) => Some(name.to_owned()),
            _ => return Err(Refusal::InvalidRequest),
        };
        let _naming = self
            .repository_naming
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(name) = &repository
            && self
                .desired
                .with_repositories(|registry| registry.get(name).is_none())
        {
            return Err(Refusal::RepositoryUnknown);
        }
        let said = repository.as_deref().map_or_else(
            || format!("repository {DEFAULT_REPOSITORY} (the default): {why}"),
            |name| format!("repository {name}: {why}"),
        );
        let mut audit = AuditEntry::new(by, "set_repository");
        audit.service = Some(request.service.clone());
        audit.reason = Some(said);
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let marks = state.services.entry(request.service.clone()).or_default();
            marks.repository = repository;
            if *marks == ServiceOps::default() {
                state.services.remove(&request.service);
            }
            Ok::<(), Refusal>(())
        })?;
        Ok(proto::SetRepositoryResponse {
            entry: Some(entry_proto(&done)),
        })
    }
}
