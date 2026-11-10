//! The policy of a group the migration makes (`docs/design/hostgroups.md` section 10): the rollout
//! file's per-service keys when every service of the group agrees, and otherwise the most careful
//! value of each key, each such choice said in a line of the report.

use crate::defer::DeferBackoff;
use crate::host_groups::{Policy, PolicyCanary, PolicyStage};
use crate::reconcile::{FailThreshold, Rollback};
use crate::services::ServiceDef;
use crate::stages::{Advance, Plan, Share, StageDef};

/// The limits the desired state of one package of the group holds for the group's role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits<'a> {
    /// The package.
    pub(crate) package: &'a str,
    /// How many hosts of the role may update at once.
    pub(crate) max_updating: u32,
    /// How many hosts of the role must stay up.
    pub(crate) min_up: u32,
}

/// A policy and the choices that made it: one line for each key whose services disagreed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Merged {
    /// The policy.
    pub(crate) policy: Policy,
    /// The lines that say a choice, each starting with two spaces.
    pub(crate) choices: Vec<String>,
}

/// The stages of the default plan, as a policy writes them: none.
fn default_stages() -> Vec<PolicyStage> {
    stages_of(&Plan::default(), 0)
}

/// The percent stages of `plan`, `skip` stages (the canary's) left out.
pub(crate) fn stages_of(plan: &Plan, skip: usize) -> Vec<PolicyStage> {
    plan.defs()
        .iter()
        .skip(skip)
        .filter_map(|stage: &StageDef| match stage.share {
            Share::Percent(percent) => Some(PolicyStage {
                percent,
                advance: stage.advance,
            }),
            Share::Hosts(_) => None,
        })
        .collect()
}

/// The stages and the canary of `def`, as a policy writes them: the default plan is none.
fn plan_of(def: &ServiceDef) -> (Vec<PolicyStage>, Option<PolicyCanary>) {
    let mut stages = stages_of(&def.plan, usize::from(def.canary.is_some()));
    if stages == default_stages() {
        stages.clear();
    }
    let canary = def.canary.map(|canary| PolicyCanary {
        count: canary.count,
    });
    (stages, canary)
}

/// How many of `hosts` hosts the first stage of `def`'s plan holds.
fn first_stage(def: &ServiceDef, hosts: usize) -> usize {
    def.plan
        .defs()
        .first()
        .map_or(0, |stage| match stage.share {
            Share::Hosts(count) => (count as usize).min(hosts),
            Share::Percent(percent) => (percent as usize * hosts).div_ceil(100),
        })
}

