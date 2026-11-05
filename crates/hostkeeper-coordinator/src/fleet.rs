//! What the coordinator knows of the hosts: the last heartbeat of each, and when it came.
//!
//! Nothing here is stored. After a restart the coordinator knows no host until it
//! heartbeats, which is why the minimums are counted against the heartbeats it has seen and
//! never against a host list (`docs/design/host-updates.md`). Time is a `Duration`
//! since the coordinator started, so the decision replays exactly in a test.
//!
//! The orders it has given are kept the same way, in memory only. A host reports that it
//! is updating on its first heartbeat after it hears the order, so until then the beats say
//! nothing is happening; an outstanding order stands in for that report, holding the host's
//! slot and counting the host as not up. Hosts and orders are kept per service; the legacy
//! service's heartbeat is the top-level one on the wire, and the coordinator records it with
//! [`Fleet::record_in`] under that service's name. A restart forgets the
//! outstanding orders: the start-up quiet period lets the hosts report first, and a host
//! that began updating says so.
//!
//! The operator's holds and the hosts a standing force orders are kept here as a copy of the ops
//! store's ([`Marks`]), refreshed under the fleet's lock, so the decisions read them with the
//! beats; the `abort` orders an operator queued are kept in memory only, like the other orders.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Agent, Health, Heartbeat, Outcome, UpdateResult};
use hostkeeper_wire::{HostId, RoleName};

use crate::bake::Bake;
use crate::defer::{DeferBackoff, Defers};
use crate::holds::{HoldMark, holding};
use crate::verdicts::Verdicts;

/// A host with no heartbeat for this long is down.
pub const DOWN_AFTER: Duration = Duration::from_secs(45);

/// After it starts, the coordinator orders no update for this long, to hear the hosts first:
/// the default of `--quiet-secs`, and its floor, two heartbeat intervals.
pub const QUIET_START: Duration = Duration::from_secs(30);

/// How long an update order holds its slot without the host reporting it. A host reports
/// within one heartbeat interval (15 s) of hearing the order, so this is eight intervals:
/// room for a lost reply and several missed beats, short enough that a host that never heard
/// frees its slot before an operator would notice.
pub const ORDER_AGE: Duration = Duration::from_secs(120);

/// An update the coordinator told a host to run, not yet seen in the host's own heartbeats.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Order {
    target: Version,
    at: Duration,
}

/// A host, as its last heartbeat and certificate left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    /// What the host runs, from its certificate, by name. Read it with [`Host::role_name`].
    role: String,
    /// What it last said.
    pub beat: Heartbeat,
    /// When it said it.
    pub seen: Duration,
}

impl Host {
    /// The host's role, by name: how coordinator logic reads a role.
    #[must_use]
    pub fn role_name(&self) -> &str {
        &self.role
    }

    /// Whether the host has heartbeated within [`DOWN_AFTER`] of `now`.
    #[must_use]
    pub fn is_live(&self, now: Duration) -> bool {
        now.saturating_sub(self.seen) < DOWN_AFTER
    }

    /// Whether the host counts as up: live, healthy, not in the middle of an update and not
    /// waiting for its promotion.
    #[must_use]
    pub fn is_up(&self, now: Duration) -> bool {
        self.is_live(now)
            && self.beat.health == Health::Healthy
            && self.beat.updating.is_none()
            && !self.beat.promotion_pending
    }

    /// Whether the host holds an update slot (one is live at a time across every service,
    /// [`crate::decide`]): it is updating, its promotion is pending, or its last update failed
    /// and it has not yet heartbeated healthy.
    #[must_use]
    pub fn holds_slot(&self) -> bool {
        let failed = self
            .beat
            .last_update
            .as_ref()
            .is_some_and(|r| r.outcome == Outcome::Failed);
        self.beat.updating.is_some()
            || self.beat.promotion_pending
            || (failed && self.beat.health != Health::Healthy)
    }
}

/// A host the coordinator has heard from, whatever it runs: noted for every heartbeat, so a
/// host is known without a beat of any one service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// What the host runs, from its certificate, by name.
    pub role: String,
    /// The features its agent announced.
    pub features: Vec<String>,
    /// The agent its last top-level heartbeat named, if it named one.
    pub agent: Option<Agent>,
    /// When it last heartbeated.
    pub seen: Duration,
    /// The services its last heartbeat named, with the health it gave each (the legacy service: the
    /// top level).
    pub announced: BTreeMap<String, Health>,
}

