//! Maintenance updates (docs/design/maintenance-updates.md): the report a host's daemon sends of
//! what is due on its host, which a daemon that announces [`FEATURE`] carries in every heartbeat
//! (`HeartbeatRequest.maintenance` over gRPC, the key `maintenance` over the JSON route, standard
//! padded base64 of the protobuf encoding). The coordinator advertises the feature
//! ([`crate::features::FEATURES`]) and sends a `maintain` order only to a daemon that announces it
//! ([`crate::decide_maintenance`]).
//!
//! # What is kept
//!
//! A report is kept ([`Kept`]) when it decodes and keeps every bound of
//! `hostkeeper_proto::maintenance::check_report`. [`Coordinator::settle_maintenance`] reads each
//! heartbeat before it is answered: a heartbeat of a daemon that does not announce the feature
//! carries none; a report that does not decode or breaks a bound is counted and logged as a
//! dropped fact ([`FACT`], [`Coordinator::note_extras_dropped`]) and the host's last report is
//! kept; a heartbeat that carries none keeps the last one too. The last report is the one this
//! coordinator's fleet holds of the host, which is the host's own last kept report, or one another
//! coordinator kept and this one folded in. The report is kept in the host's
//! node ([`crate::fleet::Node::maintenance`]), so it is stored with the host's last heartbeat and
//! a coordinator that folds that heartbeat in holds it as well. A coordinator that starts holds
//! none until each host's next heartbeat carries one.
//!
//! # What is shown
//!
//! `Hosts.List` and `Hosts.Get` show the report ([`show_maintenance`]) in `Host.maintenance` with
//! its age, and `maintenance_stale` when its last look is 0, older than [`STALE_AFTER_SECS`] or more
//! than [`AHEAD_SECS`] ahead of the coordinator's clock: an empty `due` in a stale report does not
//! mean nothing is due.

use std::sync::PoisonError;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::maintenance::check_report;
use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::Heartbeat;
use prost::Message;

use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::service::Coordinator;

/// The feature a daemon announces to report its host's maintenance report
/// (`HeartbeatRequest.maintenance`) and to take the `maintain` order; the coordinator advertises
/// it ([`crate::features::FEATURES`]).
pub const FEATURE: &str = "maintenance-v1";

/// The name a maintenance report that cannot be kept is counted under.
pub(crate) const FACT: &str = "maintenance";

/// How many seconds after its last look a report is stale.
pub const STALE_AFTER_SECS: u64 = 7200;

/// How many seconds a last look may be ahead of the coordinator's clock and not be stale: a
/// daemon whose clock is further ahead says nothing of how old its look is.
pub const AHEAD_SECS: u64 = 300;

/// A maintenance report the coordinator keeps: one that decoded and kept every bound.
#[derive(Clone, Debug, PartialEq)]
pub struct Kept(agent::MaintenanceReport);

// A report holds no float, so equality is total.
impl Eq for Kept {}

impl Kept {
    /// The report.
    #[must_use]
    pub fn report(&self) -> &agent::MaintenanceReport {
        &self.0
    }

    /// Whether something is due: a listed item, or one the daemon found and left out of the list.
    #[must_use]
    pub fn is_due(&self) -> bool {
        !self.0.due.is_empty() || self.0.due_left_out > 0
    }

    /// The report as the heartbeat's JSON key carries it.
    pub(crate) fn text(&self) -> String {
        text_of(&self.0)
    }
}

/// The standard padded base64 of the protobuf encoding of `report`, the text of the JSON key.
pub(crate) fn text_of(report: &agent::MaintenanceReport) -> String {
    STANDARD.encode(report.encode_to_vec())
}

/// The report in `text`, when it is base64 of a report that keeps every bound.
fn decode(text: &str) -> Option<Kept> {
    let bytes = STANDARD.decode(text).ok()?;
    let report = agent::MaintenanceReport::decode(bytes.as_slice()).ok()?;
    check_report(&report).ok()?;
    Some(Kept(report))
}

/// Whether `beat` announces [`FEATURE`].
fn announces(beat: &Heartbeat) -> bool {
    beat.agent_features.iter().any(|name| name == FEATURE)
}

/// The report `beat` carries, when its agent announces [`FEATURE`] and the report is one that is
/// kept.
pub(crate) fn kept_of(beat: &Heartbeat) -> Option<Kept> {
    if !announces(beat) {
        return None;
    }
    beat.maintenance.as_deref().and_then(decode)
}

impl Coordinator {
    /// Settles the report of `beat`, a heartbeat of `who` as it was read, before it is answered:
    /// none when its agent does not announce [`FEATURE`]; as it is when it is a report that is
    /// kept; otherwise (none carried, or one that does not decode or breaks a bound, which is
    /// counted and logged as a dropped fact) the text of the host's last kept report, if the
    /// fleet holds one.
    pub(crate) fn settle_maintenance(&self, who: &Identity, beat: &mut Heartbeat) {
        if !announces(beat) {
            beat.maintenance = None;
            return;
        }
        match beat.maintenance.as_deref() {
            Some(text) if decode(text).is_some() => return,
            Some(_) => {
                self.note_extras_dropped(who, "the maintenance report breaks a rule", &[FACT]);
            }
            None => {}
        }
        let fleet = self.fleet.lock().unwrap_or_else(PoisonError::into_inner);
        beat.maintenance = fleet
            .node(&who.host_id)
            .and_then(|node| node.maintenance.as_ref())
            .map(Kept::text);
    }
}

/// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id` (`fleet` is the
/// fleet as they read it, `unix_now` the coordinator's Unix time), with the report kept of its
/// daemon (only a daemon that announced [`FEATURE`] has one, [`kept_of`]): the report, the seconds
/// since its last look (none when the look is 0, 0 when it is ahead of `unix_now`) and whether it
/// is stale: a look of 0, more than [`STALE_AFTER_SECS`] old, or more than [`AHEAD_SECS`] ahead.
pub(crate) fn show_maintenance(
    fleet: &Fleet,
    id: &HostId,
    host: &mut operator::Host,
    unix_now: u64,
) {
    let Some(kept) = fleet.node(id).and_then(|node| node.maintenance.as_ref()) else {
        return;
    };
    let looked = kept.report().looked_unix;
    host.maintenance = Some(kept.report().clone());
    host.maintenance_look_age_secs = (looked != 0).then(|| unix_now.saturating_sub(looked));
    host.maintenance_stale = looked == 0
        || unix_now.saturating_sub(looked) > STALE_AFTER_SECS
        || looked.saturating_sub(unix_now) > AHEAD_SECS;
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
pub(crate) mod tests;
