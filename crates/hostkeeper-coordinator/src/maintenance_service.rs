//! The handlers of `Maintenance` of `hostkeeper.operator.v1` (docs/design/maintenance-updates.md,
//! sections 4.2 and 5.1): `Schedule`, `List`, `Get` and `Cancel`, each with the operator's
//! credential only. `RetryHost` and `PutBack` are in [`crate::maintenance_operator`].
//!
//! `Schedule` resolves where the run goes (the hosts named, each of which must be enrolled, or
//! every enrolled host of a host group, those moving into it among them) to the hosts with
//! something due of the run's kinds, as the report kept from each host's last heartbeat says
//! (every due kind when the run names none). A host with nothing due, or one the coordinator has
//! not heard since it started, is not in the run. A host is in the run and left out of it
//! (`skipped`, with why) when its daemon does not announce `maintenance-v1` (whether anything is
//! due there is not known), when a hold stands on it for any package, or when it is excluded from
//! any package; every other host in the run waits for its turn. A host that may not be ordered
//! `macos` by its report ([`crate::maintenance_mac`]: `mdm_conflict`, `no_update_account`,
//! `filevault_not_ready`) waits without `macos`, with why in `skipped_because`, and is left out
//! with that reason when `macos` is all it has due of the run's kinds; a major upgrade of macOS
//! counts as due only in a group whose `macos_major` allows one. The run is refused, in this
//! order, `nothing_due` when no host waits, `last_host` when the hosts that wait are every host of
//! one of their groups and the request does not say `take_last_host`, and `host_in_maintenance`
//! when a host that would wait has a turn of another run that has not ended, which a `Schedule`
//! with `confirm` checks on the state it stores the run in. The Update button of the design is a
//! `Schedule` of every kind due, now, with each group's batch and drain deadline: a request that
//! names no kind, no window, no batch and no drain deadline.
//!
//! A `Schedule` with `confirm` stores the run and audits `maintenance_schedule` in one change;
//! `Cancel` ends a run and audits `maintenance_cancel`, each with the caller's reason followed by
//! `run <id>`. `List`, `Get` and a `Schedule` without `confirm` change nothing and write no audit
//! entry. Every time a call reads or stores is the ops store's clock.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MaintenanceRefusal as Refusal;
use hostkeeper_proto::hostkeeper::operator::v1::{MaintenanceSkip, MaintenanceState};
use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::OrderId;

use crate::audit_actions::{MAINTENANCE_CANCEL, MAINTENANCE_SCHEDULE};
use crate::host_groups::GroupName;
use crate::maintenance::{FEATURE, Kept};
use crate::maintenance_mac;
use crate::maintenance_runs::{self, Kind, Run, RunHost, Skip, Step};
use crate::operator::{Caller, entry_proto, optional_reason, valid_reason};
use crate::ops::{AuditBy, AuditEntry, OpsState, Write};
use crate::service::Coordinator;

/// The most hosts a `Schedule` names.
pub(crate) const MAX_HOST_IDS: usize = 32;

/// The largest batch a `Schedule` names.
pub(crate) const MAX_BATCH: u32 = 10_000;

/// The largest time and the longest drain deadline a `Schedule` names, in seconds: the largest a
/// database keeps.
pub(crate) const MAX_SECS: u64 = i64::MAX as u64;

/// The most runs a `List` answers, and what a `limit` of 0 asks for.
pub(crate) const MAX_LIST: u32 = 256;

/// Where a `Schedule` goes.
enum Target {
    /// The hosts named, each once.
    Hosts(Vec<HostId>),
    /// Every host of the group.
    Group(GroupName),
}

/// A `Schedule` request whose every field keeps its rule.
struct Asked {
    kinds: Vec<Kind>,
    target: Target,
    window_start_unix: u64,
    window_end_unix: u64,
    batch: Option<u32>,
    drain_deadline_secs: Option<u64>,
    reason: Option<String>,
    take_last_host: bool,
    confirm: bool,
}

/// What the coordinator knows of a host a run may go to.
struct Seen {
    id: HostId,
    group: String,
    /// Whether the coordinator holds the host's last heartbeat.
    heard: bool,
    /// Whether that heartbeat announced `maintenance-v1`.
    announced: bool,
    /// The host's role, as its last heartbeat said it: what a hold of a role covers when the
    /// host is in no group.
    role: String,
    /// The kinds the kept report lists as due, each once, a major upgrade of macOS only when
    /// the host's group allows one ([`maintenance_mac::counts_as_due`]).
    due: Vec<Kind>,
    /// Why the kept report keeps the host from being ordered `macos`
    /// ([`maintenance_mac::macos_skip`]): read only when `macos` is due, so a Linux host's
    /// `no_update_account` (it reports no Mac facts) is never used.
    macos_skip: Option<Skip>,
}

