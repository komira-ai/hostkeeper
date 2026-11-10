//! The operator's assignment calls of `hostkeeper.operator.v1.Services`, as plain functions over
//! its messages: `Assign`, `Unassign`, `SetMinVersion` and `List` ([`crate::assignment`]). A
//! transport authenticates the bearer ([`Coordinator::caller`]) and hands the call here, as for
//! the calls of [`crate::operator`].
//!
//! Every call takes the operator token only: a valid CI token is `forbidden` and no token, or any
//! other, `unauthorized`, before the request is looked at. A refusal is one value of
//! `ServicesRefusal` and changes nothing: `invalid_request` (a node id, a role or a version that
//! breaks its rule, both or neither of a node id and a role, a change past
//! [`crate::assignment::MAX_ENTRIES`]), `unknown_service` (not in the rollout file),
//! `unknown_host` (neither heard from since the coordinator started nor named in the ops file),
//! `unknown_role` (no service of the rollout file runs on it, no host heard from has it, no
//! assignment names it), `not_assigned` (an `Unassign` of a scope the service is not assigned
//! to), `below_min_version` (a `SetMinVersion` above the desired version) and `store_failed`.
//! Every change is one entry of the audit ring and file, under the name of the operator's token,
//! and is written to the ops file before it is in force.
//!
//! [`Coordinator::assign_host`] is the one way a service is assigned to one host: `Assign` of a
//! host takes it, and so do the services an operator's enrollment approval names
//! ([`crate::operator_enrollment`]). No call but `Unassign` with `uninstall` orders an
//! uninstall.
//!
//! Once the coordinator holds a host group, `Assign` and `Unassign` are refused
//! `placed_by_host_group`, a minimum version of a role scope is the group's, and `List` answers by
//! group (`group_assign.rs`).

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::{assignment, min_version};
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::Health;
use hostkeeper_wire::{HostId, RoleName};

use crate::assignment::{self as rules, Change, Entry, Known, MAX_ENTRIES, NotAssigned, Scope};
use crate::fleet::Fleet;
use crate::holds::{holding, marks_of};
use crate::operator::{Caller, entry_proto, hold_proto};
use crate::ops::{AuditEntry, OpsState, Write};
use crate::service::Coordinator;
use crate::services::ServiceDef;

type Refusal = proto::ServicesRefusal;

impl From<NotAssigned> for Refusal {
    fn from(_: NotAssigned) -> Self {
        Self::NotAssigned
    }
}

/// The scope of an `Assign` or an `Unassign`: exactly one of a node id and a role, each by its
/// rule.
fn scope_of(node_id: &str, role: &str) -> Result<Scope, Refusal> {
    match (node_id.is_empty(), role.is_empty()) {
        (false, true) => node_id
            .parse::<HostId>()
            .map(|id| Scope::Host(id.to_string()))
            .map_err(|_| Refusal::InvalidRequest),
        (true, false) => role
            .parse::<RoleName>()
            .map(|role| Scope::Role(role.to_string()))
            .map_err(|_| Refusal::InvalidRequest),
        _ => Err(Refusal::InvalidRequest),
    }
}

/// The scope of a `SetMinVersion`.
fn min_scope_of(scope: Option<min_version::Scope>) -> Result<Scope, Refusal> {
    match scope {
        Some(min_version::Scope::Host(id)) => scope_of(&id, ""),
        Some(min_version::Scope::Role(role)) => scope_of("", &role),
        None => Err(Refusal::InvalidRequest),
    }
}

/// An audit entry by `by` for `action` about `def` in `scope`, its reason the scope and `more`.
fn audited(by: &str, action: &str, def: &ServiceDef, scope: &Scope, more: &str) -> AuditEntry {
    let mut entry = AuditEntry::new(by, action);
    entry.service = Some(def.name.clone());
    entry.reason = Some(format!("{}{more}", scope.describe()));
    if let Scope::Host(id) = scope {
        entry.node_id = Some(id.clone());
    }
    entry
}

