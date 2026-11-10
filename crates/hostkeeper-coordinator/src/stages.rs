//! The update plan of a service (`docs/design/rollout-update-plans.md`): its stages, how many
//! hosts each holds, where the plan stands, and the gate it puts on a host's order.
//!
//! A plan is a list of stages, each the cumulative percent of the service's eligible hosts that
//! run the desired version once the stage is done (`stages = [1, 10, 50, 100]` in the rollout
//! file, the default), with `canary = { count = n }` as a first stage of `n` hosts. A stage of
//! percent `p` over `n` eligible hosts holds `max(1, ceil(p * n / 100))` hosts, never fewer than
//! the stage before and never more than `n` ([`Plan::stages`]); stages of the same size collapse
//! into one.
//!
//! A host is eligible ([`View::of`]) when it is live, assigned the service, neither excluded nor
//! held, not ordered by a standing force, has no failure of the desired version an operator
//! acknowledged (a cleared halt, a version set by hand), and could be ordered the desired
//! version: it runs the service, or it is to install it and its agent takes installs and the
//! version has a package for its platform. A host has started when it is updating, under an order, at the desired
//! version, or failed an update or install to it. The started hosts count against the current
//! stage; its free places go to the eligible hosts that have not started as they ask, the first
//! to ask first (a host that declined the update and waits does not ask: it is passed over).
//! A stage is done when as many hosts are up at the desired version and have baked (or were
//! recorded done) as it holds.
//!
//! Where the plan stands is the number of stages of the rollout file it has entered, kept per
//! service in the ops store ([`crate::rollouts`]); none kept for the desired version is the
//! first stage when it advances by itself, and none when it waits for an operator. The decision
//! reads it from the fleet's marks ([`gate`]); this module reads and decides, and writes nothing.
//!
//! Inside the current stage a host is ordered under the desired state's `max_updating` of its
//! role (`no_slot`: that many hosts of the role hold an update slot of the service; a role the
//! state does not name has the default, 1, or 2 for `daemon` and `mac-daemon`) and its `min_up` (`below_min_up`: the order would leave fewer
//! of the role's live hosts up). Across services, a service whose rollout is not live is told
//! `rollout_elsewhere` while the coordinator's `--live-rollouts` others are ([`is_live`]).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Outcome};
use serde::{Deserialize, Serialize};

use crate::canary::Canary;
use crate::decide::Gate;
use crate::desired::Desired;
use crate::fleet::{Fleet, Host};
pub use crate::live::is_live;
use crate::live::live_elsewhere;
use crate::membership::HEARTBEAT_V2;
use crate::orders::INSTALL_FEATURE;
use crate::rollouts::{Pause, Rollout};
use crate::services::{ServiceDef, Services};

/// The most stages a plan may have.
pub const MAX_STAGES: usize = 16;

/// The plan of a service that names none: 1, 10, 50 and 100 percent, each entered by itself.
pub const DEFAULT_STAGES: [u32; 4] = [1, 10, 50, 100];

/// How the plan enters a stage: by itself once the stage before is done, or by an operator's
/// `Rollouts.Advance`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Advance {
    /// By itself.
    #[default]
    Auto,
    /// By an operator.
    Manual,
}

/// How many hosts a stage of the rollout file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Share {
    /// This percent of the eligible hosts, rounded up.
    Percent(u32),
    /// This many hosts: the canary.
    Hosts(u32),
}

/// A stage as the rollout file says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageDef {
    /// How many hosts it holds.
    pub share: Share,
    /// How the plan enters it.
    pub advance: Advance,
}

/// The stages of a service's plan, as the rollout file says them: the canary's first, if it has
/// one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan(Vec<StageDef>);

impl Default for Plan {
    fn default() -> Self {
        Self(
            DEFAULT_STAGES
                .iter()
                .map(|p| StageDef {
                    share: Share::Percent(*p),
                    advance: Advance::Auto,
                })
                .collect(),
        )
    }
}

/// A stage of the `stages` key: a percent, or a table with its percent and how the plan enters
/// it. Anything else is kept to be refused with a text that says what to write.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum RawStage {
    Percent(u32),
    Table(RawStageTable),
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawStageTable {
    percent: u32,
    #[serde(default)]
    advance: Advance,
}