/// The kind `value` names in a request, if it is one other than `UNSPECIFIED`.
fn kind_of(value: i32) -> Option<Kind> {
    Kind::ALL
        .iter()
        .copied()
        .find(|kind| i32::from(kind.proto()) == value)
}

/// `request` read against the rules of `ScheduleMaintenanceRequest`.
///
/// # Errors
/// `invalid_request`: a kind unspecified, unknown or named twice; both or neither of `host_ids`
/// and `host_group`; more than [`MAX_HOST_IDS`] host ids, one that is not a host id or one named
/// twice; a group name that is not one; a window start without an end, an end not after its
/// start, or an end or a drain deadline above [`MAX_SECS`]; a batch of 0 or above [`MAX_BATCH`];
/// a reason that breaks its rule, or none with `confirm`.
fn asked(request: &proto::ScheduleMaintenanceRequest) -> Result<Asked, Refusal> {
    let invalid = Refusal::InvalidRequest;
    let mut kinds = Vec::new();
    for value in &request.kinds {
        let kind = kind_of(*value).ok_or(invalid)?;
        if kinds.contains(&kind) {
            return Err(invalid);
        }
        kinds.push(kind);
    }
    let target = match (request.host_ids.is_empty(), request.host_group.is_empty()) {
        (false, true) if request.host_ids.len() <= MAX_HOST_IDS => {
            let mut ids: Vec<HostId> = Vec::new();
            for text in &request.host_ids {
                let id: HostId = text.parse().map_err(|_| invalid)?;
                if ids.contains(&id) {
                    return Err(invalid);
                }
                ids.push(id);
            }
            Target::Hosts(ids)
        }
        (true, false) => Target::Group(request.host_group.parse().map_err(|_| invalid)?),
        _ => return Err(invalid),
    };
    let (start, end) = (request.window_start_unix, request.window_end_unix);
    if (start != 0 && end <= start) || end > MAX_SECS {
        return Err(invalid);
    }
    if request
        .drain_deadline_secs
        .is_some_and(|secs| secs > MAX_SECS)
    {
        return Err(invalid);
    }
    if request
        .batch
        .is_some_and(|batch| batch == 0 || batch > MAX_BATCH)
    {
        return Err(invalid);
    }
    let reason = if request.confirm {
        Some(valid_reason(&request.reason).ok_or(invalid)?)
    } else {
        optional_reason(&request.reason, invalid)?
    };
    Ok(Asked {
        kinds,
        target,
        window_start_unix: start,
        window_end_unix: end,
        batch: request.batch,
        drain_deadline_secs: request.drain_deadline_secs,
        reason,
        take_last_host: request.take_last_host,
        confirm: request.confirm,
    })
}

/// The run id `text`, if it is one: 1 to 64 bytes of an order id's characters.
pub(crate) fn run_id(text: &str) -> Result<&str, Refusal> {
    OrderId::parse(text)
        .map(|_| text)
        .ok_or(Refusal::InvalidRequest)
}

/// `run` as `hostkeeper.operator.v1` says it at `now`, without its hosts.
pub(crate) fn run_proto(run: &Run, now: u64) -> proto::MaintenanceRun {
    let (state, ended_unix) = run.state_at(now);
    proto::MaintenanceRun {
        id: run.id.clone(),
        state: state.into(),
        kinds: kinds_proto(&run.kinds),
        host_ids: run.host_ids.clone(),
        host_group: run.host_group.clone(),
        window_start_unix: run.window_start_unix,
        window_end_unix: run.window_end_unix,
        batch: run.batch,
        drain_deadline_secs: run.drain_deadline_secs,
        take_last_host: run.take_last_host,
        reason: run.reason.clone(),
        by_name: run.by.clone(),
        scheduled_unix: run.scheduled_unix,
        ended_unix,
        halted_groups: run.halted_groups(),
        host_count: u32::try_from(run.hosts.len()).unwrap_or(u32::MAX),
    }
}

/// `host` as `hostkeeper.operator.v1` says it.
pub(crate) fn host_proto(host: &RunHost) -> proto::MaintenanceHost {
    proto::MaintenanceHost {
        host_id: host.host_id.clone(),
        host_group: host.host_group.clone(),
        step: host.step.proto().into(),
        since_unix: host.since_unix,
        kinds: kinds_proto(&host.kinds),
        applied: kinds_proto(&host.applied),
        skipped_because: host
            .skipped_because
            .map_or(MaintenanceSkip::Unspecified, Skip::proto)
            .into(),
        reason: host.reason.clone(),
    }
}

