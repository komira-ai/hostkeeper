//! Force: the fleet-wide fix, the coordinator's half (design 5.6). `Releases.Force` names a
//! service, a version, a reason and the targets (the whole fleet, a role, or a list of hosts),
//! and orders a hookless `force` at each targeted host's next heartbeat, past the one live
//! update, the canary, the bake, a deferral, a bad mark, a halt and a host's failure of the
//! version, so one call may reach every target in the same heartbeat round.
//!
//! [`Coordinator::force`] refuses, before anything changes: the service `hostkeeper`, which
//! updates itself by its own path (`self_service`); a version below the service's `min_version`
//! (`below_min_version`; the floor itself is accepted); a version CI registered no release
//! (a package digest or a release index) for (`no_digest`); a fleet-wide force whose `confirm_service` is not the service
//! (`confirm_mismatch`); a role the service does not run on, or a host the coordinator has not
//! heard from since it started (`unknown_target`).
//!
//! Otherwise it sets the desired version to the target, through the store CI's set writes
//! (the registry keeps the digest, and the floor was checked above), and then, in one write of
//! the ops file with its audit entries in this order: records the force (`force`, by the
//! operator's token, naming the targets and the hosts holding a slot it passed), lifts the
//! bad mark of the target (`unmark_bad`), marks bad each version a targeted host leaves on a
//! downgrade, from the versions those hosts last reported (`mark_bad`), and clears a standing
//! halt (`clear_halt`). It always raises the service's halt generation by one: the order
//! carries it, so a host that holds a halt of the service lifts it.
//!
//! A targeted pair an operator holds is skipped and listed `held`, unless the call sets
//! `include_held`: then it is ordered, for this call only, and no hold is removed. A target
//! whose daemon did not announce [`FEATURE`] is listed `skipped` as `daemon_too_old` and gets
//! nothing from the force; a target of a version registered with a release index that lists no
//! package for its platform is listed `skipped` as `no_package_for_platform`; a host named in
//! the call that does not run the service is listed `skipped` as `unknown_target`. Each host
//! ordered gets the package of its own platform, and the digest of the index it was taken from
//! ([`ForceRecord::package_for`]).
//!
//! The force stands until CI or an operator sets another version ([`end_by_ci`],
//! [`end_by_setter`]) or another force replaces it. While it stands the automatic rollback of the service is suspended
//! ([`crate::reconcile`]) and the pairs it orders take no update slot ([`crate::decide`]), so
//! the slot accounting is the same when it ends as if it had never been. Its order goes to each
//! host it orders at every heartbeat until the host runs the version, is running the force, or
//! reported a verdict of it ([`crate::orders`]); a host that reported the force failed is not
//! ordered again, and a second force orders it again.

use std::collections::BTreeSet;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::force_request::Targets;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::{HostId, Platform, RoleName};
use serde::{Deserialize, Serialize};

use crate::fleet::Fleet;
use crate::holds::marks_of;
use crate::operator::{Caller, entry_proto, valid_reason};
use crate::ops::{AuditEntry, HaltAck, OpsError, OpsState, OpsStore, Write};
use crate::registry::{Digest, IndexRecord, Package, Release};
use crate::service::{Coordinator, kept};
use crate::services::{ServiceDef, Services};

/// The feature a daemon announces to be sent a `force`. The coordinator advertises it too
/// ([`crate::service::FEATURES`]): a daemon runs a force only while the reply advertises it.
pub const FEATURE: &str = "forced-v1";

/// The service no force may name: hostkeeper updates itself by its own path (design 6.6).
pub const SELF_SERVICE: &str = "hostkeeper";

/// The hosts a force targets, as the call named them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForceTargets {
    /// Every host that runs the service.
    Fleet,
    /// Every host of a role that runs the service.
    Role(String),
    /// These hosts, by node id.
    Hosts(Vec<String>),
}

impl ForceTargets {
    /// The targets as the audit says them: `fleet`, `role storage` or `hosts d1,d2`.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Fleet => "fleet".to_owned(),
            Self::Role(role) => format!("role {role}"),
            Self::Hosts(hosts) => format!("hosts {}", hosts.join(",")),
        }
    }
}

