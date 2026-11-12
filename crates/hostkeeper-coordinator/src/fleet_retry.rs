//! The failed reports an operator set aside with `Hosts.Retry` ([`crate::retry`]), and the order
//! a host was given and has not answered, which `Releases.Status` and `Hosts.Get` show
//! ([`Outstanding`]).
//!
//! A host keeps the report of its last failed update until it runs another, and the coordinator
//! reads an acknowledged one back from the verdicts file, so the gate's `failed_here` holds the
//! host back from the version it failed for as long as the report stands. A report set aside is
//! taken out of each heartbeat of the host and service before any rule of the fleet reads it,
//! as a refusal for the try is ([`Fleet::record_in`]): the gate, the plan, the update slot, the
//! bake, the end of an order and the counting read the host as having no report, so it is
//! ordered again, and its try still counts towards the next one, which is past it. The halt the
//! report raised is the halt module's, which reads the heartbeat before the fleet does: a halt
//! an operator cleared stays cleared by the report it acknowledged.
//!
//! A report stays set aside until the host reports another, or reports the same one again once
//! it was seen updating, or, for a report that names no try, once a second order was given (a
//! daemon that names no try in its verdicts could fail the new order with a report equal to the
//! old one, before a heartbeat said it was updating). Like the orders, it is kept in memory only.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Heartbeat, Outcome, UpdateResult};

use super::{Fleet, ORDER_AGE};

/// A failed report set aside.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Aside {
    /// The report.
    report: UpdateResult,
    /// Whether the same report heard again is the new order's verdict: the host was seen
    /// updating since, or, for a report that names no try, a second order was given since (the
    /// first went unanswered, so the host may have failed it before a heartbeat said it was
    /// updating).
    started: bool,
    /// The try of the first order given since the report was set aside.
    first_try: Option<u64>,
}

/// The reports set aside, and the ids of the orders the coordinator gave.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Retries {
    /// Per host and service, the failed report set aside.
    set_aside: BTreeMap<(HostId, String), Aside>,
    /// Per host and service, the target and the id of the update or install the coordinator
    /// ordered last.
    named: BTreeMap<(HostId, String), (Version, String)>,
}

impl Retries {
    /// Takes the report set aside for `key` out of `beat`, if `beat` carries it, and returns its
    /// try, which the next order numbers past; ends the setting aside when `beat` carries another
    /// report, or carries it again once it is the new order's verdict ([`Aside::started`]).
    pub(super) fn observe(&mut self, key: &(HostId, String), beat: &mut Heartbeat) -> Option<u64> {
        let aside = self.set_aside.get_mut(key)?;
        let updating = beat.updating.is_some();
        let same = beat.last_update.as_ref() == Some(&aside.report);
        if same && (updating || !aside.started) {
            aside.started |= updating;
            return beat.last_update.take().and_then(|r| r.order_try);
        }
        if beat.last_update.is_some() {
            self.set_aside.remove(key);
        } else {
            aside.started |= updating;
        }
        None
    }
}

/// An order a host was given and has not answered: it has not said it is updating, reported a
/// verdict of it or declined it, and the order has not aged out ([`ORDER_AGE`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outstanding {
    /// The order's id: an uninstall's, or the update's or install's the coordinator named
    /// ([`Fleet::name_order`]).
    pub order_id: Option<String>,
    /// Its try.
    pub order_try: u64,
    /// The version ordered; for an uninstall, the version the host ran when it was ordered.
    pub target: Version,
    /// Whether it is an uninstall.
    pub uninstall: bool,
}

impl Fleet {
    /// The failed report of `service` the host `id` last made, with whether it is set aside
    /// already: the report its last heartbeat kept, counted or not, if it is a failure, else the
    /// one set aside, if its last heartbeat kept none. None when there is neither, when the
    /// fleet keeps no word of the service from the host, and when the host runs the version it
    /// failed (a halt after its switch): ordered again, it would be up to date, and a force is
    /// what repairs it.
    #[must_use]
    pub fn retryable(&self, service: &str, id: &HostId) -> Option<(UpdateResult, bool)> {
        let host = [&self.hosts, &self.outside]
            .into_iter()
            .find_map(|kept| kept.get(service).and_then(|hosts| hosts.get(id)))?;
        let runs =
            |target: &Version| host.beat.health != Health::Absent && host.beat.version == *target;
        match &host.beat.last_update {
            Some(report) if report.outcome == Outcome::Failed && !runs(&report.target) => {
                Some((report.clone(), false))
            }
            Some(_) => None,
            None => self
                .set_aside_of(service, id)
                .map(|report| (report.clone(), true)),
        }
    }

    /// Sets aside the failed report of `service` the host `id` last made, so no rule reads it from
    /// now on (the module's header), if its last heartbeat kept one ([`Self::retryable`]).
    pub fn set_aside(&mut self, service: &str, id: &HostId) {
        let host = [&mut self.hosts, &mut self.outside]
            .into_iter()
            .find_map(|kept| kept.get_mut(service).and_then(|hosts| hosts.get_mut(id)));
        if let Some(report) = host.and_then(|host| {
            host.beat
                .last_update
                .take_if(|r| r.outcome == Outcome::Failed)
        }) {
            self.retries.set_aside.insert(
                (id.clone(), service.to_owned()),
                Aside {
                    report,
                    started: false,
                    first_try: None,
                },
            );
        }
    }

    /// The report of `service` set aside for the host `id`, if one is.
    #[must_use]
    pub fn set_aside_of(&self, service: &str, id: &HostId) -> Option<&UpdateResult> {
        self.retries
            .set_aside
            .get(&(id.clone(), service.to_owned()))
            .map(|aside| &aside.report)
    }

    /// Notes the id of the update or install of `service` to `target` the host `id` was just
    /// ordered ([`Self::order`]), which [`Self::outstanding`] shows. An order given while a report that names no try is set aside, with a try other than the
    /// first given since, makes that report, heard again, the new order's verdict.
    pub fn name_order(&mut self, service: &str, id: &HostId, target: &Version, order_id: String) {
        let key = (id.clone(), service.to_owned());
        let given = self.orders.get(&key).map(|order| order.order_try);
        if let Some(aside) = self.retries.set_aside.get_mut(&key) {
            let first = *aside.first_try.get_or_insert(given.unwrap_or_default());
            aside.started |= aside.report.order_try.is_none() && given != Some(first);
        }
        self.retries.named.insert(key, (target.clone(), order_id));
    }

    /// The order of `service` the host `id` was given and has not answered at `now`, if one is
    /// outstanding.
    #[must_use]
    pub fn outstanding(&self, service: &str, id: &HostId, now: Duration) -> Option<Outstanding> {
        let key = (id.clone(), service.to_owned());
        let order = self
            .orders
            .get(&key)
            .filter(|order| now.saturating_sub(order.at) < ORDER_AGE)?;
        let named = self
            .retries
            .named
            .get(&key)
            .filter(|(target, _)| order.uninstall.is_none() && *target == order.target)
            .map(|(_, order_id)| order_id.clone());
        Some(Outstanding {
            order_id: order.uninstall.clone().or(named),
            order_try: order.order_try,
            target: order.target.clone(),
            uninstall: order.uninstall.is_some(),
        })
    }

    /// Forgets what is set aside and named for the host `id`, which the fleet forgets.
    pub(super) fn forget_retries(&mut self, id: &HostId) {
        self.retries.set_aside.retain(|(host, _), _| host != id);
        self.retries.named.retain(|(host, _), _| host != id);
    }
}
