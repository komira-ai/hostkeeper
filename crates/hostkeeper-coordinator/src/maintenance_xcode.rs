//! Whether `xcode` is due on a Mac, and what a `maintain` with `xcode` carries
//! (docs/design/maintenance-updates.md, sections 1, 1.1, 2.4, 5.1 and 6). A daemon lists no `xcode`
//! item: it reports the Xcodes installed (`MaintenanceReport.xcodes`) and the coordinator decides,
//! from them, the group's `xcode_keep` and `xcode_default`, and what its Xcode store holds
//! ([`Plan`]).
//!
//! - **Kept:** the newest `xcode_keep` releases the store holds, newest first; none when the
//!   group's `xcode_keep` is 0 (the group leaves Xcode alone) or the store holds none.
//! - **The default:** `latest` is the newest kept release; a version is the kept release of that
//!   version, and a version no kept release has is none (`xcode-select` is left as it is).
//! - **Installed:** a kept release counts as installed only in its own directory,
//!   `Xcode-<version>.app` (not `foreign`) with its build, since that is the one the daemon
//!   installs and passes over.
//! - **Due** on a Mac (its last heartbeat names `darwin-arm64`) whose report the coordinator keeps,
//!   when a kept release is not installed, or `xcode-select` does not name the installed default.
//!   The item ([`Plan::due`]) goes from the version `xcode-select` names (empty: none) to the
//!   newest kept release missing, else to the default, its label the build. So a Mac becomes due
//!   only once the store holds the release it lacks.
//! - **Removed:** each version installed in its own directory that is older than every kept
//!   release. A `foreign` Xcode (one a person, the App Store or device management put there) is
//!   never removed, nor one of a kept version.
//!
//! `Hosts.List` and `Hosts.Get` show the item in `Host.maintenance.due`
//! ([`Coordinator::show_xcode_due`]), the alert `updates_due` counts the Mac
//! ([`Coordinator::xcode_due_hosts`]), `Maintenance.Schedule` puts `xcode` in the Mac's kinds due,
//! and the order of its turn carries the kept releases' entries (version, build, size, SHA-256),
//! the versions to remove and the default ([`crate::decide_maintenance`]). A store that cannot be
//! read makes nothing due and is logged; a turn that holds `xcode` is then not started, nor its
//! `RUN` sent again, until it can be (an operator's act on a halted turn still goes).

use std::collections::BTreeSet;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::maintenance::MAX_DUE;
use hostkeeper_wire::{HostId, Platform};

use crate::fleet::Node;
use crate::maintenance_policy::{Maintenance, XcodeDefault};
use crate::orders::XcodeOrder;
use crate::service::Coordinator;
use crate::xcode_feed::XcodeVersion;
use crate::xcode_store::Archive;

/// What a group's Xcode settings and the store make of one Mac.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The kept releases, newest first; empty: Xcode is left alone.
    pub(crate) kept: Vec<Archive>,
    /// The kept release `xcode-select` is to name; none: leave it.
    pub(crate) default: Option<Archive>,
    /// The versions to remove, oldest first.
    pub(crate) remove: Vec<String>,
    /// The due item; none: nothing of Xcode is due.
    pub(crate) due: Option<agent::DueItem>,
}

/// Whether `installed` is `archive` in its own directory.
fn is_installed(installed: &agent::InstalledXcode, archive: &Archive) -> bool {
    !installed.foreign
        && installed.version == archive.id.version.as_str()
        && installed.build == archive.id.build
}

/// The plan of a Mac that reports `installed`, in a group whose maintenance table is `table`, by
/// the store's `archives` (in any order): as the module says.
pub(crate) fn plan(
    table: &Maintenance,
    archives: &[Archive],
    installed: &[agent::InstalledXcode],
) -> Plan {
    let mut newest_first = archives.to_vec();
    newest_first.sort_by(|a, b| b.id.cmp(&a.id).then_with(|| a.sha256.cmp(&b.sha256)));
    let keep = usize::try_from(table.xcode_keep).unwrap_or(usize::MAX);
    newest_first.truncate(keep);
    let kept = newest_first;
    let Some(oldest) = kept.last() else {
        return Plan::default();
    };
    let default = match &table.xcode_default {
        XcodeDefault::Latest => kept.first(),
        XcodeDefault::Version(version) => kept
            .iter()
            .find(|archive| archive.id.version.as_str() == version),
    }
    .cloned();
    let mut older: Vec<XcodeVersion> = installed
        .iter()
        .filter(|xcode| !xcode.foreign)
        .filter_map(|xcode| XcodeVersion::parse(&xcode.version).ok())
        .filter(|version| *version < oldest.id.version)
        .collect();
    older.sort();
    older.dedup();
    let remove = older.iter().map(|v| v.as_str().to_owned()).collect();
    let selected = installed
        .iter()
        .find(|xcode| xcode.selected)
        .map_or("", |xcode| xcode.version.as_str());
    let missing = kept
        .iter()
        .find(|archive| !installed.iter().any(|xcode| is_installed(xcode, archive)));
    let unselected = default.as_ref().filter(|archive| {
        !installed
            .iter()
            .any(|xcode| xcode.selected && is_installed(xcode, archive))
    });
    let due = missing.or(unselected).map(|archive| agent::DueItem {
        kind: i32::from(agent::MaintenanceKind::Xcode),
        from: selected.to_owned(),
        to: archive.id.version.as_str().to_owned(),
        needs_reboot: false,
        label: archive.id.build.clone(),
        security: false,
    });
    Plan {
        kept,
        default,
        remove,
        due,
    }
}