/// A force, as the ops file records it while it stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForceRecord {
    /// The version forced.
    pub version: Version,
    /// One more for every force of the service: the force's order id is `force-<generation>`.
    pub generation: u64,
    /// Why, in the operator's words.
    pub reason: String,
    /// The name of the operator's token that forced it.
    pub by: String,
    /// The package digest CI registered for the version, which the order names; for a version
    /// registered with a release index, the digest of that index (the packages are in
    /// `release_index`).
    pub digest: Digest,
    /// The record of the release index CI registered for the version, if it registered one:
    /// each host is ordered the package it lists for the host's platform. Written only then, so
    /// the record of a version with one package digest is as it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_index: Option<IndexRecord>,
    /// The halt generation the order carries.
    pub halt_generation: u64,
    /// The targets as the call named them.
    pub targets: ForceTargets,
    /// The call ordered the held pairs it targeted.
    pub include_held: bool,
    /// The hosts the force orders, by node id.
    pub hosts: Vec<String>,
}

impl ForceRecord {
    /// The order id of the force: `force-<generation>`, the same at every resend.
    #[must_use]
    pub fn order_id(&self) -> String {
        format!("force-{}", self.generation)
    }

    /// The package a host of `platform` is ordered: the one the index lists for it, or, for a
    /// version registered with one package digest, that digest. None for a platform the index
    /// does not list.
    #[must_use]
    pub fn package_for(&self, platform: Platform) -> Option<Package> {
        match &self.release_index {
            Some(record) => record.package_for(platform),
            None => Some(Package {
                digest: self.digest.clone(),
                index_digest: None,
            }),
        }
    }
}

/// Whom a force orders, as [`plan`] finds them in the fleet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The hosts ordered, by node id.
    pub ordered: Vec<HostId>,
    /// The targeted hosts skipped because an operator holds the pair.
    pub held: Vec<String>,
    /// The other targeted hosts not ordered, each with its reason token (`daemon_too_old`,
    /// `no_package_for_platform`, `unknown_target`).
    pub skipped: Vec<(String, &'static str)>,
    /// The versions the ordered hosts run that are newer than the version forced: a downgrade
    /// leaves them, and they are marked bad.
    pub left: Vec<Version>,
    /// The hosts holding an update slot that the force passes, as `host:service`.
    pub passed: Vec<String>,
}

/// A force names a role the service does not run on, or a host the coordinator has not heard
/// from since it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownTarget;

/// What a force orders: a version and the release CI registered for it.
#[derive(Clone, Copy, Debug)]
pub struct Forced<'a> {
    /// The version forced.
    pub version: &'a Version,
    /// Its package digest, or its release index.
    pub release: &'a Release,
}