/// The hosts the fleet has heard from, as a change of the assignment of `service` reads them.
fn known_hosts(fleet: &Fleet, state: &OpsState, service: &str) -> Vec<Known> {
    fleet
        .nodes()
        .map(|(id, node)| Known {
            node_id: id.to_string(),
            role: node.role.clone(),
            labels: state.hosts.get(id.as_str()).map(|host| host.labels.clone()),
            lists: node.announced.contains_key(service),
        })
        .collect()
}

/// Drops the assignments that hold nothing, and refuses a state past [`MAX_ENTRIES`].
fn settled(state: &mut OpsState) -> Result<(), Refusal> {
    state.assignments.retain(|_, entries| !entries.is_empty());
    let total: usize = state.assignments.values().map(|e| e.len()).sum();
    if total > MAX_ENTRIES {
        return Err(Refusal::InvalidRequest);
    }
    Ok(())
}

/// The hosts `def` is assigned to and counts, each with what it last reported, the minimum
/// version that applies to it and the holds that hold the pair.
fn assigned_hosts(fleet: &Fleet, state: &OpsState, def: &ServiceDef) -> Vec<proto::AssignedHost> {
    fleet
        .nodes()
        .filter(|(id, _)| fleet.is_assigned(def, id) && !fleet.is_excluded(&def.name, id))
        .map(|(id, node)| proto::AssignedHost {
            node_id: id.to_string(),
            role: node.role.clone(),
            installed_version: fleet
                .get_in(&def.name, id)
                .filter(|host| host.beat.health != Health::Absent)
                .map(|host| host.beat.version.to_string()),
            min_version: fleet.min_version(def, id).map(ToString::to_string),
            holds: holding(&state.holds, &def.name, id.as_str(), &node.role)
                .map(hold_proto)
                .collect(),
        })
        .collect()
}

/// One change of an assignment: the service, the change, and its audit entry.
struct Step<'a> {
    def: &'a ServiceDef,
    change: Change,
    audit: AuditEntry,
}

impl Coordinator {
    /// Whether the coordinator knows `scope`: a host heard from, or named by the labels or the
    /// assignments of the ops file; a role a service of the rollout file runs on, a host heard
    /// from has, or an assignment names.
    fn known(&self, fleet: &Fleet, state: &OpsState, scope: &Scope) -> Result<(), Refusal> {
        match scope {
            Scope::Host(id) => {
                let named = state.hosts.contains_key(id)
                    || state.assignments.values().any(|e| e.hosts.contains_key(id));
                let heard = fleet.nodes().any(|(node, _)| node.as_str() == id);
                (heard || named).then_some(()).ok_or(Refusal::UnknownHost)
            }
            Scope::Role(role) => {
                let ruled = self.services.defs().any(|def| def.roles.contains(role));
                let heard = fleet.nodes().any(|(_, node)| node.role == *role);
                let named = state
                    .assignments
                    .values()
                    .any(|e| e.roles.contains_key(role));
                (ruled || heard || named)
                    .then_some(())
                    .ok_or(Refusal::UnknownRole)
            }
        }
    }

