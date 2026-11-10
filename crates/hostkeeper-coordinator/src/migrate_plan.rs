//! The plan of `host-groups migrate` (`docs/design/hostgroups.md` section 10): a pure function of
//! the whole store ([`Moved`]), the rollout file and the flags that says the groups, their
//! policies, desired versions, marks, minimum versions, repositories, holds, the records that
//! name a group, the audit entries, and every error that makes `--apply` refuse. It reads and
//! writes nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use hostkeeper_enrollment::enroll::records::{Snapshot, pb};

use crate::assignment::{Placement, assigned};
use crate::audit_file::AuditLine;
use crate::desired::RoleLimits;
use crate::enrollment::MacAllowlist;
use crate::force::{ForceRecord, ForceTargets, SELF_SERVICE};
use crate::group_scope::{higher, package_def};
use crate::host_groups::{GroupDesired, GroupMarks, GroupName, HostGroup};
use crate::migrate_policy::{Limits, merge, policy_text};
use crate::ops::{AUDIT_RING, AuditEntry, OpsState};
use crate::rollouts::Rollout;
use crate::services::{DEFAULT_REPOSITORY, Services};
use crate::store::Loaded;
use crate::store::postgres::transfer::Moved;

/// Who the migration's changes are by, in the groups and in the audit.
pub(crate) const MIGRATION: &str = "migration";

/// The most holds the ops state keeps.
const MAX_HOLDS: usize = crate::holds::MAX_HOLDS;

/// What the plan is made from.
pub(crate) struct Inputs<'a> {
    /// The whole store, as it is now.
    pub(crate) moved: &'a Moved,
    /// The rollout file.
    pub(crate) rollout: &'a Services,
    /// The Mac serial allowlist, when one is given.
    pub(crate) allowlist: Option<&'a MacAllowlist>,
    /// `serve`'s `--live-rollouts` as it will be given: said in the report only.
    pub(crate) live_rollouts: u32,
    /// The time of the groups and the audit lines.
    pub(crate) now: u64,
}

/// What the migration would write, and what it says about it.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    /// The report, a line each, without its header and its last lines.
    pub(crate) lines: Vec<String>,
    /// Every error; `--apply` writes nothing while there is one.
    pub(crate) errors: Vec<String>,
    /// The store as it is after the migration.
    pub(crate) after: Moved,
    /// The hosts and requests whose record changed, and no other record.
    pub(crate) changed: Snapshot,
    /// The audit lines the migration adds, in their order.
    pub(crate) audit: Vec<AuditLine>,
}

/// One group to make: the role it comes from, the packages its hosts run and its hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// The role its hosts have.
    pub(crate) role: String,
    /// The packages its hosts run today, with `hostkeeper`.
    pub(crate) set: BTreeSet<String>,
    /// The node ids of its hosts, sorted.
    pub(crate) hosts: Vec<String>,
}

/// An enrolled host with the set of packages placed on it today.
struct Placed<'a> {
    host: &'a pb::Host,
    set: BTreeSet<String>,
}

/// The first group name that [`GroupName`] takes of `text`.
fn group_name(text: &str) -> Option<GroupName> {
    GroupName::from_str(text).ok()
}

/// The packages placed on a host of role `role` and node id `node_id` today: the services of the
/// rollout file its entries, role and selector place there, and `hostkeeper`.
fn set_of(input: &Inputs<'_>, node_id: &str, role: &str) -> BTreeSet<String> {
    let ops = &input.moved.loaded.ops;
    let place = Placement {
        node_id,
        role,
        labels: ops.hosts.get(node_id).map(|host| &host.labels),
    };
    let mut set: BTreeSet<String> = input
        .rollout
        .defs()
        .filter(|def| assigned(def, ops.assignments.get(&def.name), &place))
        .map(|def| def.name.clone())
        .collect();
    set.insert(SELF_SERVICE.to_owned());
    set
}

/// The roles records name that no host of its own need have: an inventory record that names a
/// role and no group, an approved request not yet polled, a Mac allowlist entry.
fn named_roles(input: &Inputs<'_>, errors: &mut Vec<String>) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    let mut name = |what: String, role: &str| {
        if role.is_empty() {
            return;
        }
        if group_name(role).is_some() {
            named.insert(role.to_owned());
        } else {
            errors.push(format!("{what}: role {role:?} is not a group name"));
        }
    };
    for record in &input.moved.loaded.ops.inventory {
        if record.host_group.is_empty() {
            name(format!("inventory record {}", record.host_id), &record.role);
        }
    }
    for request in &input.moved.records.requests {
        if let Some(approval) = unpolled(request) {
            name(format!("approval of {}", approval.host_id), &approval.role);
        }
    }
    for (serial, entry) in input.allowlist.iter().flat_map(|list| list.entries()) {
        name(format!("allowlist entry {serial}"), entry.role.as_str());
    }
    named
}