impl Plan {
    /// What a `maintain` with `xcode` carries of the plan; none when it keeps no release.
    pub(crate) fn order(&self) -> Option<XcodeOrder> {
        (!self.kept.is_empty()).then(|| XcodeOrder {
            entries: self
                .kept
                .iter()
                .map(|archive| agent::XcodeEntry {
                    version: archive.id.version.as_str().to_owned(),
                    build: archive.id.build.clone(),
                    size: archive.size,
                    sha256: archive.sha256.clone(),
                })
                .collect(),
            remove: self.remove.clone(),
            default: self
                .default
                .as_ref()
                .map(|archive| archive.id.version.as_str().to_owned()),
        })
    }
}

/// The `xcode` item due on the host `node` of a group whose maintenance table is `table`, by the
/// store's `archives`: none but on a Mac whose report the coordinator keeps.
pub(crate) fn due_of(
    node: &Node,
    table: &Maintenance,
    archives: &[Archive],
) -> Option<agent::DueItem> {
    if node.platform != Platform::DarwinArm64 {
        return None;
    }
    let report = node.maintenance.as_ref()?.report();
    plan(table, archives, &report.xcodes).due
}

/// `report` with the item `due` added: listed while the report lists fewer than [`MAX_DUE`]
/// items, else counted as left out.
fn add_due(report: &mut agent::MaintenanceReport, due: agent::DueItem) {
    if report.due.len() < MAX_DUE {
        report.due.push(due);
    } else {
        report.due_left_out = report.due_left_out.saturating_add(1);
    }
}

impl Coordinator {
    /// What the Xcode store holds, in any order: nothing without a store; none, logged, when it
    /// cannot be read.
    pub(crate) fn xcode_archives(&self) -> Option<Vec<Archive>> {
        let Some(store) = &self.xcode.store else {
            return Some(Vec::new());
        };
        match store.archives() {
            Ok(archives) => Some(archives),
            Err(why) => {
                tracing::warn!(%why, "the Xcode store cannot be read: no Mac is shown due an Xcode, and no turn with xcode starts");
                None
            }
        }
    }

    /// Adds to each of `hosts` (as `Hosts.List` and `Hosts.Get` made them, their `host_group`
    /// named) that is a Mac with a kept report its `xcode` item, when one is due ([`due_of`]).
    pub(crate) fn show_xcode_due(&self, hosts: &mut [proto::Host]) {
        if !hosts.iter().any(|host| host.maintenance.is_some()) {
            return;
        }
        let Some(archives) = self.xcode_archives() else {
            return;
        };
        let fleet = self.lock_fleet();
        for host in hosts {
            let Some(report) = host.maintenance.as_mut() else {
                continue;
            };
            let table = self.group_policy(&host.host_group).maintenance;
            let node = host.node_id.parse::<HostId>().ok();
            let due = node
                .and_then(|id| fleet.node(&id))
                .and_then(|node| due_of(node, &table, &archives));
            if let Some(due) = due {
                add_due(report, due);
            }
        }
    }

    /// The hosts `xcode` is due on ([`due_of`]), each read in the group the enrollment store
    /// records for it: none while the coordinator holds no host group, as no group then keeps
    /// an Xcode.
    pub(crate) fn xcode_due_hosts(&self) -> BTreeSet<HostId> {
        if self.desired.host_groups().is_empty() {
            return BTreeSet::new();
        }
        let Some(archives) = self.xcode_archives() else {
            return BTreeSet::new();
        };
        let macs: Vec<HostId> = self
            .lock_fleet()
            .nodes()
            .filter(|(_, node)| node.platform == Platform::DarwinArm64)
            .filter(|(_, node)| node.maintenance.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        macs.into_iter()
            .filter(|id| {
                let group = self
                    .membership_of(id)
                    .map(|(group, _)| group.to_string())
                    .unwrap_or_default();
                let table = self.group_policy(&group).maintenance;
                let fleet = self.lock_fleet();
                fleet
                    .node(id)
                    .and_then(|node| due_of(node, &table, &archives))
                    .is_some()
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "maintenance_xcode_tests.rs"]
mod tests;
