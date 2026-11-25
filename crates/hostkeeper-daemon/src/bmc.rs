//! The host's BMC as every heartbeat reports it (`HeartbeatRequest.bmc`): its IPv4 address and
//! its MAC, read from the BMC itself through the host's IPMI device ([`crate::ipmi::DevIpmi`],
//! `/dev/ipmi0`), in-band and with no BMC credential, as the enrollment claims read its MAC
//! ([`hostkeeper_enroll::bmc_lan`]). The coordinator reaches the BMC for the host's console at
//! that address, because the lab's DHCP server cannot reserve one: the address a BMC holds can
//! move, and its MAC cannot.
//!
//! The BMC is read at the first heartbeat, then again at the first heartbeat at least
//! [`REREAD_SECS`] after the last read (and after the host's clock went back), never at every
//! heartbeat: a BMC that does not answer holds a heartbeat up to a second for each question. A
//! read that finds nothing (no device, a daemon that is not root, a BMC that does not answer)
//! keeps the report of the read before, so a BMC busy for one read is not reported gone; a host
//! that never had one reports none. Only a Linux host has a reader ([`at_host`]).

use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enroll::{Ipmi, bmc_lan};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::{BmcReport, BmcSource, UnixClock};

use crate::ipmi::DevIpmi;

/// The least time between two reads of the BMC, in seconds: ten minutes, so an address its DHCP
/// lease moved reaches the coordinator within ten minutes and a heartbeat.
pub const REREAD_SECS: u64 = 600;

/// A read of the BMC: when, and the report it left.
#[derive(Clone, Debug)]
struct Read {
    at_unix: u64,
    report: Option<BmcReport>,
}

/// The host's BMC behind `ipmi`, read on `clock` as the module says.
pub struct HostBmc {
    ipmi: Arc<dyn Ipmi>,
    clock: Arc<dyn UnixClock>,
    last: Mutex<Option<Read>>,
}

impl HostBmc {
    /// The BMC behind `ipmi`, not yet read.
    pub fn new(ipmi: Arc<dyn Ipmi>, clock: Arc<dyn UnixClock>) -> HostBmc {
        HostBmc {
            ipmi,
            clock,
            last: Mutex::new(None),
        }
    }
}

impl BmcSource for HostBmc {
    fn bmc(&self) -> Option<BmcReport> {
        let now = self.clock.unix_secs();
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let due = last
            .as_ref()
            .is_none_or(|read| now < read.at_unix || now - read.at_unix >= REREAD_SECS);
        if due {
            let read = bmc_lan(self.ipmi.as_ref()).map(|lan| BmcReport {
                ip: lan.ip.to_string(),
                mac: lan.mac,
            });
            let kept = last.take().and_then(|before| before.report);
            *last = Some(Read {
                at_unix: now,
                report: read.or(kept),
            });
        }
        last.as_ref().and_then(|read| read.report.clone())
    }
}

/// The BMC every heartbeat of a host of `kind` reports: on Linux (under systemd or OpenRC) the
/// host's, behind [`DevIpmi::host`], read on `clock`; none on a Mac, which has no BMC.
pub fn at_host(kind: PlatformKind, clock: Arc<dyn UnixClock>) -> Option<Arc<dyn BmcSource>> {
    match kind {
        PlatformKind::Linux | PlatformKind::Openrc => {
            Some(Arc::new(HostBmc::new(Arc::new(DevIpmi::host()), clock)))
        }
        PlatformKind::Macos => None,
    }
}

#[cfg(test)]
#[path = "bmc_tests.rs"]
mod tests;