impl RawStage {
    /// The table form of a stage: `percent`, entered by `advance`.
    pub(crate) fn table(percent: u32, advance: Advance) -> Self {
        Self::Table(RawStageTable { percent, advance })
    }
}

impl Plan {
    /// The plan of a service from its `stages` key (the default plan when it has none) and its
    /// canary, which goes first.
    ///
    /// # Errors
    /// Why the list is refused: empty, more than [`MAX_STAGES`], a stage that is not a percent
    /// from 1 to 100 or a table of one with `advance`, percents that do not rise, or a last that
    /// is not 100.
    pub(crate) fn read(raw: Option<Vec<RawStage>>, canary: Option<Canary>) -> Result<Self, String> {
        let mut stages: Vec<StageDef> = canary
            .map(|c| StageDef {
                share: Share::Hosts(c.count),
                advance: Advance::Auto,
            })
            .into_iter()
            .collect();
        let Some(raw) = raw else {
            stages.extend(Self::default().0);
            return Ok(Self(stages));
        };
        if raw.is_empty() || raw.len() > MAX_STAGES {
            return Err(format!("stages lists 1 to {MAX_STAGES} stages"));
        }
        let mut last = 0;
        for stage in raw {
            let (percent, advance) = match stage {
                RawStage::Percent(percent) => (percent, Advance::Auto),
                RawStage::Table(RawStageTable { percent, advance }) => (percent, advance),
                RawStage::Other(_) => {
                    return Err("a stage is a percent, 10, or a table, { percent = 10, \
                                advance = \"auto\" } or \"manual\""
                        .to_owned());
                }
            };
            if !(1..=100).contains(&percent) || percent <= last {
                return Err("stages are percents from 1 to 100 that rise".to_owned());
            }
            last = percent;
            stages.push(StageDef {
                share: Share::Percent(percent),
                advance,
            });
        }
        if last != 100 {
            return Err("the last stage is 100".to_owned());
        }
        Ok(Self(stages))
    }

    /// The stages as the rollout file says them.
    #[must_use]
    pub fn defs(&self) -> &[StageDef] {
        &self.0
    }

    /// The stages over `n` eligible hosts, collapsed: none when there is no eligible host.
    ///
    /// A stage that comes out the size of the one before adds no host and is folded into it;
    /// when it is manual, the next stage that adds hosts waits for an operator instead.
    #[must_use]
    pub fn stages(&self, n: usize) -> Vec<PlanStage> {
        let mut out: Vec<PlanStage> = Vec::new();
        let mut manual = false;
        if n == 0 {
            return out;
        }
        for (i, def) in self.0.iter().enumerate() {
            let want = match def.share {
                Share::Percent(p) => (p as usize * n).div_ceil(100).max(1),
                Share::Hosts(count) => count as usize,
            };
            let prev = out.last().map_or(0, |s| s.hosts);
            let hosts = want.max(prev).min(n);
            let percent = match def.share {
                Share::Percent(p) => Some(p),
                Share::Hosts(_) => None,
            };
            match out.last_mut() {
                Some(last) if last.hosts == hosts => {
                    last.through = i + 1;
                    // A canary keeps its count: a percent folded into it is no percent of it.
                    if matches!(self.0[last.first].share, Share::Percent(_)) {
                        last.percent = percent.or(last.percent);
                    }
                    manual |= def.advance == Advance::Manual;
                }
                _ => {
                    let advance = if manual || def.advance == Advance::Manual {
                        Advance::Manual
                    } else {
                        Advance::Auto
                    };
                    manual = false;
                    out.push(PlanStage {
                        percent,
                        hosts,
                        advance,
                        first: i,
                        through: i + 1,
                    });
                }
            }
        }
        out
    }
}

/// A stage of the plan over the eligible hosts there are now, after collapsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanStage {
    /// The percent of the last stage of the rollout file folded into it; none for the canary and
    /// a stage folded into it.
    pub percent: Option<u32>,
    /// How many hosts run the desired version once it is done, the stages before included.
    pub hosts: usize,
    /// How the plan enters it.
    pub advance: Advance,
    /// The first stage of the rollout file folded into it, from 0.
    pub first: usize,
    /// One past the last.
    pub through: usize,
}

/// Where a host stands in the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    /// Up at the desired version and baked, or recorded done.
    Done,
    /// Up at the desired version, still baking.
    Baking,
    /// Ordered, updating, or at the version and not up.
    Updating,
    /// It failed an update or install to the desired version.
    Failed,
    /// It has not started.
    Waiting,
}