/// Whom a force of `def` to `forced` with `targets` orders in `fleet` at `now`: the hosts of
/// the service the targets name, less the held pairs (unless `include_held`), the hosts whose
/// daemon did not announce [`FEATURE`] and those whose platform the release lists no package
/// for. A pure function: it changes nothing.
///
/// # Errors
/// [`UnknownTarget`]: a role `def` does not run on, or a host the fleet has not heard from.
pub fn plan(
    def: &ServiceDef,
    services: &Services,
    forced: Forced<'_>,
    targets: &ForceTargets,
    include_held: bool,
    fleet: &Fleet,
    now: Duration,
) -> Result<Plan, UnknownTarget> {
    let service = def.name.as_str();
    let mut plan = Plan::default();
    let members = || {
        fleet
            .hosts_of(service)
            .filter(|(_, host)| def.roles.contains(host.role_name()))
    };
    let targeted: Vec<(HostId, Version, Platform)> = match targets {
        ForceTargets::Fleet => members()
            .map(|(id, host)| (id.clone(), host.beat.version.clone(), host.beat.platform))
            .collect(),
        ForceTargets::Role(role) => {
            if !def.roles.contains(role) {
                return Err(UnknownTarget);
            }
            members()
                .filter(|(_, host)| host.role_name() == role.as_str())
                .map(|(id, host)| (id.clone(), host.beat.version.clone(), host.beat.platform))
                .collect()
        }
        ForceTargets::Hosts(names) => {
            let mut found = Vec::new();
            for id in names.iter().filter_map(|name| name.parse::<HostId>().ok()) {
                if fleet.node(&id).is_none() {
                    return Err(UnknownTarget);
                }
                match members().find(|(member, _)| **member == id) {
                    Some((_, host)) => {
                        found.push((id.clone(), host.beat.version.clone(), host.beat.platform));
                    }
                    None => plan.skipped.push((id.to_string(), "unknown_target")),
                }
            }
            found
        }
    };
    for (id, running, platform) in targeted {
        if !include_held && fleet.is_held(service, &id) {
            plan.held.push(id.to_string());
        } else if !fleet.announced(&id, FEATURE) {
            plan.skipped.push((id.to_string(), "daemon_too_old"));
        } else if forced.release.package_for(platform).is_none() {
            plan.skipped
                .push((id.to_string(), "no_package_for_platform"));
        } else {
            if !forced.version.at_least(&running) && !plan.left.contains(&running) {
                plan.left.push(running);
            }
            plan.ordered.push(id);
        }
    }
    for other in services.defs() {
        let name = other.name.as_str();
        for (id, host) in fleet.occupants_of(name) {
            let forced = name == service && plan.ordered.contains(id);
            if !forced && fleet.holds_slot(name, id, host, now) {
                plan.passed.push(format!("{id}:{name}"));
            }
        }
    }
    Ok(plan)
}

/// What a force writes to the ops file, as the call and its [`Plan`] decided it.
struct Forcing<'a> {
    service: &'a str,
    version: &'a Version,
    release: &'a Release,
    by: &'a str,
    reason: &'a str,
    targets: &'a ForceTargets,
    include_held: bool,
    plan: &'a Plan,
}

impl Forcing<'_> {
    /// The audit's reason of the `force` entry: the operator's reason, the version, the targets
    /// and the slot holders passed.
    fn summary(&self) -> String {
        let passed = if self.plan.passed.is_empty() {
            "none".to_owned()
        } else {
            self.plan.passed.join(",")
        };
        let held = if self.include_held {
            "; include_held"
        } else {
            ""
        };
        format!(
            "{}; version {}; targets {}{held}; passed {passed}",
            self.reason,
            self.version,
            self.targets.describe()
        )
    }

    /// Makes the force's changes to `state` and answers its audit entries, in order: `force`,
    /// then `unmark_bad`, `mark_bad` and `clear_halt` as they apply.
    fn apply(&self, state: &mut OpsState) -> Vec<AuditEntry> {
        let (service, version) = (self.service, self.version);
        let entry = |action: &str, reason: String| {
            let mut entry = AuditEntry::new(self.by, action);
            entry.service = Some(service.to_owned());
            entry.reason = Some(reason);
            entry
        };
        let mut entries = vec![entry("force", self.summary())];
        let marks = state.services.entry(service.to_owned()).or_default();
        marks.force_generation = marks.force_generation.saturating_add(1);
        marks.halt_generation = marks.halt_generation.saturating_add(1);
        if marks.bad.contains(version) {
            marks.bad.retain(|bad| bad != version);
            marks.bad_by_force.retain(|bad| bad != version);
            entries.push(entry("unmark_bad", format!("{version} is forced")));
        }
        for left in &self.plan.left {
            if !marks.bad.contains(left) {
                marks.bad.push(left.clone());
                marks.bad_by_force.push(left.clone());
                entries.push(entry(
                    "mark_bad",
                    format!("{left} is left by the force to {version}"),
                ));
            }
        }
        if let Some(halt) = marks.halt.take() {
            let seen = std::mem::take(&mut marks.halt_seen);
            for report in std::iter::once(HaltAck::from(halt)).chain(seen) {
                marks.halt_acked.retain(|ack| ack.node_id != report.node_id);
                marks.halt_acked.push(report);
            }
            entries.push(entry(
                "clear_halt",
                format!("cleared by the force to {version}"),
            ));
        }
        marks.force = Some(ForceRecord {
            version: version.clone(),
            generation: marks.force_generation,
            reason: self.reason.to_owned(),
            by: self.by.to_owned(),
            digest: match self.release {
                Release::Package(digest) => digest.clone(),
                Release::Index(record) => record.digest.clone(),
            },
            release_index: match self.release {
                Release::Package(_) => None,
                Release::Index(record) => Some(record.clone()),
            },
            halt_generation: marks.halt_generation,
            targets: self.targets.clone(),
            include_held: self.include_held,
            hosts: self.plan.ordered.iter().map(ToString::to_string).collect(),
        });
        entries
    }
}

