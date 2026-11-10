//! The host groups in the desired state store ([`crate::host_groups`]): a copy of the groups
//! now, one group, whether any is held, and the desired state a change of the groups makes,
//! staged under the writers' lock for a call that commits it with its audit entries and the
//! operations state's marks ([`crate::ops::OpsStore::decide_changes_with`]).

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError};

use hostkeeper_wire::version::Version;

use super::{Desired, DesiredStore, Held, Recorded, RoleLimits, Staged, StoreError};
use crate::host_groups::{GroupDesired, GroupName, HostGroup, HostGroups};
use crate::releases::Release as Kept;
use crate::services::ServiceDef;
use crate::write_record::{Replaced, WriteAsk, WriteError, WriteId, WriteRecord};

/// The desired state of `package` in the group `name`, as the decision reads it: the group's
/// version `wanted`, the group's `max_updating` and `min_up` as the limits of the group's name
/// (the role the fleet records for its hosts), and the release CI registered for the version.
fn projected(
    held: &Held,
    name: &GroupName,
    group: &HostGroup,
    package: &str,
    wanted: &GroupDesired,
) -> Desired {
    let mut desired = Desired {
        version: wanted.version.clone(),
        max_updating: RoleLimits::from([(name.as_str(), group.policy.max_updating)]),
        min_up: RoleLimits::from([(name.as_str(), group.policy.min_up)]),
        release_digest: None,
        release_index: None,
    };
    desired.set_release(
        held.digests
            .get(package)
            .and_then(|registry| registry.get(&wanted.version))
            .cloned(),
    );
    desired
}

impl DesiredStore {
    /// The host groups now, by name.
    pub(crate) fn host_groups(&self) -> HostGroups {
        self.lock().host_groups.clone()
    }

    /// The group `name` now.
    pub(crate) fn host_group(&self, name: &GroupName) -> Option<HostGroup> {
        self.lock().host_groups.0.get(name).cloned()
    }

    /// The desired state of `package` in the group `name` ([`projected`]); none for a package the
    /// group has no desired version for.
    pub(crate) fn group_desired(&self, name: &GroupName, package: &str) -> Option<Desired> {
        let held = self.lock();
        let group = held.host_groups.0.get(name)?;
        let wanted = group.desired.get(package)?;
        Some(projected(&held, name, group, package, wanted))
    }

    /// The desired state of every package of the group `name` that has one, by package.
    pub(crate) fn group_desireds(&self, name: &GroupName) -> BTreeMap<String, Desired> {
        let held = self.lock();
        let desired = held.host_groups.0.get(name).map(|group| {
            group.desired.iter().map(|(package, wanted)| {
                (
                    package.clone(),
                    projected(&held, name, group, package, wanted),
                )
            })
        });
        desired.into_iter().flatten().collect()
    }

    /// The desired state of the service `def` stands for: its group's, for one made for a group.
    pub(crate) fn of_def(&self, def: &ServiceDef) -> Option<Desired> {
        match def.group() {
            None => self.get(&def.name),
            Some(group) => self.group_desired(group, &def.name),
        }
    }

    /// Every (group, package, version) the groups hold a desired version of, for the releases the
    /// decisions read.
    pub(crate) fn group_versions(&self) -> Vec<(String, Version)> {
        self.lock()
            .host_groups
            .0
            .values()
            .flat_map(|group| {
                group
                    .desired
                    .iter()
                    .map(|(package, wanted)| (package.clone(), wanted.version.clone()))
            })
            .collect()
    }

