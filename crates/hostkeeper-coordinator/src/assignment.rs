//! Which services each host runs: the coordinator's assignment of a service to roles and hosts,
//! and the minimum version that applies to a host (design 4.3 and 5.7). Pure: no I/O, no clock.
//!
//! The rollout file is the base: a service is assigned to the hosts of the roles it lists, or to
//! the hosts its selector selects. The operator's entries (`Services.Assign` and
//! `Services.Unassign`), kept in the ops file ([`crate::ops::OpsState::assignments`]), are
//! applied on top: an entry of the host first, then an entry of the host's role, then the base
//! ([`assigned`]). An entry includes its scope or takes the service off it ([`Entry`]), so one
//! host can be added to a role's assignment, or taken out of it. A role entry is matched against
//! the host's role at the time of the decision, so a host whose role changes follows the new one.
//!
//! The minimum version that applies to a host is its own, else its role's, as an operator set
//! them (`Services.SetMinVersion`), and never below the rollout file's `min_version`
//! ([`effective_min_version`]).
//!
//! An `Unassign` with `uninstall` records, for each host that stops being assigned and lists the
//! service, an uninstall intent ([`ServiceAssignments::uninstall`]): the rollout gate orders it
//! `uninstall` until the host reports the service gone. No other change records one
//! ([`apply`]).

use std::collections::BTreeMap;

use hostkeeper_wire::version::Version;
use serde::{Deserialize, Serialize};

use crate::services::ServiceDef;

/// The most entries (of roles, hosts, minimum versions and uninstall intents) the ops file keeps
/// across every service. Raise it here if a farm needs more.
pub const MAX_ENTRIES: usize = 4096;

/// What an operator's entry does to its scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    /// The service is assigned to the scope.
    Include,
    /// The service is taken off the scope, though the rollout file or a role assigns it.
    Exclude,
}

/// The operator's assignments of one service.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceAssignments {
    /// The entries of roles, by role name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, Entry>,
    /// The entries of hosts, by node id; they stand before the role's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hosts: BTreeMap<String, Entry>,
    /// The minimum versions set for roles, by role name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub role_min_versions: BTreeMap<String, Version>,
    /// The minimum versions set for hosts, by node id; they stand before the role's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub host_min_versions: BTreeMap<String, Version>,
    /// The hosts ordered to uninstall the service, by node id, each with the generation of the
    /// `Unassign` that ordered it: its order id is `uninstall-<generation>`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub uninstall: BTreeMap<String, u64>,
    /// How many `Unassign` calls with `uninstall` have ordered hosts to uninstall the service,
    /// so that no two of their orders share an id. Written only once it is not zero, and never
    /// dropped after.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uninstall_generation: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Whether `assignments` holds nothing, so that the ops file need not name them: the file then
/// keeps the shape the release before reads.
#[must_use]
pub fn none(assignments: &BTreeMap<String, ServiceAssignments>) -> bool {
    assignments.values().all(ServiceAssignments::is_empty)
}

impl ServiceAssignments {
    /// Whether it holds nothing, so the ops file need not keep it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// How many entries it holds, as [`MAX_ENTRIES`] counts them.
    #[must_use]
    pub fn len(&self) -> usize {
        self.roles.len()
            + self.hosts.len()
            + self.role_min_versions.len()
            + self.host_min_versions.len()
            + self.uninstall.len()
    }

    /// Drops every entry of the host `node_id`: its own assignment or exclusion, its minimum
    /// version, and an uninstall it is ordered; a role's entries, which the host's role matches,
    /// stay. What a retired host leaves (`Hosts.Retire`).
    pub fn forget_host(&mut self, node_id: &str) {
        self.hosts.remove(node_id);
        self.host_min_versions.remove(node_id);
        self.uninstall.remove(node_id);
    }

    /// The order id of the uninstall the host `node_id` is ordered, if it is.
    #[must_use]
    pub fn uninstall_order(&self, node_id: &str) -> Option<String> {
        self.uninstall
            .get(node_id)
            .map(|generation| format!("uninstall-{generation}"))
    }
}

