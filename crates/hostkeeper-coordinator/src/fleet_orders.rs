//! Whether an order the coordinator gave still holds its host's update slot (design sections 5.4
//! and 8.4).
//!
//! An order holds the slot from the moment it is given until the host reports it. A host that
//! goes on heartbeating without `updating` did not hear it, so the order's age ([`ORDER_AGE`])
//! frees the slot. A host that went silent after it was ordered ([`DOWN_AFTER`] without a beat)
//! keeps the slot past the order's age: it may have heard the order and started, and only its
//! beats are lost. It keeps it as a host whose last word was an update running does, until it has
//! been silent for [`SILENT_UPDATE_AFTER`]: from then on the order holds no slot and no rollout
//! place, and `silent_while_updating` names the host ([`crate::silent::silent_updates`], one
//! rule for both: a host cut off for hours must not hold every other rollout). The order is kept
//! while the host stays silent (so the alert stands, and the row of a shared store is not ended);
//! the host's first beat is read as any other's, and the order then ends by its age. An operator
//! frees the slot before the bound with `Hosts.Exclude` and `abandon` ([`Fleet::abandon_in`]). A
//! host the fleet has never heard (an order given before anything of the host was noted, as only a
//! test does) is judged by the order's age alone.
//!
//! A restarted coordinator knows no host until it beats, and forgets its orders (design 8.2). A
//! host that was updating when last heard, as its enrollment kept it, holds its slot from the
//! start, as a silent updating host does ([`Fleet::hold_restored`]): one silent through the quiet
//! start would otherwise let a second update be ordered while it runs.

use std::time::Duration;

use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Heartbeat};
use hostkeeper_wire::{HostId, Platform};

use super::{DOWN_AFTER, Fleet, Host, ORDER_AGE, Order};
use crate::silent::SILENT_UPDATE_AFTER;

impl Fleet {
    /// When the host `id` was last heard: the latest of its node's last heartbeat and its last
    /// word about any service, counted or not; `None` for a host never heard.
    fn last_heard(&self, id: &HostId) -> Option<Duration> {
        let node = self.nodes.get(id).map(|node| node.seen);
        self.hosts
            .values()
            .chain(self.outside.values())
            .filter_map(|hosts| hosts.get(id))
            .map(|host| host.seen)
            .chain(node)
            .max()
    }

    /// How long the host `id` has been silent at `now`; `None` for a host never heard.
    fn silence(&self, id: &HostId, now: Duration) -> Option<Duration> {
        self.last_heard(id).map(|seen| now.saturating_sub(seen))
    }

    /// Whether `order`, given to the host `id`, is kept at `now`: given within [`ORDER_AGE`], or
    /// given before while the host has not heartbeated within [`DOWN_AFTER`], whatever the
    /// silence. A kept order holds the slot only while [`Self::order_holds`] says so.
    pub(super) fn order_kept(&self, id: &HostId, order: &Order, now: Duration) -> bool {
        now.saturating_sub(order.at) < ORDER_AGE
            || self
                .silence(id, now)
                .is_some_and(|silent| silent >= DOWN_AFTER)
    }

    /// Whether `order`, given to the host `id`, is outstanding at `now`, holding its slot: given
    /// within [`ORDER_AGE`], or given before while the host has been silent for [`DOWN_AFTER`]
    /// or more and less than [`SILENT_UPDATE_AFTER`] (the bound of [`Host::is_lost`]).
    pub(super) fn order_holds(&self, id: &HostId, order: &Order, now: Duration) -> bool {
        now.saturating_sub(order.at) < ORDER_AGE
            || self
                .silence(id, now)
                .is_some_and(|silent| silent >= DOWN_AFTER && silent < SILENT_UPDATE_AFTER)
    }

