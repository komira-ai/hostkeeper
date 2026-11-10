//! The start's check of the Mac serial allowlist (`--mac-serial-allowlist`) against the
//! coordinator it is read for ([`check_allowlist`]).

use super::StartError;
use crate::enrollment::MacAllowlist;
use crate::enrollment::groups::held_in;
use crate::host_groups::GroupName;
use crate::service::Coordinator;

/// Refuses `allowlist` for `coordinator`, as it is at the start. A coordinator that holds host
/// groups (`docs/design/hostgroups.md` section 1.2) places a Mac in its entry's group: an
/// entry that names services is refused, and so is one whose group it does not hold. One that
/// holds none approves a Mac by its entry's role and assigns its services: each must be one it
/// manages.
///
/// # Errors
/// [`StartError::MacServicesRetired`] naming the serial, [`StartError::MacHostGroup`] naming the
/// group, [`StartError::MacService`] naming the service.
pub(super) fn check_allowlist(
    allowlist: &MacAllowlist,
    coordinator: &Coordinator,
) -> Result<(), StartError> {
    let Some(held) = held_in(&coordinator.desired) else {
        return match allowlist
            .services()
            .find(|service| coordinator.services.get(service.as_str()).is_none())
        {
            Some(unknown) => Err(StartError::MacService(unknown.as_str().to_owned())),
            None => Ok(()),
        };
    };
    if let Some(serial) = allowlist.naming_services() {
        return Err(StartError::MacServicesRetired(serial.to_owned()));
    }
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
