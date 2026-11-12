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
//! A host's clock is held against the coordinator's wall clock, which the caller passes in
//! ([`Fleet::note_node`]): the count of each host's heartbeats whose clock was off
//! ([`Node::skewed_beats`]) starts again from zero after a restart.
//!
//! The operator's holds, the hosts a standing force orders and where each service's update plan
//! stands are kept here as a copy of the ops store's ([`Marks`]), refreshed under the fleet's
//! lock, so the decisions read them with the beats; the `abort` and `clear_record` orders an
//! operator queued are kept in memory only, like the other orders.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Agent, Health, Heartbeat, Outcome, UpdateResult};
use hostkeeper_wire::{HostId, Platform, RoleName};

use crate::assignment::{Placement, assigned, effective_min_version};
use crate::bake::{Bake, Found};
use crate::defer::{DeferBackoff, Defers};
use crate::holds::holding;
use crate::rollouts::Rollout;
use crate::services::ServiceDef;
use crate::verdicts::Verdicts;

/// A host with no heartbeat for this long is down.
pub const DOWN_AFTER: Duration = Duration::from_secs(45);

/// After it starts, the coordinator orders no update for this long, to hear the hosts first:
/// the default of `--quiet-secs`, and its floor, two heartbeat intervals.
pub const QUIET_START: Duration = Duration::from_secs(30);

/// How far a host's clock may be from the coordinator's wall clock, either way, before its
/// heartbeat counts as skewed ([`Fleet::note_node`]): the default of `--clock-skew-secs`. Two
/// minutes is far beyond the second or two a heartbeat is in flight, and well inside what a host
/// whose clock was reset (a Mac back from a power cut) is off by, which is what the count is for.
pub const CLOCK_SKEW_AFTER: Duration = Duration::from_secs(120);

/// How long an update order holds its slot without the host reporting it. A host reports
/// within one heartbeat interval (15 s) of hearing the order, so this is eight intervals:
/// room for a lost reply and several missed beats, short enough that a host that never heard
/// frees its slot before an operator would notice.
pub const ORDER_AGE: Duration = Duration::from_secs(120);

/// An update, an install or an uninstall the coordinator told a host to run, not yet seen in the
/// host's own heartbeats.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Order {
    /// The version ordered; for an uninstall, the version the host ran when it was ordered.
    target: Version,
    at: Duration,
    /// The order id of an uninstall; none for an update or an install.
    uninstall: Option<String>,
    /// The try it was sent at ([`Fleet::order_try`]).
    order_try: u64,
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

    /// Whether the host holds an update slot of its service, counted against its role's
    /// `max_updating` ([`crate::stages`]): updating, promotion pending, or failed and not yet
    /// healthy. A failed install rolled back to nothing (reported absent) holds none; one halted
    /// after its switch is installed, unhealthy, and does. Neither makes a rollout live.
    #[must_use]
    pub fn holds_slot(&self) -> bool {
        let failed = self
            .beat
            .last_update
            .as_ref()
            .is_some_and(|r| r.outcome == Outcome::Failed);
        self.beat.updating.is_some()
            || self.beat.promotion_pending
            || (failed && !matches!(self.beat.health, Health::Healthy | Health::Absent))
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
    /// The platform its last heartbeat named: which package of a release index the host is
    /// ordered, including a service it does not run yet.
    pub platform: Platform,
    /// When it last heartbeated.
    pub seen: Duration,
    /// The services its last heartbeat named, with the health it gave each (the legacy service: the
    /// top level).
    pub announced: BTreeMap<String, Health>,
    /// The host's clock when it made its last heartbeat, as the heartbeat said it.
    pub sent_unix: Option<u64>,
    /// The last second the host's certificate is valid, as its last heartbeat said it.
    pub cert_not_after_unix: Option<u64>,
    /// Why the host's last step of enrollment or renewal failed, as its last heartbeat said it.
    pub last_renew_error: Option<String>,
    /// The host's Sigstore trusted root, as its last heartbeat said it: unknown after one that
    /// said nothing of it.
    pub trusted_root: crate::trusted_root::TrustedRootSaid,
    /// How many of its heartbeats since the coordinator started were skewed: counted by the
    /// fleet ([`Fleet::note_node`]), which replaces what the node given to it says.
    pub skewed_beats: u64,
}

