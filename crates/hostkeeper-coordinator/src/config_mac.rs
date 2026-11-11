//! The start's check of the Mac serial allowlist (`--mac-serial-allowlist`) against the
//! coordinator it is read for ([`check_allowlist`]).

use super::StartError;
use crate::enrollment::MacAllowlist;
use crate::host_groups::GroupName;
use crate::service::Coordinator;

/// Refuses `allowlist` for `coordinator`, as it is at the start. The coordinator places a Mac in
/// its entry's host group (`docs/design/hostgroups.md` section 1.2), whether or not it holds a
/// group yet: an entry that names services is refused, and so is one whose group it does not
/// hold.
///
/// # Errors
/// [`StartError::MacServicesRetired`] naming the serial, [`StartError::MacHostGroup`] naming the
/// group.
pub(super) fn check_allowlist(
    allowlist: &MacAllowlist,
    coordinator: &Coordinator,
) -> Result<(), StartError> {
    if let Some(serial) = allowlist.naming_services() {
        return Err(StartError::MacServicesRetired(serial.to_owned()));
    }
    let held = coordinator.desired.host_groups();
    let unheld = allowlist.groups().find(|name| {
        name.parse::<GroupName>()
            .map_or(true, |name| !held.0.contains_key(&name))
    });
    match unheld {
        Some(name) => Err(StartError::MacHostGroup(name.to_owned())),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "config_mac_tests.rs"]
mod tests;