/// An eligible host of the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// The host.
    pub id: HostId,
    /// Its role.
    pub role: String,
    /// Where it stands.
    pub state: HostState,
    /// The stage it is in or waits for, from 1.
    pub stage: usize,
    /// Whether it declined the update and its wait is not over.
    pub deferring: bool,
}

/// Why a plan orders no new host: the status says it in snake case (`manual_advance`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Wait {
    /// The service is halted, or its desired version is marked bad.
    Halted,
    /// An operator paused the plan.
    Paused,
    /// The coordinator started less than its quiet start ago.
    Quiet,
    /// Other services' rollouts are live, as many as `--live-rollouts` lets be.
    RolloutElsewhere,
    /// The stage is done, or none is entered yet, and the next waits for an operator's Advance.
    ManualAdvance,
    /// A host of the stage failed the desired version and is still eligible.
    FailedHost,
    /// The next host of the stage would leave fewer hosts of its role up than `min_up`.
    BelowMinUp,
    /// Hosts of the stage are ordered or updating, or its free places are not taken yet.
    Updating,
    /// The hosts of the stage that are left to order declined the update and wait.
    Deferring,
    /// Every host of the stage runs the desired version, and some are still baking.
    Baking,
}

/// The plan of a service for its desired version, over the fleet at a moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// The desired version.
    pub version: Version,
    /// The stages over the eligible hosts there are, collapsed.
    pub stages: Vec<PlanStage>,
    /// How far the plan has entered: one past the first stage of the rollout file of the last
    /// stage it entered. A stage of the rollout file folded into that one is entered with it
    /// only while it adds no host: one that comes apart from it as hosts join is entered on its
    /// own, by an operator when it is manual.
    pub entered: usize,
    /// How many of [`Self::stages`] it has entered: it is in stage `current` of
    /// `stages.len()`, from 1; 0 before a manual first stage is entered.
    pub current: usize,
    /// Every eligible host, in the order the stages take them.
    pub members: Vec<Member>,
    /// The pause, if an operator paused the plan.
    pub paused: Option<Pause>,
    /// The hosts recorded done for the desired version.
    pub done: BTreeSet<String>,
}

impl View {
    /// The plan of `def` for `want` at `now`, at the position `kept` (the ops store's, which
    /// counts only when it is for `want`'s version).
    #[must_use]
    pub fn of(
        def: &ServiceDef,
        want: &Desired,
        kept: Option<&Rollout>,
        fleet: &Fleet,
        now: Duration,
    ) -> Self {
        let entered = kept.map_or_else(
            || first_entered(&def.plan),
            |kept| kept.entered_for(&def.plan, &want.version),
        );
        let done = kept
            .filter(|kept| kept.version.as_ref() == Some(&want.version))
            .map(|kept| kept.done.clone())
            .unwrap_or_default();
        let mut view = Self::at(def, want, entered, done, fleet, now);
        view.paused = kept.and_then(|kept| kept.paused.clone());
        view
    }

    /// The plan of `def` for `want` at `now` from the position `kept`, having entered every
    /// stage it may enter by itself since ([`reach`]): where the decision and the status read it.
    /// It enters none while the plan is paused, the service is halted or its version marked bad,
    /// or the coordinator is quiet, as the coordinator writes none then; otherwise the
    /// coordinator writes the position it reaches at the same heartbeat
    /// ([`crate::rollouts::advance`]), with its audit entry.
    #[must_use]
    pub fn effective(
        def: &ServiceDef,
        want: &Desired,
        kept: Option<&Rollout>,
        fleet: &Fleet,
        now: Duration,
    ) -> Self {
        let view = Self::of(def, want, kept, fleet, now);
        if view.paused.is_some()
            || fleet.is_stopped(&def.name, &want.version)
            || fleet.is_quiet(now)
        {
            return view;
        }
        let held = view.clone();
        reach(def, want, view, false, fleet, now).unwrap_or(held)
    }

