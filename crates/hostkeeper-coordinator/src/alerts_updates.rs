//! The alert `updates_due` (`docs/design/maintenance-updates.md`, section 4): the hosts whose last
//! maintenance report has something due, counted by host group.
//!
//! A host counts while the report the fleet keeps for it (which only a daemon that announces
//! `maintenance-v1` has, [`crate::maintenance::kept_of`]) lists a due item that counts as due in
//! its group ([`crate::maintenance_mac::counts_as_due`]: a major upgrade of macOS only in a group
//! whose `macos_major` is true) or says some were left out, or while the coordinator finds `xcode`
//! due on it ([`crate::maintenance_xcode`],
//! [`Due::xcode`]). One alert stands for each host group that has such hosts (and one for those in
//! no group), and it ends when the beats of its hosts report nothing due. A report that is stale
//! still counts what it lists.
//!
//! The groups are read from the enrollment store before the fleet is
//! ([`Coordinator::groups_of_reports`]), for every host with a kept report: a host whose first
//! report came in between the two reads is not in them, and is counted at the next observation
//! rather than in no group.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_wire::HostId;

use super::{Alert, Kind, Said, Subject};
use crate::fleet::{Fleet, Node};
use crate::maintenance_mac::counts_as_due;
use crate::service::Coordinator;

/// What the alert reads besides the fleet.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Due {
    /// The host group of each heard host with a kept maintenance report
    /// ([`Coordinator::groups_of_reports`]); none while the coordinator holds no host group.
    pub(crate) groups: Option<BTreeMap<HostId, String>>,
    /// The hosts the coordinator finds `xcode` due on ([`Coordinator::xcode_due_hosts`]).
    pub(crate) xcode: BTreeSet<HostId>,
    /// The groups of `groups` whose maintenance table allows a major upgrade of macOS
    /// (`macos_major`).
    pub(crate) macos_major: BTreeSet<String>,
}

/// Whether the kept report of `node` has updates due in a group whose `macos_major` is
/// `macos_major`: an item that counts as due there ([`counts_as_due`]), or items left out.
fn is_due(node: &Node, macos_major: bool) -> bool {
    node.maintenance.as_ref().is_some_and(|kept| {
        let report = kept.report();
        report.due_left_out > 0
            || report
                .due
                .iter()
                .any(|item| counts_as_due(item, macos_major))
    })
}

/// The `updates_due` alerts: the hosts of `fleet` that are due (by their report, read under their
/// group's `macos_major`, or `xcode` by `due`), counted by their group in `due`'s groups, one
/// alert for each group in group order.
/// Without groups (a coordinator that holds no host group) every host is in none; with them, a
/// host they map to the empty name is in none and a host they do not name is not counted.
pub(super) fn updates_due(fleet: &Fleet, due: &Due) -> Vec<Alert> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let groups = due.groups.as_ref();
    for (id, node) in fleet.nodes() {
        let group = match groups {
            None => "",
            Some(groups) => match groups.get(id) {
                Some(group) => group.as_str(),
                None => continue,
            },
        };
        if is_due(node, due.macos_major.contains(group)) || due.xcode.contains(id) {
            *counts.entry(group).or_default() += 1;
        }
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
                said: Said::default(),
            }
        })
        .collect()
}

impl Coordinator {
    /// What the alert `updates_due` reads besides the fleet ([`Due`]).
    pub(super) fn due_facts(&self) -> Due {
        let groups = self.groups_of_reports();
        let macos_major = groups
            .iter()
            .flat_map(BTreeMap::values)
            .filter(|group| self.group_policy(group).maintenance.macos_major)
            .cloned()
            .collect();
        Due {
            groups,
            xcode: self.xcode_due_hosts(),
            macos_major,
        }
    }

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
