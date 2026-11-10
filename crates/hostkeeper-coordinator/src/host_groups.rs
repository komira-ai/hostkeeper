//! What the coordinator keeps of its host groups (`docs/design/hostgroups.md`, sections 1.1, 2.3,
//! 3 and 8): each group's name, the ordered list of its packages, its rollout policy and its
//! generation, its desired version of each package, and, per (group, package), the marks of the
//! operations state (the minimum version, the last good version, the halt and its reports and
//! generation, the standing force and its generation, the update plan's position).
//!
//! This is the stored form only. Nothing decides by it yet: placement, the decision by group and
//! the `HostGroups` calls are built on it later, and until then no write puts a group in it.
//!
//! The groups and their desired versions are part of the desired state ([`crate::desired::Held`]),
//! kept in the state file under the top-level key `host_groups`; the marks are part of the
//! operations state ([`crate::ops::OpsState`]), kept in the ops file under its top-level key
//! `host_groups`. Each key is written only when it holds something, so a coordinator that holds no
//! group writes its files byte for byte as the release before did; once one is held, the release
//! before refuses both files at start, as its strict readers refuse any key they do not know. The
//! database keeps them in the tables of migration `0008`, which says older releases must not run on
//! it.
//!
//! A read is as strict as the file's: a group whose name, packages, policy or generation breaks
//! its rule is refused, naming the group, as is a key this build does not know.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use hostkeeper_wire::RoleName;
use hostkeeper_wire::version::Version;
use serde::{Deserialize, Serialize};

use crate::bake::MAX_BAKE_BEATS;
use crate::canary::Canary;
use crate::defer::DeferBackoff;
use crate::desired::MAX_COUNT;
use crate::force::{ForceRecord, SELF_SERVICE};
use crate::ops::{Halt, HaltAck};
use crate::reconcile::{FailThreshold, Rollback};
use crate::rollouts::Rollout;
use crate::stages::{Advance, Plan, RawStage};
use crate::write_record::WriteId;

/// The most packages a group lists.
pub(crate) const MAX_PACKAGES: usize = 64;

/// The most bytes of a package's name.
pub(crate) const MAX_PACKAGE_BYTES: usize = 255;

/// The most packages of one group that may roll at once, when its policy names a number.
pub(crate) const MAX_LIVE_ROLLOUTS: u32 = 16;

/// A group's `max_updating` when its policy names none.
pub(crate) const DEFAULT_MAX_UPDATING: u32 = 1;

/// The name of a host group: the grammar of a role name ([`RoleName`]), 1 to 32 bytes of `a-z`,
/// `0-9` and `-`, a letter first, no `--`, not ending in `-`, so that every role name is a group
/// name. Read from text only through [`FromStr`], which serde uses too.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct GroupName(String);

/// Text that is no group name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a host group name: 1 to 32 bytes of a-z, 0-9 and -, starting with a letter")]
pub(crate) struct BadGroupName;

impl GroupName {
    /// The name as text.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for GroupName {
    type Err = BadGroupName;

    fn from_str(name: &str) -> Result<Self, BadGroupName> {
        name.parse::<RoleName>().map_err(|_| BadGroupName)?;
        Ok(Self(name.to_owned()))
    }
}

impl TryFrom<String> for GroupName {
    type Error = BadGroupName;

    fn try_from(name: String) -> Result<Self, BadGroupName> {
        name.parse()
    }
}

impl From<GroupName> for String {
    fn from(name: GroupName) -> Self {
        name.0
    }
}

impl fmt::Display for GroupName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `name` is a package's name as a group lists it: 1 to [`MAX_PACKAGE_BYTES`] bytes of
/// `A-Z`, `a-z`, `0-9`, `.`, `_` and `-`.
pub(crate) fn is_package_name(name: &str) -> bool {
    (1..=MAX_PACKAGE_BYTES).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// One stage of a group's update plan: the percent of the group's eligible hosts it reaches, and
/// how the plan enters it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyStage {
    /// 1 to 100, rising from one stage to the next, the last 100.
    pub(crate) percent: u32,
    /// By itself (the default, not written) or by an operator.
    #[serde(default, skip_serializing_if = "is_auto")]
    pub(crate) advance: Advance,
}

