//! The fleet as a cache of the fleet's rows (`docs/design/coordinator-multi-instance.md`, section
//! 5): when coordinators share a database, what the orders, the bakes and the quiet time are is
//! what the rows say, read after the rollout gate is held, and the fleet takes them in before
//! every decision. Between decisions the fleet is a copy; no decision reads one that was not
//! refreshed from the rows under the gate.
//!
//! The hosts' beats come in the way they always did ([`Fleet::record_named_in`]): the coordinator
//! that heard a beat notes it, and one that did not notes it from the host's row when it reads
//! the rows.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;

use super::{Fleet, ORDER_AGE, Order};
use crate::bake::Bake;
use crate::store::postgres::fleet_rows::{BakeRow, OrderRow};

/// [`ORDER_AGE`] in seconds.
const ORDER_AGE_SECS: i64 = ORDER_AGE.as_secs().cast_signed();

/// The moment `at_unix` was, on the clock of the fleet, whose `now` is the Unix time `unix_now`.
fn on_clock(now: Duration, unix_now: i64, at_unix: i64) -> Duration {
    let age = u64::try_from(unix_now.saturating_sub(at_unix)).unwrap_or(0);
    now.saturating_sub(Duration::from_secs(age))
}

impl Fleet {
    /// Takes the orders of `rows` as the fleet's orders, at `now` of the fleet's clock, which is
    /// the Unix time `unix_now`: an order the rows do not hold is no longer outstanding, one they
    /// hold is as they say it, and one older than [`ORDER_AGE`] counts for nothing (the rows
    /// drop it when the next order is asked for).
    pub(crate) fn sync_orders(&mut self, rows: &[OrderRow], now: Duration, unix_now: i64) {
        self.take_orders(rows, now, unix_now, &|_| true);
    }

    /// [`Self::sync_orders`] for the orders of the host `id` alone: what the rows hold of it before
    /// its beat is noted, so that the beat ends the order the coordinator that gave it holds, as
    /// the beat does where the order was given.
    pub(crate) fn sync_orders_of(
        &mut self,
        id: &HostId,
        rows: &[OrderRow],
        now: Duration,
        unix_now: i64,
    ) {
        self.take_orders(rows, now, unix_now, &|host| host == id);
    }

    fn take_orders(
        &mut self,
        rows: &[OrderRow],
        now: Duration,
        unix_now: i64,
        of: &dyn Fn(&HostId) -> bool,
    ) {
        let wanted: BTreeMap<(HostId, String), Order> = rows
            .iter()
            .filter(|row| unix_now.saturating_sub(row.at_unix) < ORDER_AGE_SECS)
            .filter_map(|row| {
                let id = row.host_id.parse().ok()?;
                let target = Version::parse(&row.target).ok()?;
                let order = Order {
                    target,
                    at: on_clock(now, unix_now, row.at_unix),
                    uninstall: row.uninstall_id.clone(),
                    order_try: u64::try_from(row.order_try).unwrap_or(0),
                };
                Some(((id, row.service.clone()), order))
            })
            .collect();
        let gone: Vec<((HostId, String), Order)> = self
            .orders
            .iter()
            .filter(|(key, _)| of(&key.0) && !wanted.contains_key(*key))
            .map(|(key, order)| (key.clone(), order.clone()))
            .collect();
        for (key, order) in gone {
            self.orders.remove(&key);
            self.no_longer_outstanding(key, &order);
        }
        for (key, order) in wanted {
            if let Some(replaced) = self.orders.insert(key.clone(), order) {
                self.no_longer_outstanding(key, &replaced);
            }
        }
    }

    /// Takes the bakes of `rows` as the fleet's, on the clock as [`Self::sync_orders`].
    pub(crate) fn sync_bakes(&mut self, rows: &[BakeRow], now: Duration, unix_now: i64) {
        self.bake = rows
            .iter()
            .filter_map(|row| {
                let id: HostId = row.host_id.parse().ok()?;
                let bake = Bake {
                    target: Version::parse(&row.target).ok()?,
                    beats: u32::try_from(row.beats).unwrap_or(u32::MAX),
                    at: on_clock(now, unix_now, row.at_unix),
                };
                Some(((row.service.clone(), id), bake))
            })
            .collect();
    }

    /// Takes the quiet time of the rows (`until_unix`, 0 for none) as the one the fleet's
    /// decisions hold back until, on the clock as [`Self::sync_orders`]: from now on the fleet's
    /// own start does not say it.
    pub(crate) fn sync_quiet(&mut self, until_unix: i64, now: Duration, unix_now: i64) {
        let left = u64::try_from(until_unix.saturating_sub(unix_now)).unwrap_or(0);
        self.quiet_until = Some(now.saturating_add(Duration::from_secs(left)));
    }

    /// How long after its start the fleet orders nothing, when it does not read the rows.
    pub(crate) fn quiet(&self) -> Duration {
        self.quiet
    }

    /// The orders [`Self::record_in`] ended since this was last called, by host and service.
    pub(crate) fn take_ended_orders(&mut self) -> Vec<(HostId, String)> {
        std::mem::take(&mut self.ended)
    }
}
