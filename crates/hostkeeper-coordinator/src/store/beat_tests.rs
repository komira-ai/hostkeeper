use hostkeeper_wire::identity::Identity;
use hostkeeper_wire::wire::{Health, Heartbeat, ServiceBeat};

use super::{HostBeat, ServiceSaid};
use crate::testutil::{beat, id, role, unhealthy, updating, version};

fn who() -> Identity {
    Identity {
        host_id: id("host-1"),
        role: role("storage"),
    }
}

/// The entry of `service` at `running` in a heartbeat's list: healthy, not updating.
fn entry(service: &str, running: &str) -> ServiceBeat {
    ServiceBeat {
        local_halt: None,
        name: service.to_owned(),
        version: version(running),
        health: Health::Healthy,
        last_update: None,
        updating: None,
        step: None,
        commit: None,
        step_secs: Default::default(),
        promotion_pending: false,
        deferred: None,
        record_try: None,
    }
}

fn said(service: &str, version: &str, counts: bool, runs: bool) -> ServiceSaid {
    ServiceSaid {
        service: service.to_owned(),
        version: version.to_owned(),
        counts,
        runs,
    }
}

/// Proves: a heartbeat is kept with its host and role as its certificate names them, its own
/// clock, the heartbeat as the coordinator read it (it reads back as the same heartbeat), and each
/// service once: the legacy top level first, then the list in its order, an entry of the list
/// named as the legacy service left out (the reader refuses a list that names a service twice, so
/// that is the one way a beat names one twice).
///
/// Catches: a row keyed by anything but the host, a beat stored in another shape than the one
/// read, and a service named twice in one beat, which would make the statement's upsert of its
/// bake touch one row twice and fail.
#[test]
fn a_heartbeat_is_kept_with_each_service_once_the_legacy_one_first() {
    let mut heartbeat = beat("1.0");
    heartbeat.sent_unix = Some(1_900_000_015);
    heartbeat.services = Some(vec![
        entry("web", "2.0"),
        entry("legacy", "9.9"),
        entry("db", "3.0"),
    ]);
    let kept = HostBeat::of(&who(), &heartbeat, Some("legacy"));
    assert_eq!(kept.host_id, "host-1");
    assert_eq!(kept.role, "storage");
    assert_eq!(kept.sent_unix, 1_900_000_015);
    assert_eq!(
        kept.services,
        vec![
            said("legacy", "1.0", true, true),
            said("web", "2.0", true, true),
            said("db", "3.0", true, true),
        ]
    );
    let read: Heartbeat = serde_json::from_slice(&kept.beat).expect("the kept beat reads");
    assert_eq!(read, heartbeat);
}

/// Proves: a beat counts toward a bake only when the service is healthy, not updating and not
/// waiting for a promotion; it says the host runs the version unless the service is absent; with
/// no legacy service the top level is no service; an agent that sends no clock is kept at 0, and a
/// clock past what the column holds at its greatest.
///
/// Catches: a bake counted on an unhealthy, updating or unpromoted beat, an order freed by an
/// absent copy of its version, and a top level kept as a service when the coordinator names none.
#[test]
fn a_beat_counts_and_runs_by_its_health_and_its_clock_is_kept_whole() {
    let mut absent = entry("absent", "4.0");
    absent.health = Health::Absent;
    let mut pending = entry("pending", "5.0");
    pending.promotion_pending = true;
    let mut sick = entry("sick", "6.0");
    sick.health = Health::Unhealthy;
    let mut moving = entry("moving", "7.0");
    moving.updating = Some(version("7.1"));
    let mut heartbeat = beat("1.0");
    heartbeat.services = Some(vec![absent, pending, sick, moving]);
    let kept = HostBeat::of(&who(), &heartbeat, None);
    assert_eq!(kept.sent_unix, 0);
    assert_eq!(
        kept.services,
        vec![
            said("absent", "4.0", false, false),
            said("pending", "5.0", false, true),
            said("sick", "6.0", false, true),
            said("moving", "7.0", false, true),
        ]
    );

    let mut top = updating(unhealthy(beat("1.0")), "1.1");
    top.sent_unix = Some(u64::MAX);
    let kept = HostBeat::of(&who(), &top, Some("legacy"));
    assert_eq!(kept.sent_unix, i64::MAX);
    assert_eq!(kept.services, vec![said("legacy", "1.0", false, true)]);
}