/// `kinds` as the proto's values.
fn kinds_proto(kinds: &[Kind]) -> Vec<i32> {
    kinds.iter().map(|kind| i32::from(kind.proto())).collect()
}

/// Why `seen` is left out of a run, by the holds and exclusions of `state`: the first of no
/// `maintenance-v1`, a hold of any package, an exclusion from any package.
fn skip_of(seen: &Seen, state: &OpsState) -> Option<Skip> {
    let id = seen.id.as_str();
    let role = if seen.group.is_empty() {
        seen.role.as_str()
    } else {
        seen.group.as_str()
    };
    if !seen.announced {
        Some(Skip::NoMaintenanceV1)
    } else {
        crate::decide_maintenance::package_skip(state, id, role)
    }
}

/// The turn of `seen` in a run of `kinds` (every kind due, when empty), by the holds and
/// exclusions of `state`: waiting with the kinds due, or left out with why; none when it is not
/// in the run (not heard since the coordinator started, or nothing due of the kinds). A host
/// that may not be ordered `macos` waits without it, with why; it is left out when `macos` is all
/// that is due of the kinds.
fn turn_of(seen: &Seen, kinds: &[Kind], state: &OpsState) -> Option<RunHost> {
    if !seen.heard {
        return None;
    }
    let mut due: Vec<Kind> = if seen.announced {
        let due: Vec<Kind> = seen
            .due
            .iter()
            .copied()
            .filter(|kind| kinds.is_empty() || kinds.contains(kind))
            .collect();
        if due.is_empty() {
            return None;
        }
        due
    } else {
        kinds.to_vec()
    };
    let mut skipped_because = skip_of(seen, state);
    let mut step = if skipped_because.is_some() {
        Step::Skipped
    } else {
        Step::Waiting
    };
    if step == Step::Waiting
        && due.contains(&Kind::Macos)
        && let Some(why) = seen.macos_skip
    {
        skipped_because = Some(why);
        if due == [Kind::Macos] {
            step = Step::Skipped;
        } else {
            due.retain(|kind| *kind != Kind::Macos);
        }
    }
    Some(RunHost {
        host_id: seen.id.as_str().to_owned(),
        host_group: seen.group.clone(),
        step,
        since_unix: 0,
        kinds: due,
        applied: Vec::new(),
        skipped_because,
        reason: String::new(),
        offered_again: 0,
        baked_beats: 0,
        queued: None,
        order_try: 0,
    })
}

/// Whether a host of `hosts` that waits has a turn of one of `runs` that has not ended at `now`.
fn in_maintenance(runs: &[Run], hosts: &[RunHost], now: u64) -> bool {
    hosts
        .iter()
        .filter(|host| host.step == Step::Waiting)
        .any(|host| runs.iter().any(|run| run.holds_turn(&host.host_id, now)))
}

