//! The BMC a Linux server's daemon reports in every heartbeat over gRPC (`HeartbeatRequest.bmc`):
//! the BMC's address and MAC, read from the BMC itself in-band. It is how the console finds a BMC
//! whose DHCP lease can move ([`crate::console`]).
//!
//! A report that keeps the rules of `BmcReport` ([`read`]) is kept with the host's enrollment
//! ([`hostkeeper_enrollment::enroll::EnrollmentStore::note_bmc`], written when the address or the
//! MAC changes), so that every coordinator sharing the store finds the BMC's last known address,
//! after a restart and while the host is down. A report that breaks a rule is counted and logged
//! as a dropped fact, and the one kept before stays; a heartbeat with none leaves the one kept as
//! it is. A retiring host's heartbeat is not read for it, nor a duplicate host's (two daemons on
//! one host id, each with its own BMC, would write the record in turn at every heartbeat). Nothing in the report is trusted for
//! what is at the address: the console checks there that the BMC has the inventory record's BMC
//! MAC before every command.

use std::net::IpAddr;

use hostkeeper_enrollment::enroll::HostBmc;
use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;

use crate::identity::Identity;
use crate::service::Coordinator;

/// The name a report that breaks a rule is counted under.
pub(crate) const FACT: &str = "bmc";

/// The BMC `report` names, when it keeps the rules of `BmcReport`: its `ip` is an IPv4 address in
/// dotted decimal that a BMC can be reached at (not `0.0.0.0`, a loopback, link-local, multicast
/// or broadcast address), and its `mac` a MAC address that is not all zeros.
pub(crate) fn read(report: &agent::BmcReport) -> Option<HostBmc> {
    let ip = report.ip.parse::<std::net::Ipv4Addr>().ok()?;
    let unusable = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast();
    let ip = IpAddr::from(ip);
    let mac = MacAddress::parse(&report.mac).ok()?;
    (!unusable && mac.octets() != [0; 6]).then_some(HostBmc { ip, mac })
}

impl Coordinator {
    /// Notes the BMC a heartbeat over gRPC from `who` reports, if it reports one, as the module
    /// says.
    pub(crate) fn hear_bmc(&self, who: &Identity, request: &agent::HeartbeatRequest) {
        let Some(report) = &request.bmc else {
            return;
        };
        let Some(bmc) = read(report) else {
            self.note_extras_dropped(who, "the BMC report breaks a rule", &[FACT]);
            return;
        };
        if let Some(enrollment) = self.enrollment()
            && !enrollment.is_duplicate(&who.host_id)
        {
            enrollment.note_bmc(&who.host_id, bmc);
        }
    }
}

#[cfg(test)]
#[path = "bmc_report_tests.rs"]
mod tests;
