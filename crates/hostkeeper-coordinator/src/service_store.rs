//! The care of the store's link at each heartbeat (`docs/design/coordinator-storage.md`,
//! section 5), kept apart so that `service.rs` stays short.
//!
//! While the backend's link is lost the coordinator orders nothing, as in its quiet start (the
//! fleet says so, `Fleet::store_lost`), refuses every storage-first change
//! `store_failed`, keeps memory-first changes pending, and still answers heartbeats and keeps the
//! fleet view. Each heartbeat asks the backend to make the link again, on its own thread and never
//! under the fleet's lock ([`crate::store::Backend::revive`]). Once it is back, a commit whose
//! outcome was not known is settled and, when it landed, taken into memory; then what is pending
//! is committed. When another coordinator led the database meanwhile, the pending changes are
//! dropped knowingly and the coordinator stops.

use std::time::Duration;

use super::Coordinator;
use crate::fleet::Fleet;
use crate::holds::marks_of;
use crate::repositories::Registry;
use crate::store::Health;

impl Coordinator {
    /// Refreshes the operator's marks the decisions of `fleet` read, and whether the store's link
    /// is lost, which a refresh of the marks alone keeps.
    pub(crate) fn refresh_marks(&self, fleet: &mut Fleet) {
        fleet.store_lost = self.ops.health() != Health::Up;
        fleet.set_marks(self.ops.read(marks_of));
        self.keep_releases(fleet);
    }

    /// Refreshes the releases the picks of `fleet` read ([`crate::picks`]): those of every
    /// service's desired version and of every standing force's version, and the channels of the
    /// registry's private repositories.
    pub(crate) fn keep_releases(&self, fleet: &mut Fleet) {
        let forced = self.ops.read(|state| {
            state
                .services
                .iter()
                .filter_map(|(name, marks)| {
                    Some((name.clone(), marks.force.as_ref()?.version.clone()))
                })
                .collect()
        });
        fleet.picks.keep(self.desired.releases_of(forced));
        fleet
            .picks
            .keep_private(self.desired.with_repositories(Registry::private_channels));
    }

    /// Tends the store's link at a heartbeat at the coordinator's time `now`: asks a lost link to
    /// be made again, takes a settled commit into memory, deposes a store another coordinator led,
    /// and, while the link is up, commits what is pending (every memory-first change whose commit
    /// failed is committed again at every heartbeat until it lands).
    pub(crate) fn tend_store(&self, now: Duration) {
        let backend = self.ops.backend();
        backend.revive();
        // Read before the landings: a revival makes its landings known before it is up, so a
        // link found up has no landing this heartbeat has not taken.
        let health = backend.health();
        for landing in backend.take_landings() {
            if landing.landed {
                self.desired.take_landed(&landing);
                self.ops.take_landed(&landing);
                self.verdicts.take_landed(&landing);
            }
            tracing::warn!(
                alert = "store_unreachable",
                number = landing.number,
                landed = landing.landed,
                "the commit whose outcome was not known is settled: memory takes it when it landed"
            );
        }
        match health {
            Health::Up => self.ops.retry_unwritten(now),
            Health::Lost => {}
            Health::Deposed { found } => self.ops.depose(found),
        }
    }

    /// Waits until the store is deposed: another coordinator led its database while this one's
    /// link was lost. Answers the epoch this coordinator held and the one found.
    pub async fn deposed(&self) -> (u64, u64) {
        let found = self.ops.deposed().await;
        (self.ops.backend().epoch().unwrap_or_default(), found)
    }
}