    /// Makes `steps` in one write of the ops file, under the fleet's lock, with the hosts the
    /// fleet has heard from and the host `extra` (a node id and its role, for a host it has not
    /// heard from), and refreshes the fleet's marks. A host a step takes the service off is no
    /// longer counted by it ([`Fleet::forget_in`]): what it said is kept only as what it occupies.
    fn change(
        &self,
        fleet: &mut Fleet,
        steps: Vec<Step<'_>>,
        extra: Option<(&str, &str)>,
    ) -> Result<Vec<AuditEntry>, Refusal> {
        let mut stopped: Vec<(String, String)> = Vec::new();
        let done = self.ops.decide_changes(
            Write::BeforeMemory,
            |state| -> Result<Vec<AuditEntry>, Refusal> {
                let mut entries = Vec::new();
                for step in steps {
                    let mut known = known_hosts(fleet, state, &step.def.name);
                    if let Some((id, role)) = extra
                        && !known.iter().any(|k| k.node_id == id)
                    {
                        known.push(Known {
                            node_id: id.to_owned(),
                            role: role.to_owned(),
                            labels: state.hosts.get(id).map(|host| host.labels.clone()),
                            lists: true,
                        });
                    }
                    let mut assigned = state.assignments.remove(&step.def.name).unwrap_or_default();
                    let off = rules::apply(step.def, &mut assigned, &step.change, &known)?;
                    stopped.extend(off.into_iter().map(|id| (step.def.name.clone(), id)));
                    state.assignments.insert(step.def.name.clone(), assigned);
                    entries.push(step.audit);
                }
                settled(state)?;
                Ok(entries)
            },
        )?;
        fleet.set_marks(self.ops.read(marks_of));
        let mut forget: Vec<(String, HostId)> = Vec::new();
        for (id, _) in fleet.nodes() {
            for (service, off) in &stopped {
                if off == id.as_str() {
                    forget.push((service.clone(), id.clone()));
                }
            }
        }
        for (service, id) in forget {
            fleet.forget_in(&service, &id);
        }
        Ok(done)
    }

    /// Assigns each of `services` to the host `host_id`: the one path by which a service is
    /// assigned to one host, for `Services.Assign` of a host, for the services an operator's
    /// enrollment approval names (`Enrollment.Approve`), and for those of the allowlist entry a
    /// Mac is approved as by its MDM nonce (`Enroll`). `role` is the host's role, as an
    /// approval names it, for a host the fleet has not heard from (one it has heard from is read
    /// with the role it heartbeated with). Each service is one audit entry `assign` by `by`, all
    /// in one write; an uninstall the host was ordered of one of them is dropped. The host's own entry is written even when its role
    /// assigns it the service already, so that it keeps the service when the role is taken off.
    ///
    /// # Errors
    /// `unknown_service` (a service not in the rollout file; nothing is assigned),
    /// `invalid_request` (past [`MAX_ENTRIES`]), `store_failed`.
    pub fn assign_host(
        &self,
        by: &str,
        host_id: &HostId,
        role: Option<&RoleName>,
        services: &[String],
    ) -> Result<Vec<AuditEntry>, Refusal> {
        let defs = services
            .iter()
            .map(|name| self.services.get(name).ok_or(Refusal::UnknownService))
            .collect::<Result<Vec<_>, _>>()?;
        let mut fleet = self.lock_fleet();
        let role = role.map_or("", RoleName::name);
        self.assign_in(&mut fleet, by, host_id.as_str(), role, defs)
    }

    /// [`Self::assign_host`] under the fleet's lock, for the host `node_id` of the role `role`
    /// (empty when the caller does not know it) and the services `defs`.
    fn assign_in(
        &self,
        fleet: &mut Fleet,
        by: &str,
        node_id: &str,
        role: &str,
        defs: Vec<&ServiceDef>,
    ) -> Result<Vec<AuditEntry>, Refusal> {
        let scope = Scope::Host(node_id.to_owned());
        let steps = defs
            .into_iter()
            .map(|def| Step {
                def,
                change: Change::Assign(scope.clone()),
                audit: audited(by, "assign", def, &scope, ""),
            })
            .collect();
        self.change(fleet, steps, Some((node_id, role)))
    }

    /// `Services.Assign`: assigns the service to a host ([`Self::assign_host`]) or to a role,
    /// those of its hosts that join later included. An assigned host that does not run the
    /// service is ordered to install it at its next heartbeat, unless a hold stands.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the scope, or past
    /// [`MAX_ENTRIES`]), `unknown_host`, `unknown_role`, `store_failed`.
    pub fn assign(
        &self,
        caller: &Caller,
        request: proto::AssignRequest,
    ) -> Result<proto::AssignResponse, Refusal> {
        let by = caller.operator()?;
        if self.desired.in_group_mode() {
            return Err(Refusal::PlacedByHostGroup);
        }
        let def = self
            .services
            .get(&request.service)
            .ok_or(Refusal::UnknownService)?;
        let scope = scope_of(&request.node_id, &request.role)?;
        let mut fleet = self.lock_fleet();
        self.ops.read(|state| self.known(&fleet, state, &scope))?;
        let done = match &scope {
            Scope::Host(id) => self.assign_in(&mut fleet, by, id, "", vec![def])?,
            Scope::Role(_) => {
                let step = Step {
                    def,
                    change: Change::Assign(scope.clone()),
                    audit: audited(by, "assign", def, &scope, ""),
                };
                self.change(&mut fleet, vec![step], None)?
            }
        };
        Ok(proto::AssignResponse {
            entry: done.first().map(entry_proto),
        })
    }

