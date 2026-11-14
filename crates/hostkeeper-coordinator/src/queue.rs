//! One service rolls at a time in the whole cluster, and the services that wait for it queue in
//! the order their desired versions were set (`docs/design/rollout-update-plans.md`, section 1.6).
//!
//! A service is a package name: `hostkeeper` in every host group is one service, and so is any
//! other package that more than one group lists. The coordinator snapshots, at each take of the
//! fleet's lock, every (group, package) it holds with the desired state of each ([`Cluster`]), so
//! that a decision for a host of one group sees the rollouts of the packages of every other group
//! ([`crate::live::live_elsewhere`]).
//!
//! The order is the time the coordinator first saw a desired version, kept in memory; among
//! entries first seen together (after a restart every one of them) it is the group's last change
//! (`at_unix`), then the group's name, the package's place in the group's list, and its name.
//! Nothing new is stored, so an older release reads the same data. A coordinator that places by
//! the rollout file (no groups) keeps no queue: it counts its services' live rollouts alone.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::live::why_live;
use crate::services::ServiceDef;
use crate::stages::{HostState, current};

/// One package of one group with its desired state.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// The definition: the group's view of the package.
    pub(crate) def: ServiceDef,
    /// What the group wants of it, when it wants a version.
    pub(crate) want: Option<Desired>,
    /// The group's last change, in Unix seconds: the order of entries first seen together.
    pub(crate) at_unix: u64,
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

/// Where an entry stands in the queue: when its desired version was first seen; the group's last
/// change; the group's name; its place in the group's list (the earlier listed first); and the
/// package's name.
type Order = (Duration, u64, String, i64, String);

impl Cluster {
    /// Whether the cluster holds no entry: a fleet nothing built a snapshot for.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The cluster of `entries` at `now`: the desired versions already seen keep their stamp, the
    /// others are stamped `now`, and a stamp of a version no entry wants any more is dropped.
    pub(crate) fn refreshed(&self, entries: Vec<Entry>, now: Duration) -> Self {
        let stamps = entries
            .iter()
            .map(|entry| {
                let key = entry.key();
                let at = self.stamps.get(&key).copied().unwrap_or(now);
                (key, at)
            })
            .collect();
        Self { entries, stamps }
    }

    fn order(&self, entry: &Entry) -> Order {
        let key = entry.key();
        (
            self.stamps.get(&key).copied().unwrap_or_default(),
            entry.at_unix,
            key.0,
            -i64::from(entry.def.priority),
            key.1,
        )
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
    /// a version marked bad, paused, held by a failed host, or whose every host that has not
    /// started defers, takes no place), that were set before `name`. Empty when `name` has no
    /// entry.
    pub(crate) fn ahead_of(&self, name: &str, fleet: &Fleet, now: Duration) -> Vec<String> {
        let Some(mine) = self.of(name).map(|e| self.order(e)).min() else {
            return Vec::new();
        };
        let mut waiting: BTreeMap<Order, &str> = BTreeMap::new();
        for entry in self.entries.iter().filter(|e| e.def.name != name) {
            let order = self.order(entry);
            if order < mine && orderable(entry, fleet, now) {
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

/// Whether `entry` waits for its turn: no live rollout, a desired version not on every host, and a
/// host something can order.
fn orderable(entry: &Entry, fleet: &Fleet, now: Duration) -> bool {
    let Some(want) = entry.want.as_ref() else {
        return false;
    };
    let def = &entry.def;
    if fleet.is_stopped_in(def, &want.version) || why_live(def, Some(want), fleet, now).is_some() {
        return false;
    }
    let view = current(def, want, fleet, now);
    !view.finished()
        && view.paused.is_none()
        && !view.members.iter().any(|m| m.state == HostState::Failed)
        && view
            .members
            .iter()
            .any(|m| m.state == HostState::Waiting && !m.deferring)
}