/// The stages as the report says them: `1/10/50 (manual)/100`, or `default`.
pub(crate) fn stages_text(stages: &[PolicyStage]) -> String {
    if stages.is_empty() {
        return "default".to_owned();
    }
    stages
        .iter()
        .map(|stage| match stage.advance {
            Advance::Auto => stage.percent.to_string(),
            Advance::Manual => format!("{} (manual)", stage.percent),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// `name value` of each service, in the order of `defs`.
fn listed<T: std::fmt::Display>(defs: &[&ServiceDef], value: impl Fn(&ServiceDef) -> T) -> String {
    defs.iter()
        .map(|def| format!("{} {}", def.name, value(def)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The one value of `value` over `first` and `others` when they all agree, else the one `pick`
/// folds out of them, with the line that says the choice (`key` is its name, `how` what the
/// choice was).
fn chosen<T: Copy + PartialEq + std::fmt::Display>(
    first: &ServiceDef,
    others: &[&ServiceDef],
    key: &str,
    how: &str,
    value: impl Fn(&ServiceDef) -> T,
    pick: impl Fn(T, T) -> T,
    choices: &mut Vec<String>,
) -> T {
    let one = value(first);
    let picked = others.iter().fold(one, |held, def| pick(held, value(def)));
    if others.iter().any(|def| value(def) != one) {
        let mut all = vec![first];
        all.extend(others);
        choices.push(format!(
            "  {key} {picked}: {how} of {}",
            listed(&all, &value)
        ));
    }
    picked
}

/// The policy of a group of `hosts` hosts whose services the rollout file defines as `defs` (in
/// the order of their names), over the limits `limits` of the packages that have a desired state
/// and, with none, `fallback`: the role's default limits, `(max_updating, min_up)`.
pub(crate) fn merge(
    defs: &[&ServiceDef],
    hosts: usize,
    limits: &[Limits<'_>],
    fallback: (u32, u32),
) -> Merged {
    let mut choices = Vec::new();
    let mut policy = Policy::default();
    if let Some((first, others)) = defs.split_first() {
        let first = *first;
        let smallest = if others.iter().all(|def| plan_of(def) == plan_of(first)) {
            first
        } else {
            let fewest = defs
                .iter()
                .min_by_key(|def| first_stage(def, hosts))
                .map_or(first, |def| *def);
            let (stages, canary) = plan_of(fewest);
            choices.push(format!(
                "  stages {}, canary {}: those of {}, whose first stage holds the fewest hosts ({})",
                stages_text(&stages),
                canary.map_or_else(|| "none".to_owned(), |c| c.count.to_string()),
                fewest.name,
                listed(defs, |def| first_stage(def, hosts)),
            ));
            fewest
        };
        (policy.stages, policy.canary) = plan_of(smallest);
        let mut pick = |key: &str, how: &str, value: &dyn Fn(&ServiceDef) -> u64, low: bool| {
            let pick: fn(u64, u64) -> u64 = if low { u64::min } else { u64::max };
            chosen(first, others, key, how, value, pick, &mut choices)
        };
        let small = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        policy.bake_beats = small(pick(
            "bake_beats",
            "the highest",
            &|def| u64::from(def.bake_beats),
            false,
        ));
        let percent = pick(
            "rollback_after_failed percent",
            "the lowest",
            &|def| u64::from(def.rollback_after_failed.percent),
            true,
        );
        let min_hosts = pick(
            "rollback_after_failed min_hosts",
            "the lowest",
            &|def| u64::from(def.rollback_after_failed.min_hosts),
            true,
        );
        policy.rollback_after_failed = FailThreshold {
            percent: small(percent),
            min_hosts: small(min_hosts),
        };
        let initial_secs = pick(
            "defer_backoff initial_secs",
            "the longest",
            &|def| def.defer_backoff.initial_secs,
            false,
        );
        let factor = pick(
            "defer_backoff factor",
            "the longest",
            &|def| u64::from(def.defer_backoff.factor),
            false,
        );
        let max_secs = pick(
            "defer_backoff max_secs",
            "the longest",
            &|def| def.defer_backoff.max_secs,
            false,
        );
        policy.defer_backoff = DeferBackoff {
            initial_secs,
            factor: small(factor),
            max_secs,
        };
        policy.rollback = if defs.iter().any(|def| def.rollback == Rollback::Manual) {
            Rollback::Manual
        } else {
            Rollback::Automatic
        };
        if defs.iter().any(|def| def.rollback != first.rollback) {
            choices.push(format!(
                "  rollback manual: manual over automatic, of {}",
                listed(defs, |def| match def.rollback {
                    Rollback::Manual => "manual",
                    Rollback::Automatic => "automatic",
                })
            ));
        }
    }
    let (max_updating, min_up) = limited(limits, fallback, &mut choices);
    policy.max_updating = max_updating;
    policy.min_up = min_up;
    Merged { policy, choices }
}

/// The lowest `max_updating` and the highest `min_up` of `limits`, or `fallback` when there is
/// none; a line in `choices` for each of the two whose packages disagree.
fn limited(limits: &[Limits<'_>], fallback: (u32, u32), choices: &mut Vec<String>) -> (u32, u32) {
    let Some(first) = limits.first() else {
        return fallback;
    };
    let say = |key: &str, how: &str, value: u32, of: &dyn Fn(&Limits<'_>) -> u32| {
        let all = limits
            .iter()
            .map(|l| format!("{} {}", l.package, of(l)))
            .collect::<Vec<_>>()
            .join(", ");
        format!("  {key} {value}: {how} of {all}")
    };
    let lowest = limits
        .iter()
        .map(|l| l.max_updating)
        .fold(first.max_updating, u32::min);
    let highest = limits.iter().map(|l| l.min_up).fold(first.min_up, u32::max);
    if limits.iter().any(|l| l.max_updating != lowest) {
        choices.push(say("max_updating", "the lowest", lowest, &|l| {
            l.max_updating
        }));
    }
    if limits.iter().any(|l| l.min_up != highest) {
        choices.push(say("min_up", "the highest", highest, &|l| l.min_up));
    }
    (lowest, highest)
}

/// The non-default keys of `policy` as the report says them: `stages 1/50/100, canary 1,
/// bake_beats 2`, or `default` for a policy that changes none.
pub(crate) fn policy_text(policy: &Policy) -> String {
    let none = Policy::default();
    let mut parts = Vec::new();
    if policy.stages != none.stages {
        parts.push(format!("stages {}", stages_text(&policy.stages)));
    }
    if let Some(canary) = policy.canary {
        parts.push(format!("canary {}", canary.count));
    }
    if policy.bake_beats != none.bake_beats {
        parts.push(format!("bake_beats {}", policy.bake_beats));
    }
    if policy.rollback != none.rollback {
        parts.push("rollback automatic".to_owned());
    }
    if policy.rollback_after_failed != none.rollback_after_failed {
        let threshold = policy.rollback_after_failed;
        parts.push(format!(
            "rollback_after_failed {}%/{}",
            threshold.percent, threshold.min_hosts
        ));
    }
    if policy.defer_backoff != none.defer_backoff {
        let backoff = policy.defer_backoff;
        parts.push(format!(
            "defer_backoff {}/{}/{}",
            backoff.initial_secs, backoff.factor, backoff.max_secs
        ));
    }
    if policy.max_updating != none.max_updating {
        parts.push(format!("max_updating {}", policy.max_updating));
    }
    if policy.min_up != none.min_up {
        parts.push(format!("min_up {}", policy.min_up));
    }
    if parts.is_empty() {
        return "default".to_owned();
    }
    parts.join(", ")
}
