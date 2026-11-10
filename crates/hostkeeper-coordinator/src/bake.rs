//! Baking: how many healthy beats a host has sent at the version it runs.
//!
//! A host that has just updated bakes until it has beaten healthy
//! [`crate::services::ServiceDef::bake_beats`] times (`docs/design/host-updates.md`, section
//! 5.3). The stage of the service's update plan the host is in is not done until every host of it
//! has baked, so the next stage does not start before, and the service's rollout stays live for
//! the others ([`crate::stages`]); a baking host holds no update slot of the role's
//! `max_updating`. The count is kept in memory, per service and host, beside the heartbeats
//! ([`crate::fleet::Fleet`]): a restart of the coordinator forgets it, and a host at the
//! desired version bakes again. So does a host that was silent for [`DOWN_AFTER`] or more
//! between two beats: the count only grows on beats, and a host that vanished is not
//! proven by the beats from before it went.
//!
//! A bake after a restart proves the host again, but it is no update: a host the coordinator
//! first heard, since it started, already settled at a version ([`Found`]) did not update under
//! it, so its bake at that version makes no rollout live ([`crate::live`]) until the coordinator
//! sees it move (another version, an update, a promotion, an order).

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Heartbeat};

use crate::fleet::DOWN_AFTER;

/// The most beats a definition may ask a host to bake for.
pub const MAX_BAKE_BEATS: u32 = 100;

/// The beats a host has sent healthy in a row at one version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bake {
    /// The version the host ran in those beats.
    pub target: Version,
    /// How many.
    pub beats: u32,
    /// When the last of them came.
    pub at: Duration,
}

impl Bake {
    /// The count after `beat`, which came at `now`, given the count before it: one more when
    /// the beat is healthy, not updating and not waiting for a promotion at the same version
    /// as before, one at a new version or after a silence of [`DOWN_AFTER`] or more, and none
    /// (`None`) for any other beat.
    #[must_use]
    pub fn after(before: Option<&Self>, beat: &Heartbeat, now: Duration) -> Option<Self> {
        let counts =
            beat.health == Health::Healthy && beat.updating.is_none() && !beat.promotion_pending;
        counts.then(|| Self {
            target: beat.version.clone(),
            beats: before
                .filter(|b| b.target == beat.version)
                .filter(|b| now.saturating_sub(b.at) < DOWN_AFTER)
                .map_or(1, |b| b.beats.saturating_add(1)),
            at: now,
        })
    }
}

/// The version each host ran, per service, when the coordinator first heard it since it started,
/// for the hosts it has not seen move since: settled there (not updating, no promotion pending,
/// not absent), at the same version in every beat after, and given no order. Kept in memory only,
/// beside the bakes: what a restart cannot tell is whether such a host updated just before it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Found(BTreeMap<(String, HostId), Version>);

impl Found {
    /// Notes `beat`, the host's word of the service in `key`; `first` when the fleet kept no word
    /// of it before (the first since the coordinator started, or since it forgot the host).
    pub(crate) fn observe(&mut self, key: (String, HostId), beat: &Heartbeat, first: bool) {
        let settled =
            beat.updating.is_none() && !beat.promotion_pending && beat.health != Health::Absent;
        if first && settled {
            self.0.insert(key, beat.version.clone());
        } else if !settled || self.0.get(&key).is_some_and(|v| *v != beat.version) {
            self.0.remove(&key);
        }
    }

    /// Forgets the host and service of `key`: it was ordered, or the fleet forgot it.
    pub(crate) fn forget(&mut self, key: &(String, HostId)) {
        self.0.remove(key);
    }

    /// Forgets the host `id` for every service.
    pub(crate) fn forget_host(&mut self, id: &HostId) {
        self.0.retain(|(_, host), _| host != id);
    }

    /// Whether the host `id` was found settled at `version` of `service` and has not moved since.
    #[must_use]
    pub(crate) fn at(&self, service: &str, id: &HostId, version: &Version) -> bool {
        self.0
            .get(&(service.to_owned(), id.clone()))
            .is_some_and(|v| v == version)
    }
}