/// The targets of a force request, checked: a fleet target must confirm the service by name, a
/// role must be a role name, a host list must name one host at least, each a node id.
fn targets_of(
    service: &str,
    targets: Option<Targets>,
) -> Result<ForceTargets, proto::ReleasesRefusal> {
    match targets.ok_or(proto::ReleasesRefusal::InvalidRequest)? {
        Targets::Fleet(fleet) if fleet.confirm_service == service => Ok(ForceTargets::Fleet),
        Targets::Fleet(_) => Err(proto::ReleasesRefusal::ConfirmMismatch),
        Targets::Role(role) => role
            .parse::<RoleName>()
            .map(|role| ForceTargets::Role(role.to_string()))
            .map_err(|_| proto::ReleasesRefusal::InvalidRequest),
        Targets::Hosts(list) => {
            let ids = list
                .node_ids
                .iter()
                .map(|id| id.parse::<HostId>().map(|id| id.to_string()))
                .collect::<Result<BTreeSet<String>, _>>()
                .map_err(|_| proto::ReleasesRefusal::InvalidRequest)?;
            if ids.is_empty() {
                return Err(proto::ReleasesRefusal::InvalidRequest);
            }
            Ok(ForceTargets::Hosts(ids.into_iter().collect()))
        }
    }
}

/// Why a CI set ended no force.
enum Kept {
    /// No force stands, or it is of the version set.
    Standing,
    /// The ops file could not be written.
    Store(OpsError),
}

impl From<OpsError> for Kept {
    fn from(why: OpsError) -> Self {
        Self::Store(why)
    }
}

/// Ends the force of `service` when CI has set a version other than the one forced, `version`,
/// in the run of `workflow`: written first, audited as CI's (`force_ended`). Answers whether a
/// force ended.
///
/// # Errors
/// [`OpsError`]: the ops file could not be written; the force stands.
pub fn end_by_ci(
    ops: &OpsStore,
    service: &str,
    version: &Version,
    workflow: &str,
) -> Result<bool, OpsError> {
    end_by_setter(ops, service, version, crate::reconcile::CI, workflow)
}

/// [`end_by_ci`] for any setter of the desired version: the audit entry `force_ended` is by
/// `actor`, and its reason says the version was set by `set_by`.
///
/// # Errors
/// [`OpsError`]: the ops file could not be written; the force stands.
pub(crate) fn end_by_setter(
    ops: &OpsStore,
    service: &str,
    version: &Version,
    actor: &str,
    set_by: &str,
) -> Result<bool, OpsError> {
    let ended = ops.decide_change(Write::BeforeMemory, |state| {
        let force = state
            .services
            .get_mut(service)
            .and_then(|marks| marks.force.take_if(|force| force.version != *version))
            .ok_or(Kept::Standing)?;
        let mut entry = AuditEntry::new(actor, "force_ended");
        entry.service = Some(service.to_owned());
        entry.reason = Some(format!(
            "{version} set by {set_by}; the force to {} ends",
            force.version
        ));
        Ok::<_, Kept>(entry)
    });
    match ended {
        Ok(_) => Ok(true),
        Err(Kept::Standing) => Ok(false),
        Err(Kept::Store(why)) => Err(why),
    }
}

