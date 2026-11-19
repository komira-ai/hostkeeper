//! Where the coordinator reaches a Linux server's BMC: the addresses it knows for the BMC, in the
//! order it tries them ([`Coordinator::bmc_addresses`]), and the calls that try them in turn
//! ([`through_addresses`]). The lab's DHCP server cannot reserve an address, so a BMC's address
//! is found, not configured:
//!
//! 1. The console targets file (`--console-targets-file`), when it names the BMC's MAC, overrides
//!    every other source: its address alone is tried.
//! 2. Otherwise the address the host's daemon last reported in a heartbeat
//!    (`HeartbeatRequest.bmc`, kept with the host's enrollment so that every coordinator sharing
//!    the store finds it), when the MAC it reported is the inventory record's BMC MAC. While the
//!    host is not heartbeating (no heartbeat in the last [`crate::fleet::DOWN_AFTER`] that this
//!    coordinator knows of), the address is the last known one and is marked stale: the BMC's
//!    lease may have moved since.
//! 3. Then the coordinator's own neighbour (ARP) table, when it holds a complete entry for the
//!    BMC's MAC at another address ([`Neighbors`], read from [`PROC_NET_ARP`]). Nothing is probed
//!    to fill it: it holds a BMC the coordinator's host spoke to recently.
//!
//! No source is trusted for what is at the address: before every command, the BMC there must have
//! the record's BMC MAC ([`hostkeeper_console::bmc`]). With more than one address, the MAC is first
//! checked at each in turn, in a session that sends nothing else, and the call is made once, at
//! the first whose BMC answers with the record's MAC ([`through_addresses`]): a command whose
//! answer was lost is never sent again elsewhere. A host with no address at all is `no_address`.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_console::bmc::BmcError;
use hostkeeper_console::targets::{Address, Targets};
use hostkeeper_enrollment::enroll::EnrolledHost;
use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;

use crate::service::Coordinator;

/// The Linux kernel's neighbour table of IPv4, as text.
pub const PROC_NET_ARP: &str = "/proc/net/arp";

/// The address the coordinator's neighbour table holds for a MAC, if it holds one.
pub type Neighbors = Arc<dyn Fn(&MacAddress) -> Option<IpAddr> + Send + Sync>;

/// The neighbour table in the file at `path`, in the format of [`PROC_NET_ARP`], read afresh at
/// each lookup; a file that cannot be read holds no entry.
#[must_use]
pub fn arp_table(path: &Path) -> Neighbors {
    let path = PathBuf::from(path);
    Arc::new(move |mac: &MacAddress| {
        let text = std::fs::read_to_string(&path).ok()?;
        arp_entry(&text, mac)
    })
}

/// The address of the first complete entry for `mac` in `text`, a neighbour table in the format
/// of [`PROC_NET_ARP`]: a heading line, then one line per entry of the address, the hardware
/// type, the flags (`0x2`, complete, set for an entry whose MAC is known), the MAC, the mask and
/// the device. An incomplete entry, or a line that does not read, is passed over.
pub(crate) fn arp_entry(text: &str, mac: &MacAddress) -> Option<IpAddr> {
    text.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [ip, _kind, flags, hardware, ..] = fields.as_slice() else {
            return None;
        };
        let flags = u32::from_str_radix(flags.trim_start_matches("0x"), 16).ok()?;
        let complete = flags & 0x2 != 0;
        let same = MacAddress::parse(hardware).ok() == Some(*mac);
        (complete && same).then(|| ip.parse().ok()).flatten()
    })
}

/// Where an address was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The console targets file.
    TargetsFile,
    /// The host's daemon's last report.
    Heartbeat,
    /// The coordinator's neighbour table.
    NeighborTable,
}

/// An address of a host's device, and where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) address: Address,
    pub(crate) source: Source,
    /// A heartbeat's address while the host is not heartbeating.
    pub(crate) stale: bool,
    /// For a heartbeat's address, when it was first reported (unix seconds); 0 otherwise.
    pub(crate) reported_unix: u64,
}

impl Found {
    /// An address of `source` that is never stale.
    pub(crate) fn of(address: Address, source: Source) -> Found {
        Found {
            address,
            source,
            stale: false,
            reported_unix: 0,
        }
    }