    /// The orders of `service` kept for a host that has been silent for [`SILENT_UPDATE_AFTER`]
    /// or more at `now`, so they hold nothing: each host, the version ordered (for an uninstall,
    /// the version the host ran) and how long the host has been silent.
    pub(crate) fn lost_orders<'a>(
        &'a self,
        service: &'a str,
        now: Duration,
    ) -> impl Iterator<Item = (&'a HostId, &'a Version, Duration)> + 'a {
        self.orders
            .iter()
            .filter(move |((_, s), order)| {
                s == service && now.saturating_sub(order.at) >= ORDER_AGE
            })
            .filter_map(move |((id, _), order)| {
                let silent = self.silence(id, now)?;
                (silent >= SILENT_UPDATE_AFTER).then_some((id, &order.target, silent))
            })
    }

    /// The hosts with an order of `service` outstanding at `now` ([`Self::order_holds`]) whose
    /// last word about it the fleet does not keep: a host told to install a service it does not
    /// list. They hold their slot as the others do ([`crate::decide`]).
    pub fn ordered_unseen(&self, service: &str, now: Duration) -> impl Iterator<Item = &HostId> {
        self.orders
            .iter()
            .filter(move |((id, s), order)| {
                s == service
                    && self.order_holds(id, order, now)
                    && self.occupant(service, id).is_none()
            })
            .map(|((id, _), _)| id)
    }

    /// Whether `id` has an order outstanding for `service` at `now` ([`Self::order_holds`]).
    #[must_use]
    pub fn is_ordered(&self, service: &str, id: &HostId, now: Duration) -> bool {
        self.orders
            .get(&(id.clone(), service.to_owned()))
            .is_some_and(|order| self.order_holds(id, order, now))
    }

    /// Whether the host `id` is busy with a service other than `service`: one of its other
    /// services is updating ([`Host::is_updating`]: not silent past
    /// [`crate::silent::SILENT_UPDATE_AFTER`]), or has an order outstanding for it. A host does
    /// one update at a time. A host that is moving ([`crate::moving`]) is also busy with an
    /// update of a package it is leaving, which no service counts it for.
    #[must_use]
    pub fn is_busy_besides(&self, service: &str, id: &HostId, now: Duration) -> bool {
        let leaving = self.outside.iter().filter(|_| self.is_moving(id));
        let updating = self
            .hosts
            .iter()
            .chain(leaving)
            .filter(|(s, _)| s.as_str() != service)
            .any(|(_, hosts)| hosts.get(id).is_some_and(|h| h.is_updating(now)));
        updating
            || self
                .orders
                .iter()
                .any(|((o, s), order)| o == id && s != service && self.order_holds(id, order, now))
    }

    /// Whether the host `id` holds an update slot of `service`: [`Host::holds_slot_at`] (a host
    /// silent for [`crate::silent::SILENT_UPDATE_AFTER`] holds none for an update it last said
    /// ran), or an order is outstanding for it. A host an operator excluded from `service` holds one only
    /// while it runs an update (updating, or its promotion pending) or has an order outstanding,
    /// so a host excluded mid-update keeps its slot until that update ends; one whose last update
    /// failed and that is not healthy holds none, since the service no longer orders it and
    /// nothing of an update of it runs: it would keep its role's `max_updating` taken for as long
    /// as it stays excluded.
    #[must_use]
    pub fn holds_slot(&self, service: &str, id: &HostId, host: &Host, now: Duration) -> bool {
        let running = host.is_updating(now) || host.is_promoting(now);
        let held = match self.is_excluded(service, id) {
            true => running,
            false => host.holds_slot_at(now),
        };
        held || self.is_ordered(service, id, now)
    }

    /// Forgets the host `id` for `service` altogether, its order with it (in the rows of a shared
    /// store too: [`Self::drop_orders`]), so the slot it held is free now: an operator abandoned
    /// it (`Hosts.Exclude` with `abandon`), the one way to free the slot of a host that went silent
    /// with an order. Answers whether it held the slot at `now`. A beat it sends later is noted as
    /// any other.
    pub fn abandon_in(&mut self, service: &str, id: &HostId, now: Duration) -> bool {
        let held = self
            .occupant(service, id)
            .is_some_and(|host| self.holds_slot(service, id, host, now));
        for hosts in [&mut self.hosts, &mut self.outside] {
            hosts.entry(service.to_owned()).or_default().remove(id);
        }
        let key = (id.clone(), service.to_owned());
        self.orders.remove(&key);
        self.drop_orders([key], now);
        held
    }

    /// Notes that the host `id`, of the role named `role` on `platform`, said when it was last
    /// heard, before the coordinator started, that it was updating `service` from `running` to
    /// `updating`: until a word of it about the service replaces this, it holds the service's
    /// update slot as a silent updating host does, unhealthy and taken as last heard at the
    /// coordinator's start: the silence the coordinator can count begins there, so the word holds
    /// for [`crate::silent::SILENT_UPDATE_AFTER`] after the start at most. It is kept as what the host occupies ([`Self::occupants_of`]), never as one of the
    /// service's hosts, so no plan counts it. A host the fleet already holds a word of for the
    /// service is left as it is.
    pub fn hold_restored(
        &mut self,
        service: &str,
        id: HostId,
        role: &str,
        platform: Platform,
        (running, updating): (Version, Version),
    ) {
        if self.occupant(service, &id).is_some() {
            return;
        }
        let beat = Heartbeat {
            local_halt: None,
            platform,
            version: running,
            health: Health::Unhealthy,
            last_update: None,
            updating: Some(updating),
            services: None,
            agent_features: Vec::new(),
            step: None,
            commit: None,
            step_secs: Default::default(),
            promotion_pending: false,
            deferred: None,
            agent: None,
            sent_unix: None,
            cert_not_after_unix: None,
            last_renew_error: None,
            record_try: None,
            maintenance: None,
        };
        let host = Host {
            role: role.to_owned(),
            beat,
            seen: self.started,
        };
        self.outside
            .entry(service.to_owned())
            .or_default()
            .insert(id, host);
    }
}

#[cfg(test)]
#[path = "fleet_orders_tests.rs"]
mod tests;
