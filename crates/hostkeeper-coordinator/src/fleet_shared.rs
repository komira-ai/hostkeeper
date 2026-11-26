//! The fleet in memory as a cache of the fleet's rows (`docs/design/coordinator-multi-instance.md`,
//! sections 5.1 to 5.3): what a heartbeat's decision, under the rollout gate, takes in from the rows
//! before it decides ([`Fleet::take_rows`]), and the orders it writes back when it is done
//! ([`Fleet::writes_since`]).
//!
//! Over a backend other coordinators share, the rows are what every decision reads: the orders
//! that hold a slot (whichever coordinator gave them), the bakes, the tries no longer outstanding
//! and the quiet time after a gap in the beats. The fleet in memory takes them in place of its
//! own, so the pure decision code reads the fleet as the rows have it. Times are the database's in
//! the rows and the coordinator's clock in memory: a row's time is placed as far before `now` as
//! it is before the database's clock when the rows were read, and back again.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;

use super::{Fleet, Order};
use crate::bake::Bake;
use crate::store::decision::{OrderRow, Rows, TryRow, Writes};

/// What the fleet keeps of the rows besides its maps.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shared {
    /// Until when the fleet is quiet after a gap in the beats, as the rows last said: once the
    /// rows were read, this and not the coordinator's own start decides [`Fleet::is_quiet`].
    quiet_until: Option<Duration>,
    /// The row of each host another coordinator heard that this fleet took in, as it was: a row
    /// not changed since is not taken in again.
    folded: BTreeMap<String, (i64, i64, String)>,
    /// The orders memory dropped of each host and service, and when: an operator abandoned the
    /// host, or it retired ([`Fleet::drop_orders`]). A row of one given by then is not taken in,
    /// so the next decision ends it; a mark goes once no row of its host and service is read.
    dropped: BTreeMap<(HostId, String), Duration>,
}

/// `unix`, a time of the database's clock, placed on the coordinator's clock: `now` is the
/// database's `now_unix`.
fn placed(unix: i64, now: Duration, now_unix: i64) -> Duration {
    let ahead = u64::try_from(unix.saturating_sub(now_unix)).unwrap_or(0);
    let behind = u64::try_from(now_unix.saturating_sub(unix)).unwrap_or(0);
    now.saturating_add(Duration::from_secs(ahead))
        .saturating_sub(Duration::from_secs(behind))
}

/// `at`, a time of the coordinator's clock, back on the database's: `now` is `now_unix`.
fn unplaced(at: Duration, now: Duration, now_unix: i64) -> i64 {
    let behind = i64::try_from(now.saturating_sub(at).as_secs()).unwrap_or(i64::MAX);
    now_unix.saturating_sub(behind)
}

fn try_of(order_try: i64) -> u64 {
    u64::try_from(order_try).unwrap_or(0)
}

impl Fleet {
    /// Whether the rows' quiet time after a gap in the beats holds at `now`, once the rows were
    /// read; `None` before.
    pub(super) fn quiet_by_rows(&self, now: Duration) -> Option<bool> {
        self.shared.quiet_until.map(|until| now < until)
    }

    /// Takes in `rows`, read under the gate at `now`: the orders, bakes and quiet time become the
    /// rows', and the tries no longer outstanding the highest of memory's and the rows'. An order
    /// memory held that the rows do not is no longer outstanding: its try is kept as one the next
    /// order numbers past. A row that does not read (a host id or a version this build cannot
    /// parse) is left out; the order rows it kept are answered, the only ones a decision may
    /// write back over ([`Self::writes_since`]): a row a newer release wrote is never ended by it.
    pub(crate) fn take_rows(&mut self, rows: &Rows, now: Duration) -> Vec<OrderRow> {
        let at = |unix: i64| placed(unix, now, rows.now_unix);
        let (mut orders, mut kept) = (BTreeMap::new(), Vec::new());
        for row in &rows.orders {
            let (Ok(host), Ok(target)) =
                (row.host_id.parse::<HostId>(), Version::parse(&row.target))
            else {
                continue;
            };
            let order = Order {
                target,
                at: at(row.at_unix),
                uninstall: row.uninstall_id.clone(),
                order_try: try_of(row.order_try),
            };
            kept.push(row.clone());
            let key = (host, row.service.clone());
            if self
                .shared
                .dropped
                .get(&key)
                .is_some_and(|by| order.at <= *by)
            {
                continue;
            }
            orders.insert(key, order);
        }
        self.shared.dropped.retain(|(host, service), _| {
            kept.iter()
                .any(|row| row.host_id == host.as_str() && row.service == *service)
        });
        let gone: Vec<_> = self
            .orders
            .iter()
            .filter(|(key, _)| !orders.contains_key(*key))
            .map(|(key, order)| (key.clone(), order.order_try))
            .collect();
        for (key, order_try) in gone {
            let given = self.sent.entry(key).or_default();
            *given = (*given).max(order_try);
        }
        self.orders = orders;
        self.bake.clear();
        for row in &rows.bakes {
            let (Ok(host), Ok(target)) =
                (row.host_id.parse::<HostId>(), Version::parse(&row.target))
            else {
                continue;
            };
            let bake = Bake {
                target,
                beats: u32::try_from(row.beats).unwrap_or(u32::MAX),
                at: at(row.at_unix),
            };
            self.bake.insert((row.service.clone(), host), bake);
        }
        for row in &rows.tries {
            let Ok(host) = row.host_id.parse::<HostId>() else {
                continue;
            };
            let given = self.sent.entry((host, row.service.clone())).or_default();
            *given = (*given).max(try_of(row.order_try));
        }
        self.shared.quiet_until = Some(at(rows.quiet_until));
        kept
    }

