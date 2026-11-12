//! The start's checks of the listen addresses, kept apart so that `config.rs` stays short.

use super::{Serve, StartError};

/// Refuses the listen addresses `flags` name that the start does not take: a wildcard
/// `--private-listen` unless `--private-listen-wildcard` is given, a wildcard `--lan-listen`, a wildcard `--mdm-listen` unless `--mdm-listen-wildcard` is given,
/// and a `--health-listen` that is not a loopback address. An IPv4 address written as IPv6 is
/// judged as the IPv4 address it is.
pub(super) fn check_listen(flags: &Serve) -> Result<(), StartError> {
    if !flags.private_listen_wildcard && flags.private_listen.ip().to_canonical().is_unspecified() {
        return Err(StartError::PrivateListen(flags.private_listen));
    }
    if let Some(lan) = flags
        .lan_listen
        .filter(|lan| lan.ip().to_canonical().is_unspecified())
    {
        return Err(StartError::LanListen(lan));
    }
    if let Some(mdm) = flags
        .mdm
        .mdm_listen
        .filter(|mdm| !flags.mdm.mdm_listen_wildcard && mdm.ip().to_canonical().is_unspecified())
    {
        return Err(StartError::MdmListen(mdm));
    }
    if let Some(health) = flags
        .health_listen
        .filter(|health| !health.ip().to_canonical().is_loopback())
    {
        return Err(StartError::HealthListen(health));
    }
    Ok(())
}
