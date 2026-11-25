//! What a host's daemon reported of the serial console baseline it keeps on the host's port
//! (docs/design/console-broker.md, section 5.2; `SerialConsoleReport` in `agent.proto`).
//!
//! The last [`agent::SerialConsoleReport`] of each daemon is kept, in memory only: after a restart
//! the coordinator holds none until the next heartbeat of each host, which carries it again. It is
//! read from any heartbeat over gRPC that carries one, with no feature to announce (like the BMC
//! report), and a heartbeat that carries none leaves the one kept as it is. A report whose `port`
//! is not `ttyS<n>` or `ttyAMA<n>` (`n` from 0 to 99, [`SerialConsole::parse`]) or whose `state`
//! is over 32 bytes is dropped, counted and logged as a dropped fact, and the report kept before
//! stays; a `state` outside the closed set is kept as `other` and a `detail` over 128 bytes is cut
//! at a character boundary. `Hosts.List` and `Hosts.Get` show the report in the host's heartbeat
//! ([`Coordinator::show_serial_console_report`]); a retired host's report is forgotten.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use hostkeeper_config::serial_console::SerialConsole;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_wire::HostId;

use crate::identity::Identity;
use crate::repository_push::cut;
use crate::service::Coordinator;

/// The name a serial console report that breaks a rule is counted under.
pub(crate) const FACT: &str = "serial_console";

/// The most bytes of a report's `state`.
const MAX_STATE: usize = 32;

/// The most bytes of a report's `detail`.
const MAX_DETAIL: usize = 128;

/// The states of a report: the closed set of `SerialConsoleReport.state`.
const STATES: &[&str] = &["applied", "reboot_pending", "failed", "unsupported"];

/// The last report of each host, under a lock of its own that holds nothing else.
#[derive(Debug, Default)]
pub(crate) struct SerialConsoleReports {
    seen: Mutex<BTreeMap<HostId, agent::SerialConsoleReport>>,
}

impl SerialConsoleReports {
    /// The last report of the host `id`; none for a host that never sent one.
    pub(crate) fn report_of(&self, id: &HostId) -> Option<agent::SerialConsoleReport> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Notes `report` as the host `id`'s last.
    fn note(&self, id: &HostId, report: agent::SerialConsoleReport) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.clone(), report);
    }

    /// Forgets the host `id`'s report, when the host is retired.
    pub(crate) fn forget(&self, id: &HostId) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
}

/// The report `said` holds, or `None` when it breaks a rule: a `port` that is not a serial port
/// or a `state` over [`MAX_STATE`] bytes. A `state` outside [`STATES`] is kept as `other`, and the
/// `detail` is cut to [`MAX_DETAIL`] bytes at a character boundary.
fn kept(said: &agent::SerialConsoleReport) -> Option<agent::SerialConsoleReport> {
    if said.state.len() > MAX_STATE || SerialConsole::parse(&said.port).is_err() {
        return None;
    }
    let state = if STATES.contains(&said.state.as_str()) {
        said.state.clone()
    } else {
        "other".to_owned()
    };
    Some(agent::SerialConsoleReport {
        port: said.port.clone(),
        state,
        detail: cut(&said.detail, MAX_DETAIL).to_owned(),
    })
}

impl Coordinator {
    /// Notes the serial console report a heartbeat over gRPC from `who` carries, if it carries
    /// one; a report that breaks a rule is counted and logged as a dropped fact, and the one read
    /// before is kept, as is the one read before when the heartbeat carries none.
    pub(crate) fn hear_serial_console_report(
        &self,
        who: &Identity,
        request: &agent::HeartbeatRequest,
    ) {
        let Some(said) = &request.serial_console else {
            return;
        };
        match kept(said) {
            Some(report) => self.serial_console_reports.note(&who.host_id, report),
            None => {
                self.note_extras_dropped(who, "the serial console report breaks a rule", &[FACT]);
            }
        }
    }

    /// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id`, with the last
    /// serial console report of its daemon, in its heartbeat.
    pub(crate) fn show_serial_console_report(&self, id: &HostId, host: &mut operator::Host) {
        if let Some(report) = self.serial_console_reports.report_of(id) {
            host.heartbeat.get_or_insert_default().serial_console = Some(report);
        }
    }
}

#[cfg(test)]
#[path = "serial_console_report_tests.rs"]
mod tests;
