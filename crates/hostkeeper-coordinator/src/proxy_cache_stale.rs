//! The index half of the package proxy's cache (`docs/design/package-proxy.md`, section 4): an
//! object that changes in place (an apt index) is served from its file while its age is under
//! [`Limits::index_ttl`], and after that revalidated with one conditional read of the origin.
//!
//! - **Fresh.** An age under the TTL is a plain [`ProxyCache::get`]: a hit, with no origin read.
//! - **Stale.** The first asker leads the revalidation: one conditional read, with the validators
//!   the object was stored with. An origin that answers 304 renews the object (its fetch time and
//!   its sidecar) and the asker is answered from the file. An origin that answers 200 has the new
//!   object forwarded to the asker and written to the cache, which replaces the old one when it
//!   commits; the old one is forgotten as the replacement begins, so that no asker is given an
//!   index older than its TTL while the new one is being written. A 404 forgets the object and is
//!   the asker's answer. Any other failure is the asker's (an index past its TTL is not served
//!   from the cache when the origin is down: a stale index could hide a release).
//! - **A crowd.** The other askers of the key wait for the revalidation at most
//!   [`Limits::wait`] and then ask again; a waiter whose wait ended first, or whose fill could not
//!   be stored, is passed through to the origin on its own. A failed revalidation lets the next
//!   waiter lead its own.
//! - **A range of a stale object** is answered whole when the origin has a newer one: the
//!   revalidation fills the cache, and a whole `200` is a valid answer to a range request.

use tokio::sync::watch;

use super::{
    Answer, ByteRange, Condition, Done, Entry, FetchError, Fetched, Flight, Key, Limits, Meta,
    Origin, ProxyCache, ProxyError, Request, Slot,
};

/// What an asker of an index does.
enum Refresh {
    /// There is no stored object, or it is fresh: a plain [`ProxyCache::get`].
    Fresh,
    /// The asker revalidates; the object read was committed at the tick.
    Lead(Flight, u64, Meta),
    /// Another asker revalidates.
    Wait(watch::Receiver<Done>),
}

impl ProxyCache {
    /// The time the cache keeps, in unix seconds.
    #[must_use]
    pub fn now(&self) -> u64 {
        self.shared.clock.now_unix()
    }

    /// What the cache stores of the committed object `key`, if there is one. It is not a use of
    /// the object.
    #[must_use]
    pub fn peek(&self, key: &Key) -> Option<Meta> {
        self.shared
            .state()
            .entries
            .get(&key.digest())
            .map(|entry| entry.meta.clone())
    }

    /// Whether the committed `meta` is older than the index TTL of `limits`.
    #[must_use]
    pub fn is_stale(&self, meta: &Meta, limits: &Limits) -> bool {
        self.now().saturating_sub(meta.fetched_at) >= limits.index_ttl.as_secs()
    }

    /// The answer for the index `key`, or for `range` of it: [`ProxyCache::get`] while the stored
    /// object is fresh or missing, and the revalidation of the module's text when it is stale.
    ///
    /// # Errors
    /// [`ProxyError`]: as [`ProxyCache::get`]; also [`FetchError::NotFound`] when the origin no
    /// longer has the file.
    pub async fn get_index(
        &self,
        key: &Key,
        range: Option<ByteRange>,
        origin: &Origin,
        limits: &Limits,
    ) -> Result<Answer, ProxyError> {
        let digest = key.digest();
        loop {
            match self.claim_refresh(&digest, limits) {
                Refresh::Fresh => return self.get(key, range, origin, limits).await,
                Refresh::Lead(flight, born, meta) => {
                    return self
                        .revalidate(key, range, (flight, born, meta), origin, limits)
                        .await;
                }
                Refresh::Wait(mut done) => {
                    let ended = tokio::time::timeout(
                        limits.wait,
                        done.wait_for(|done| *done != Done::Pending),
                    )
                    .await;
                    let pass = ended.map_or(true, |ended| {
                        ended.is_ok_and(|done| *done == Done::Uncached)
                    });
                    if pass {
                        return self.pass(key, range, origin, limits).await;
                    }
                }
            }
        }
    }

    /// Makes the asker the leader of the revalidation of the object `digest` when it is stale, or
    /// gives it the flight to wait on, or says there is nothing to revalidate. One step under the
    /// lock, so that no renewal or replacement lands between the look and the claim.
    fn claim_refresh(&self, digest: &str, limits: &Limits) -> Refresh {
        let now = self.now();
        let mut state = self.shared.state();
        let (born, meta) = match state.entries.get(digest) {
            Some(Entry { meta, born, .. })
                if now.saturating_sub(meta.fetched_at) >= limits.index_ttl.as_secs() =>
            {
                (*born, meta.clone())
            }
            _ => return Refresh::Fresh,
        };
        if let Some(flight) = state.flights.get(digest) {
            return Refresh::Wait(flight.subscribe());
        }
        state
            .flights
            .insert(digest.to_owned(), watch::channel(Done::Pending).0);
        Refresh::Lead(
            Flight {
                shared: self.shared.clone(),
                digest: digest.to_owned(),
                finished: false,
            },
            born,
            meta,
        )
    }

    /// The revalidation the asker leads: one conditional read, and then the file renewed or the
    /// new body filled.
    async fn revalidate(
        &self,
        key: &Key,
        range: Option<ByteRange>,
        (flight, born, meta): (Flight, u64, Meta),
        origin: &Origin,
        limits: &Limits,
    ) -> Result<Answer, ProxyError> {
        let digest = key.digest();
        let slot = self.slot(limits)?;
        let read = origin(Request {
            key: key.clone(),
            range: None,
            condition: Some(Condition::of(&meta)),
        })
        .await;
        let fetched = match read {
            Ok(fetched) => fetched,
            Err(why) => {
                if why == FetchError::NotFound {
                    self.shared.forget(&digest, born);
                }
                return Err(why.into());
            }
        };
        if fetched.unchanged {
            self.renew(key, born);
            drop(slot);
            flight.finish(Done::Committed);
            return match self.hit(&digest, range)? {
                Some(answer) => Ok(answer),
                None => self.get(key, range, origin, limits).await,
            };
        }
        Ok(self.replace(key, born, flight, fetched, slot, limits))
    }

    /// The fill of a changed index: the old object is forgotten, and the new body streamed and
    /// stored.
    fn replace(
        &self,
        key: &Key,
        born: u64,
        flight: Flight,
        fetched: Fetched,
        slot: Slot,
        limits: &Limits,
    ) -> Answer {
        self.shared.forget(&key.digest(), born);
        self.start_fill(key, flight, fetched, slot, limits)
    }

    /// Records that the origin confirmed the object `key` committed at `born`: its fetch time is
    /// now, in memory and in the sidecar.
    fn renew(&self, key: &Key, born: u64) {
        let now = self.now();
        let n = self
            .shared
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut state = self.shared.state();
        let Some(entry) = state.entries.get_mut(&key.digest()) else {
            return;
        };
        if entry.born != born {
            return;
        }
        entry.meta.fetched_at = now;
        if let Err(why) = self.shared.layout.rewrite_meta(key, &entry.meta, n) {
            // The renewal still holds in memory; a restart reads the older time and revalidates.
            tracing::warn!(error = %why, "the proxy cache could not rewrite a sidecar");
        }
    }
}

#[cfg(test)]
#[path = "proxy_cache_stale_tests.rs"]
mod tests;
