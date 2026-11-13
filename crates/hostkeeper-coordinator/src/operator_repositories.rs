//! `Repositories` of `hostkeeper.operator.v1` (`List`, `Get`, `Put`, `Remove`, `Discover`, `Add`),
//! as plain functions over its messages, as the other operator calls are ([`crate::operator`]): the
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
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::descriptor::Descriptor;
use hostkeeper_release::grammar::is_repository_name;
use hostkeeper_release::registry;

use crate::builtin;
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::AuditEntry;
use crate::repositories::{Kept, PutError, Registry, platforms_of, put_reason, remove_reason};
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

/// The `Authorization` line a private repository is read with, from the credential file; none for
/// a public one.
fn authorization_of(access: &str, credential_file: &str) -> Result<Option<String>, Refusal> {
    match access {
        "" | registry::ACCESS_PUBLIC if credential_file.is_empty() => Ok(None),
        registry::ACCESS_HOSTS if !credential_file.is_empty() => {
            repository_credential::read(credential_file)
                .map(Some)
                .map_err(|why| {
                    tracing::warn!(%why, "refused a private repository");
                    Refusal::CredentialUnusable
                })
        }
        _ => Err(Refusal::InvalidRequest),
    }
}

/// The platforms a descriptor's repository serves: all, and the Linux ones only for a private one
/// (its macOS format is not served).
fn served(descriptor: &Descriptor, private: bool) -> Vec<String> {
    let mut platforms = descriptor.platforms();
    if private {
        platforms.retain(|platform| !platform.starts_with("macos-"));
    }
    platforms
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

    /// `kept` as the answer says it, with its keyring and descriptor only when `with_keyring`.
    fn registered(&self, kept: &Kept, with_keyring: bool) -> proto::RegisteredRepository {
        let mut repository = kept.repository.clone();
        let keyring_sha256 = registry::keyring_sha256(&repository.keyring);
        if !with_keyring {
            repository.keyring.clear();
            repository.descriptor.clear();
        }
        let builtin = self
            .desired
            .with_repositories(|held| held.is_builtin(&kept.repository.name));
        proto::RegisteredRepository {
            platforms: platforms_of(&kept.repository),
            repository: Some(repository),
            keyring_sha256,
            generation: kept.generation,
            services: self.services_naming(&kept.repository.name),
            builtin,
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
                .listed()
                .into_iter()
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
        self.put_entry(by, &why, repository)
    }

    /// Puts `repository` as `by`, for the reason `why`: the checks, the credential of a private
    /// one, the registry's write and the audit entry `repository_put`, one commit.
    fn put_entry(
        &self,
        by: &str,
        why: &str,
        repository: Repository,
    ) -> Result<proto::PutRepositoryResponse, Refusal> {
        registry::check(&repository).map_err(|why| entry_refusal(&why))?;
        if registry::is_private(&repository) {
            repository_credential::read(&repository.credential_file).map_err(|why| {
                tracing::warn!(repository = %repository.name, %why, "refused a private repository");
                Refusal::CredentialUnusable
            })?;
        }
        let audit = entry(by, "repository_put", put_reason(why, &repository));
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

    /// `Repositories.Discover`: reads the descriptor and the key of the repository at the URL and
    /// answers what they say, with the fingerprints computed from the keyring. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `invalid_request` (the URL, the access, or a private
    /// repository with no credential file), `credential_unusable`, `discovery_failed`,
    /// `descriptor_invalid`.
    pub async fn repositories_discover(
        &self,
        caller: &Caller,
        request: proto::DiscoverRepositoryRequest,
    ) -> Result<proto::DiscoverRepositoryResponse, Refusal> {
        caller.operator()?;
        let authorization = authorization_of(&request.access, &request.credential_file)?;
        let found = self
            .read_repository(&request.base_uri, authorization.as_deref())
            .await?;
        let private = request.access == registry::ACCESS_HOSTS;
        Ok(proto::DiscoverRepositoryResponse {
            discovery: Some(proto::Discovery {
                name: found.descriptor.name.clone(),
                description: found.descriptor.description.clone(),
                channels: found.descriptor.channels.clone(),
                platforms: served(&found.descriptor, private),
                macos_packages: found
                    .descriptor
                    .macos_pkg
                    .iter()
                    .flat_map(|mac| mac.packages.keys().cloned())
                    .collect(),
                pinned: builtin::is_pinned(&found.fingerprints),
                fingerprints: found.fingerprints,
                user_ids: found.user_ids,
                descriptor_sha256: found.descriptor_sha256,
                keyring_sha256: found.keyring_sha256,
            }),
        })
    }

    /// `Repositories.Add`: reads the repository at the URL again and, when its descriptor and
    /// fingerprints are the ones confirmed, puts the entry they make. Audited as
    /// `repository_put`.
    ///
    /// # Errors
    /// Those of `Discover`, `descriptor_changed`, `builtin` (the name `komira` with a key other
    /// than the pinned one), and those of `Put`.
    pub async fn repositories_add(
        &self,
        caller: &Caller,
        request: proto::AddRepositoryRequest,
    ) -> Result<proto::AddRepositoryResponse, Refusal> {
        let by = caller.operator()?;
        if !is_repository_name(&request.name) {
            return Err(Refusal::InvalidRequest);
        }
        let why = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let authorization = authorization_of(&request.access, &request.credential_file)?;
        let found = self
            .read_repository(&request.base_uri, authorization.as_deref())
            .await?;
        let confirmed: BTreeSet<&String> = request.fingerprints.iter().collect();
        let read: BTreeSet<&String> = found.fingerprints.iter().collect();
        if request.descriptor_sha256 != found.descriptor_sha256 || confirmed != read {
            tracing::warn!(repository = %request.name, "refused an add: what the repository serves is not what was confirmed");
            return Err(Refusal::DescriptorChanged);
        }
        if request.name == builtin::NAME && !builtin::is_pinned(&found.fingerprints) {
            return Err(Refusal::Builtin);
        }
        let private = request.access == registry::ACCESS_HOSTS;
        let suites = if private {
            // The suites of a private repository are host groups' (hostgroups.md section 6.6).
            self.desired
                .host_groups()
                .0
                .keys()
                .take(registry::MAX_SUITES)
                .map(|group| group.as_str().to_owned())
                .collect()
        } else {
            found.descriptor.channels.clone()
        };
        let repository = Repository {
            name: request.name,
            kind: registry::KIND_APT.to_owned(),
            uri: found.descriptor.apt_uri(&request.base_uri),
            suites,
            components: found.descriptor.apt.components.clone(),
            fingerprints: found.fingerprints,
            keyring: found.keyring,
            access: request.access,
            credential_file: request.credential_file,
            descriptor: found.descriptor_bytes,
            base_uri: request.base_uri,
        };
        let put = self.put_entry(by, &why, repository)?;
        Ok(proto::AddRepositoryResponse {
            repository: put.repository,
            entry: put.entry,
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
        // An entry put over the built-in one is removed whatever services name it: they are served
        // by the built-in one after.
        let in_use =
            request.name != builtin::NAME && !self.services_naming(&request.name).is_empty();
        let staged = self.desired.stage_repositories(|held| {
            if held.is_builtin(&request.name) {
                return Err(Refusal::Builtin);
            }
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