    /// The plan of `def` for `want` at `now`, having entered `entered` stages of the rollout file
    /// with the hosts `done` recorded done.
    #[must_use]
    pub(crate) fn at(
        def: &ServiceDef,
        want: &Desired,
        entered: usize,
        done: BTreeSet<String>,
        fleet: &Fleet,
        now: Duration,
    ) -> Self {
        let mut members: Vec<Member> = eligible(def, want, fleet, now)
            .into_iter()
            .map(|(id, host)| {
                let state = state_of(def, want, &done, fleet, &id, host, now);
                let deferring =
                    state == HostState::Waiting && is_deferring(def, want, fleet, &id, now);
                Member {
                    role: fleet.role_in(&def.name, &id).to_owned(),
                    id,
                    state,
                    stage: 0,
                    deferring,
                }
            })
            .collect();
        // The hosts that started fill the stages first, the done ones first; then the hosts
        // left to order, in node-id order, the deferring ones last.
        members.sort_by(|a, b| (a.state, a.deferring, &a.id).cmp(&(b.state, b.deferring, &b.id)));
        let stages = def.plan.stages(members.len());
        let mut stage = 0;
        for (i, member) in members.iter_mut().enumerate() {
            while stages.get(stage).is_some_and(|s| s.hosts <= i) {
                stage += 1;
            }
            member.stage = stage + 1;
        }
        let current = stages.iter().filter(|s| s.first < entered).count();
        Self {
            version: want.version.clone(),
            stages,
            entered,
            current,
            members,
            paused: None,
            done,
        }
    }

    /// How many hosts the current stage holds: none before the first is entered.
    #[must_use]
    pub fn size(&self) -> usize {
        self.current
            .checked_sub(1)
            .and_then(|i| self.stages.get(i))
            .map_or(0, |s| s.hosts)
    }

    fn started(&self) -> impl Iterator<Item = &Member> {
        self.members
            .iter()
            .filter(|m| m.state != HostState::Waiting)
    }

    /// How many places of the current stage no host has taken: an eligible host that has not
    /// started takes one when it is ordered, the first to ask first.
    #[must_use]
    pub fn free(&self) -> usize {
        self.size().saturating_sub(self.started().count())
    }

    /// Whether the current stage is done: it is entered, and as many hosts are done as it holds.
    /// The hosts are in the order the stages take them, the done ones first, so these are the
    /// stage's own: a host the next stage already ordered does not undo it.
    #[must_use]
    pub fn stage_done(&self) -> bool {
        self.current > 0
            && !self.started().any(|m| m.state == HostState::Failed)
            && self
                .members
                .iter()
                .filter(|m| m.state == HostState::Done)
                .count()
                >= self.size()
    }

    /// The stage after the current one, if there is one.
    #[must_use]
    pub fn next(&self) -> Option<&PlanStage> {
        self.stages.get(self.current)
    }

    /// Whether every eligible host is done, and there is one: there is nothing left to roll. A
    /// plan with no eligible host (every host down, say) has rolled nothing.
    #[must_use]
    pub fn finished(&self) -> bool {
        !self.members.is_empty() && self.members.iter().all(|m| m.state == HostState::Done)
    }

    /// Whether the plan has started a host and has one left to order: the rollout is under way.
    /// A host that is deferring is not counted as left.
    #[must_use]
    pub fn under_way(&self) -> bool {
        self.started().next().is_some()
            && self
                .members
                .iter()
                .any(|m| m.state == HostState::Waiting && !m.deferring)
    }

    /// Why the plan orders no new host, from what it holds alone (the caller adds a halt, the
    /// quiet start and the other services' rollouts): none when it has a free place to order,
    /// or nothing is left to roll.
    #[must_use]
    pub fn wait(
        &self,
        def: &ServiceDef,
        want: &Desired,
        fleet: &Fleet,
        now: Duration,
    ) -> Option<Wait> {
        if self.finished() {
            return None;
        }
        if self.paused.is_some() {
            return Some(Wait::Paused);
        }
        if self.members.is_empty() {
            return None;
        }
        if self.current == 0 {
            return Some(Wait::ManualAdvance);
        }
        if self.stage_done() {
            return self.next().map(|_| Wait::ManualAdvance);
        }
        if self.started().any(|m| m.state == HostState::Failed) {
            return Some(Wait::FailedHost);
        }
        let next = self
            .members
            .iter()
            .find(|m| m.state == HostState::Waiting && !m.deferring);
        if let Some(next) = next.filter(|_| self.free() > 0) {
            return below_min_up(def, want, fleet, &next.id, now).then_some(Wait::BelowMinUp);
        }
        if self.started().any(|m| m.state == HostState::Updating) {
            return Some(Wait::Updating);
        }
        if self.started().count() < self.size() {
            return Some(Wait::Deferring);
        }
        Some(Wait::Baking)
    }
}