    /// `Services.Unassign`: takes the service off a host or a role. The hosts it no longer has
    /// any assignment of are never ordered it again. With `uninstall`, each of those that listed
    /// the service at its last heartbeat (and the host of a host scope the coordinator has not
    /// heard from since it started) is ordered to uninstall it, until it reports it gone; without,
    /// the service stays installed there, unmanaged.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the scope, or past
    /// [`MAX_ENTRIES`]), `unknown_host`, `unknown_role`, `not_assigned`, `store_failed`.
    pub fn unassign(
        &self,
        caller: &Caller,
        request: proto::UnassignRequest,
    ) -> Result<proto::UnassignResponse, Refusal> {
        let by = caller.operator()?;
        if self.desired.in_group_mode() {
            return Err(Refusal::PlacedByHostGroup);
        }
        let def = self
            .services
            .get(&request.service)
            .ok_or(Refusal::UnknownService)?;
        let scope = scope_of(&request.node_id, &request.role)?;
        let mut fleet = self.lock_fleet();
        self.ops.read(|state| self.known(&fleet, state, &scope))?;
        let more = if request.uninstall { "; uninstall" } else { "" };
        let step = Step {
            def,
            change: Change::Unassign {
                scope: scope.clone(),
                uninstall: request.uninstall,
            },
            audit: audited(by, "unassign", def, &scope, more),
        };
        let extra = match &scope {
            Scope::Host(id) => Some((id.as_str(), "")),
            Scope::Role(_) => None,
        };
        let done = self.change(&mut fleet, vec![step], extra)?;
        Ok(proto::UnassignResponse {
            entry: done.first().map(entry_proto),
        })
    }