/// The approval of `request` when it is approved and its host has not polled yet, and the
/// approval names no group.
fn unpolled(request: &pb::Request) -> Option<&pb::Approval> {
    let approval = request.approved.as_ref()?;
    (request.state == pb::RequestState::Approved as i32 && approval.host_group.is_empty())
        .then_some(approval)
}

/// Every role name the store, the rollout file and the allowlist use: the names a derived group
/// name must not be.
fn known_roles(
    input: &Inputs<'_>,
    named: &BTreeSet<String>,
    hosts: &[Placed<'_>],
) -> BTreeSet<String> {
    let ops = &input.moved.loaded.ops;
    let mut known: BTreeSet<String> = named.clone();
    known.extend(hosts.iter().map(|placed| placed.host.role.clone()));
    known.extend(
        input
            .rollout
            .defs()
            .flat_map(|def| def.roles.iter().cloned()),
    );
    for entries in ops.assignments.values() {
        known.extend(entries.roles.keys().cloned());
        known.extend(entries.role_min_versions.keys().cloned());
    }
    for hold in &ops.holds {
        if let crate::holds::HoldScope::Role(role) = &hold.scope {
            known.insert(role.clone());
        }
    }
    known
}

/// Each enrolled host that is not denied with the set placed on it today; a host with no role, or
/// whose role is not a group name, is an error.
fn placed_hosts<'a>(input: &'a Inputs<'_>, errors: &mut Vec<String>) -> Vec<Placed<'a>> {
    let records = &input.moved.records;
    let denied: BTreeSet<&str> = records
        .deny
        .iter()
        .filter(|deny| deny.state != pb::DenyState::Cleared as i32)
        .map(|deny| deny.host_id.as_str())
        .collect();
    let mut hosts: Vec<&pb::Host> = records
        .hosts
        .iter()
        .filter(|host| !denied.contains(host.host_id.as_str()))
        .collect();
    hosts.sort_by(|a, b| a.host_id.cmp(&b.host_id));
    let mut placed = Vec::new();
    for host in hosts {
        if host.role.is_empty() {
            errors.push(format!(
                "host {}: has no role, so no group takes it",
                host.host_id
            ));
        } else if group_name(&host.role).is_none() {
            errors.push(format!(
                "host {}: role {:?} is not a group name",
                host.host_id, host.role
            ));
        } else {
            placed.push(Placed {
                host,
                set: set_of(input, &host.host_id, &host.role),
            });
        }
    }
    placed
}

/// The groups: per role, the hosts partitioned by their set, the most common set named after the
/// role and each other `<role>-2`, `<role>-3` in the order of the node id of its first host; a
/// role that records name and no host has one group with no host.
fn layout(
    input: &Inputs<'_>,
    hosts: &[Placed<'_>],
    named: &BTreeSet<String>,
    known: &BTreeSet<String>,
    errors: &mut Vec<String>,
) -> BTreeMap<GroupName, Layout> {
    let mut by_role: BTreeMap<&str, BTreeMap<&BTreeSet<String>, Vec<String>>> = BTreeMap::new();
    for placed in hosts {
        by_role
            .entry(placed.host.role.as_str())
            .or_default()
            .entry(&placed.set)
            .or_default()
            .push(placed.host.host_id.clone());
    }
    let mut groups = BTreeMap::new();
    for (role, sets) in by_role {
        let mut sets: Vec<(&BTreeSet<String>, Vec<String>)> = sets.into_iter().collect();
        // The most hosts first; of equal counts, the set whose first host sorts first.
        sets.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.1[0].cmp(&b.1[0])));
        let mut rest = sets.split_off(1);
        rest.sort_by(|a, b| a.1[0].cmp(&b.1[0]));
        let common = sets.remove(0);
        let others = rest.into_iter().enumerate().map(|(i, set)| {
            let derived = format!("{role}-{}", i + 2);
            (derived, set)
        });
        let first = std::iter::once((role.to_owned(), common));
        for (name, (set, mut ids)) in first.chain(others) {
            ids.sort();
            let made = if name == role {
                group_name(&name)
            } else if known.contains(&name) {
                errors.push(format!(
                    "role {role}: the group name {name} is the name of a role"
                ));
                None
            } else {
                let made = group_name(&name);
                if made.is_none() {
                    errors.push(format!(
                        "role {role}: the group name {name} is not a group name (over 32 bytes)"
                    ));
                }
                made
            };
            if let Some(made) = made {
                groups.insert(
                    made,
                    Layout {
                        role: role.to_owned(),
                        set: set.clone(),
                        hosts: ids,
                    },
                );
            }
        }
    }
    for role in named {
        if let Some(name) = group_name(role).filter(|name| !groups.contains_key(name)) {
            groups.insert(
                name,
                Layout {
                    role: role.clone(),
                    set: set_of(input, "", role),
                    hosts: Vec::new(),
                },
            );
        }
    }
    groups
}

