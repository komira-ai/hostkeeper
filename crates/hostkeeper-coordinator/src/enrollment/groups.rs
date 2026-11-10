//! The host group an approval or an inventory record names (`docs/design/hostgroups.md` section
//! 1.2, and the messages of its section 7.2).
//!
//! A request names a group by its `host_group`, or else by its `role`, read as the name of a
//! group: the migration names groups after roles, so a client that names a role keeps placing
//! its host. A `role` beside a `host_group` must be the same name (`invalid_role`): a request is
//! never placed by one of two names it gives. The name must be a group the coordinator holds
//! (`unknown_host_group`); one that breaks the grammar of a group's name names none.
//!
//! A coordinator that holds no group places nothing by group: every approval and record is read
//! as before groups, by its role, and one that names a `host_group` is refused
//! `unknown_host_group`, since no group has the name.

use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;

use crate::desired::DesiredStore;
use crate::host_groups::{GroupName, HostGroups};

/// The host groups of `desired` an approval or a record names, now; `None` when it holds none,
/// and nothing is placed by group.
pub(crate) fn held_in(desired: &DesiredStore) -> Option<HostGroups> {
    Some(desired.host_groups()).filter(|groups| !groups.is_empty())
}

/// The host group a request names by `host_group`, or else by `role`, checked against `held`,
/// the groups the coordinator holds: `None` when it names neither.
///
/// # Errors
/// `invalid_role`: the role breaks the grammar of a name, or is another name than
/// `host_group`; `unknown_host_group`: the name is no group of `held`.
pub(crate) fn group_in(
    host_group: &str,
    role: &str,
    held: &HostGroups,
) -> Result<Option<GroupName>, EnrollmentRefusal> {
    let named = match (host_group, role) {
        ("", "") => return Ok(None),
        ("", role) => role
            .parse::<GroupName>()
            .map_err(|_| EnrollmentRefusal::InvalidRole)?,
        (group, role) if !role.is_empty() && role != group => {
            return Err(EnrollmentRefusal::InvalidRole);
        }
        (group, _) => group
            .parse::<GroupName>()
            .map_err(|_| EnrollmentRefusal::UnknownHostGroup)?,
    };
    if held.0.contains_key(&named) {
        Ok(Some(named))
    } else {
        Err(EnrollmentRefusal::UnknownHostGroup)
    }
}
