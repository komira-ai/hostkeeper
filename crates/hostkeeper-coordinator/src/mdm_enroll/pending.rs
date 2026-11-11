//! What waits between a Mac's posts and an operator's click, and the limit on a peer's requests.
//!
//! All of it is in memory. A request that waits is a Mac whose Setup Assistant posts again; a
//! restart loses the list and the Mac's next post makes it again, so nothing here needs a table.
//! A confirmation is lost the same way and is made again by a click.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};

/// A request that waits for an operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    /// The UDID the unverified blob claims.
    pub(crate) udid: String,
    /// The product the blob claims, when it names one.
    pub(crate) product: Option<String>,
    /// Why the blob did not verify, for the operator: the text of the error.
    pub(crate) reason: String,
    /// When the first post was seen.
    pub(crate) first_unix: i64,
    /// When the last post was seen.
    pub(crate) last_unix: i64,
    /// How many posts.
    pub(crate) count: u32,
    /// Until when an operator's confirmation stands; none before one.
    pub(crate) confirmed_until: Option<i64>,
}

/// What [`Pending::hold`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held1 {
    /// The serial was not waiting: it is now.
    New,
    /// It was waiting: its row was refreshed.
    Seen,
    /// The list is full of requests an operator confirmed, and this one is not on it.
    Full,
}

#[derive(Default)]
struct Inner {
    held: BTreeMap<String, Held>,
    /// The serials declined and the second each was last audited at.
    declined: BTreeMap<String, i64>,
}

/// The waiting requests and the declined serials.
#[derive(Default)]
pub(crate) struct Pending {
    inner: Mutex<Inner>,
}

impl Pending {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a post of an unverified blob for `serial`. At most `max` serials wait: a new one past
    /// that replaces the one with the oldest last post that no operator confirmed.
    pub(crate) fn hold(
        &self,
        serial: &str,
        udid: &str,
        product: Option<&str>,
        reason: &str,
        now: i64,
        max: usize,
    ) -> Held1 {
        let mut inner = self.inner();
        if let Some(row) = inner.held.get_mut(serial) {
            if row.confirmed_until.is_none_or(|until| until <= now) {
                row.udid = udid.to_owned();
                row.confirmed_until = None;
            }
            row.product = product.map(str::to_owned);
            row.reason = reason.to_owned();
            row.last_unix = now;
            row.count = row.count.saturating_add(1);
            return Held1::Seen;
        }
        if inner.held.len() >= max {
            let oldest = inner
                .held
                .iter()
                .filter(|(_, row)| row.confirmed_until.is_none_or(|until| until <= now))
                .min_by_key(|(serial, row)| (row.last_unix, (*serial).clone()))
                .map(|(serial, _)| serial.clone());
            let Some(oldest) = oldest else {
                return Held1::Full;
            };
            inner.held.remove(&oldest);
        }
        inner.held.insert(
            serial.to_owned(),
            Held {
                udid: udid.to_owned(),
                product: product.map(str::to_owned),
                reason: reason.to_owned(),
                first_unix: now,
                last_unix: now,
                count: 1,
                confirmed_until: None,
            },
        );
        Held1::New
    }

    /// The waiting requests, by serial.
    pub(crate) fn list(&self) -> Vec<(String, Held)> {
        self.inner()
            .held
            .iter()
            .map(|(serial, row)| (serial.clone(), row.clone()))
            .collect()
    }

    /// An operator's confirmation of `serial`, valid until `until`. `None` when the serial is not
    /// waiting; else the row as it is now.
    pub(crate) fn confirm(&self, serial: &str, until: i64) -> Option<Held> {
        let mut inner = self.inner();
        let row = inner.held.get_mut(serial)?;
        row.confirmed_until = Some(until);
        Some(row.clone())
    }

    /// Spends the confirmation of `serial` for the post of a blob that claims `udid` at `now`: true
    /// once, when the serial was confirmed, the confirmation stands and the UDID is the one that
    /// waited. The row is gone after it.
    pub(crate) fn spend(&self, serial: &str, udid: &str, now: i64) -> bool {
        let mut inner = self.inner();
        let spent = inner.held.get(serial).is_some_and(|row| {
            row.udid == udid && row.confirmed_until.is_some_and(|until| now < until)
        });
        if spent {
            inner.held.remove(serial);
        }
        spent
    }

    /// Whether the decline of `serial` is to be audited: the first one, and then one `window`
    /// seconds after the last audited. The table holds at most `max` serials, the oldest dropped.
    pub(crate) fn first_decline(&self, serial: &str, now: i64, window: i64, max: usize) -> bool {
        let mut inner = self.inner();
        if inner
            .declined
            .get(serial)
            .is_some_and(|last| now.saturating_sub(*last) < window)
        {
            return false;
        }
        if !inner.declined.contains_key(serial) && inner.declined.len() >= max {
            let oldest = inner
                .declined
                .iter()
                .min_by_key(|(serial, last)| (**last, (*serial).clone()))
                .map(|(serial, _)| serial.clone());
            oldest.iter().for_each(|oldest| {
                inner.declined.remove(oldest);
            });
        }
        inner.declined.insert(serial.to_owned(), now);
        true
    }
}

/// The requests each peer address made in the current minute.
#[derive(Default)]
pub(crate) struct RateLimit {
    window: Mutex<(i64, BTreeMap<IpAddr, u32>)>,
}

impl RateLimit {
    /// Counts a request from `peer` at `now`; false when it is past `per_min` this minute. A
    /// request with no address (a transport that has none) is counted together as one peer.
    pub(crate) fn allow(&self, peer: Option<IpAddr>, now: i64, per_min: u32) -> bool {
        let minute = now.div_euclid(60);
        let mut window = self.window.lock().unwrap_or_else(PoisonError::into_inner);
        if window.0 != minute {
            *window = (minute, BTreeMap::new());
        }
        let key = peer.unwrap_or(IpAddr::from([0, 0, 0, 0]));
        let seen = window.1.entry(key).or_insert(0);
        *seen = seen.saturating_add(1);
        *seen <= per_min
    }
}