impl Coordinator {
    /// `Maintenance.Schedule` of `request`, by `caller`: the run as it would be, and with
    /// `confirm` stored and audited.
    ///
    /// # Errors
    /// `unauthorized` and `forbidden`; `invalid_request` ([`asked`]); `window_ended`;
    /// `unknown_group`; `unknown_host`; `nothing_due`; `last_host`; `host_in_maintenance`;
    /// `store_failed` (an enrolled host or a group's hosts cannot be read, or the run not stored);
    /// `audit_unavailable`.
    pub(crate) fn maintenance_schedule(
        &self,
        caller: &Caller,
        request: proto::ScheduleMaintenanceRequest,
    ) -> Result<proto::ScheduleMaintenanceResponse, Refusal> {
        let by = caller.operator()?;
        let asked = asked(&request)?;
        let now = self.ops.unix_now();
        if asked.window_end_unix != 0 && asked.window_end_unix <= now {
            return Err(Refusal::WindowEnded);
        }
        let seen = self.seen(&asked.target)?;
        let mut hosts: Vec<RunHost> = self.ops.read(|state| {
            seen.iter()
                .filter_map(|seen| turn_of(seen, &asked.kinds, state))
                .collect()
        });
        hosts.sort_by(|a, b| a.host_id.cmp(&b.host_id));
        if !hosts.iter().any(|host| host.step == Step::Waiting) {
            return Err(Refusal::NothingDue);
        }
        if !asked.take_last_host && self.takes_a_last_host(&hosts)? {
            return Err(Refusal::LastHost);
        }
        let reason = asked.reason.unwrap_or_default();
        let (host_ids, host_group) = match &asked.target {
            Target::Hosts(ids) => (
                ids.iter().map(|id| id.as_str().to_owned()).collect(),
                String::new(),
            ),
            Target::Group(name) => (Vec::new(), name.as_str().to_owned()),
        };
        let mut run = Run {
            id: String::new(),
            kinds: asked.kinds,
            host_ids,
            host_group,
            window_start_unix: asked.window_start_unix,
            window_end_unix: asked.window_end_unix,
            batch: asked.batch,
            drain_deadline_secs: asked.drain_deadline_secs,
            take_last_host: asked.take_last_host,
            reason: reason.clone(),
            by: by.by_name().to_owned(),
            scheduled_unix: now,
            cancelled_unix: None,
            hosts,
        };
        let entry = if asked.confirm {
            let mut entry = AuditEntry::new(by, MAINTENANCE_SCHEDULE);
            entry.host_group = Some(run.host_group.clone()).filter(|group| !group.is_empty());
            let done = self.ops.decide_change(Write::BeforeMemory, |state| {
                if in_maintenance(&state.maintenance_runs, &run.hosts, now) {
                    return Err(Refusal::HostInMaintenance);
                }
                run.id = maintenance_runs::new_id(&state.maintenance_runs, now);
                entry.reason = Some(format!("{reason} run {}", run.id));
                state.maintenance_runs.push(run.clone());
                maintenance_runs::prune(&mut state.maintenance_runs, now);
                Ok(entry)
            })?;
            Some(entry_proto(&done))
        } else {
            if self
                .ops
                .read(|state| in_maintenance(&state.maintenance_runs, &run.hosts, now))
            {
                return Err(Refusal::HostInMaintenance);
            }
            None
        };
        Ok(proto::ScheduleMaintenanceResponse {
            run: Some(run_proto(&run, now)),
            hosts: run.hosts.iter().map(host_proto).collect(),
            entry,
        })
    }

    /// What the coordinator knows of each host `target` names, in the order named or, for a
    /// group, of its enrolled hosts.
    ///
    /// # Errors
    /// `unknown_group`: no group has the name. `unknown_host`: a host named is not enrolled.
    /// `store_failed`: the enrollment store cannot be read.
    fn seen(&self, target: &Target) -> Result<Vec<Seen>, Refusal> {
        let placed: Vec<(HostId, String)> = match target {
            Target::Group(name) => {
                if self.desired.host_group(name).is_none() {
                    return Err(Refusal::UnknownGroup);
                }
                self.hosts_in(name)
                    .map_err(|_| Refusal::StoreFailed)?
                    .into_iter()
                    .map(|id| (id, name.as_str().to_owned()))
                    .collect()
            }
            Target::Hosts(ids) => {
                let mut placed = Vec::new();
                for id in ids {
                    let host = self
                        .enrolled_host(id.as_str())
                        .map_err(|why| {
                            tracing::error!(%why, "an enrolled host could not be read");
                            Refusal::StoreFailed
                        })?
                        .ok_or(Refusal::UnknownHost)?;
                    placed.push((id.clone(), host.host_group));
                }
                placed
            }
        };
        let fleet = self.lock_fleet();
        Ok(placed
            .into_iter()
            .map(|(id, group)| {
                let node = fleet.node(&id);
                let report = node
                    .and_then(|node| node.maintenance.as_ref())
                    .map(Kept::report);
                let macos_major = self.macos_major_of(&group);
                let mut due: Vec<Kind> = report
                    .map(|report| {
                        report
                            .due
                            .iter()
                            .filter(|item| maintenance_mac::counts_as_due(item, macos_major))
                            .filter_map(|item| kind_of(item.kind))
                            .collect()
                    })
                    .unwrap_or_default();
                due.sort();
                due.dedup();
                Seen {
                    heard: node.is_some(),
                    announced: fleet.announced(&id, FEATURE),
                    role: node.map(|node| node.role.clone()).unwrap_or_default(),
                    due,
                    macos_skip: maintenance_mac::macos_skip(report),
                    id,
                    group,
                }
            })
            .collect())
    }

    /// The `macos_major` of the maintenance table of the group `group` (empty: none): false for
    /// a host in no group, or in a group the coordinator does not hold.
    fn macos_major_of(&self, group: &str) -> bool {
        group
            .parse::<GroupName>()
            .ok()
            .and_then(|name| self.desired.host_group(&name))
            .is_some_and(|held| held.policy.maintenance.macos_major)
    }