/// How many stages of the rollout file a plan with no position kept has entered: the first, when
/// it advances by itself.
#[must_use]
pub fn first_entered(plan: &Plan) -> usize {
    usize::from(
        plan.defs()
            .first()
            .is_some_and(|s| s.advance == Advance::Auto),
    )
}

/// The eligible hosts of `def` at `now`, by node id, each with what the service heard of it.
fn eligible<'a>(
    def: &ServiceDef,
    want: &Desired,
    fleet: &'a Fleet,
    now: Duration,
) -> BTreeMap<HostId, Option<&'a Host>> {
    let service = def.name.as_str();
    let ids: BTreeSet<&HostId> = fleet
        .nodes()
        .map(|(id, _)| id)
        .chain(fleet.hosts_of(service).map(|(id, _)| id))
        .collect();
    ids.into_iter()
        .filter_map(|id| {
            // What the fleet kept of a service the host's last heartbeat no longer lists is no
            // word of it, as the decision reads it.
            let host = fleet.get_in(service, id).filter(|_| {
                fleet
                    .node(id)
                    .is_none_or(|node| node.announced.contains_key(service))
            });
            let live = host.map_or_else(
                || fleet.node(id).is_some_and(|node| node.is_live(now)),
                |h| h.is_live(now),
            );
            let installed = host.is_some_and(|h| h.beat.health != Health::Absent);
            let installable = (host.is_some() || fleet.announced(id, HEARTBEAT_V2))
                && fleet.announced(id, INSTALL_FEATURE)
                && crate::picks::picked(fleet, service, &want.version, id);
            // A failure of the desired version an operator acknowledged (a cleared halt, a
            // version set by hand) is resolved: the host leaves the plan, as a held one does.
            let resolved = host.is_some_and(|h| {
                h.beat.last_update.as_ref().is_some_and(|r| {
                    r.outcome == Outcome::Failed
                        && r.target == want.version
                        && fleet.is_acked(service, id, &want.version)
                })
            });
            // A host the desired version is below the minimum of is never ordered it.
            let below = fleet
                .min_version(def, id)
                .is_some_and(|floor| !want.version.at_least(floor));
            let ok = live
                && !resolved
                && !below
                && fleet.is_assigned(def, id)
                && !fleet.is_excluded(service, id)
                && !fleet.is_held(service, id)
                && !fleet.is_forced(service, id)
                && !crate::picks::unpicked(fleet, service, &want.version, id)
                && (installed || installable);
            ok.then(|| (id.clone(), host))
        })
        .collect()
}

fn state_of(
    def: &ServiceDef,
    want: &Desired,
    done: &BTreeSet<String>,
    fleet: &Fleet,
    id: &HostId,
    host: Option<&Host>,
    now: Duration,
) -> HostState {
    let service = def.name.as_str();
    let ordered = fleet.is_ordered(service, id, now);
    let Some(host) = host else {
        return if ordered {
            HostState::Updating
        } else {
            HostState::Waiting
        };
    };
    let at = host.beat.health != Health::Absent && host.beat.version == want.version;
    let failed = host
        .beat
        .last_update
        .as_ref()
        .is_some_and(|r| r.outcome == Outcome::Failed && r.target == want.version);
    // A halted update or install leaves the version in place without a rollback: the host
    // reports it, unhealthy (a first install: installed, never started), and is failed, not
    // updating, until it beats healthy.
    if at
        && failed
        && !ordered
        && host.beat.updating.is_none()
        && host.beat.health != Health::Healthy
    {
        return HostState::Failed;
    }
    if at && !ordered && host.beat.updating.is_none() {
        if !fleet.is_up(service, id, host, now) {
            return HostState::Updating;
        }
        let baked = done.contains(id.as_str())
            || fleet.bake_beats(service, id, &want.version) >= def.bake_beats;
        return if baked {
            HostState::Done
        } else {
            HostState::Baking
        };
    }
    if ordered || host.beat.updating.is_some() || at {
        HostState::Updating
    } else if failed {
        HostState::Failed
    } else {
        HostState::Waiting
    }
}