fn is_auto(advance: &Advance) -> bool {
    *advance == Advance::Auto
}

/// A group's canary: the hosts that go alone first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyCanary {
    /// 1 to [`MAX_COUNT`].
    pub(crate) count: u32,
}

/// The rollout policy of every package of a group (section 3). Each key at its default is not
/// written, so a group that names none is written `{}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Policy {
    /// The stages; none is the default plan, 1, 10, 50 and 100 percent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) stages: Vec<PolicyStage>,
    /// The canary, which goes before the first stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) canary: Option<PolicyCanary>,
    /// Healthy beats before a stage is done, 0 to [`MAX_BAKE_BEATS`].
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) bake_beats: u32,
    /// Whether the coordinator rolls back by itself.
    #[serde(skip_serializing_if = "is_manual")]
    pub(crate) rollback: Rollback,
    /// How many failed hosts make an automatic rollback.
    #[serde(skip_serializing_if = "is_default_threshold")]
    pub(crate) rollback_after_failed: FailThreshold,
    /// How long a deferred host is left alone.
    #[serde(skip_serializing_if = "is_default_backoff")]
    pub(crate) defer_backoff: DeferBackoff,
    /// The most hosts of the group updating one package at once, 0 to [`MAX_COUNT`]; 0 holds
    /// the group.
    #[serde(skip_serializing_if = "is_default_max_updating")]
    pub(crate) max_updating: u32,
    /// The fewest of the group's live hosts that stay up for a package, 0 to [`MAX_COUNT`].
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) min_up: u32,
    /// How many of the group's packages may roll at once, 1 to [`MAX_LIVE_ROLLOUTS`]; none: the
    /// coordinator's `--live-rollouts`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) live_rollouts: Option<u32>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            stages: Vec::new(),
            canary: None,
            bake_beats: 0,
            rollback: Rollback::Manual,
            rollback_after_failed: FailThreshold::default(),
            defer_backoff: DeferBackoff::default(),
            max_updating: DEFAULT_MAX_UPDATING,
            min_up: 0,
            live_rollouts: None,
        }
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_manual(rollback: &Rollback) -> bool {
    *rollback == Rollback::Manual
}

fn is_default_threshold(threshold: &FailThreshold) -> bool {
    *threshold == FailThreshold::default()
}