/// The packages of `set` in the order of the rollout file's `priority`, the higher first, then by
/// name; a package the file does not define has priority 0.
fn ordered(input: &Inputs<'_>, set: &BTreeSet<String>) -> Vec<String> {
    let mut packages: Vec<&String> = set.iter().collect();
    packages.sort_by_key(|name| {
        (
            std::cmp::Reverse(input.rollout.get(name).map_or(0, |def| def.priority)),
            (*name).clone(),
        )
    });
    packages.into_iter().cloned().collect()
}

/// The force `force` as it stands in `group`, whose hosts are `lay.hosts`: none when it orders no
/// host of the group.
fn force_in(force: &ForceRecord, lay: &Layout, group: &GroupName) -> Option<ForceRecord> {
    let here = |hosts: &[String]| -> Vec<String> {
        hosts
            .iter()
            .filter(|host| lay.hosts.contains(host))
            .cloned()
            .collect()
    };
    let hosts = here(&force.hosts);
    if hosts.is_empty() {
        return None;
    }
    let targets = match &force.targets {
        ForceTargets::Fleet => ForceTargets::Fleet,
        ForceTargets::Role(_) => ForceTargets::Role(group.to_string()),
        ForceTargets::Hosts(named) => ForceTargets::Hosts(here(named)),
    };
    Some(ForceRecord {
        hosts,
        targets,
        ..force.clone()
    })
}

/// The marks of `service` in `group`: what the service's own marks and its plan position say,
/// the force and the done hosts cut to the group's hosts, the position's stage no further than the
/// group's plan has.
fn marks_in(
    ops: &OpsState,
    service: &str,
    lay: &Layout,
    group: &GroupName,
    stages: usize,
) -> GroupMarks {
    let own = ops.services.get(service).cloned().unwrap_or_default();
    let rollout = ops.rollouts.get(service).map(|at| Rollout {
        version: at.version.clone(),
        entered: at.entered.min(u32::try_from(stages).unwrap_or(u32::MAX)),
        done: at
            .done
            .iter()
            .filter(|node| lay.hosts.contains(node))
            .cloned()
            .collect(),
        paused: at.paused.clone(),
    });
    GroupMarks {
        min_version: None,
        last_good: own.last_good,
        halt: own.halt,
        halt_acked: own.halt_acked,
        halt_seen: own.halt_seen,
        halt_generation: own.halt_generation,
        force: own
            .force
            .as_ref()
            .and_then(|force| force_in(force, lay, group)),
        force_generation: own.force_generation,
        rollout: rollout.unwrap_or_default(),
    }
}

/// The lines that say the marks of `service` in a group, and its desired version.
fn mark_lines(service: &str, marks: &GroupMarks, desired: Option<&GroupDesired>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(desired) = desired {
        lines.push(format!(
            "  desired {service} {} (generation {})",
            desired.version, desired.generation
        ));
    }
    if let Some(version) = &marks.last_good {
        lines.push(format!("  last_good {service} {version}"));
    }
    if let Some(halt) = &marks.halt {
        lines.push(format!(
            "  halt {service}: {} failed {} on {}, generation {}",
            halt.target, halt.check, halt.node_id, marks.halt_generation
        ));
    } else if marks.halt_generation > 0 {
        lines.push(format!(
            "  halt {service}: none standing, generation {}",
            marks.halt_generation
        ));
    }
    if let Some(force) = &marks.force {
        lines.push(format!(
            "  force {service} {} ({}, generation {}, hosts {})",
            force.version,
            force.targets.describe(),
            force.generation,
            force.hosts.join(",")
        ));
    } else if marks.force_generation > 0 {
        lines.push(format!(
            "  force {service}: none standing, generation {}",
            marks.force_generation
        ));
    }
    let at = &marks.rollout;
    if *at != Rollout::default() {
        let pause = at.paused.as_ref().map_or_else(String::new, |pause| {
            format!(", paused by {}: {}", pause.by, pause.reason)
        });
        lines.push(format!(
            "  plan position {service}: {} stages entered, {} hosts done{pause}",
            at.entered,
            at.done.len()
        ));
    }
    lines
}

