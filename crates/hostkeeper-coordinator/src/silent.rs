//! A host silent in the middle of an update: how long its last word of an update running holds
//! anything, and the alert that names it once that word holds nothing.
//!
//! An order the coordinator sent ages out ([`crate::fleet::ORDER_AGE`]) while its host still
//! heartbeats; the host's own last heartbeat does not, and nor does the order of a host gone
//! silent, which may have heard it and started ([`crate::fleet::Fleet::is_ordered`]). A host that
//! died, or lost its route to the coordinator, while its last heartbeat said it was updating (or
//! waiting for its promotion), or after it was ordered, would keep its service's
//! rollout live ([`crate::live`]) and its update slot ([`crate::stages`]) for as long as it stays
//! silent, and every other service would wait `rollout_elsewhere` behind it, the release that
//! fixes the fault included. So that word, and that order, count only while the host has been
//! silent for less than [`SILENT_UPDATE_AFTER`]: from then on the host holds no update slot and no
//! rollout place for it, and `silent_while_updating` names the host, the service and the version
//! ([`silent_updates`]). A failure the host last reported holds what it held before
//! ([`Host::holds_slot`]): its slot of its own service is the operator's to free, and it never
//! made a rollout live.
//!
//! Nothing of the host is forgotten, and nothing is sent to it: a silent host is not live, so no
//! plan counts it and no decision orders it, and an order only goes out in the reply to a
//! heartbeat. An update still running on it runs on: the coordinator stops nothing.
//!
//! A host that comes back is read from its first heartbeat, as any other host: if that beat
//! still says it updates, the host holds its slot and its service's rollout place again from
//! that beat on. A rollout that took the place meanwhile is not stopped either: both run to
//! their end, and no further rollout starts until fewer are live than `--live-rollouts` lets be.

use std::collections::BTreeSet;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, Outcome};

use crate::alerts::{Alert, Kind, Said, Subject};
use crate::fleet::{Fleet, Host};

/// How long a host may be silent while its last word is an update running before that word
/// holds nothing ([`Host::is_lost`]): an hour. The longest `verify_deadline_secs` a hook of the
/// farm's services declares is 2500 s, after which the updater on the host ends the update by
/// itself; the other 1100 s are the margin, over seventy heartbeat intervals of 15 s and nine
/// times [`crate::fleet::ORDER_AGE`]. A host silent this long is not coming back from a lost
/// reply or a restart of its daemon.
pub const SILENT_UPDATE_AFTER: Duration = Duration::from_secs(3600);

impl Host {
    /// Whether the host has been silent for [`SILENT_UPDATE_AFTER`] or more at `now`: its last
    /// word of an update running holds nothing.
    pub(crate) fn is_lost(&self, now: Duration) -> bool {
        now.saturating_sub(self.seen) >= SILENT_UPDATE_AFTER
    }

    /// Whether the host's last word is an update of the service running, and still holds at
    /// `now`: the host is not lost ([`Host::is_lost`]).
    pub(crate) fn is_updating(&self, now: Duration) -> bool {
        self.beat.updating.is_some() && !self.is_lost(now)
    }

    /// Whether the host's last word is that it waits for the promotion of its update, and still
    /// holds at `now`.
    pub(crate) fn is_promoting(&self, now: Duration) -> bool {
        self.beat.promotion_pending && !self.is_lost(now)
    }

    /// [`Host::holds_slot`] at `now`: an update running, or its promotion pending, holds the
    /// slot only while the host is not lost; a failure, not healthy, holds it whatever the
    /// silence.
    pub(crate) fn holds_slot_at(&self, now: Duration) -> bool {
        self.is_updating(now) || self.is_promoting(now) || self.failed_unhealthy()
    }

    /// Whether the host's last update failed and it is neither healthy nor absent: an install
    /// rolled back to nothing (absent) holds no slot, one halted after its switch (installed,
    /// unhealthy) does.
    pub(crate) fn failed_unhealthy(&self) -> bool {
        let failed = self
            .beat
            .last_update
            .as_ref()
            .is_some_and(|r| r.outcome == Outcome::Failed);
        failed && !matches!(self.beat.health, Health::Healthy | Health::Absent)
    }
}

/// `silent_while_updating`: each host of a service of `packages`, counted by the service or not
/// ([`Fleet::occupants_of`]), whose last word of it is an update running or its promotion
/// pending and that has been silent for [`SILENT_UPDATE_AFTER`] or more at `now`, and each host
/// silent as long since it was ordered an update of the service and did not report it
/// ([`Fleet::lost_orders`]). The version is the update's target, or, for a promotion, the version
/// the host runs. It stands until the host heartbeats again, or the fleet forgets its word of the
/// service or its order.
pub(crate) fn silent_updates(
    fleet: &Fleet,
    packages: &BTreeSet<String>,
    now: Duration,
) -> Vec<Alert> {
    packages
        .iter()
        .flat_map(|name| {
            let reported = fleet.occupants_of(name).filter_map(move |(id, host)| {
                let version = match (&host.beat.updating, host.beat.promotion_pending) {
                    (Some(target), _) => target,
                    (None, true) => &host.beat.version,
                    (None, false) => return None,
                };
                if !host.is_lost(now) {
                    return None;
                }
                let silent = now.saturating_sub(host.seen).as_secs();
                let said = format!("its last word was an update of {name} to {version}");
                Some(alert(name, id, version, silent, &said))
            });
            let ordered = fleet
                .lost_orders(name, now)
                .map(move |(id, version, silent)| {
                    let said = format!(
                        "it was ordered an update of {name} to {version} and never reported it"
                    );
                    alert(name, id, version, silent.as_secs(), &said)
                });
            let mut alerts: Vec<Alert> = reported.collect();
            for one in ordered {
                if !alerts.iter().any(|a| a.subject.host == one.subject.host) {
                    alerts.push(one);
                }
            }
            alerts
        })
        .collect()
}

/// The `silent_while_updating` alert of the host `id`, silent `silent` seconds, about an update of
/// `name` to `version` that `said` tells.
fn alert(name: &str, id: &HostId, version: &Version, silent: u64, said: &str) -> Alert {
    Alert {
        subject: Subject {
            service: name.to_owned(),
            host: id.to_string(),
            version: version.to_string(),
            ..Subject::of(Kind::SilentWhileUpdating)
        },
        message: format!(
            "{id} has not heartbeated for {silent} s and {said}: after {} s of silence that \
             word holds no update slot and no rollout place, so the other rollouts go on. The \
             update may still be running on the host: check the host before acting on it",
            SILENT_UPDATE_AFTER.as_secs()
        ),
        said: Said::default(),
    }
}
