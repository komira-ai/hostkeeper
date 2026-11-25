//! The start's checks of the listen addresses, kept apart so that `config.rs` stays short.

use std::net::{IpAddr, SocketAddr};

use clap::Args;

use crate::proxy_protocol::ProxySource;

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
    /// A DNS name daemons dial the LAN listener by, such as `coordinator.internal.example.com`:
    /// the server certificate names each one given as a DNS name, beside the IP address it
    /// names. Repeatable, so a name can be added before daemons move to it. Lowercase letters,
    /// digits, `-` and `.`; at most 253 bytes, each label 1 to 63 and not starting or ending
    /// with `-`; no wildcard, no IP address, no trailing dot. A certificate that names other
    /// names is made again at start. Requires --lan-listen.
    #[arg(long = "lan-name", requires = "lan_listen")]
    pub lan_names: Vec<String>,
}

/// The opt-in of a health listener bound on every interface, as a Swarm task's is for the edge's
/// check.
#[derive(Clone, Debug, Args)]
pub struct HealthFlags {
    /// Take a wildcard --health-listen (`0.0.0.0` or `[::]`): the health listener then listens on
    /// every interface of the coordinator's network namespace. Give it only in a namespace whose
    /// interfaces are loopback and one private network, such as a Swarm task's on one overlay
    /// network, where the edge checks `/readyz` at an address not known before the task starts.
    /// The listener answers with no token and no TLS, and says only `ok` or a reason token. Off
    /// unless given; a loopback address or the address of one interface is taken as without it.
    /// Requires --health-listen.
    #[arg(long, requires = "health_listen")]
    pub health_listen_wildcard: bool,
}

/// The proxies whose connections to the heartbeat listener start with a PROXY protocol header.
#[derive(Clone, Debug, Args)]
pub struct ProxyFlags {
    /// A source of the heartbeat listener's connections that sends a PROXY protocol header
    /// (version 1 or 2) before the TLS handshake, such as a TCP passthrough in front of the
    /// listener: an IP address, or a network written `ADDRESS/BITS` (not `/0`). Repeatable. A
    /// connection from one of them must start with a header, and the address the coordinator
    /// records for the host that heartbeats on it, and dials for its SSH tunnels, is the
    /// header's source; one without a header is closed. A connection from any other address is
    /// read as TLS from its first byte, so a header it sends fails its handshake. The LAN
    /// listener reads no header. Without it, no connection's header is read.
    #[arg(long = "heartbeat-proxy-protocol-from", value_name = "ADDRESS[/BITS]")]
    pub heartbeat_proxy_protocol_from: Vec<ProxySource>,
}

/// Why `--lan-name` takes `name` as no DNS name, as the start's refusal states it, or `Ok`.
fn lan_name_refusal(name: &str) -> Option<&'static str> {
    let bare = name.strip_suffix('.');
    if bare.is_some() {
        Some("has a trailing dot; write the name without it")
    } else if name.is_empty() {
        Some("is empty")
    } else if name.contains('*') {
        Some("is a wildcard; name each host")
    } else if name.parse::<IpAddr>().is_ok() {
        Some("is an IP address; --lan-address or --lan-listen names the address")
    } else if name.len() > 253 {
        Some("is longer than 253 bytes")
    } else if name.split('.').any(str::is_empty) {
        Some("has an empty label")
    } else if name.split('.').any(|label| label.len() > 63) {
        Some("has a label longer than 63 bytes")
    } else if name
        .split('.')
        .any(|label| label.starts_with('-') || label.ends_with('-'))
    {
        Some("has a label that starts or ends with '-'")
    } else if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        Some("has a character other than a-z, 0-9, '-' and '.'")
    } else {
        None
    }
}

impl LanFlags {
    /// The IP address the server certificate names for the LAN listener: `--lan-address` when
    /// given, else the address of `lan_listen`, either as IPv4 when it is an IPv4 address.
    pub(super) fn named(&self, lan_listen: Option<SocketAddr>) -> Option<IpAddr> {
        self.lan_address
            .or_else(|| lan_listen.map(|lan| lan.ip()))
            .map(|ip| ip.to_canonical())
    }

    /// The DNS names the server certificate names: `server_names` (`--server-name`) and then
    /// each `--lan-name` not among them, so a name given to both is named once.
    pub(super) fn dns_names(&self, server_names: &[String]) -> Vec<String> {
        let mut names = server_names.to_vec();
        for name in &self.lan_names {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }
}

/// Refuses the listen addresses `flags` name that the start does not take: a wildcard
/// `--private-listen` unless `--private-listen-wildcard` is given, a wildcard `--lan-listen` unless `--lan-listen-wildcard` is given, a wildcard `--lan-address`, a `--lan-name` that is no DNS name,
/// and a wildcard `--health-listen` unless `--health-listen-wildcard` is given. An IPv4 address
/// written as IPv6 is judged as the IPv4 address it is.
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
    if let Some((name, reason)) = flags
        .lan
        .lan_names
        .iter()
        .find_map(|name| lan_name_refusal(name).map(|reason| (name, reason)))
    {
        return Err(StartError::LanName {
            name: name.clone(),
            reason,
        });
    }
    if let Some(health) = flags.health_listen.filter(|health| {
        !flags.health.health_listen_wildcard && health.ip().to_canonical().is_unspecified()
    }) {
        return Err(StartError::HealthListen(health));
    }
    Ok(())
}
