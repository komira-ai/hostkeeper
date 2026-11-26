//! Whether an order the coordinator gave still holds its host's update slot (design sections 5.4
//! and 8.4).
//!
//! An order holds the slot from the moment it is given until the host reports it. A host that
//! goes on heartbeating without `updating` did not hear it, so the order's age ([`ORDER_AGE`])
//! frees the slot. A host that went silent after it was ordered ([`DOWN_AFTER`] without a beat)
//! keeps the slot whatever the order's age: it may have heard the order and started, and only its
//! beats are lost. An operator ends that wait with `Hosts.Exclude` and `abandon`
//! ([`Fleet::abandon_in`]); a beat of the host, updating or not, ends it as any beat does. A host
//! the fleet has never heard (an order given before anything of the host was noted, as only a test
//! does) is judged by the order's age alone.

use std::time::Duration;

use hostkeeper_wire::HostId;

use super::{DOWN_AFTER, Fleet, Host, ORDER_AGE, Order};

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

    /// Whether `order`, given to the host `id`, is outstanding at `now`: given within
    /// [`ORDER_AGE`], or given before while the host has not heartbeated within [`DOWN_AFTER`].
    pub(super) fn order_holds(&self, id: &HostId, order: &Order, now: Duration) -> bool {
        now.saturating_sub(order.at) < ORDER_AGE
            || self
                .last_heard(id)
                .is_some_and(|seen| now.saturating_sub(seen) >= DOWN_AFTER)
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
            || self
                .orders
                .iter()
                .any(|((o, s), order)| o == id && s != service && self.order_holds(id, order, now))
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
}

#[cfg(test)]
#[path = "fleet_orders_tests.rs"]
mod tests;