    /// `Services.SetMinVersion`: sets the minimum version of the service for a role or a host,
    /// below which the rollout gate orders it there no update or install; an empty version
    /// removes the one an operator set. A host's own stands before its role's, and neither goes
    /// below the rollout file's `min_version` ([`crate::assignment::effective_min_version`]).
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`, `unknown_service`, `invalid_request` (the scope, a version
    /// the service's scheme does not read, or past [`MAX_ENTRIES`]), `unknown_host`,
    /// `unknown_role`, `below_min_version` (above the service's desired version), `store_failed`.
    pub fn set_min_version(
        &self,
        caller: &Caller,
        request: proto::SetMinVersionRequest,
    ) -> Result<proto::SetMinVersionResponse, Refusal> {
        let by = caller.operator()?;
        let def = self
            .package_def(&request.service)
            .ok_or(Refusal::UnknownService)?;
        let min = request.min_version.ok_or(Refusal::InvalidRequest)?;
        let scope = min_scope_of(min.scope)?;
        let version = match min.version.as_str() {
            "" => None,
            text => Some(
                Version::parse(text)
                    .ok()
                    .filter(|version| def.version_scheme.parses(version))
                    .ok_or(Refusal::InvalidRequest)?,
            ),
        };
        if self.desired.in_group_mode()
            && let Scope::Role(group) = &scope
        {
            return self.set_group_min_version(by, &def.name, group, version);
        }
        let mut fleet = self.lock_fleet();
        self.ops.read(|state| self.known(&fleet, state, &scope))?;
        let ceiling = self.desired_for_scope(&fleet, &scope, &def.name);
        let above = ceiling.is_some_and(|desired| {
            version
                .as_ref()
                .is_some_and(|floor| !desired.version.at_least(floor))
        });
        if above {
            return Err(Refusal::BelowMinVersion);
        }
        let said = version
            .as_ref()
            .map_or_else(|| "none".to_owned(), ToString::to_string);
        let audit = audited(by, "set_min_version", &def, &scope, &format!(": {said}"));
        let done = self.ops.try_change(Write::BeforeMemory, audit, |state| {
            let entries = state.assignments.entry(def.name.clone()).or_default();
            rules::set_min_version(entries, &scope, version);
            settled(state)
        })?;
        fleet.set_marks(self.ops.read(marks_of));
        Ok(proto::SetMinVersionResponse {
            entry: Some(entry_proto(&done)),
        })
    }

    /// `Services.List`: every service the coordinator manages, in name order, with its desired
    /// version, the roles it is assigned to, the operator's assignments and minimum versions, the
    /// hosts heard from that it is assigned to (each with the version it reports installed, the
    /// minimum version that applies to it and the holds that hold the pair), the hosts ordered to
    /// uninstall it and its holds; and every hold that stands. Changes nothing.
    ///
    /// # Errors
    /// `unauthorized`, `forbidden`.
    pub fn list_services(
        &self,
        caller: &Caller,
        _request: proto::ListServicesRequest,
    ) -> Result<proto::ListServicesResponse, Refusal> {
        caller.operator()?;
        let fleet = self.lock_fleet();
        let state = self.ops.state();
        let services = if self.desired.in_group_mode() {
            self.entries_in_groups(&fleet, &state)
        } else {
            self.services
                .defs()
                .map(|def| self.entry_of(&fleet, &state, def))
                .collect()
        };
        Ok(proto::ListServicesResponse {
            services,
            holds: state.holds.iter().map(hold_proto).collect(),
        })
    }

    /// What `Services.List` says of `def`.
    fn entry_of(&self, fleet: &Fleet, state: &OpsState, def: &ServiceDef) -> proto::ServiceEntry {
        let entries = state.assignments.get(&def.name);
        let assigned = assigned_hosts(fleet, state, def);
        let scoped = |scope: assignment::Scope, entry: &Entry| proto::Assignment {
            scope: Some(scope),
            removed: *entry == Entry::Exclude,
        };
        let assignments = entries
            .into_iter()
            .flat_map(|e| {
                let roles = e
                    .roles
                    .iter()
                    .map(|(role, entry)| scoped(assignment::Scope::Role(role.clone()), entry));
                let hosts = e
                    .hosts
                    .iter()
                    .map(|(id, entry)| scoped(assignment::Scope::Host(id.clone()), entry));
                roles.chain(hosts).collect::<Vec<_>>()
            })
            .collect();
        let min_versions = entries
            .into_iter()
            .flat_map(|e| {
                let roles = e.role_min_versions.iter().map(|(role, version)| {
                    (min_version::Scope::Role(role.clone()), version.to_string())
                });
                let hosts = e.host_min_versions.iter().map(|(id, version)| {
                    (min_version::Scope::Host(id.clone()), version.to_string())
                });
                roles.chain(hosts).collect::<Vec<_>>()
            })
            .map(|(scope, version)| proto::MinVersion {
                scope: Some(scope),
                version,
            })
            .collect();
        proto::ServiceEntry {
            service: def.name.clone(),
            hosts: assigned.iter().map(|host| host.node_id.clone()).collect(),
            min_versions,
            holds: state
                .holds
                .iter()
                .filter(|hold| hold.service == def.name)
                .map(hold_proto)
                .collect(),
            desired_version: self
                .desired
                .get(&def.name)
                .map(|desired| desired.version.to_string()),
            roles: rules::roles_of(def, entries),
            assignments,
            assigned,
            repository: def.repository.clone(),
            uninstalling: entries
                .into_iter()
                .flat_map(|e| e.uninstall.keys().cloned())
                .collect(),
            host_groups: Vec::new(),
        }
    }
}

#[path = "group_assign.rs"]
mod groups;
