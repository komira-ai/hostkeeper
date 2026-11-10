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
use crate::intents::CANCELLED;
use crate::ops::{AuditEntry, OpsState};
use crate::reconcile::COORDINATOR;

/// A package, and the hosts to uninstall it from, by node id.
pub(crate) type Wanted = (String, Vec<String>);

/// For each of `packages`, in the order given, the hosts among `hosts` whose last heartbeat
/// lists it; a package no host lists is left out.
pub(crate) fn listed_by<'a>(
    fleet: &Fleet,
    packages: impl Iterator<Item = &'a String>,
    hosts: &[HostId],
) -> Vec<Wanted> {
    packages
        .filter_map(|package| {
            let listing: Vec<String> = hosts
                .iter()
                .filter(|host| fleet.lists(host, package))
                .map(ToString::to_string)
                .collect();
            (!listing.is_empty()).then(|| (package.clone(), listing))
        })
        .collect()
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
/// `hosts`.
pub(crate) fn cancel_uninstalls(
    state: &mut OpsState,
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
                cancelled.push(entry);
            }
        }
    }
    state.assignments.retain(|_, entries| !entries.is_empty());
    cancelled
}