    /// The answer's account of it.
    pub(crate) fn to_proto(&self) -> proto::ConsoleAddress {
        proto::ConsoleAddress {
            address: self.address.to_string(),
            source: match self.source {
                Source::TargetsFile => proto::ConsoleAddressSource::TargetsFile,
                Source::Heartbeat => proto::ConsoleAddressSource::Heartbeat,
                Source::NeighborTable => proto::ConsoleAddressSource::NeighborTable,
            }
            .into(),
            stale: self.stale,
            reported_unix: self.reported_unix,
        }
    }
}

impl Coordinator {
    /// The addresses of the BMC whose MAC is `mac`, the BMC of the enrolled `host` whose id is
    /// `host_id`, in the order the module says; empty when none is known.
    pub(crate) fn bmc_addresses(
        &self,
        host_id: &HostId,
        host: &EnrolledHost,
        mac: MacAddress,
        targets: &Targets,
    ) -> Vec<Found> {
        if let Some(address) = targets.address_of(&mac) {
            return vec![Found::of(address.clone(), Source::TargetsFile)];
        }
        let mut found = Vec::new();
        if let Some(kept) = host.bmc.filter(|kept| kept.bmc.mac == mac) {
            let now = (self.clock)();
            let live = self
                .lock_fleet()
                .node(host_id)
                .is_some_and(|node| node.is_live(now));
            if !live {
                tracing::info!(host_id = %host_id, address = %kept.bmc.ip, "the host is not heartbeating: its BMC's last reported address may be stale");
            }
            found.push(Found {
                address: Address::Ip(kept.bmc.ip.to_canonical()),
                source: Source::Heartbeat,
                stale: !live,
                reported_unix: kept.at_unix,
            });
        }
        let neighbor = (self.console.neighbors)(&mac)
            .map(|ip| Address::Ip(ip.to_canonical()))
            .filter(|address| found.iter().all(|f| f.address != *address));
        found.extend(neighbor.map(|address| Found::of(address, Source::NeighborTable)));
        found
    }
}

/// Runs `call` once, at the first of `addresses` where the BMC answers with the record's MAC, and
/// answers its answer with the address that gave it. With one address, `call` runs there (it
/// checks the MAC itself). With more, `check` is made at each in turn, as the module says, before
/// `call`: a command is sent once, never again at another address after its answer was lost.
///
/// # Errors
/// `call`'s error; or, when no address passes `check`, [`BmcError::AddressMismatch`] if each BMC
/// answered with another MAC, else [`BmcError::Unreachable`]; or the first other error of `check`
/// (a refused credential, a busy BMC), which ends the tries.
pub(crate) fn through_addresses<'a, T>(
    host_id: &str,
    addresses: impl IntoIterator<Item = &'a Found>,
    check: impl Fn(&Address) -> Result<(), BmcError>,
    call: impl FnOnce(&Address) -> Result<T, BmcError>,
) -> Result<(T, &'a Found), BmcError> {
    let addresses: Vec<&Found> = addresses.into_iter().collect();
    let found = match addresses.as_slice() {
        [only] => *only,
        more => reached(host_id, more, check)?,
    };
    call(&found.address).map(|answer| (answer, found))
}

/// The first of `addresses` that passes `check`, as [`through_addresses`] says.
fn reached<'a>(
    host_id: &str,
    addresses: &[&'a Found],
    check: impl Fn(&Address) -> Result<(), BmcError>,
) -> Result<&'a Found, BmcError> {
    let mut failed = BmcError::AddressMismatch;
    for found in addresses {
        match check(&found.address) {
            Ok(()) => return Ok(found),
            Err(why @ (BmcError::AddressMismatch | BmcError::Unreachable)) => {
                tracing::info!(%host_id, address = %found.address, cause = %why, "the BMC was not reached at one of its addresses");
                if why == BmcError::Unreachable {
                    failed = why;
                }
            }
            Err(why) => return Err(why),
        }
    }
    Err(failed)
}

#[cfg(test)]
#[path = "address_tests.rs"]
mod tests;