fn is_default_backoff(backoff: &DeferBackoff) -> bool {
    *backoff == DeferBackoff::default()
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_default_max_updating(n: &u32) -> bool {
    *n == DEFAULT_MAX_UPDATING
}

/// A policy key out of its range: the key, and why.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{key}: {why}")]
pub(crate) struct PolicyError {
    /// The key, as the policy spells it.
    pub(crate) key: &'static str,
    /// What is wrong with it.
    pub(crate) why: String,
}

impl Policy {
    /// The update plan the policy's stages and canary make, the canary's stage first.
    ///
    /// # Errors
    /// [`PolicyError`] of `stages`: more than 16, a percent outside 1 to 100, percents that do
    /// not rise, or a last that is not 100.
    pub(crate) fn plan(&self) -> Result<Plan, PolicyError> {
        let stages = (!self.stages.is_empty()).then(|| {
            self.stages
                .iter()
                .map(|s| RawStage::table(s.percent, s.advance))
                .collect()
        });
        let canary = self.canary.map(|c| Canary { count: c.count });
        Plan::read(stages, canary).map_err(|why| PolicyError { key: "stages", why })
    }

    /// Whether every key is in its range (section 3), the ranges of the rollout file and of the
    /// desired state's limits.
    ///
    /// # Errors
    /// [`PolicyError`]: the first key, in the order of the policy, that is not.
    pub(crate) fn check(&self) -> Result<(), PolicyError> {
        let no = |key, why: String| Err(PolicyError { key, why });
        self.plan()?;
        if self
            .canary
            .is_some_and(|c| !(1..=MAX_COUNT).contains(&c.count))
        {
            return no("canary", format!("the count is not 1 to {MAX_COUNT}"));
        }
        if self.bake_beats > MAX_BAKE_BEATS {
            return no("bake_beats", format!("is over {MAX_BAKE_BEATS}"));
        }
        let threshold = self.rollback_after_failed;
        if !(1..=100).contains(&threshold.percent) {
            return no(
                "rollback_after_failed",
                "percent is not 1 to 100".to_owned(),
            );
        }
        if !(1..=MAX_COUNT).contains(&threshold.min_hosts) {
            return no(
                "rollback_after_failed",
                format!("min_hosts is not 1 to {MAX_COUNT}"),
            );
        }
        if let Err(why) = self.defer_backoff.check() {
            return no("defer_backoff", why);
        }
        if self.max_updating > MAX_COUNT {
            return no("max_updating", format!("is over {MAX_COUNT}"));
        }
        if self.min_up > MAX_COUNT {
            return no("min_up", format!("is over {MAX_COUNT}"));
        }
        if self
            .live_rollouts
            .is_some_and(|n| !(1..=MAX_LIVE_ROLLOUTS).contains(&n))
        {
            return no("live_rollouts", format!("is not 1 to {MAX_LIVE_ROLLOUTS}"));
        }
        Ok(())
    }
}

/// A group's desired version of one package, and how its writes are recorded: the generation,
/// one more for every accepted write; the write id of the last write; the version the last change
/// of version replaced. Kept on both stores, so a group's deployments read the same on either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupDesired {
    /// The version.
    pub(crate) version: Version,
    /// At least 1: a desired version is written once at least.
    pub(crate) generation: u64,
    /// The writer's id of the last write; none when it named none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) write_id: Option<String>,
    /// The version the last change of version replaced; none before the first change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) replaced_version: Option<Version>,
}

/// One host group (section 1.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostGroup {
    /// The packages its hosts run, in the order a host is offered them: 1 to [`MAX_PACKAGES`],
    /// none twice, `hostkeeper` among them.
    pub(crate) packages: Vec<String>,
    /// The rollout policy of every package of the group.
    #[serde(default, skip_serializing_if = "is_default_policy")]
    pub(crate) policy: Policy,
    /// Raised by every change of the group; at least 1.
    pub(crate) generation: u64,
    /// Who made the last change: the name of the operator's token, or `migration`.
    pub(crate) by: String,
    /// When, in unix seconds.
    pub(crate) at_unix: u64,
    /// The group's desired version of each package that has one, by package.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) desired: BTreeMap<String, GroupDesired>,
}

fn is_default_policy(policy: &Policy) -> bool {
    *policy == Policy::default()
}