impl Node {
    /// Whether the node has heartbeated within [`DOWN_AFTER`] of `now`.
    #[must_use]
    pub fn is_live(&self, now: Duration) -> bool {
        now.saturating_sub(self.seen) < DOWN_AFTER
    }
}

#[path = "fleet_groups.rs"]
mod groups;
pub use groups::Marks;
#[path = "fleet_retry.rs"]
pub mod retry;

/// Every host the coordinator has heard from since it started.
#[derive(Clone, Debug)]
pub struct Fleet {
    started: Duration,
    /// How long after `started` nothing is ordered.
    quiet: Duration,
    /// How far a heartbeat's clock may be from the coordinator's before it counts as skewed.
    skew_after: Duration,
    /// How many services may have a live rollout at once ([`crate::stages`]).
    live_rollouts: usize,
    nodes: BTreeMap<HostId, Node>,
    hosts: BTreeMap<String, BTreeMap<HostId, Host>>,
    /// What hosts the service no longer counts (excluded, or dropped by its selector) last
    /// said of it, kept so that an update they still run holds its slot
    /// ([`Self::occupants_of`]). A host is in `hosts` or here for a service, never both.
    outside: BTreeMap<String, BTreeMap<HostId, Host>>,
    orders: BTreeMap<(HostId, String), Order>,
    /// Healthy beats in a row at one version, per service and host ([`Bake`]).
    bake: BTreeMap<(String, HostId), Bake>,
    /// The hosts first heard settled at a version and not seen to move since ([`Found`]).
    pub(crate) found: Found,
    defers: Defers,
    /// The release failures heard, each counted once ([`crate::verdicts`]).
    verdicts: Verdicts,
    /// The operator's marks the decisions read ([`Marks`]).
    marks: Marks,
    /// The `abort` orders an operator queued, per host and service, each with its order id: sent
    /// once, at the host's next heartbeat ([`crate::orders`]).
    aborts: BTreeMap<(HostId, String), String>,
    /// The `clear_record` orders an operator queued ([`crate::clear_record`]).
    pub(crate) clear_records: crate::clear_record::Queue,
    retries: retry::Retries,
    /// The canaries that failed a release, per service and version, by node id
    /// ([`crate::reconcile::Change::CanaryFailed`]): kept in memory only, like the orders.
    canaries: BTreeMap<(String, String), String>,
    /// The highest order try heard of each host and service, from every verdict its heartbeats
    /// carried (an acknowledged one read back from the verdicts file among them, and a refusal
    /// for the try) and from the try its step record holds (`record_try`): what
    /// [`Self::next_try`] numbers past. Kept in memory only, never lowered.
    tries: BTreeMap<(HostId, String), u64>,
    /// The highest try of an order of each host and service the coordinator gave and no longer
    /// counts as outstanding (it aged out, was replaced, or ended), whether or not the host
    /// reported it: a host that answers an order with no verdict the coordinator hears (an
    /// install of a service its heartbeat does not list) may hold that try in its step record
    /// all the same. Kept in memory only, never lowered.
    sent: BTreeMap<(HostId, String), u64>,
    /// Whether the store's link to its database is lost (`docs/design/coordinator-storage.md`,
    /// section 5), read from the backend at each take of the fleet's lock: nothing is ordered.
    pub(crate) store_lost: bool,
    /// The installers daemons said over gRPC and the releases the picks read ([`crate::picks`]).
    pub(crate) picks: crate::picks::Picks,
    /// The hosts whose move into a host group is under way ([`crate::moving`]).
    pub(crate) moves: crate::moving::Moves,
    /// The facts generations daemons reported over gRPC ([`crate::facts`]).
    pub(crate) facts: crate::facts::Reported,
}

