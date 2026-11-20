//! The alert `updates_due` (`docs/design/maintenance-updates.md`, section 4): the hosts whose last
//! maintenance report has something due, counted by host group.
//!
//! A host counts while the report the fleet keeps for it (which only a daemon that announces
//! `maintenance-v1` has, [`crate::maintenance::kept_of`]) lists a due item or says some were left
//! out. One alert stands for each host group that has such hosts (and one for those in no group),
//! and it ends when the beats of its hosts report nothing due. A report that is stale still counts
//! what it lists.
//!
//! The groups are read from the enrollment store before the fleet is
//! ([`Coordinator::groups_of_reports`]), for every host with a kept report: a host whose first
//! report came in between the two reads is not in them, and is counted at the next observation
//! rather than in no group.

use std::collections::BTreeMap;

use hostkeeper_wire::HostId;

use super::{Alert, Kind, Subject};
use crate::fleet::{Fleet, Node};
use crate::maintenance::Kept;
use crate::service::Coordinator;

/// Whether the kept report of `node` has updates due.
fn is_due(node: &Node) -> bool {
    node.maintenance.as_ref().is_some_and(Kept::is_due)
}

/// The `updates_due` alerts: the hosts of `fleet` that are due, counted by their group in
/// `groups`, one alert for each group in group order. Without `groups` (a coordinator that holds no
/// host group) every host is in none; with them, a host they map to the empty name is in none and
/// a host they do not name is not counted.
pub(super) fn updates_due(fleet: &Fleet, groups: Option<&BTreeMap<HostId, String>>) -> Vec<Alert> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (id, _) in fleet.nodes().filter(|(_, node)| is_due(node)) {
        let group = match groups {
            None => "",
            Some(groups) => match groups.get(id) {
                Some(group) => group.as_str(),
                None => continue,
            },
        };
        *counts.entry(group).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(group, count)| {
            let hosts = if count == 1 { "host" } else { "hosts" };
            let place = if group.is_empty() {
                "in no host group".to_owned()
            } else {
                format!("of {group}")
            };
            Alert {
                subject: Subject {
                    group: group.to_owned(),
                    ..Subject::of(Kind::UpdatesDue)
                },
                message: format!("Updates due on {count} {hosts} {place}"),
            }
        })
        .collect()
}

impl Coordinator {
    /// The host group the enrollment store records for each host of the fleet that keeps a
    /// maintenance report, the empty name for a host in none; none while the coordinator holds no
    /// host group.
    pub(super) fn groups_of_reports(&self) -> Option<BTreeMap<HostId, String>> {
        if self.desired.host_groups().is_empty() {
            return None;
        }
        let kept: Vec<HostId> = self
            .lock_fleet()
            .nodes()
            .filter(|(_, node)| node.maintenance.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        Some(
            kept.into_iter()
                .map(|id| {
                    let group = self
                        .membership_of(&id)
                        .map(|(group, _)| group.to_string())
                        .unwrap_or_default();
                    (id, group)
                })
                .collect(),
        )
    }
}

#[cfg(test)]
#[path = "alerts_updates_tests.rs"]
mod tests;