impl Node {
    /// Whether the node has heartbeated within [`DOWN_AFTER`] of `now`.
    #[must_use]
    pub fn is_live(&self, now: Duration) -> bool {
        now.saturating_sub(self.seen) < DOWN_AFTER
    }
}

/// What the fleet keeps of the operator's marks for its decisions: a copy of the holds and of
/// the hosts each standing force orders, as the ops store holds them
/// ([`crate::holds::marks_of`]), refreshed under the fleet's lock ([`Fleet::set_marks`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    /// The holds that stand.
    pub holds: Vec<HoldMark>,
    /// Per service, the hosts its standing force orders ([`crate::force`]).
    pub forced: BTreeMap<String, BTreeSet<HostId>>,
}

/// Every host the coordinator has heard from since it started.
#[derive(Clone, Debug)]
pub struct Fleet {
    started: Duration,
    /// How long after `started` nothing is ordered.
    quiet: Duration,
    nodes: BTreeMap<HostId, Node>,
    hosts: BTreeMap<String, BTreeMap<HostId, Host>>,
    /// What hosts the service no longer counts (excluded, or dropped by its selector) last
    /// said of it, kept so that an update they still run holds its slot
    /// ([`Self::occupants_of`]). A host is in `hosts` or here for a service, never both.
    outside: BTreeMap<String, BTreeMap<HostId, Host>>,
    orders: BTreeMap<(HostId, String), Order>,
    /// Healthy beats in a row at one version, per service and host ([`Bake`]).
    bake: BTreeMap<(String, HostId), Bake>,
    defers: Defers,
    /// The release failures heard, each counted once ([`crate::verdicts`]).
    verdicts: Verdicts,
    /// The operator's marks the decisions read ([`Marks`]).
    marks: Marks,
    /// The `abort` orders an operator queued, per host and service, each with its order id: sent
    /// once, at the host's next heartbeat ([`crate::orders`]).
    aborts: BTreeMap<(HostId, String), String>,
    /// The held hosts on a bad version already alerted, per service, host and version
    /// ([`crate::holds::alert_on_bad_version`]).
    alerted: BTreeSet<(String, HostId, String)>,
}

impl Fleet {
    /// A fleet that knows no host, for a coordinator that started at `started` and orders
    /// nothing for `quiet` after it ([`QUIET_START`] by default).
    #[must_use]
    pub fn new(started: Duration, quiet: Duration) -> Self {
        Self {
            started,
            quiet,
            nodes: BTreeMap::new(),
            hosts: BTreeMap::new(),
            outside: BTreeMap::new(),
            orders: BTreeMap::new(),
            bake: BTreeMap::new(),
            defers: Defers::default(),
            verdicts: Verdicts::default(),
            marks: Marks::default(),
            aborts: BTreeMap::new(),
            alerted: BTreeSet::new(),
        }
    }

    /// Whether `now` is still within the quiet start: the `quiet` of [`Self::new`] after the
    /// coordinator's start.
    #[must_use]
    pub fn is_quiet(&self, now: Duration) -> bool {
        now < self.started.saturating_add(self.quiet)
    }

    /// The release failures of `service` about `version` heard since the start, each once: the
    /// host and its report ([`crate::verdicts`]).
    pub fn verdicts_of(
        &self,
        service: &str,
        version: &Version,
    ) -> impl Iterator<Item = (&HostId, &UpdateResult)> {
        self.verdicts.of(service, version)
    }

    /// Forgets the verdicts of `service` about any version but `version`, its desired one, and
    /// those `acked` acknowledges ([`crate::verdicts`]).
    pub fn keep_verdicts_of(
        &mut self,
        service: &str,
        version: &Version,
        acked: &[crate::ops::FailureAck],
    ) {
        self.verdicts.keep_only(service, version, acked);
    }

    /// When the coordinator started.
    #[must_use]
    pub const fn started(&self) -> Duration {
        self.started
    }

    /// Notes that the host `id` heartbeated, as `node` says.
    pub fn note_node(&mut self, id: HostId, node: Node) {
        self.nodes.insert(id, node);
    }

