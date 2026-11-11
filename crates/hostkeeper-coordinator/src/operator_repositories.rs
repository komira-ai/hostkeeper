//! `Repositories` of `hostkeeper.operator.v1` (`List`, `Get`, `Put`, `Remove`), as plain
//! functions over its messages, as the other operator calls are ([`crate::operator`]): the
//! operator's registry of package repositories ([`crate::repositories`]).
//!
//! Every call takes the operator token only: a valid CI token is `forbidden`, and no token or any
//! other `unauthorized`, before the request is looked at. `List` and `Get` change nothing. `Put`
//! and `Remove` change the registry and write one audit entry, `repository_put` and
//! `repository_remove`, in the same commit ([`crate::ops::OpsStore::decide_changes_with`]): a
//! change whose audit line cannot be appended is refused `audit_unavailable` and not made, and
//! one whose commit fails `store_failed`; each leaves the registry as it was. The registry's
//! generation grows by one at each.
//!
//! A repository is judged by the rules of `hostkeeper-release` ([`registry::check`]): a keyring
//! that breaks one is `keyring_invalid`, any other field `invalid_request`. A `Put` whose
//! channels (`<name>-<suite>`) are another repository's is `conflict`, and a new name while the
//! registry holds [`registry::MAX_REPOSITORIES`] is `capacity`. A `Remove` of a repository a service of the
//! rollout file names is `in_use`.

use std::collections::BTreeSet;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::RepositoriesRefusal as Refusal;
use hostkeeper_release::grammar::is_repository_name;
use hostkeeper_release::registry;

use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::AuditEntry;
use crate::repositories::{Kept, PutError, Registry, put_reason, remove_reason};
use crate::repository_credential;
use crate::service::Coordinator;

/// The refusal a rule of the registry makes of a `Put`: a keyring's is `keyring_invalid`.
fn entry_refusal(why: &registry::EntryError) -> Refusal {
    if why.is_keyring() {
        Refusal::KeyringInvalid
    } else {
        Refusal::InvalidRequest
    }
}

/// An audit entry by `by` for `action`, about no service, with `reason`.
fn entry(by: &str, action: &str, reason: String) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.reason = Some(reason);
    entry
}

impl Coordinator {
    /// The services that come from the repository `name`, by name: those of the rollout file, the
    /// packages a host group lists and those with a repository set, each by its repository
    /// ([`crate::ops::OpsState::repository_of`]).
    fn services_naming(&self, name: &str) -> Vec<String> {
        let state = self.ops.state();
        self.packages_with_repositories(&state)
            .into_iter()
            .filter(|package| state.repository_of(&self.services, package) == name)
            .collect()
    }

    /// `kept` as the answer says it, with its keyring only when `with_keyring`.
    fn registered(&self, kept: &Kept, with_keyring: bool) -> proto::RegisteredRepository {
        let mut repository = kept.repository.clone();
        let keyring_sha256 = registry::keyring_sha256(&repository.keyring);
        if !with_keyring {
            repository.keyring.clear();
        }
        proto::RegisteredRepository {
            repository: Some(repository),
            keyring_sha256,
            generation: kept.generation,
            services: self.services_naming(&kept.repository.name),
        }
    }

    /// `Repositories.List`: every repository of the registry by name, without its keyring, and
    /// the registry's generation; and the repositories the rollout file's services name that the
    /// registry lacks. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`.
    pub fn repositories_list(
        &self,
        caller: &Caller,
        _request: &proto::ListRepositoriesRequest,
    ) -> Result<proto::ListRepositoriesResponse, Refusal> {
        caller.operator()?;
        let held = self.desired.with_repositories(Registry::clone);
        let state = self.ops.state();
        let unregistered: BTreeSet<String> = self
            .packages_with_repositories(&state)
            .iter()
            .map(|package| state.repository_of(&self.services, package))
            .filter(|name| held.get(name).is_none())
            .collect();
        Ok(proto::ListRepositoriesResponse {
            repositories: held
                .entries
                .values()
                .map(|kept| self.registered(kept, false))
                .collect(),
            generation: held.generation,
            unregistered: unregistered.into_iter().collect(),
        })
    }

