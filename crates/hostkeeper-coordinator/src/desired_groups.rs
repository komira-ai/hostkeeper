//! The host groups in the desired state store ([`crate::host_groups`]): a copy of the groups
//! now, one group, whether any is held, and the desired state a change of the groups makes,
//! staged under the writers' lock for a call that commits it with its audit entries and the
//! operations state's marks ([`crate::ops::OpsStore::decide_changes_with`]).

use std::sync::PoisonError;

use super::{DesiredStore, Staged};
use crate::host_groups::{GroupName, HostGroup, HostGroups};

impl DesiredStore {
    /// The host groups now, by name.
    pub(crate) fn host_groups(&self) -> HostGroups {
        self.lock().host_groups.clone()
    }

    /// The group `name` now.
    pub(crate) fn host_group(&self, name: &GroupName) -> Option<HostGroup> {
        self.lock().host_groups.0.get(name).cloned()
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