    /// Whether the row of `host`, which another coordinator heard, changed since this fleet took
    /// it in; it is noted as taken in.
    pub(crate) fn row_is_new(
        &mut self,
        host: &str,
        seen_unix: i64,
        sent_unix: i64,
        by: &str,
    ) -> bool {
        let row = (seen_unix, sent_unix, by.to_owned());
        self.shared.folded.insert(host.to_owned(), row.clone()) != Some(row)
    }

    /// The tries no longer outstanding, as memory has them: what [`Self::writes_since`] compares.
    #[must_use]
    pub(crate) fn sent_tries(&self) -> BTreeMap<(HostId, String), u64> {
        self.sent.clone()
    }

    /// Notes that the orders of `keys` (host and service), which memory drops at `now`, end in
    /// the rows too, once the rows were read (over a store other coordinators share): the next take
    /// of the rows leaves out a row of each given by `now`, and that decision ends it. Without it
    /// the next take would put the order back, and the order of a silent host would hold its slot
    /// for ever ([`Fleet::order_holds`]).
    pub(super) fn drop_orders(
        &mut self,
        keys: impl IntoIterator<Item = (HostId, String)>,
        now: Duration,
    ) {
        if self.shared.quiet_until.is_some() {
            self.shared
                .dropped
                .extend(keys.into_iter().map(|key| (key, now)));
        }
    }

    /// What a decision writes back of the orders and tries: the difference between `read` (the
    /// rows of `fleet_orders` it read at `now_unix`) and the orders memory holds at `now` that
    /// are still outstanding ([`Fleet::order_holds`]), and the tries memory raised above `sent`
    /// (those it held when it took the rows in). An order read that memory no longer holds ends,
    /// and its try is kept.
    #[must_use]
    pub(crate) fn writes_since(
        &self,
        read: &[OrderRow],
        sent: &BTreeMap<(HostId, String), u64>,
        now: Duration,
        now_unix: i64,
    ) -> Writes {
        // A row's time placed before the coordinator's start is its start: written back, it keeps
        // the row's own time, so an order is never made younger by the round trip.
        let held: Vec<OrderRow> = self
            .orders
            .iter()
            .filter(|((host, _), order)| self.order_holds(host, order, now))
            .map(|((host, service), order)| {
                let at_unix = read
                    .iter()
                    .find(|was| was.host_id == host.as_str() && was.service == *service)
                    .filter(|was| placed(was.at_unix, now, now_unix) == order.at)
                    .map_or_else(|| unplaced(order.at, now, now_unix), |was| was.at_unix);
                OrderRow {
                    host_id: host.as_str().to_owned(),
                    service: service.clone(),
                    target: order.target.to_string(),
                    uninstall_id: order.uninstall.clone(),
                    at_unix,
                    order_try: i64::try_from(order.order_try).unwrap_or(i64::MAX),
                }
            })
            .collect();
        let mut writes = Writes::default();
        // Each order held is matched to the row read of its host and service, else of its host.
        let mut left: Vec<Option<&OrderRow>> = held.iter().map(Some).collect();
        for was in read {
            let mine = |row: &OrderRow| row.host_id == was.host_id && row.service == was.service;
            let of_host = |row: &OrderRow| row.host_id == was.host_id;
            let at = left
                .iter()
                .position(|row| row.is_some_and(mine))
                .or_else(|| left.iter().position(|row| row.is_some_and(of_host)));
            match at.and_then(|at| left[at].take()) {
                Some(now) if now == was => {}
                Some(now) => writes.replaced.push((was.clone(), now.clone())),
                None => {
                    writes.tries.push(TryRow {
                        host_id: was.host_id.clone(),
                        service: was.service.clone(),
                        order_try: was.order_try,
                    });
                    writes.ended.push(was.clone());
                }
            }
        }
        // A second order of one host is written too, and refused by the host's key: one update
        // at a time on a host.
        writes.given.extend(left.into_iter().flatten().cloned());
        for ((host, service), order_try) in &self.sent {
            if sent.get(&(host.clone(), service.clone())) != Some(order_try) {
                writes.tries.push(TryRow {
                    host_id: host.as_str().to_owned(),
                    service: service.clone(),
                    order_try: i64::try_from(*order_try).unwrap_or(i64::MAX),
                });
            }
        }
        writes
    }
}

#[cfg(test)]
#[path = "fleet_shared_tests.rs"]
mod tests;