    /// The deployment of `desired` (its version, and the release CI registered with a digest or an
    /// index) of `package` to each of `groups`, with the release `release` set with targets, if
    /// any, staged under the writers' lock and not committed: each group's desired version is
    /// replaced and its generation raised by one, the write id kept, the version it replaces
    /// remembered, and the version's release recorded once for the package, as a set of a service
    /// does ([`Self::record_as`]), the versions `keep` kept whatever their age at the registry's cap.
    ///
    /// # Errors
    /// [`WriteError::Conflict`]: the write names a generation, and it is not that of a group's
    /// desired version of the package (0 before the first write). Nothing was staged.
    pub(crate) fn stage_group_set(
        &self,
        groups: &[GroupName],
        package: &str,
        desired: &Desired,
        release: Option<Arc<Kept>>,
        keep: &[Version],
        ask: &WriteAsk,
    ) -> Result<Staged<'_>, WriteError> {
        let writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let before = self.lock().clone();
        let mut after = before.clone();
        for group in after
            .host_groups
            .0
            .iter_mut()
            .filter(|(name, _)| groups.contains(name))
            .map(|(_, group)| group)
        {
            let old = group.desired.get(package);
            ask.admits(old.map_or(0, |old| old.generation))?;
            let replaced_version = match old {
                Some(old) if old.version != desired.version => Some(old.version.clone()),
                other => other.and_then(|old| old.replaced_version.clone()),
            };
            let generation = old.map_or(1, |old| old.generation.saturating_add(1));
            group.desired.insert(
                package.to_owned(),
                GroupDesired {
                    version: desired.version.clone(),
                    generation,
                    write_id: ask.write_id.as_ref().map(|id| id.as_str().to_owned()),
                    replaced_version,
                },
            );
        }
        let keeps = |version: &Version| keep.contains(version);
        if let Some(kept) = release {
            let book = after.releases.entry(package.to_owned()).or_default();
            crate::releases::record(book, &desired.version, kept, &keeps);
        }
        if let Some(release) = desired.release() {
            let mut registry = after.digests_of(package);
            registry.record(&desired.version, release, keeps);
            after.digests.insert(package.to_owned(), registry);
        }
        Ok(Staged {
            store: self,
            _writer: writer,
            before,
            after,
        })
    }

    /// How the writes of `package` in the group `name` are recorded: its generation, the version
    /// its last change of version replaced and the write id of its last write; none for a package
    /// the group has no desired version for.
    pub(crate) fn group_record(&self, name: &GroupName, package: &str) -> Option<WriteRecord> {
        let held = self.lock();
        let wanted = held.host_groups.0.get(name)?.desired.get(package)?;
        Some(WriteRecord {
            generation: wanted.generation,
            replaced: wanted.replaced_version.clone().map(|version| Replaced {
                version,
                min_up: RoleLimits::default(),
            }),
            write_id: wanted
                .write_id
                .as_deref()
                .and_then(|id| WriteId::parse(id).ok()),
        })
    }

    /// Sets the desired version of `package` in the group `name` to `version`, as a rollback does:
    /// the group's generation is raised, the version it replaces is kept, and the write names no id.
    /// Nothing changes for a package the group has no desired version for.
    ///
    /// # Errors
    /// [`StoreError`], as [`Self::set`]: on [`StoreError::Io`] nothing changed.
    pub(crate) fn set_group_version(
        &self,
        name: &GroupName,
        package: &str,
        version: &Version,
    ) -> Result<Recorded, StoreError> {
        let writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let before = self.lock().clone();
        let mut after = before.clone();
        let wanted = after
            .host_groups
            .0
            .get_mut(name)
            .and_then(|group| group.desired.get_mut(package));
        wanted.into_iter().for_each(|wanted| {
            wanted.replaced_version = Some(std::mem::replace(&mut wanted.version, version.clone()));
            wanted.generation = wanted.generation.saturating_add(1);
            wanted.write_id = None;
        });
        self.commit_staged(Staged {
            store: self,
            _writer: writer,
            before,
            after,
        })
    }

    /// Whether the coordinator holds a host group: the mode in which hosts are placed by group.
    #[must_use]
    pub fn in_group_mode(&self) -> bool {
        !self.lock().host_groups.is_empty()
    }

    /// The desired state with the groups `edit` makes of the groups now, staged under the
    /// writers' lock and not committed, with what `edit` answers beside them. The edit runs
    /// under the lock, on the groups it replaces, so a check it makes holds however many
    /// callers there are.
    ///
    /// # Errors
    /// The edit's own error: nothing was staged.
    pub(crate) fn stage_host_groups<T, E>(
        &self,
        edit: impl FnOnce(&HostGroups) -> Result<(HostGroups, T), E>,
    ) -> Result<(Staged<'_>, T), E> {
        let writer = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let before = self.lock().clone();
        let mut after = before.clone();
        let (groups, made) = edit(&before.host_groups)?;
        after.host_groups = groups;
        let staged = Staged {
            store: self,
            _writer: writer,
            before,
            after,
        };
        Ok((staged, made))
    }
}
