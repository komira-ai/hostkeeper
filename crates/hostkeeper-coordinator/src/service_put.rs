//! The registry of packages: `Services.Put` and `Services.Remove`, the calls behind the console's
//! Packages page.
//!
//! A package is registered when a host group lists it, the rollout file defines it, or its
//! repository is set; the last is what `Put` writes, so a package can be registered before any
//! group lists it, and is stored where `Services.SetRepository` stores a repository (the ops
//! store's marks of the package), so no format changes. `Put` records the repository the package
//! comes from (empty: [`DEFAULT_REPOSITORY`]); the registry must hold it. `Remove` clears that
//! record. A package a group lists, or the rollout file defines, is registered whatever the record
//! says, so `Remove` refuses it `in_use`; a package nothing registers is `unknown_service`.
//!
//! Both take the operator token only and write one audit entry in the commit of the change
//! (`service_put`, `service_remove`). Their refusals are those of `Services`: `unauthorized`,
//! `forbidden`, `invalid_request` (the reason, the package's name or the repository's), `unknown_service`,
//! `repository_unknown`, `in_use`, `store_failed` and `audit_unavailable`. A refused call changes
//! nothing.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_release::grammar::is_repository_name;
use hostkeeper_wire::wire::is_service_name;

use crate::operator::{entry_proto, valid_reason};
use crate::ops::{AuditEntry, ServiceOps, Write};
use crate::service::Coordinator;
use crate::services::DEFAULT_REPOSITORY;

type Refusal = proto::ServicesRefusal;

impl Coordinator {
    /// `Services.Put`: registers `package` with the repository it comes from. Audited as
    /// `service_put`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `repository_unknown`, `store_failed`,
    /// `audit_unavailable`.
    pub fn put_service(
        &self,
        caller: &crate::operator::Caller,
        request: proto::PutServiceRequest,
    ) -> Result<proto::PutServiceResponse, Refusal> {
        let by = caller.operator()?;
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        if !is_service_name(&request.service) {
            return Err(Refusal::InvalidRequest);
        }
        let repository = match request.repository.as_str() {
            "" => DEFAULT_REPOSITORY,
            name if is_repository_name(name) => name,
            _ => return Err(Refusal::InvalidRequest),
        };
        let _naming = self
            .repository_naming
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .desired
            .with_repositories(|registry| registry.get(repository).is_none())
        {
            return Err(Refusal::RepositoryUnknown);
        }
        let mut audit = AuditEntry::new(by, "service_put");
        audit.service = Some(request.service.clone());
        audit.reason = Some(format!("repository {repository}: {why}"));
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            state
                .services
                .entry(request.service.clone())
                .or_default()
                .repository = Some(repository.to_owned());
            Ok::<(), Refusal>(())
        })?;
        Ok(proto::PutServiceResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Services.Remove`: takes `package` out of the registry. Audited as `service_remove`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request`, `unknown_service`, `in_use`,
    /// `store_failed`, `audit_unavailable`.
    pub fn remove_service(
        &self,
        caller: &crate::operator::Caller,
        request: proto::RemoveServiceRequest,
    ) -> Result<proto::RemoveServiceResponse, Refusal> {
        let by = caller.operator()?;
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        if !is_service_name(&request.service) {
            return Err(Refusal::InvalidRequest);
        }
        let _naming = self
            .repository_naming
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.services.get(&request.service).is_some() || self.listed_by_a_group(&request.service)
        {
            return Err(Refusal::InUse);
        }
        let mut audit = AuditEntry::new(by, "service_remove");
        audit.service = Some(request.service.clone());
        audit.reason = Some(why);
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let marks = state
                .services
                .get_mut(&request.service)
                .filter(|marks| marks.repository.is_some())
                .ok_or(Refusal::UnknownService)?;
            marks.repository = None;
            state
                .services
                .retain(|_, marks| *marks != ServiceOps::default());
            Ok::<(), Refusal>(())
        })?;
        Ok(proto::RemoveServiceResponse {
            entry: Some(entry_proto(&done)),
        })
    }
}
