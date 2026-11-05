//! The verdicts the coordinator has counted toward an automatic rollback (design section 5.4,
//! "Replayed verdicts"): a host resends its `last_update` until it is acknowledged, so a verdict
//! is counted once per host, order id and try, however often it is resent; a new try of the same
//! order is a new verdict.
//!
//! The set is kept in memory only, per service and per version the verdicts are about, and is
//! rebuilt from the heartbeats after a restart (design section 8.2): a host that still reports
//! its verdict is counted again. Only release failures are kept
//! ([`crate::reconcile::is_release_failure_beat`]): nothing else counts. A verdict from an
//! updater that names no order (one that predates `order_id`) is keyed by its host alone, so
//! such a host counts once, its latest report standing for it. The coordinator keeps, per
//! service, only the verdicts about the desired version, and forgets a verdict an override
//! acknowledged ([`crate::acks`]): it never counts again, even once the host reports something
//! else and its ack is dropped (`keep_only`, on every heartbeat). The same report made again
//! after that is a new failure, and counts.

use std::collections::BTreeMap;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Heartbeat, OrderId, UpdateResult};

use crate::ops::FailureAck;
use crate::reconcile::is_release_failure_beat;

/// One verdict: the host, the order and the try.
type Key = (HostId, Option<OrderId>, Option<u64>);

/// The verdicts counted, by service and version.
#[derive(Clone, Debug, Default)]
pub struct Verdicts {
    seen: BTreeMap<String, BTreeMap<String, BTreeMap<Key, UpdateResult>>>,
}

impl Verdicts {
    /// Notes the verdict that `beat`, a heartbeat of `service` from the host `id`, reports, if it
    /// is a release failure. The same verdict again changes nothing.
    pub fn observe(&mut self, service: &str, id: &HostId, beat: &Heartbeat) {
        let Some(last) = beat.last_update.as_ref() else {
            return;
        };
        if !is_release_failure_beat(beat, &last.target) {
            return;
        }
        let key = (id.clone(), last.order_id.clone(), last.order_try);
        self.seen
            .entry(service.to_owned())
            .or_default()
            .entry(last.target.as_str().to_owned())
            .or_default()
            .insert(key, last.clone());
    }

    /// Forgets the verdicts of `service` about every version but `version`, and those `acked`
    /// acknowledges (the same host and the whole report).
    pub fn keep_only(&mut self, service: &str, version: &Version, acked: &[FailureAck]) {
        if let Some(versions) = self.seen.get_mut(service) {
            versions.retain(|about, _| about == version.as_str());
            for verdicts in versions.values_mut() {
                verdicts.retain(|(id, _, _), report| {
                    let node = id.to_string();
                    !acked
                        .iter()
                        .any(|ack| ack.node_id == node && ack.report == *report)
                });
            }
        }
    }

    /// The verdicts of `service` about `version`: the host of each and its report.
    pub fn of(
        &self,
        service: &str,
        version: &Version,
    ) -> impl Iterator<Item = (&HostId, &UpdateResult)> {
        self.seen
            .get(service)
            .and_then(|versions| versions.get(version.as_str()))
            .into_iter()
            .flatten()
            .map(|((id, _, _), report)| (id, report))
    }
}