impl Fleet {
    /// A fleet that knows no host, for a coordinator that started at `started` and orders
    /// nothing for `quiet` after it ([`QUIET_START`] by default).
    #[must_use]
    pub fn new(started: Duration, quiet: Duration) -> Self {
        Self {
            started,
            quiet,
            skew_after: CLOCK_SKEW_AFTER,
            live_rollouts: 1,
            nodes: BTreeMap::new(),
            hosts: BTreeMap::new(),
            outside: BTreeMap::new(),
            orders: BTreeMap::new(),
            bake: BTreeMap::new(),
            found: Found::default(),
            defers: Defers::default(),
            verdicts: Verdicts::default(),
            marks: Marks::default(),
            aborts: BTreeMap::new(),
            clear_records: crate::clear_record::Queue::default(),
            retries: retry::Retries::default(),
            canaries: BTreeMap::new(),
            tries: BTreeMap::new(),
            sent: BTreeMap::new(),
            store_lost: false,
            picks: crate::picks::Picks::default(),
            moves: crate::moving::Moves::default(),
            facts: crate::facts::Reported::default(),
        }
    }

    /// The same fleet ordering nothing for `quiet` after its start: what it holds (the verdicts
    /// counted before a restart among it) is kept.
    pub(crate) fn set_quiet(&mut self, quiet: Duration) {
        self.quiet = quiet;
    }

    /// The same fleet counting a heartbeat as skewed when its clock is further than `after` from
    /// the coordinator's, instead of [`CLOCK_SKEW_AFTER`].
    pub(crate) fn set_skew_after(&mut self, after: Duration) {
        self.skew_after = after;
    }

    /// The same fleet letting `n` services have a live rollout at once instead of one.
    pub(crate) fn set_live_rollouts(&mut self, n: usize) {
        self.live_rollouts = n;
    }

    /// How many services may have a live rollout at once.
    #[must_use]
    pub fn live_rollouts(&self) -> usize {
        self.live_rollouts
    }

    /// Where the update plan of `service` stands, as the ops store keeps it.
    #[must_use]
    pub fn rollout_of(&self, service: &str) -> Option<&Rollout> {
        self.marks.rollouts.get(service)
    }

    /// Whether `service` is halted, or `version` of it is marked bad: its plan orders no one.
    #[must_use]
    pub fn is_stopped(&self, service: &str, version: &Version) -> bool {
        self.marks.halted.contains(service)
            || self
                .marks
                .bad
                .get(service)
                .is_some_and(|bad| bad.contains(version))
    }

    /// Whether an operator acknowledged the failure of the host `id` at `version` of `service`:
    /// it cleared the halt the failure raised, or set a version by hand over it.
    #[must_use]
    pub fn is_acked(&self, service: &str, id: &HostId, version: &Version) -> bool {
        self.marks.acked.get(service).is_some_and(|acked| {
            acked.contains(&(id.as_str().to_owned(), version.as_str().to_owned()))
        })
    }

    /// Whether the coordinator orders nothing at `now`: within the quiet start (the `quiet` of
    /// [`Self::new`] after the coordinator's start), or while the store's link is lost.
    #[must_use]
    pub fn is_quiet(&self, now: Duration) -> bool {
        self.store_lost || now < self.started.saturating_add(self.quiet)
    }

    /// The try of the next order of `service` for the host `id`: one past the highest try any
    /// heartbeat of the host has carried for the service, whatever order its verdict ended, the
    /// try its step record holds among them, or of any order of it the coordinator gave that is
    /// no longer outstanding ([`Self::order_try`]), and 1 when there is none. The daemon keys
    /// tries per service (design 8.3): its step record holds the try of the service's last order
    /// and refuses the same try of another package (`try_conflict`) and a lower one
    /// (`stale_try`). The highest is kept, not the last heard,
    /// so a verdict that stops being sent once acknowledged, one that names no try, or an older
    /// one heard after a newer never lowers it. A resend before the host reports keeps the try;
    /// a restarted coordinator rebuilds it from the heartbeats, an acknowledged verdict read back
    /// from the verdicts file and the step record's try among them (design 8.2), so its first
    /// order of a host starts past a try the host ran across its own restart and never reported
    /// (the install of a self-update). A refusal for the try (`try_conflict`, `stale_try`) moves
    /// it past the refused try, and past the record's when the host sends it.
    #[must_use]
    pub fn next_try(&self, service: &str, id: &HostId) -> u64 {
        let key = (id.clone(), service.to_owned());
        let heard = self.tries.get(&key).copied().unwrap_or(0);
        let given = self.sent.get(&key).copied().unwrap_or(0);
        heard.max(given).saturating_add(1)
    }