/// A host as an assignment reads it: its node id, its role now, and the labels an operator gave
/// it.
#[derive(Clone, Copy, Debug)]
pub struct Placement<'a> {
    /// The node id.
    pub node_id: &'a str,
    /// Its role, from its certificate; empty for a host whose role the coordinator does not know.
    pub role: &'a str,
    /// Its labels, if it has any.
    pub labels: Option<&'a BTreeMap<String, String>>,
}

/// Whether the rollout file alone assigns `def` to `host`: its selector selects the host, or,
/// without one, it lists the host's role.
#[must_use]
pub fn by_rollout(def: &ServiceDef, host: &Placement<'_>) -> bool {
    match &def.selector {
        Some(selector) => selector.selects(host.role, host.labels),
        None => def.has_role(host.role),
    }
}

/// Whether `def` is assigned to `host`: the host's own entry in `entries`, else its role's, else
/// the rollout file ([`by_rollout`]). A service made for a host group ([`crate::group_scope`]) is
/// assigned to the hosts of its roles alone, whatever the entries say, and its role is its group's
/// name, which the fleet records as the role of a host in group mode.
#[must_use]
pub fn assigned(
    def: &ServiceDef,
    entries: Option<&ServiceAssignments>,
    host: &Placement<'_>,
) -> bool {
    if def.scope.is_some() {
        return by_rollout(def, host);
    }
    let entry = entries.and_then(|e| e.hosts.get(host.node_id).or_else(|| e.roles.get(host.role)));
    match entry {
        Some(Entry::Include) => true,
        Some(Entry::Exclude) => false,
        None => by_rollout(def, host),
    }
}

/// The minimum version of `def` that applies to `host`: its own, else its role's, as `entries`
/// hold them, and never below the rollout file's `min_version`; none when there is none.
#[must_use]
pub fn effective_min_version<'a>(
    def: &'a ServiceDef,
    entries: Option<&'a ServiceAssignments>,
    host: &Placement<'_>,
) -> Option<&'a Version> {
    let set = entries.and_then(|e| {
        e.host_min_versions
            .get(host.node_id)
            .or_else(|| e.role_min_versions.get(host.role))
    });
    match (set, def.min_version.as_ref()) {
        (Some(set), Some(floor)) if !set.at_least(floor) => Some(floor),
        (set, floor) => set.or(floor),
    }
}

/// Where an assignment or a minimum version applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// One host, by node id.
    Host(String),
    /// Every host of a role, those that join it later included.
    Role(String),
}

impl Scope {
    /// The scope as the audit says it: `host d1` or `role storage`.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Host(id) => format!("host {id}"),
            Self::Role(role) => format!("role {role}"),
        }
    }
}

/// A host the coordinator knows, as a change of assignment reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Known {
    /// The node id.
    pub node_id: String,
    /// Its role; empty when the coordinator does not know it.
    pub role: String,
    /// The labels an operator gave it.
    pub labels: Option<BTreeMap<String, String>>,
    /// Whether its last heartbeat listed the service (or the coordinator cannot tell).
    pub lists: bool,
}

impl Known {
    /// The host as [`assigned`] reads it.
    #[must_use]
    pub fn placement(&self) -> Placement<'_> {
        Placement {
            node_id: &self.node_id,
            role: &self.role,
            labels: self.labels.as_ref(),
        }
    }
}

/// A change of a service's assignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// Assign the service to the scope.
    Assign(Scope),
    /// Take it off the scope; with `uninstall`, order the hosts that stop being assigned and list
    /// it to uninstall it.
    Unassign {
        /// Where.
        scope: Scope,
        /// Whether the hosts that stop being assigned are ordered to uninstall it.
        uninstall: bool,
    },
}

/// Why a change was refused: `Unassign` of a scope the service is not assigned to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the service is not assigned there")]
pub struct NotAssigned;

/// The host `node_id` among `known`, or one whose role the coordinator does not know.
fn placement_of<'a>(known: &'a [Known], node_id: &'a str) -> Placement<'a> {
    known.iter().find(|k| k.node_id == node_id).map_or(
        Placement {
            node_id,
            role: "",
            labels: None,
        },
        Known::placement,
    )
}

/// Whether the role entry of `role`, else the rollout file's roles, assign `def` to the role.
fn role_assigned(def: &ServiceDef, entries: &ServiceAssignments, role: &str) -> bool {
    entries
        .roles
        .get(role)
        .map_or(def.roles.contains(role), |entry| *entry == Entry::Include)
}

