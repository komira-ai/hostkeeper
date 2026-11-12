//! The start's checks of the listen addresses, kept apart so that `config.rs` stays short.

use std::net::{IpAddr, SocketAddr};

use clap::Args;

use super::{Serve, StartError};

/// The flags of a LAN listener bound on every interface, as a Swarm task's is.
#[derive(Clone, Debug, Args)]
pub struct LanFlags {
    /// Take a wildcard --lan-listen (`0.0.0.0` or `[::]`): the LAN listener then listens on
    /// every interface of the coordinator's network namespace, such as a Swarm task's on an
    /// overlay network whose port is published on the host in `mode=host`. The listener's
    /// address is not the one daemons dial, so --lan-address names that one. Off unless given;
    /// an address of one interface is taken as without it. Requires --lan-listen and
    /// --lan-address.
    #[arg(long, requires_all = ["lan_listen", "lan_address"])]
    pub lan_listen_wildcard: bool,
    /// The address daemons dial the LAN listener at, the host's LAN address (an IPv4 address
    /// written as IPv6 is named as IPv4): the server certificate names it in place of the
    /// address of --lan-listen. Not a wildcard address. Requires --lan-listen-wildcard.
    #[arg(long, requires = "lan_listen_wildcard")]
    pub lan_address: Option<IpAddr>,
}

impl LanFlags {
    /// The IP address the server certificate names for the LAN listener: `--lan-address` when
    /// given, else the address of `lan_listen`, either as IPv4 when it is an IPv4 address.
    pub(super) fn named(&self, lan_listen: Option<SocketAddr>) -> Option<IpAddr> {
        self.lan_address
            .or_else(|| lan_listen.map(|lan| lan.ip()))
            .map(|ip| ip.to_canonical())
    }
}

/// Refuses the listen addresses `flags` name that the start does not take: a wildcard
/// `--private-listen` unless `--private-listen-wildcard` is given, a wildcard `--lan-listen` unless `--lan-listen-wildcard` is given, a wildcard `--lan-address`, a wildcard `--mdm-listen` unless `--mdm-listen-wildcard` is given,
/// and a `--health-listen` that is not a loopback address. An IPv4 address written as IPv6 is
/// judged as the IPv4 address it is.
pub(super) fn check_listen(flags: &Serve) -> Result<(), StartError> {
    if !flags.private_listen_wildcard && flags.private_listen.ip().to_canonical().is_unspecified() {
        return Err(StartError::PrivateListen(flags.private_listen));
    }
    if let Some(lan) = flags
        .lan_listen
        .filter(|lan| !flags.lan.lan_listen_wildcard && lan.ip().to_canonical().is_unspecified())
    {
        return Err(StartError::LanListen(lan));
    }
    if let Some(address) = flags
        .lan
        .lan_address
        .filter(|address| address.to_canonical().is_unspecified())
    {
        return Err(StartError::LanAddress(address));
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