    /// The try of the order of `service` to `target` for the host `id` at `now`, an uninstall
    /// when `uninstall`. The order of the same kind to `target` given within [`ORDER_AGE`] keeps
    /// its try until the host reports that try or a later one, so a host that asks again before
    /// it reports is told the same order again; an order of the other kind is a new one. An
    /// order given before that aged out with no word of it is followed by one try past it: a
    /// host that refused it in a way the coordinator does not hear (a `try_conflict` with a try
    /// its step record holds, for a service its heartbeat does not list) runs the next one.
    /// Otherwise [`Self::next_try`].
    #[must_use]
    pub fn order_try(
        &self,
        service: &str,
        id: &HostId,
        target: &Version,
        uninstall: bool,
        now: Duration,
    ) -> u64 {
        let key = (id.clone(), service.to_owned());
        let heard = self.tries.get(&key).copied().unwrap_or(0);
        let next = self.next_try(service, id);
        match self.orders.get(&key) {
            Some(order)
                if order.target == *target
                    && order.uninstall.is_some() == uninstall
                    && now.saturating_sub(order.at) < ORDER_AGE
                    && order.order_try > heard =>
            {
                order.order_try
            }
            Some(order) => next.max(order.order_try.saturating_add(1)),
            None => next,
        }
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

    /// Every verdict of `service` counted ([`crate::verdicts::Verdicts::all_of`]).
    pub(crate) fn counted_of(&self, service: &str) -> Vec<(HostId, UpdateResult)> {
        self.verdicts.all_of(service)
    }

    /// Counts again a verdict counted before a restart ([`crate::verdicts::Verdicts::restore`]).
    pub(crate) fn restore_verdict(&mut self, service: &str, id: HostId, report: UpdateResult) {
        self.verdicts.restore(service, id, report);
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

    /// Notes that the host `id` heartbeated, as `node` says, when the coordinator's wall clock
    /// read `unix_now`. The node's [`Node::skewed_beats`] is the count the host had before, plus
    /// one when the heartbeat's `sent_unix` is further than the skew bound
    /// ([`CLOCK_SKEW_AFTER`] by default) from `unix_now`, either way; a heartbeat that says no
    /// time is not counted.
    pub fn note_node(&mut self, id: HostId, mut node: Node, unix_now: u64) {
        let before = self.nodes.get(&id).map_or(0, |known| known.skewed_beats);
        let skewed = node
            .sent_unix
            .is_some_and(|sent| sent.abs_diff(unix_now) > self.skew_after.as_secs());
        node.skewed_beats = before.saturating_add(u64::from(skewed));
        self.nodes.insert(id, node);
    }

    /// The node `id`, if it has heartbeated.
    #[must_use]
    pub fn node(&self, id: &HostId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// The platform the host `id` last named: its node's, else the last word of it about
    /// `service`.
    #[must_use]
    pub fn platform_of(&self, service: &str, id: &HostId) -> Option<Platform> {
        self.node(id)
            .map(|node| node.platform)
            .or_else(|| self.get_in(service, id).map(|host| host.beat.platform))
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
    /// retiring host frees its update slot once it has finished (design 6.1). Returns
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
        self.forget_retries(id);
        self.bake.retain(|(_, o), _| o != id);
        self.found.forget_host(id);
        self.nodes.remove(id);
        self.picks.forget(id);
        true
    }

    /// Notes a heartbeat of `service`: the host's latest word about it replaces its earlier
    /// one.
    ///
    /// An order for the host and service ends when the beat shows it updating (the beat
    /// counts from then), or reports that it declined it ([`crate::defer`]: a deferral holds no
    /// slot, so the order must not hold it for [`ORDER_AGE`] either); an update or an install
    /// also ends when the host runs the ordered version (not as an `absent` copy) or reports
    /// that it failed it, and an uninstall when the host reports a verdict of its order id. A
    /// refusal for the try ([`UpdateResult::is_try_refusal`]) ends the order of the try it
    /// refused, moves the try, and is not kept as the host's report: the host is ordered again
    /// past the try, and never counted as failing the version.
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
        role: &str,
        beat: Heartbeat,
        now: Duration,
    ) {
        self.note(service, id, role, beat, now, false);
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
        // A refusal of an order for its try alone is no word on the release: it is taken out of
        // the beat, so no rule reads it as the host failing the version (the gate's
        // `failed_here`, the plan's failed host, a force's verdict), and it only moves the try.
        let mut beat = beat;
        let refusal = beat.last_update.take_if(|r| r.is_try_refusal());
        let aside = self.retries.observe(&key, &mut beat);
        let had_order = self.orders.contains_key(&key);
        let mut deferred_order = false;
        if let Some(order) = self.orders.get(&key) {
            deferred_order = beat
                .deferred
                .as_ref()
                .is_some_and(|d| d.target == order.target);
            // An uninstall ends at its verdict, whatever version the host still runs.
            let ended = match &order.uninstall {
                // An install is done once the version runs: an absent copy at it is staged only.
                None => {
                    (beat.version == order.target && beat.health != Health::Absent)
                        || beat.last_update.as_ref().is_some_and(|r| {
                            r.outcome == Outcome::Failed && r.target == order.target
                        })
                }
                Some(order_id) => beat
                    .last_update
                    .as_ref()
                    .and_then(|r| r.order_id.as_ref())
                    .is_some_and(|reported| reported.as_str() == order_id),
            };
            // The refusal ends the order of the try it refused, and no later one: one read back
            // from the verdicts file after the order was given again names the older try. One
            // that names no try is taken as the outstanding order's.
            let refused = refusal
                .as_ref()
                .is_some_and(|r| r.order_try.is_none_or(|t| t == order.order_try));
            if (beat.updating.is_some() || ended || deferred_order || refused)
                && let Some(order) = self.orders.remove(&key)
            {
                if refused {
                    tracing::warn!(
                        %service,
                        node_id = %key.0,
                        order_try = order.order_try,
                        record_try = ?beat.record_try,
                        "the host refused the order for its try; it is ordered again past it"
                    );
                }
                self.no_longer_outstanding(key.clone(), &order);
            }
        }
        let baking = (key.1.clone(), key.0.clone());
        // A beat that ends an order is the host's move, even its first: it is never found.
        let first = self.occupant(&key.1, &key.0).is_none() && !had_order;
        self.found.observe(baking.clone(), &beat, first);
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
        // The try of the verdict, of the refusal (whose order the step record holds a try at
        // or past) or of the report set aside, and the one the step record holds, which the host
        // sends whether or not a verdict reached the coordinator.
        let heard = [
            beat.last_update.as_ref().and_then(|r| r.order_try),
            refusal.as_ref().and_then(|r| r.order_try).max(aside),
            beat.record_try,
        ];
        if let Some(heard) = heard.into_iter().flatten().max() {
            let highest = self.tries.entry(key.clone()).or_default();
            *highest = (*highest).max(heard);
        }
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

    /// Notes that the host `id` was told at `now` to update `service` to `target`, or to
    /// install it at `target`. An order for the same target that has not aged out stays as it
    /// was, so a host that asks again before it reports is told again without its slot being
    /// held longer.
    pub fn order(&mut self, service: &str, id: HostId, target: Version, now: Duration) {
        self.note_order(service, id, target, None, now);
    }

    /// Notes that the host `id`, which runs `running` of `service`, was told at `now` to
    /// uninstall it by the order `order_id`. It holds the host's slot as an update's order does,
    /// until the host reports it running, reports its verdict, declines it (a deferral of
    /// `running`) or the order ages out; asking again under it does not hold the slot longer.
    pub fn order_uninstall(
        &mut self,
        service: &str,
        id: HostId,
        running: Version,
        order_id: String,
        now: Duration,
    ) {
        self.note_order(service, id, running, Some(order_id), now);
    }

    fn note_order(
        &mut self,
        service: &str,
        id: HostId,
        target: Version,
        uninstall: Option<String>,
        now: Duration,
    ) {
        let key = (id, service.to_owned());
        self.found.forget(&(service.to_owned(), key.0.clone()));
        if self.is_ordered(service, &key.0, now)
            && self
                .orders
                .get(&key)
                .is_some_and(|o| o.target == target && o.uninstall == uninstall)
        {
            return;
        }
        let order_try = self.order_try(service, &key.0, &target, uninstall.is_some(), now);
        let order = Order {
            target,
            at: now,
            uninstall,
            order_try,
        };
        if let Some(replaced) = self.orders.insert(key.clone(), order) {
            self.no_longer_outstanding(key, &replaced);
        }
    }

    /// Keeps the try of `order`, which no longer counts as outstanding, as one
    /// [`Self::next_try`] numbers past.
    fn no_longer_outstanding(&mut self, key: (HostId, String), order: &Order) {
        let given = self.sent.entry(key).or_default();
        *given = (*given).max(order.order_try);
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

    /// The hosts with an order of `service` outstanding at `now` whose last word about it the
    /// fleet does not keep: a host told to install a service it does not list. They hold their
    /// slot as the others do ([`crate::decide`]).
    pub fn ordered_unseen(&self, service: &str, now: Duration) -> impl Iterator<Item = &HostId> {
        self.orders
            .iter()
            .filter(move |((id, s), order)| {
                s == service
                    && now.saturating_sub(order.at) < ORDER_AGE
                    && self.occupant(service, id).is_none()
            })
            .map(|((id, _), _)| id)
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
    /// time. A host that is moving ([`crate::moving`]) is also busy with an update of a package it
    /// is leaving, which no service counts it for.
    #[must_use]
    pub fn is_busy_besides(&self, service: &str, id: &HostId, now: Duration) -> bool {
        let leaving = self.outside.iter().filter(|_| self.is_moving(id));
        let updating = self
            .hosts
            .iter()
            .chain(leaving)
            .filter(|(s, _)| s.as_str() != service)
            .any(|(_, hosts)| hosts.get(id).is_some_and(|h| h.beat.updating.is_some()));
        updating
            || self.orders.iter().any(|((o, s), order)| {
                o == id && s != service && now.saturating_sub(order.at) < ORDER_AGE
            })
    }

    /// Whether the host `id` holds an update slot of `service`: [`Host::holds_slot`], or an
    /// order is outstanding for it. A host an operator excluded from `service` holds one only
    /// while it runs an update (updating, or its promotion pending) or has an order outstanding,
    /// so a host excluded mid-update keeps its slot until that update ends; one whose last update
    /// failed and that is not healthy holds none, since the service no longer orders it and
    /// nothing of an update of it runs: it would keep its role's `max_updating` taken for as long
    /// as it stays excluded.
    #[must_use]
    pub fn holds_slot(&self, service: &str, id: &HostId, host: &Host, now: Duration) -> bool {
        let running = host.beat.updating.is_some() || host.beat.promotion_pending;
        let held = match self.is_excluded(service, id) {
            true => running,
            false => host.holds_slot(),
        };
        held || self.is_ordered(service, id, now)
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
    /// service's hosts and the ones it no longer counts. The update slots of a plan are counted
    /// over these ([`crate::decide`], [`crate::stages`]), so a host that leaves a service
    /// mid-update still holds its slot.
    pub fn occupants_of(&self, service: &str) -> impl Iterator<Item = (&HostId, &Host)> {
        self.hosts_of(service)
            .chain(self.outside.get(service).into_iter().flatten())
    }

    /// Replaces the operator's marks the fleet decides by with `marks`.
    pub fn set_marks(&mut self, marks: Marks) {
        self.marks = marks.in_mode_of(&self.marks);
    }

    /// What the fleet keeps of the host `id` for `service`, counted or not.
    pub(crate) fn occupant(&self, service: &str, id: &HostId) -> Option<&Host> {
        self.occupants_of(service)
            .find(|(occupant, _)| *occupant == id)
            .map(|(_, host)| host)
    }

    /// The role the host `id` has now: its certificate's at its last heartbeat; for a host the
    /// fleet knows only by its beats of `service`, the role of those; empty for a host it does
    /// not know.
    pub(crate) fn role_in(&self, service: &str, id: &HostId) -> &str {
        match self.nodes.get(id) {
            Some(node) => node.role.as_str(),
            None => self.occupant(service, id).map_or("", Host::role_name),
        }
    }

    /// Whether an operator holds `service` on the host `id`: a hold of the host, of the role it
    /// has now (its certificate's at its last heartbeat; for a host the fleet knows only by its
    /// beats of `service`, the role of those), or of the whole fleet.
    #[must_use]
    pub fn is_held(&self, service: &str, id: &HostId) -> bool {
        holding(
            &self.marks.holds,
            service,
            id.as_str(),
            self.role_in(service, id),
        )
        .next()
        .is_some()
    }

    /// The host `id` as an assignment of `service` reads it: its role now and its labels.
    fn placement<'a>(&'a self, service: &str, id: &'a HostId) -> Placement<'a> {
        Placement {
            node_id: id.as_str(),
            role: self.role_in(service, id),
            labels: self.marks.labels.get(id.as_str()),
        }
    }

    /// Whether `def` is assigned to the host `id`, by its role now and its labels
    /// ([`crate::assignment::assigned`]).
    #[must_use]
    pub fn is_assigned(&self, def: &ServiceDef, id: &HostId) -> bool {
        assigned(
            def,
            self.marks.assignments.get(&def.name),
            &self.placement(&def.name, id),
        )
    }

    /// The minimum version of `def` that applies to the host `id`
    /// ([`crate::assignment::effective_min_version`]).
    #[must_use]
    pub fn min_version<'a>(&'a self, def: &'a ServiceDef, id: &HostId) -> Option<&'a Version> {
        effective_min_version(
            def,
            self.marks.assignments.get(&def.name),
            &self.placement(&def.name, id),
        )
    }

    /// Whether an operator excluded the host `id` from `service`.
    #[must_use]
    pub fn is_excluded(&self, service: &str, id: &HostId) -> bool {
        self.marks
            .excluded
            .contains(&(service.to_owned(), id.as_str().to_owned()))
    }

    /// The order id of the uninstall of `service` the host `id` is ordered, if an operator's
    /// `Unassign` ordered one ([`crate::assignment`]).
    #[must_use]
    pub fn uninstall_order(&self, service: &str, id: &HostId) -> Option<String> {
        self.marks
            .assignments
            .get(service)?
            .uninstall_order(id.as_str())
    }

    /// Whether the last heartbeat of the host `id` listed `service`.
    #[must_use]
    pub fn lists(&self, id: &HostId, service: &str) -> bool {
        self.nodes
            .get(id)
            .is_some_and(|node| node.announced.contains_key(service))
    }

    /// Forgets everything of the host `id` for `service`, counted or not, with its order and
    /// bake: the host no longer has the service at all (it reported it gone after an
    /// uninstall), so nothing of it occupies a slot.
    pub fn drop_in(&mut self, service: &str, id: &HostId) {
        for hosts in [&mut self.hosts, &mut self.outside] {
            hosts.entry(service.to_owned()).or_default().remove(id);
        }
        self.orders.remove(&(id.clone(), service.to_owned()));
        self.bake.remove(&(service.to_owned(), id.clone()));
        self.found.forget(&(service.to_owned(), id.clone()));
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

    /// Notes that the canary `node_id` failed the release of `version` of `service`; a later
    /// canary failing the same version replaces it.
    pub fn note_failed_canary(&mut self, service: &str, version: &Version, node_id: &str) {
        self.canaries.insert(
            (service.to_owned(), version.as_str().to_owned()),
            node_id.to_owned(),
        );
    }

    /// The canaries that failed a release of `service`: each version, by its text, with the node
    /// id of the canary that failed it.
    pub fn failed_canaries(&self, service: &str) -> impl Iterator<Item = (&str, &str)> {
        self.canaries
            .iter()
            .filter(move |((s, _), _)| s == service)
            .map(|((_, version), node)| (version.as_str(), node.as_str()))
    }
}