    /// Whether the hosts of `hosts` that wait are every enrolled host of one of their groups.
    ///
    /// # Errors
    /// `store_failed`: a group's hosts cannot be read.
    fn takes_a_last_host(&self, hosts: &[RunHost]) -> Result<bool, Refusal> {
        let mut taken: BTreeMap<&str, usize> = BTreeMap::new();
        for host in hosts.iter().filter(|host| host.step == Step::Waiting) {
            if !host.host_group.is_empty() {
                *taken.entry(host.host_group.as_str()).or_default() += 1;
            }
        }
        for (group, count) in taken {
            let Ok(name) = group.parse::<GroupName>() else {
                continue;
            };
            let members = self.hosts_in(&name).map_err(|_| Refusal::StoreFailed)?;
            if count >= members.len() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `Maintenance.List` of `request`, by `caller`: the newest runs first, in the states asked
    /// (every run when none is), at most `limit` of them (capped at [`MAX_LIST`]; 0: the cap).
    ///
    /// # Errors
    /// `unauthorized` and `forbidden`; `invalid_request`: a state unspecified or unknown.
    pub(crate) fn maintenance_list(
        &self,
        caller: &Caller,
        request: &proto::ListMaintenanceRunsRequest,
    ) -> Result<proto::ListMaintenanceRunsResponse, Refusal> {
        caller.operator()?;
        let mut states = Vec::new();
        for value in &request.states {
            match MaintenanceState::try_from(*value) {
                Ok(MaintenanceState::Unspecified) | Err(_) => return Err(Refusal::InvalidRequest),
                Ok(state) => states.push(state),
            }
        }
        let limit = if request.limit == 0 {
            MAX_LIST
        } else {
            request.limit.min(MAX_LIST)
        };
        let now = self.ops.unix_now();
        let runs = self.ops.read(|state| {
            state
                .maintenance_runs
                .iter()
                .rev()
                .map(|run| run_proto(run, now))
                .filter(|run| states.is_empty() || states.contains(&run.state()))
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .collect()
        });
        Ok(proto::ListMaintenanceRunsResponse { runs })
    }

    /// `Maintenance.Get` of `request`, by `caller`: the run, with each of its hosts by node id.
    ///
    /// # Errors
    /// `unauthorized` and `forbidden`; `invalid_request`: not a run id; `unknown_run`.
    pub(crate) fn maintenance_get(
        &self,
        caller: &Caller,
        request: &proto::GetMaintenanceRunRequest,
    ) -> Result<proto::GetMaintenanceRunResponse, Refusal> {
        caller.operator()?;
        let id = run_id(&request.run_id)?;
        let now = self.ops.unix_now();
        self.ops
            .read(|state| {
                state
                    .maintenance_runs
                    .iter()
                    .find(|run| run.id == id)
                    .map(|run| proto::GetMaintenanceRunResponse {
                        run: Some(run_proto(run, now)),
                        hosts: run
                            .hosts
                            .iter()
                            .map(|host| host_proto(&run.shown(host, now)))
                            .collect(),
                    })
            })
            .ok_or(Refusal::UnknownRun)
    }

    /// `Maintenance.Cancel` of `request`, by `caller`: no host of the run starts from now on; a
    /// host in its turn finishes it, and each host that had not started ends as not reached.
    ///
    /// # Errors
    /// `unauthorized` and `forbidden`; `invalid_request`: not a run id, or a reason that breaks
    /// its rule; `unknown_run`; `run_ended`: it is cancelled, done or past its window;
    /// `store_failed`; `audit_unavailable`.
    pub(crate) fn maintenance_cancel(
        &self,
        caller: &Caller,
        request: proto::CancelMaintenanceRunRequest,
    ) -> Result<proto::CancelMaintenanceRunResponse, Refusal> {
        let by = caller.operator()?;
        let id = run_id(&request.run_id)?;
        let reason = valid_reason(&request.reason).ok_or(Refusal::InvalidRequest)?;
        let now = self.ops.unix_now();
        let mut entry = AuditEntry::new(by, MAINTENANCE_CANCEL);
        entry.reason = Some(format!("{reason} run {id}"));
        let mut cancelled = None;
        let done = self.ops.try_change(Write::BeforeMemory, entry, |state| {
            let run = state
                .maintenance_runs
                .iter_mut()
                .find(|run| run.id == id)
                .ok_or(Refusal::UnknownRun)?;
            if run.ended_at(now) {
                return Err(Refusal::RunEnded);
            }
            run.cancel(now);
            cancelled = Some(run_proto(run, now));
            Ok(())
        })?;
        Ok(proto::CancelMaintenanceRunResponse {
            run: cancelled,
            entry: Some(entry_proto(&done)),
        })
    }
}

#[cfg(test)]
#[path = "maintenance_service_tests.rs"]
mod tests;
