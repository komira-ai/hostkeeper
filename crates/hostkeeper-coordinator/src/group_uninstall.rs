//! The uninstall intents a change of a host group writes and cancels: the intents of
//! `OpsState.assignments` (`uninstall`, `uninstall_generation`), the ones an `Unassign` with
//! `uninstall` writes, so that their order ids, their settling at the host's heartbeat and their
//! audit stay what they are ([`crate::intents`]).
//!
//! A change writes the intents of the packages a host runs and its group no longer lists, all
//! in one commit. Each package's intents are given a generation above every generation any
//! package has been given, the first package of the change the lowest: the order the caller
//! gives the packages in is the order of the generations, and a host's intents are carried out
//! lowest first, so a package listed after the one it configures goes first.

use hostkeeper_wire::HostId;

use crate::fleet::Fleet;
use crate::group_scope::effective;
use crate::host_groups::{GroupName, HostGroups};
use crate::intents::CANCELLED;
use crate::ops::{AuditEntry, OpsState};
use crate::reconcile::COORDINATOR;

/// A package, and the hosts to uninstall it from, by node id.
pub(crate) type Wanted = (String, Vec<String>);

/// The packages a host in the group `own` (none: in no specific group) runs, by name, in the
/// order it is offered them ([`crate::group_scope::effective`]).
pub(crate) fn runs(groups: &HostGroups, own: Option<&GroupName>) -> Vec<String> {
    effective(groups, own)
        .into_iter()
        .map(|(_, _, package)| package.to_owned())
        .collect()
}

/// A host, the specific group it is in before a change, and the one it is in after.
pub(crate) type Placed = (HostId, Option<GroupName>, Option<GroupName>);

/// For each of `members`, the packages it runs in its group before, under `before`, and not in its
/// group after, under `after`, the hosts among them whose last heartbeat lists it: each package
/// once, in the order it first goes, which is the reverse of the order it was offered in.
pub(crate) fn lost_by(
    fleet: &Fleet,
    before: &HostGroups,
    after: &HostGroups,
    members: &[Placed],
) -> Vec<Wanted> {
    let mut wanted: Vec<Wanted> = Vec::new();
    for (host, from, to) in members {
        let kept = runs(after, to.as_ref());
        for package in runs(before, from.as_ref()).iter().rev() {
            if kept.contains(package) || !fleet.lists(host, package) {
                continue;
            }
            match wanted.iter_mut().find(|(name, _)| name == package) {
                Some((_, hosts)) => hosts.push(host.to_string()),
                None => wanted.push((package.clone(), vec![host.to_string()])),
            }
        }
    }
    wanted
}

/// Records an uninstall intent of each package of `wanted` for each of its hosts, the packages'
/// generations rising in the order given, above every generation the state has given.
pub(crate) fn order_uninstalls(state: &mut OpsState, wanted: &[Wanted]) {
    let mut generation = state
        .assignments
        .values()
        .map(|entries| entries.uninstall_generation)
        .max()
        .unwrap_or(0);
    for (package, hosts) in wanted {
        generation += 1;
        let entries = state.assignments.entry(package.clone()).or_default();
        entries.uninstall_generation = generation;
        for host in hosts {
            entries.uninstall.insert(host.clone(), generation);
        }
    }
}

/// Drops the uninstall intents of `hosts` for `packages`, and answers the audit entries of those
/// dropped, `uninstall_cancelled` by the coordinator, in the order of `packages` and then of
/// `hosts`; each names `group`, the group the change that cancels it is of.
pub(crate) fn cancel_uninstalls(
    state: &mut OpsState,
    group: &GroupName,
    packages: &[String],
    hosts: &[String],
) -> Vec<AuditEntry> {
    let mut cancelled = Vec::new();
    for package in packages {
        let Some(entries) = state.assignments.get_mut(package) else {
            continue;
        };
        for host in hosts {
            if entries.uninstall.remove(host).is_some() {
                let mut entry = AuditEntry::new(COORDINATOR, CANCELLED);
                entry.service = Some(package.clone());
                entry.node_id = Some(host.clone());
                entry.host_group = Some(group.to_string());
                cancelled.push(entry);
            }
        }
    }
    state.assignments.retain(|_, entries| !entries.is_empty());
    cancelled
}