fn is_deferring(
    def: &ServiceDef,
    want: &Desired,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> bool {
    fleet
        .deferred_until(&def.name, id, &want.version, &def.defer_backoff)
        .is_some_and(|until| now < until)
}

/// Whether ordering the host `id` would leave fewer live hosts of its role up for `def` than the
/// desired state's `min_up` of the role. Every live host of the role that runs the service and
/// is not excluded counts, held ones included: `min_up` is about what stays serving. A host that
/// is not up now (unhealthy, or not running the service) takes nothing away by its order.
fn below_min_up(
    def: &ServiceDef,
    want: &Desired,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> bool {
    let service = def.name.as_str();
    let role = fleet.role_in(service, id);
    let min = want.min_up.min_up_of(role) as usize;
    let up_now = fleet
        .get_in(service, id)
        .is_some_and(|h| fleet.is_up(service, id, h, now));
    if min == 0 || !up_now {
        return false;
    }
    // Up after the order: every other live host of the role that is up now.
    let up = fleet
        .hosts_of(service)
        .filter(|(o, h)| {
            *o != id
                && h.role_name() == role
                && h.beat.health != Health::Absent
                && !fleet.is_excluded(service, o)
                && fleet.is_up(service, o, h, now)
        })
        .count();
    up < min
}

/// Whether as many hosts of the role of `id` hold an update slot of `def` as the desired state's
/// `max_updating` of the role lets: `id` itself left out, the pairs a standing force orders left
/// out, a held pair counted only while it runs an update or has an order outstanding.
fn no_slot(def: &ServiceDef, want: &Desired, fleet: &Fleet, id: &HostId, now: Duration) -> bool {
    let service = def.name.as_str();
    let role = fleet.role_in(service, id);
    let cap = want.max_updating.max_updating_of(role);
    let others =
        |o: &HostId| o != id && !fleet.is_forced(service, o) && fleet.role_in(service, o) == role;
    let occupying = fleet
        .occupants_of(service)
        .filter(|(o, _)| others(o))
        .filter(|(o, h)| {
            if fleet.is_held(service, o) {
                h.beat.updating.is_some() || fleet.is_ordered(service, o, now)
            } else {
                fleet.holds_slot(service, o, h, now)
            }
        })
        .count();
    let unseen = fleet
        .ordered_unseen(service, now)
        .filter(|o| others(o))
        .count();
    occupying + unseen >= cap as usize
}

/// Whether `service` may not start a rollout now: its own is not live and as many other
/// services' are as `--live-rollouts` lets be.
pub(crate) fn elsewhere(
    services: &Services,
    all: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    now: Duration,
) -> bool {
    let kept = MEMO.with(|memo| {
        memo.borrow()
            .as_ref()
            .map(|plans| plans.elsewhere.get(service).copied())
    });
    match kept {
        Some(Some(held)) => held,
        Some(None) => {
            let held = live_elsewhere(services, all, service, fleet, now).is_some();
            MEMO.with(|memo| {
                memo.borrow_mut()
                    .as_mut()
                    .map(|plans| plans.elsewhere.insert(service.to_owned(), held))
            });
            held
        }
        None => live_elsewhere(services, all, service, fleet, now).is_some(),
    }
}

thread_local! {
    /// The plans [`current`] built while a [`Memo`] stands, by service and desired version.
    static MEMO: RefCell<Option<Plans>> = const { RefCell::new(None) };
}

/// What a [`Memo`] keeps: each service's plan by service and desired version, and whether
/// another service's rollout holds it ([`elsewhere`]).
#[derive(Default)]
struct Plans {
    views: BTreeMap<(String, String), Rc<View>>,
    elsewhere: BTreeMap<String, bool>,
}

/// While it stands, each service's plan is built once at its desired version and read again from
/// memory ([`current`]): for a pass over a fleet that does not change under it, such as the
/// decisions of one heartbeat until it is ordered something, or a status. The pass begins one
/// and drops it before it changes the fleet. A memo begun while another stands leaves that one.
pub(crate) struct Memo {
    began: bool,
}

impl Memo {
    /// A memo for this thread, unless one stands already.
    #[must_use]
    pub(crate) fn begin() -> Self {
        let began = MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            let began = memo.is_none();
            if began {
                *memo = Some(Plans::default());
            }
            began
        });
        Self { began }
    }
}

impl Drop for Memo {
    fn drop(&mut self) {
        if self.began {
            MEMO.with(|memo| *memo.borrow_mut() = None);
        }
    }
}