/// Why a group is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum GroupError {
    /// The list is empty or longer than [`MAX_PACKAGES`].
    #[error("lists {0} packages, not 1 to {MAX_PACKAGES}")]
    Packages(usize),
    /// A package's name breaks its rule.
    #[error("{0:?} is not a package name")]
    Package(String),
    /// A package is listed twice.
    #[error("lists {0} twice")]
    Twice(String),
    /// `hostkeeper` is not listed.
    #[error("does not list {SELF_SERVICE}")]
    HostkeeperNotListed,
    /// A policy key is out of its range.
    #[error("policy {0}")]
    Policy(#[from] PolicyError),
    /// A generation is 0.
    #[error("{0} has generation 0")]
    Generation(&'static str),
    /// A desired version's package or write id breaks its rule.
    #[error("the desired version of {0:?}: {1}")]
    Desired(String, &'static str),
    /// Who made the last change is not said.
    #[error("names nobody who changed it")]
    By,
}

impl HostGroup {
    /// Whether the group keeps its rules (section 1.1).
    ///
    /// # Errors
    /// [`GroupError`]: the first rule it breaks.
    pub(crate) fn check(&self) -> Result<(), GroupError> {
        if !(1..=MAX_PACKAGES).contains(&self.packages.len()) {
            return Err(GroupError::Packages(self.packages.len()));
        }
        let mut seen = BTreeSet::new();
        for package in &self.packages {
            if !is_package_name(package) {
                return Err(GroupError::Package(package.clone()));
            }
            if !seen.insert(package.as_str()) {
                return Err(GroupError::Twice(package.clone()));
            }
        }
        if !seen.contains(SELF_SERVICE) {
            return Err(GroupError::HostkeeperNotListed);
        }
        self.policy.check()?;
        if self.generation == 0 {
            return Err(GroupError::Generation("the group"));
        }
        if self.by.is_empty() {
            return Err(GroupError::By);
        }
        for (package, desired) in &self.desired {
            if !is_package_name(package) {
                return Err(GroupError::Desired(package.clone(), "not a package name"));
            }
            if desired.generation == 0 {
                return Err(GroupError::Generation("a desired version"));
            }
            if desired
                .write_id
                .as_deref()
                .is_some_and(|id| WriteId::parse(id).is_err())
            {
                return Err(GroupError::Desired(package.clone(), "a bad write id"));
            }
        }
        Ok(())
    }
}

/// Every host group the coordinator holds, by name: the part of the desired state that is the
/// groups.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct HostGroups(pub(crate) BTreeMap<GroupName, HostGroup>);

impl HostGroups {
    /// Whether no group is held.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The groups the JSON `value` holds, each checked.
    ///
    /// # Errors
    /// Why they are refused, the group named.
    pub(crate) fn read(value: serde_json::Value) -> Result<Self, String> {
        let groups: Self =
            serde_json::from_value(value).map_err(|e| format!("host_groups: {e}"))?;
        for (name, group) in &groups.0 {
            group
                .check()
                .map_err(|e| format!("host group {name}: {e}"))?;
        }
        Ok(groups)
    }
}

/// The operations state's marks of one package in one group (section 2.3), each at its default
/// left out of the file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupMarks {
    /// The group's minimum version of the package, written by `Services.SetMinVersion` alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) min_version: Option<Version>,
    /// The newest version that converged and baked in the group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_good: Option<Version>,
    /// The standing halt, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) halt: Option<Halt>,
    /// The halt reports an operator cleared: one per host, the latest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) halt_acked: Vec<HaltAck>,
    /// The other halt reports received while the standing halt stood: one per host, the latest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) halt_seen: Vec<HaltAck>,
    /// How many halts an operator has cleared.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub(crate) halt_generation: u64,
    /// The force that stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) force: Option<ForceRecord>,
    /// How many times the package was forced in the group.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub(crate) force_generation: u64,
    /// Where the group's update plan of the package stands.
    #[serde(default, skip_serializing_if = "Rollout::is_empty")]
    pub(crate) rollout: Rollout,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// The marks of every (group, package) that has any, by group and then by package: the part of
/// the operations state that is the groups'. Written without the entries at their default
/// ([`GroupsMarks::without_defaults`]), which the code reads the same as absent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub(crate) struct GroupsMarks(pub(crate) BTreeMap<GroupName, BTreeMap<String, GroupMarks>>);

impl Serialize for GroupsMarks {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.clone().without_defaults().0.serialize(serializer)
    }
}

impl GroupsMarks {
    /// Whether no (group, package) has a mark other than its default.
    pub(crate) fn is_empty(&self) -> bool {
        self.0
            .values()
            .flat_map(BTreeMap::values)
            .all(|marks| *marks == GroupMarks::default())
    }

    /// The marks with every entry at its default left out, and every group left with none: the
    /// code reads a missing entry as its default, and a backend that stores none gives this back.
    #[must_use]
    pub(crate) fn without_defaults(mut self) -> Self {
        for packages in self.0.values_mut() {
            packages.retain(|_, marks| *marks != GroupMarks::default());
        }
        self.0.retain(|_, packages| !packages.is_empty());
        self
    }

    /// Whether every package named is a package's name.
    ///
    /// # Errors
    /// The (group, package) whose package is not.
    pub(crate) fn check(&self) -> Result<(), String> {
        for (group, packages) in &self.0 {
            if let Some(bad) = packages.keys().find(|p| !is_package_name(p)) {
                return Err(format!("host group {group}: {bad:?} is not a package name"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "host_groups_tests.rs"]
pub(crate) mod tests;