    /// `Repositories.Get`: one repository, with its keyring. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (not a repository name),
    /// `unknown_repository`.
    pub fn repositories_get(
        &self,
        caller: &Caller,
        request: &proto::GetRepositoryRequest,
    ) -> Result<proto::GetRepositoryResponse, Refusal> {
        caller.operator()?;
        if !is_repository_name(&request.name) {
            return Err(Refusal::InvalidRequest);
        }
        let kept = self
            .desired
            .with_repositories(|held| held.get(&request.name).cloned())
            .ok_or(Refusal::UnknownRepository)?;
        Ok(proto::GetRepositoryResponse {
            repository: Some(self.registered(&kept, true)),
        })
    }

    /// `Repositories.Put`: adds the repository, or replaces the one of its name (a key
    /// rotation, when its keyring or fingerprints differ), and raises the registry's generation
    /// by one. Audited as `repository_put`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the reason, or a field of the repository
    /// that breaks its rule), `keyring_invalid`, `credential_unusable`, `conflict`, `capacity`, `store_failed`,
    /// `audit_unavailable`.
    pub fn repositories_put(
        &self,
        caller: &Caller,
        request: proto::PutRepositoryRequest,
    ) -> Result<proto::PutRepositoryResponse, Refusal> {
        let by = caller.operator()?;
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let repository = request.repository.ok_or(Refusal::InvalidRequest)?;
        registry::check(&repository).map_err(|why| entry_refusal(&why))?;
        if registry::is_private(&repository) {
            repository_credential::read(&repository.credential_file).map_err(|why| {
                tracing::warn!(repository = %repository.name, %why, "refused a private repository");
                Refusal::CredentialUnusable
            })?;
        }
        let audit = entry(by, "repository_put", put_reason(&why, &repository));
        let name = repository.name.clone();
        // The checks that read the registry run under the writers' lock, on the registry the
        // change replaces.
        let staged = self.desired.stage_repositories(|held| {
            held.put(repository).map_err(|why| match why {
                PutError::Conflict(_) => Refusal::Conflict,
                PutError::Capacity => Refusal::Capacity,
            })
        })?;
        let kept = staged.after.repositories.get(&name).cloned();
        let done = self
            .ops
            .decide_changes_with(staged, |_| vec![audit])?
            .swap_remove(0);
        Ok(proto::PutRepositoryResponse {
            repository: kept.map(|kept| self.registered(&kept, true)),
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Repositories.Remove`: removes the repository, which no service may come from (the rollout
    /// file's, a host group's package, or one with the repository set), and raises the registry's generation by one. Audited as `repository_remove`.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the name or the reason),
    /// `unknown_repository`, `in_use`, `store_failed`, `audit_unavailable`.
    pub fn repositories_remove(
        &self,
        caller: &Caller,
        request: proto::RemoveRepositoryRequest,
    ) -> Result<proto::RemoveRepositoryResponse, Refusal> {
        let by = caller.operator()?;
        if !is_repository_name(&request.name) {
            return Err(Refusal::InvalidRequest);
        }
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let audit = entry(by, "repository_remove", remove_reason(&why, &request.name));
        let _naming = self
            .repository_naming
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let in_use = !self.services_naming(&request.name).is_empty();
        let staged = self.desired.stage_repositories(|held| {
            let next = held
                .remove(&request.name)
                .ok_or(Refusal::UnknownRepository)?;
            if in_use {
                return Err(Refusal::InUse);
            }
            Ok(next)
        })?;
        let generation = staged.after.repositories.generation;
        let done = self
            .ops
            .decide_changes_with(staged, |_| vec![audit])?
            .swap_remove(0);
        Ok(proto::RemoveRepositoryResponse {
            generation,
            entry: Some(entry_proto(&done)),
        })
    }
}