/// Makes `change` to the assignments `entries` of `def`, given the hosts the coordinator knows
/// (`known`, the host of a host scope among them), and answers the hosts of `known` that stop
/// being assigned. An `Unassign` with `uninstall` records an uninstall intent for each of those
/// that lists the service, under a new generation; any change drops the intent of a host it
/// assigns again.
///
/// A host is assigned by its own entry, so that it keeps the service when its role is taken off
/// it; a role entry that the rollout file makes needless is not written. Assigning a scope
/// already assigned changes no decision.
///
/// # Errors
/// [`NotAssigned`]: `Unassign` of a host or a role the service is not assigned to; nothing is
/// changed.
pub fn apply(
    def: &ServiceDef,
    entries: &mut ServiceAssignments,
    change: &Change,
    known: &[Known],
) -> Result<Vec<String>, NotAssigned> {
    let before: Vec<bool> = known
        .iter()
        .map(|k| assigned(def, Some(entries), &k.placement()))
        .collect();
    let uninstall = match change {
        Change::Assign(Scope::Host(id)) => {
            entries.hosts.insert(id.clone(), Entry::Include);
            false
        }
        Change::Assign(Scope::Role(role)) => {
            entries.roles.remove(role);
            if def.selector.is_some() || !def.roles.contains(role) {
                entries.roles.insert(role.clone(), Entry::Include);
            }
            false
        }
        Change::Unassign {
            scope: Scope::Host(id),
            uninstall,
        } => {
            let host = placement_of(known, id);
            if !assigned(def, Some(entries), &host) {
                return Err(NotAssigned);
            }
            entries.hosts.remove(id);
            if assigned(def, Some(entries), &host) {
                entries.hosts.insert(id.clone(), Entry::Exclude);
            }
            *uninstall
        }
        Change::Unassign {
            scope: Scope::Role(role),
            uninstall,
        } => {
            if !role_assigned(def, entries, role) {
                return Err(NotAssigned);
            }
            entries.roles.remove(role);
            if def.roles.contains(role) {
                entries.roles.insert(role.clone(), Entry::Exclude);
            }
            *uninstall
        }
    };
    let mut stopped = Vec::new();
    let mut targets = Vec::new();
    for (host, was) in known.iter().zip(before) {
        if assigned(def, Some(entries), &host.placement()) {
            entries.uninstall.remove(&host.node_id);
        } else if was {
            stopped.push(host.node_id.clone());
            if host.lists {
                targets.push(host.node_id.clone());
            }
        }
    }
    if uninstall && !targets.is_empty() {
        entries.uninstall_generation += 1;
        for target in targets {
            entries
                .uninstall
                .insert(target, entries.uninstall_generation);
        }
    }
    Ok(stopped)
}

/// Sets the minimum version of the scope in `entries` to `version`, or removes the one an
/// operator set for it when `version` is none.
pub fn set_min_version(entries: &mut ServiceAssignments, scope: &Scope, version: Option<Version>) {
    let (versions, key) = match scope {
        Scope::Host(id) => (&mut entries.host_min_versions, id),
        Scope::Role(role) => (&mut entries.role_min_versions, role),
    };
    match version {
        Some(version) => {
            versions.insert(key.clone(), version);
        }
        None => {
            versions.remove(key);
        }
    }
}

/// The roles `def` is assigned to: the rollout file's, in its order, but those an operator took
/// it off, then those an operator assigned it to, by name.
#[must_use]
pub fn roles_of(def: &ServiceDef, entries: Option<&ServiceAssignments>) -> Vec<String> {
    let entry = |role: &str| entries.and_then(|e| e.roles.get(role)).copied();
    let base = def
        .role_names()
        .into_iter()
        .filter(|role| entry(role) != Some(Entry::Exclude))
        .map(str::to_owned);
    let added = entries
        .into_iter()
        .flat_map(|e| e.roles.iter())
        .filter(|(role, entry)| **entry == Entry::Include && !def.roles.contains(role.as_str()))
        .map(|(role, _)| role.clone());
    base.chain(added).collect()
}
