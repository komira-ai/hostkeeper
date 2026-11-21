//! One service rolls at a time in the whole cluster, and the services that wait for it queue in
//! the order their desired versions were set (`docs/design/rollout-update-plans.md`, section 1.6).
//!
//! A service is a package name: `hostkeeper` in every host group is one service, and so is any
//! other package that more than one group lists. The coordinator snapshots, at each take of the
//! fleet's lock, every (group, package) it holds with the desired state of each ([`Cluster`]), so
//! that a decision for a host of one group sees the rollouts of the packages of every other group
//! ([`crate::live::live_elsewhere`]).
//!
//! The order is, first, the number the desired version was set under (`set_seq`), taken from the
//! database's sequence in the transaction that set it, so every coordinator on the database
//! orders alike (`docs/design/coordinator-multi-instance.md`, section 5.2); a row with no number
//! ranks before every numbered one. Among entries with the same number, or none (every entry
//! over the files), it is the time the coordinator first saw a desired version, kept in memory;
//! among entries first seen together (after a restart every one of them) it is the group's last
//! change (`at_unix`), then the group's name, the package's place in the group's list, and its
//! name. A coordinator that places by the rollout file (no groups) keeps no queue: it counts its
//! services' live rollouts alone.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::live::why_live;
use crate::services::ServiceDef;
use crate::stages::{HostState, Wait, current};

/// One package of one group with its desired state.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// The definition: the group's view of the package.
    pub(crate) def: ServiceDef,
    /// What the group wants of it, when it wants a version.
    pub(crate) want: Option<Desired>,
    /// The group's last change, in Unix seconds: the order of entries first seen together.
    pub(crate) at_unix: u64,
    /// The number the desired version was set under, taken from the database's sequence in the
    /// transaction that set it; none over the files and for a row not yet numbered.
    pub(crate) set_seq: Option<i64>,
}

impl Entry {
    /// The key a desired version is stamped under: the group, the package and the version.
    fn key(&self) -> (String, String, String) {
        (
            self.def
                .group()
                .map(ToString::to_string)
                .unwrap_or_default(),
            self.def.name.clone(),
            self.want
                .as_ref()
                .map(|want| want.version.as_str().to_owned())
                .unwrap_or_default(),
        )
    }
}

/// Every package the coordinator rolls out, in every group, at one take of the fleet's lock.
#[derive(Clone, Debug, Default)]
pub(crate) struct Cluster {
    entries: Vec<Entry>,
    /// When each desired version was first seen.
    stamps: BTreeMap<(String, String, String), Duration>,
}

/// Where an entry stands in the queue: the number its desired version was set under (none before
/// every number); when it was first seen; the group's last change; the group's name; its place in
/// the group's list (the earlier listed first); and the package's name.
pub(crate) type Order = (Option<i64>, Duration, u64, String, i64, String);

impl Cluster {
    /// Whether the cluster holds no entry: a fleet nothing built a snapshot for.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The cluster of `entries` at `now`: the desired versions already seen keep their stamp, the
    /// others are stamped `now`, and a stamp of a version no entry wants any more is dropped.
    pub(crate) fn refreshed(&self, entries: Vec<Entry>, now: Duration) -> Self {
        let mut stamps: BTreeMap<_, _> = entries
            .iter()
            .map(|entry| {
                let key = entry.key();
                let at = self.stamps.get(&key).copied().unwrap_or(now);
                (key, at)
            })
            .collect();
        // A package that wants no version for a moment keeps its place for the version it wanted.
        let absent: BTreeSet<_> = entries
            .iter()
            .filter(|entry| entry.want.is_none())
            .map(|entry| (entry.key().0, entry.key().1))
            .collect();
        for (key, at) in &self.stamps {
            if absent.contains(&(key.0.clone(), key.1.clone())) {
                stamps.entry(key.clone()).or_insert(*at);
            }
        }
        Self { entries, stamps }
    }

    /// When the desired version of `entry` was first seen; zero for one not stamped.
    pub(crate) fn stamp_of(&self, entry: &Entry) -> Duration {
        self.stamps.get(&entry.key()).copied().unwrap_or_default()
    }