/// One audit entry of the migration, not yet numbered.
fn entry(
    action: &str,
    service: Option<&str>,
    group: Option<&GroupName>,
    reason: String,
) -> AuditEntry {
    AuditEntry {
        service: service.map(str::to_owned),
        host_group: group.map(GroupName::to_string),
        reason: Some(reason),
        ..AuditEntry::new(MIGRATION, action)
    }
}

/// Every host and package whose place after the migration is not the place today: the package
/// is placed on the host today (the set, or for a package of the rollout file not in it, not
/// placed) exactly when the host's group lists it and `package_def` and `assigned` place it.
/// `groups` are the groups made, `hosts` each host's node id, group and set today, `names` every
/// package of the rollout file and `hostkeeper`.
pub(crate) fn verify(
    groups: &BTreeMap<GroupName, HostGroup>,
    ops: &OpsState,
    hosts: &[(String, GroupName, BTreeSet<String>)],
    names: &BTreeSet<String>,
) -> Vec<String> {
    let none = Services::default();
    let mut errors = Vec::new();
    for (node, name, set) in hosts {
        let Some(group) = groups.get(name) else {
            errors.push(format!("host {node}: its group {name} is not made"));
            continue;
        };
        for package in names {
            let place = Placement {
                node_id: node,
                role: name.as_str(),
                labels: None,
            };
            let after = package_def(name, group, &none, ops, package)
                .is_some_and(|def| assigned(&def, None, &place));
            let before = set.contains(package);
            if before != after {
                errors.push(format!(
                    "host {node}: {package} is {} today and {} after the migration (group {name})",
                    if before { "placed" } else { "not placed" },
                    if after { "placed" } else { "not placed" },
                ));
            }
        }
    }
    errors
}