/// The plan of `def` at its desired version `want` as the decision reads it
/// ([`View::effective`]), from the memo while one stands.
pub(crate) fn current(def: &ServiceDef, want: &Desired, fleet: &Fleet, now: Duration) -> Rc<View> {
    let build = || {
        Rc::new(View::effective(
            def,
            want,
            fleet.rollout_of(&def.name),
            fleet,
            now,
        ))
    };
    MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        let Some(plans) = memo.as_mut() else {
            return build();
        };
        let key = (def.name.clone(), want.version.as_str().to_owned());
        Rc::clone(plans.views.entry(key).or_insert_with(build))
    })
}

/// The gate the plan of `def` puts on ordering the host `id` the desired version `want` at
/// `now`, after every rule of the decision before it held nothing; none when it may be ordered.
///
/// A host under its own order is never held: it is told the same again. Then, in order: a paused
/// plan holds every host (`paused`); a service whose rollout is not live waits for the others
/// (`rollout_elsewhere`); while the current stage has a free place, a host is held only by its
/// role's `max_updating` (`no_slot`) and `min_up` (`below_min_up`); once it has none, a host
/// waits for its stage:
/// for an operator's Advance (`advance_wait`), for a failed host of the stage to be resolved
/// (`stage_failed`), for the stage's bake (`baking`), or for the stage to fill and update
/// (`stage_wait`). A pair the plan does not count (a standing force orders it) is held by none of
/// these.
#[must_use]
pub fn gate(
    services: &Services,
    all: &BTreeMap<String, Desired>,
    def: &ServiceDef,
    want: &Desired,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> Option<Gate> {
    let service = def.name.as_str();
    if fleet.is_ordered(service, id, now) {
        return None;
    }
    let view = current(def, want, fleet, now);
    if !view.members.iter().any(|m| m.id == *id) {
        return None;
    }
    if view.paused.is_some() {
        return Some(Gate::Paused);
    }
    if view.free() > 0 {
        // A plan with a free place whose rollout is not live has started no host, or has none
        // left but this one: only then can another service's rollout hold it.
        return if elsewhere(services, all, service, fleet, now) {
            Some(Gate::RolloutElsewhere)
        } else if no_slot(def, want, fleet, id, now) {
            Some(Gate::NoSlot)
        } else if below_min_up(def, want, fleet, id, now) {
            Some(Gate::BelowMinUp)
        } else {
            None
        };
    }
    Some(match view.wait(def, want, fleet, now) {
        Some(Wait::ManualAdvance) => Gate::AdvanceWait,
        Some(Wait::FailedHost) => Gate::StageFailed,
        Some(Wait::Baking) => Gate::Baking,
        _ => Gate::StageWait,
    })
}

/// The gate on an uninstall of `service` ordered to a host: the service waits for the others'
/// rollouts as an update does (`rollout_elsewhere`), and nothing else of the plan holds it.
#[must_use]
pub fn uninstall_gate(
    services: &Services,
    all: &BTreeMap<String, Desired>,
    service: &str,
    fleet: &Fleet,
    now: Duration,
) -> Option<Gate> {
    elsewhere(services, all, service, fleet, now).then_some(Gate::RolloutElsewhere)
}

/// The position the plan of `def` reaches from `view` when it enters every stage it may now:
/// each stage whose stage before is done and that enters by itself, and, with `granted`, the
/// first that waits for an operator. `None` when it enters none.
pub(crate) fn reach(
    def: &ServiceDef,
    want: &Desired,
    mut view: View,
    mut granted: bool,
    fleet: &Fleet,
    now: Duration,
) -> Option<View> {
    let mut moved = false;
    loop {
        let next = if view.current == 0 {
            view.stages.first()
        } else if view.stage_done() {
            view.next()
        } else {
            None
        };
        let Some(next) = next.copied() else { break };
        if next.advance == Advance::Manual {
            if !granted {
                break;
            }
            granted = false;
        }
        let mut done = view.done.clone();
        done.extend(
            view.members
                .iter()
                .filter(|m| m.state == HostState::Done)
                .map(|m| m.id.to_string()),
        );
        let paused = view.paused.take();
        view = View::at(def, want, next.first + 1, done, fleet, now);
        view.paused = paused;
        moved = true;
    }
    moved.then_some(view)
}