impl Coordinator {
    /// `Releases.Force`: forces `request.service` to `request.version` on the targeted hosts,
    /// for the operator `caller` (the module docs say what it refuses and what it changes).
    ///
    /// # Errors
    /// The refusal, as `ReleasesRefusal`: `unauthorized` or `forbidden` for a caller that is no
    /// operator, then `self_service`, `unknown_service`, `invalid_request`, `confirm_mismatch`,
    /// `below_min_version`, `no_digest`, `unknown_target`; `store_failed` when the desired state
    /// or the ops file could not be written; `audit_unavailable` when the force's audit lines
    /// could not be appended, and the desired state is put back as it was.
    pub fn force(
        &self,
        caller: &Caller,
        request: proto::ForceRequest,
    ) -> Result<proto::ForceResponse, proto::ReleasesRefusal> {
        let by = caller.operator()?;
        if request.service == SELF_SERVICE {
            return Err(proto::ReleasesRefusal::SelfService);
        }
        let def = self
            .services
            .get(&request.service)
            .ok_or(proto::ReleasesRefusal::UnknownService)?;
        let version = Version::parse(&request.version)
            .ok()
            .filter(|version| def.version_scheme.parses(version))
            .ok_or(proto::ReleasesRefusal::InvalidRequest)?;
        let reason = valid_reason(&request.reason).ok_or(proto::ReleasesRefusal::InvalidRequest)?;
        let targets = targets_of(&def.name, request.targets)?;
        if def
            .min_version
            .as_ref()
            .is_some_and(|floor| !version.at_least(floor))
        {
            return Err(proto::ReleasesRefusal::BelowMinVersion);
        }
        let release = self
            .desired
            .digests(&def.name)
            .get(&version)
            .cloned()
            .ok_or(proto::ReleasesRefusal::NoDigest)?;
        let now = (self.clock)();
        // The fleet's lock is held to the end: no heartbeat decides or rolls back between the
        // plan and the writes, as for CI's set.
        let mut fleet = self.lock_fleet();
        let plan = plan(
            def,
            &self.services,
            Forced {
                version: &version,
                release: &release,
            },
            &targets,
            request.include_held,
            &fleet,
            now,
        )
        .map_err(|UnknownTarget| proto::ReleasesRefusal::UnknownTarget)?;
        // The desired state first: a force whose ops write fails leaves the target desired and
        // its bad mark standing, so nothing is ordered of it until the force is asked again. A
        // registered digest comes only with a desired state CI set, so there is one; its limits
        // are kept.
        let before = self
            .desired
            .get(&def.name)
            .ok_or(proto::ReleasesRefusal::NoDigest)?;
        let mut wanted = before.clone();
        wanted.version = version.clone();
        wanted.set_release(Some(release.clone()));
        let marks = self.ops.marks(&def.name);
        self.desired
            .record(&def.name, wanted, |v| kept(&marks, v))
            .map_err(OpsError::from)?;
        let forcing = Forcing {
            service: &def.name,
            version: &version,
            release: &release,
            by,
            reason: &reason,
            targets: &targets,
            include_held: request.include_held,
            plan: &plan,
        };
        let changed = self.ops.decide_changes(Write::BeforeMemory, |state| {
            Ok::<_, proto::ReleasesRefusal>(forcing.apply(state))
        });
        // A force the audit cannot record is not made: its desired state, written above, is put
        // back (the fleet's lock is still held, so no heartbeat has read it). A put-back that
        // fails is logged and leaves the target desired with no force, as a failed ops write
        // does.
        if changed == Err(proto::ReleasesRefusal::AuditUnavailable)
            && let Err(why) = self.desired.set(&def.name, before)
        {
            tracing::error!(
                service = %def.name,
                %why,
                "a force refused for its audit could not put the desired state back"
            );
        }
        let entries = changed?;
        fleet.set_marks(marks_of(&self.ops.state()));
        tracing::warn!(
            service = %def.name,
            %version,
            by,
            ordered = plan.ordered.len(),
            "a force was ordered: it passes the one live update, the canary and the bake"
        );
        Ok(proto::ForceResponse {
            entries: entries.iter().map(entry_proto).collect(),
            held: plan.held,
            targeted: u32::try_from(plan.ordered.len()).unwrap_or(u32::MAX),
            skipped: plan
                .skipped
                .into_iter()
                .map(|(node_id, reason)| proto::SkippedHost {
                    node_id,
                    reason: reason.to_owned(),
                })
                .collect(),
        })
    }
}