/// Plans the migration of the store in `input`.
#[expect(
    clippy::too_many_lines,
    reason = "one pass over the plan's steps in the order the design gives them"
)]
pub(crate) fn plan(input: &Inputs<'_>) -> Plan {
    let moved = input.moved;
    let (held, ops) = (&moved.loaded.desired, &moved.loaded.ops);
    let mut errors = Vec::new();
    let named = named_roles(input, &mut errors);
    let hosts = placed_hosts(input, &mut errors);
    let known = known_roles(input, &named, &hosts);
    let layout = layout(input, &hosts, &named, &known, &mut errors);
    let mut lines = Vec::new();
    let mut new_held = held.clone();
    let mut new_ops = ops.clone();
    let mut entries = Vec::new();
    let mut groups: BTreeMap<GroupName, HostGroup> = BTreeMap::new();
    for (name, lay) in &layout {
        let packages = ordered(input, &lay.set);
        let defs: Vec<_> = lay
            .set
            .iter()
            .filter_map(|package| input.rollout.get(package))
            .collect();
        let limits: Vec<Limits<'_>> = lay
            .set
            .iter()
            .filter_map(|package| {
                held.desired.get(package).map(|desired| Limits {
                    package,
                    max_updating: desired.max_updating.max_updating_of(&lay.role),
                    min_up: desired.min_up.min_up_of(&lay.role),
                })
            })
            .collect();
        let fallback = (
            RoleLimits::default().max_updating_of(&lay.role),
            RoleLimits::default().min_up_of(&lay.role),
        );
        let merged = merge(&defs, lay.hosts.len(), &limits, fallback);
        let desired: BTreeMap<String, GroupDesired> = lay
            .set
            .iter()
            .filter_map(|package| {
                let at = held.desired.get(package)?;
                let record = held.records.get(package);
                Some((
                    package.clone(),
                    GroupDesired {
                        version: at.version.clone(),
                        generation: record.map_or(1, |record| record.generation.max(1)),
                        write_id: record
                            .and_then(|record| record.write_id.as_ref())
                            .map(|id| id.as_str().to_owned()),
                        replaced_version: record
                            .and_then(|record| record.replaced.as_ref())
                            .map(|replaced| replaced.version.clone()),
                    },
                ))
            })
            .collect();
        let group = HostGroup {
            packages: packages.clone(),
            policy: merged.policy.clone(),
            generation: 1,
            by: MIGRATION.to_owned(),
            at_unix: input.now,
            desired,
        };
        if let Err(why) = group.check() {
            errors.push(format!("group {name}: {why}"));
        }
        lines.push(format!(
            "group {name} (from role {}): {} hosts; packages {}",
            lay.role,
            lay.hosts.len(),
            packages.join(", ")
        ));
        lines.push(format!("  policy: {}", policy_text(&merged.policy)));
        lines.extend(merged.choices.iter().cloned());
        entries.push(entry(
            "host_group_put",
            None,
            Some(name),
            format!("migrated from role {}: {} hosts", lay.role, lay.hosts.len()),
        ));
        let stages = group.policy.plan().map_or(0, |plan| plan.defs().len());
        let mut minimums = Vec::new();
        for package in &packages {
            let mut marks = marks_in(ops, package, lay, name, stages);
            let floor = input
                .rollout
                .get(package)
                .and_then(|def| def.min_version.clone());
            let role_min = ops
                .assignments
                .get(package)
                .and_then(|entries| entries.role_min_versions.get(&lay.role))
                .cloned();
            let from_role = role_min
                .as_ref()
                .is_some_and(|role| floor.as_ref().is_none_or(|floor| role.at_least(floor)));
            marks.min_version = higher(role_min, floor);
            lines.extend(mark_lines(package, &marks, group.desired.get(package)));
            if let Some(version) = &marks.min_version {
                let source = if from_role {
                    format!("role {}", lay.role)
                } else {
                    "the rollout file".to_owned()
                };
                lines.push(format!("  minimum {package} {version} (from {source})"));
                minimums.push((package.clone(), version.clone(), source));
            }
            if marks != GroupMarks::default() {
                new_ops
                    .host_groups
                    .0
                    .entry(name.clone())
                    .or_default()
                    .insert(package.clone(), marks);
            }
        }
        for (package, version, source) in minimums {
            entries.push(entry(
                "set_min_version",
                Some(&package),
                Some(name),
                format!("min_version {version}: from {source}"),
            ));
        }
        groups.insert(name.clone(), group);
    }
    new_held.host_groups.0 = groups.clone();
    lines.push("hosts:".to_owned());
    for (name, lay) in layout.iter().filter(|(_, lay)| !lay.hosts.is_empty()) {
        lines.push(format!("  {name}: {}", lay.hosts.join(", ")));
    }
    let mut of_host: BTreeMap<&str, &GroupName> = BTreeMap::new();
    for (name, lay) in &layout {
        of_host.extend(lay.hosts.iter().map(|node| (node.as_str(), name)));
    }
    let checked: Vec<(String, GroupName, BTreeSet<String>)> = hosts
        .iter()
        .filter_map(|placed| {
            let name = of_host.get(placed.host.host_id.as_str())?;
            Some((
                placed.host.host_id.clone(),
                (*name).clone(),
                placed.set.clone(),
            ))
        })
        .collect();
    let mut names: BTreeSet<String> = input.rollout.names().map(str::to_owned).collect();
    names.insert(SELF_SERVICE.to_owned());
    errors.extend(verify(&groups, &new_ops, &checked, &names));

    // Holds of a role: kept, and copied to each other group of the role.
    let mut holds = new_ops.holds.clone();
    for hold in &ops.holds {
        if let crate::holds::HoldScope::Role(role) = &hold.scope {
            for (name, lay) in &layout {
                if lay.role == *role && name.as_str() != role {
                    let mut copy = hold.clone();
                    copy.scope = crate::holds::HoldScope::Role(name.to_string());
                    holds.push(copy);
                }
            }
        }
    }
    if holds.len() > MAX_HOLDS {
        errors.push(format!("the holds would be over {MAX_HOLDS}"));
    }
    new_ops.holds = holds;

    // The repositories of the rollout file.
    lines.push("repositories:".to_owned());
    for def in input
        .rollout
        .defs()
        .filter(|def| def.repository != DEFAULT_REPOSITORY)
    {
        if held.repositories.get(&def.repository).is_none() {
            errors.push(format!(
                "service {}: its repository {} is not in the registry",
                def.name, def.repository
            ));
        }
        new_ops
            .services
            .entry(def.name.clone())
            .or_default()
            .repository = Some(def.repository.clone());
        entries.push(entry(
            "set_repository",
            Some(&def.name),
            None,
            format!("repository {}: from the rollout file", def.repository),
        ));
        lines.push(format!("  {}: {}", def.name, def.repository));
    }

    // Records.
    let mut changed = Snapshot::default();
    lines.push("records:".to_owned());
    for placed in &hosts {
        let Some(name) = of_host.get(placed.host.host_id.as_str()) else {
            continue;
        };
        if placed.host.host_group != name.as_str() || placed.host.moving_from.is_some() {
            changed.hosts.push(pb::Host {
                host_group: name.to_string(),
                moving_from: None,
                ..placed.host.clone()
            });
        }
    }
    for request in &moved.records.requests {
        let Some(approval) = unpolled(request) else {
            continue;
        };
        if let Some(name) = group_name(&approval.role).filter(|name| layout.contains_key(name)) {
            let mut approved = approval.clone();
            approved.host_group = name.to_string();
            changed.requests.push(pb::Request {
                approved: Some(approved),
                ..request.clone()
            });
            lines.push(format!(
                "  approval of {}: host_group {name}",
                approval.host_id
            ));
        }
    }
    for record in &mut new_ops.inventory {
        if record.host_group.is_empty()
            && let Some(name) = group_name(&record.role).filter(|name| layout.contains_key(name))
        {
            record.host_group = name.to_string();
            lines.push(format!(
                "  inventory record {}: host_group {name}",
                record.host_id
            ));
        }
    }
    for (serial, entry) in input.allowlist.iter().flat_map(|list| list.entries()) {
        let group = group_name(entry.role.as_str()).and_then(|name| layout.get(&name));
        let stray: Vec<&str> = entry
            .services
            .iter()
            .map(|service| service.as_str())
            .filter(|service| group.is_none_or(|lay| !lay.set.contains(*service)))
            .collect();
        if !stray.is_empty() {
            errors.push(format!(
                "allowlist entry {serial}: its services {} are not packages of group {}",
                stray.join(", "),
                entry.role.as_str()
            ));
        }
        let drop = if entry.services.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = entry.services.iter().map(|s| s.as_str()).collect();
            format!(", and its services {} go", names.join(", "))
        };
        lines.push(format!(
            "  allowlist entry {serial}: host_group = \"{}\"{drop}",
            entry.role.as_str()
        ));
    }
    lines.push(format!(
        "live_rollouts: {} groups may each roll {} packages at once where the fleet rolled {}",
        layout.len(),
        input.live_rollouts,
        input.live_rollouts
    ));
    if layout.is_empty() {
        errors.push("no host and no record names a role: there is no group to make".to_owned());
    }

    // The audit entries, numbered after the store's newest line.
    let newest = ops
        .audit
        .iter()
        .map(|entry| entry.seq)
        .chain(moved.audit.last().map(|line| line.seq))
        .max()
        .unwrap_or(0);
    let mut audit = Vec::new();
    for (i, mut made) in entries.into_iter().enumerate() {
        made.seq = newest + 1 + i as u64;
        new_ops.audit.push(made.clone());
        audit.push(AuditLine::of(&made, input.now));
    }
    let surplus = new_ops.audit.len().saturating_sub(AUDIT_RING);
    new_ops.audit.drain(..surplus);
    let mut after_records = moved.records.clone();
    for host in &changed.hosts {
        if let Some(slot) = after_records
            .hosts
            .iter_mut()
            .find(|h| h.host_id == host.host_id)
        {
            *slot = host.clone();
        }
    }
    for request in &changed.requests {
        if let Some(slot) = after_records
            .requests
            .iter_mut()
            .find(|r| r.request_id == request.request_id)
        {
            *slot = request.clone();
        }
    }
    let mut after_audit = moved.audit.clone();
    after_audit.extend(audit.iter().cloned());
    Plan {
        lines,
        errors,
        after: Moved {
            loaded: Loaded {
                desired: new_held,
                ops: new_ops,
                ..moved.loaded.clone()
            },
            audit: after_audit,
            records: after_records,
            ..moved.clone()
        },
        changed,
        audit,
    }
}