    /// Where `entry` stands in the queue: the smaller, the earlier.
    pub(crate) fn order(&self, entry: &Entry) -> Order {
        let key = entry.key();
        (
            entry.set_seq,
            self.stamp_of(entry),
            entry.at_unix,
            key.0,
            -i64::from(entry.def.priority),
            key.1,
        )
    }

    /// The entries of the packages of the group `group`, in the group's order.
    pub(crate) fn in_group<'a>(
        &'a self,
        group: &'a crate::host_groups::GroupName,
    ) -> impl Iterator<Item = &'a Entry> {
        self.entries
            .iter()
            .filter(move |e| e.def.group() == Some(group))
    }

    /// The entries of the service `name`.
    pub(crate) fn of<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Entry> {
        self.entries.iter().filter(move |e| e.def.name == name)
    }

    /// The services other than `name` whose rollouts are live, each once, by name, with why.
    pub(crate) fn live_others(
        &self,
        name: &str,
        fleet: &Fleet,
        now: Duration,
    ) -> Vec<(String, crate::live::Live)> {
        let mut seen = BTreeSet::new();
        self.entries
            .iter()
            .filter(|e| e.def.name != name)
            .filter_map(|e| {
                let live = why_live(&e.def, e.want.as_ref(), fleet, now)?;
                seen.insert(e.def.name.clone())
                    .then(|| (e.def.name.clone(), live))
            })
            .collect()
    }

    /// Whether the service `name` is live in any group.
    pub(crate) fn is_live(&self, name: &str, fleet: &Fleet, now: Duration) -> bool {
        self.of(name)
            .any(|e| why_live(&e.def, e.want.as_ref(), fleet, now).is_some())
    }

    /// The services queued ahead of `name`, first in line first, each once: those with work left
    /// whose rollout is not live and that something can still order (a service that is halted, at
    /// a version marked bad, paused, held by a failed host, waiting for an Advance or for `min_up`,
    /// or whose every host that has not started defers or is held by a rule before the plan's gate,
    /// [`crate::orderable`], takes no place), that were set before `name`. Empty when `name` has no
    /// entry.
    pub(crate) fn ahead_of(&self, name: &str, fleet: &Fleet, now: Duration) -> Vec<String> {
        // The place of the service is that of its entries something can order; a service with
        // none holds the place of its first entry.
        let own: Vec<(bool, Order)> = self
            .of(name)
            .map(|e| {
                (
                    orderable(&e.def, e.want.as_ref(), fleet, now),
                    self.order(e),
                )
            })
            .collect();
        let ready = own.iter().filter(|(ok, _)| *ok).map(|(_, order)| order);
        let Some(mine) = ready.min().or_else(|| own.iter().map(|(_, o)| o).min()) else {
            return Vec::new();
        };
        let mut waiting: BTreeMap<Order, &str> = BTreeMap::new();
        for entry in self.entries.iter().filter(|e| e.def.name != name) {
            let order = self.order(entry);
            if order < *mine && orderable(&entry.def, entry.want.as_ref(), fleet, now) {
                waiting.entry(order).or_insert(entry.def.name.as_str());
            }
        }
        let mut seen = BTreeSet::new();
        waiting
            .into_values()
            .filter(|name| seen.insert(*name))
            .map(str::to_owned)
            .collect()
    }
}

/// Whether `def`, wanted at `want`, waits for its turn: no live rollout, a desired version not on
/// every host, and a host something can order now (not a plan that waits for an Advance or for
/// `min_up`, and a host no rule before the plan's gate holds, [`crate::orderable`]).
pub(crate) fn orderable(
    def: &ServiceDef,
    want: Option<&Desired>,
    fleet: &Fleet,
    now: Duration,
) -> bool {
    let Some(want) = want else {
        return false;
    };
    if fleet.is_stopped_in(def, &want.version) || why_live(def, Some(want), fleet, now).is_some() {
        return false;
    }
    let view = current(def, want, fleet, now);
    // A plan that waits for an operator's Advance, or whose next host would leave too few up,
    // orders nobody now: it holds no place, or it would hold every service behind it for good.
    let blocked = matches!(
        view.wait(def, want, fleet, now),
        Some(Wait::ManualAdvance | Wait::BelowMinUp)
    );
    !blocked
        && !view.finished()
        && view.paused.is_none()
        && !view.members.iter().any(|m| m.state == HostState::Failed)
        && crate::orderable::can_order(def, want, &view, fleet, now)
}