    /// The node `id`, if it has heartbeated.
    #[must_use]
    pub fn node(&self, id: &HostId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Every node that has heartbeated, by host id.
    pub fn nodes(&self) -> impl Iterator<Item = (&HostId, &Node)> {
        self.nodes.iter()
    }

    /// Stops counting the host `id` for `service`: what it said moves out of the service's
    /// hosts, to be kept with any order for it as what the host still occupies
    /// ([`Self::occupants_of`]), until its own beats, recorded with
    /// [`Self::record_outside_in`], or the order's age release it.
    pub fn forget_in(&mut self, service: &str, id: &HostId) {
        if let Some(host) = self
            .hosts
            .get_mut(service)
            .and_then(|hosts| hosts.remove(id))
        {
            self.outside
                .entry(service.to_owned())
                .or_default()
                .insert(id.clone(), host);
        }
    }

    /// Forgets the host `id` everywhere, if its update record is final: no service it was
    /// heard for (counted or not) shows it updating or waiting for its promotion, and no order
    /// for it is outstanding at `now`. Its slot, its bake and its orders go with it, so a
    /// retiring host frees the one live update slot once it has finished (design 6.1). Returns
    /// whether it was forgotten.
    pub fn release_if_final(&mut self, id: &HostId, now: Duration) -> bool {
        let in_flight = self
            .hosts
            .values()
            .chain(self.outside.values())
            .filter_map(|hosts| hosts.get(id))
            .any(|host| host.beat.updating.is_some() || host.beat.promotion_pending);
        let ordered = self
            .orders
            .iter()
            .any(|((o, _), order)| o == id && now.saturating_sub(order.at) < ORDER_AGE);
        if in_flight || ordered {
            return false;
        }
        for hosts in self.hosts.values_mut().chain(self.outside.values_mut()) {
            hosts.remove(id);
        }
        self.orders.retain(|(o, _), _| o != id);
        self.bake.retain(|(_, o), _| o != id);
        self.nodes.remove(id);
        true
    }

    /// Notes a heartbeat of `service`: the host's latest word about it replaces its earlier
    /// one.
    ///
    /// An order for the host and service ends when the beat shows it updating (the beat
    /// counts from then), runs the ordered version, reports that update failed, or reports
    /// that it declined it ([`crate::defer`]): a deferral holds no slot, so the order must not
    /// hold it for [`ORDER_AGE`] either.
    pub fn record_in(
        &mut self,
        service: &str,
        id: HostId,
        role: RoleName,
        beat: Heartbeat,
        now: Duration,
    ) {
        self.record_named_in(service, id, role.name(), beat, now);
    }

    /// [`Self::record_in`] for a host whose role is named `role`.
    pub(crate) fn record_named_in(
        &mut self,
        service: &str,
        id: HostId,
        role: &str,
        beat: Heartbeat,
        now: Duration,
    ) {
        self.note(service, id, role, beat, now, true);
    }

    /// Notes a heartbeat of `service` from a host the service does not count (excluded, or
    /// dropped by its selector): it ends an order and counts a bake as [`Self::record_in`]
    /// does, and is kept only as what the host occupies ([`Self::occupants_of`]), never as one
    /// of the service's hosts.
    pub fn record_outside_in(
        &mut self,
        service: &str,
        id: HostId,
        role: &RoleName,
        beat: Heartbeat,
        now: Duration,
    ) {
        self.note(service, id, role.name(), beat, now, false);
    }

    /// The one place a role enters the fleet: the beat ends an order, counts a bake and a
    /// deferral, and is kept as one of the service's hosts when `counted`, else as outside.
    fn note(
        &mut self,
        service: &str,
        id: HostId,
        role: &str,
        beat: Heartbeat,
        now: Duration,
        counted: bool,
    ) {
        let key = (id, service.to_owned());
        let mut deferred_order = false;
        if let Some(order) = self.orders.get(&key) {
            let failed = beat
                .last_update
                .as_ref()
                .is_some_and(|r| r.outcome == Outcome::Failed && r.target == order.target);
            deferred_order = beat
                .deferred
                .as_ref()
                .is_some_and(|d| d.target == order.target);
            if beat.updating.is_some() || beat.version == order.target || failed || deferred_order {
                self.orders.remove(&key);
            }
        }
        let baking = (key.1.clone(), key.0.clone());
        match Bake::after(self.bake.get(&baking), &beat, now) {
            Some(bake) => {
                self.bake.insert(baking, bake);
            }
            None => {
                self.bake.remove(&baking);
            }
        }
        self.defers.observe(&key, &beat, deferred_order, now);
        self.verdicts.observe(&key.1, &key.0, &beat);
        let (kept, other) = if counted {
            (&mut self.hosts, &mut self.outside)
        } else {
            (&mut self.outside, &mut self.hosts)
        };
        if let Some(hosts) = other.get_mut(&key.1) {
            hosts.remove(&key.0);
        }
        kept.entry(key.1).or_default().insert(
            key.0,
            Host {
                role: role.to_owned(),
                beat,
                seen: now,
            },
        );
    }

    /// Notes that the host `id` was told at `now` to update `service` to `target`. An order
    /// for the same target that has not aged out stays as it was, so a host that asks again
    /// before it reports is told again without its slot being held longer.
    pub fn order(&mut self, service: &str, id: HostId, target: Version, now: Duration) {
        if self.is_ordered(service, &id, now)
            && self
                .orders
                .get(&(id.clone(), service.to_owned()))
                .is_some_and(|o| o.target == target)
        {
            return;
        }
        self.orders
            .insert((id, service.to_owned()), Order { target, at: now });
    }

    /// When the wait of the host `id` ends, if it declined an update of `service` to `target`.
    #[must_use]
    pub fn deferred_until(
        &self,
        service: &str,
        id: &HostId,
        target: &Version,
        backoff: &DeferBackoff,
    ) -> Option<Duration> {
        self.defers
            .until(&(id.clone(), service.to_owned()), target, backoff)
    }

    /// Whether `id` has an order outstanding for `service` at `now`.
    #[must_use]
    pub fn is_ordered(&self, service: &str, id: &HostId, now: Duration) -> bool {
        self.orders
            .get(&(id.clone(), service.to_owned()))
            .is_some_and(|o| now.saturating_sub(o.at) < ORDER_AGE)
    }

    /// Whether the host `id` is busy with a service other than `service`: one of its other
    /// services is updating, or has an order outstanding for it. A host does one update at a
    /// time.
    #[must_use]
    pub fn is_busy_besides(&self, service: &str, id: &HostId, now: Duration) -> bool {
        let updating = self
            .hosts
            .iter()
            .filter(|(s, _)| s.as_str() != service)
            .any(|(_, hosts)| hosts.get(id).is_some_and(|h| h.beat.updating.is_some()));
        updating
            || self.orders.iter().any(|((o, s), order)| {
                o == id && s != service && now.saturating_sub(order.at) < ORDER_AGE
            })
    }

    /// Whether the host `id` holds an update slot of `service`: [`Host::holds_slot`], or an
    /// order is outstanding for it.
    #[must_use]
    pub fn holds_slot(&self, service: &str, id: &HostId, host: &Host, now: Duration) -> bool {
        host.holds_slot() || self.is_ordered(service, id, now)
    }

    /// How many healthy beats in a row the host `id` has sent `service` at `version`.
    #[must_use]
    pub fn bake_beats(&self, service: &str, id: &HostId, version: &Version) -> u32 {
        self.bake
            .get(&(service.to_owned(), id.clone()))
            .filter(|b| b.target == *version)
            .map_or(0, |b| b.beats)
    }

    /// Whether the host `id` is still baking: live, at `version`, and has beaten healthy
    /// fewer than `need` times in a row.
    #[must_use]
    pub fn is_baking(
        &self,
        service: &str,
        id: &HostId,
        host: &Host,
        version: &Version,
        need: u32,
        now: Duration,
    ) -> bool {
        host.is_live(now)
            && host.beat.version == *version
            && self.bake_beats(service, id, version) < need
    }

    /// Whether the host `id` counts as up for `service`: [`Host::is_up`] and no order
    /// outstanding for it.
    #[must_use]
    pub fn is_up(&self, service: &str, id: &HostId, host: &Host, now: Duration) -> bool {
        host.is_up(now) && !self.is_ordered(service, id, now)
    }

    /// The host `id` as `service` last heard it, if it has heartbeated.
    #[must_use]
    pub fn get_in(&self, service: &str, id: &HostId) -> Option<&Host> {
        self.hosts.get(service)?.get(id)
    }

    /// Every host that heartbeated `service`, by host id.
    pub fn hosts_of(&self, service: &str) -> impl Iterator<Item = (&HostId, &Host)> {
        self.hosts.get(service).into_iter().flatten()
    }

    /// Every host whose last word about `service` the fleet keeps, counted or not: the
    /// service's hosts and the ones it no longer counts. One live update at a time is counted
    /// over these ([`crate::decide`]), so a host that leaves a service mid-update still holds
    /// its slot.
    pub fn occupants_of(&self, service: &str) -> impl Iterator<Item = (&HostId, &Host)> {
        self.hosts_of(service)
            .chain(self.outside.get(service).into_iter().flatten())
    }

    /// Replaces the operator's marks the fleet decides by with `marks`.
    pub fn set_marks(&mut self, marks: Marks) {
        self.marks = marks;
    }

    /// What the fleet keeps of the host `id` for `service`, counted or not.
    fn occupant(&self, service: &str, id: &HostId) -> Option<&Host> {
        self.occupants_of(service)
            .find(|(occupant, _)| *occupant == id)
            .map(|(_, host)| host)
    }

    /// Whether an operator holds `service` on the host `id`: a hold of the host, of the role it
    /// has now (its certificate's at its last heartbeat; for a host the fleet knows only by its
    /// beats of `service`, the role of those), or of the whole fleet.
    #[must_use]
    pub fn is_held(&self, service: &str, id: &HostId) -> bool {
        let role = match self.nodes.get(id) {
            Some(node) => node.role.as_str(),
            None => self.occupant(service, id).map_or("", Host::role_name),
        };
        holding(&self.marks.holds, service, id.as_str(), role)
            .next()
            .is_some()
    }

    /// Whether the standing force of `service` orders the host `id`.
    #[must_use]
    pub fn is_forced(&self, service: &str, id: &HostId) -> bool {
        self.marks
            .forced
            .get(service)
            .is_some_and(|hosts| hosts.contains(id))
    }

    /// Whether the agent of the host `id` announced `feature` in its last heartbeat.
    #[must_use]
    pub fn announced(&self, id: &HostId, feature: &str) -> bool {
        self.nodes
            .get(id)
            .is_some_and(|node| node.features.iter().any(|f| f == feature))
    }

    /// Queues an `abort` of `service` for the host `id` as the order `order_id`, unless one is
    /// queued for them already: the first stands, so asking twice sends one abort.
    pub fn queue_abort(&mut self, id: HostId, service: String, order_id: String) {
        self.aborts.entry((id, service)).or_insert(order_id);
    }

    /// Takes an `abort` queued for the host `id`, the first by service name: the service and its
    /// order id. Taken, it is not sent again.
    pub fn take_abort(&mut self, id: &HostId) -> Option<(String, String)> {
        let key = self.aborts.keys().find(|(host, _)| host == id).cloned()?;
        self.aborts
            .remove_entry(&key)
            .map(|((_, service), order_id)| (service, order_id))
    }

    /// Forgets the host `id` for `service` altogether, its order with it, so the slot it held is
    /// free now: an operator abandoned it (`Hosts.Exclude` with `abandon`). Answers whether it
    /// held the slot at `now`. A beat it sends later is noted as any other.
    pub fn abandon_in(&mut self, service: &str, id: &HostId, now: Duration) -> bool {
        let held = self
            .occupant(service, id)
            .is_some_and(|host| self.holds_slot(service, id, host, now));
        self.hosts.entry(service.to_owned()).or_default().remove(id);
        self.outside
            .entry(service.to_owned())
            .or_default()
            .remove(id);
        self.orders.remove(&(id.clone(), service.to_owned()));
        held
    }

    /// Notes the alert of the held host `id` running `version` of `service`, marked bad, and
    /// answers whether it is the first for them.
    pub fn first_alert(&mut self, service: &str, id: &HostId, version: &Version) -> bool {
        self.alerted
            .insert((service.to_owned(), id.clone(), version.as_str().to_owned()))
    }
}
