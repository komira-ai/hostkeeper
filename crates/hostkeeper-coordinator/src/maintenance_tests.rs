//! The maintenance report ([`crate::maintenance`]): which reports are kept, how a heartbeat is
//! settled before it is answered, and how a kept report is shown. The reports as a gRPC heartbeat
//! carries them and as `Hosts.Get` and `Hosts.List` answer them are in `grpc/tests/maintenance.rs`.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_wire::wire::Heartbeat;
use serde_json::{Value, json};

use super::{
    AHEAD_SECS, FACT, FEATURE, Kept, STALE_AFTER_SECS, decode, kept_of, show_maintenance, text_of,
};
use crate::fleet::{Fleet, Node};
use crate::http::{BeatCaller, beat};
use crate::marks_support::who;
use crate::testutil::{OPERATOR, Rig, id};

/// The coordinator's Unix time in the display tests.
const NOW: u64 = 1_800_000_000;

/// A report of an Ubuntu host with `due` security updates due, last looked at `looked`.
pub(crate) fn report(due: usize, looked: u64) -> agent::MaintenanceReport {
    agent::MaintenanceReport {
        host: Some(agent::MaintenanceHost {
            os_name: "ubuntu".to_owned(),
            os_version: "24.04".to_owned(),
            ..agent::MaintenanceHost::default()
        }),
        due: (0..due)
            .map(|n| agent::DueItem {
                kind: agent::MaintenanceKind::Linux as i32,
                from: "1.0".to_owned(),
                to: "1.1".to_owned(),
                label: format!("package-{n}"),
                security: true,
                ..agent::DueItem::default()
            })
            .collect(),
        looked_unix: looked,
        ..agent::MaintenanceReport::default()
    }
}

/// The JSON heartbeat of a host that announces `features` and carries `maintenance`.
fn body(features: &[&str], maintenance: Option<&str>) -> Value {
    let mut announced = vec!["heartbeat-v2"];
    announced.extend(features);
    let mut value = json!({
        "platform": "linux-x86_64",
        "version": "1.0",
        "health": "healthy",
        "agent_features": announced,
    });
    if let Some(text) = maintenance {
        value["maintenance"] = text.into();
    }
    value
}

/// Sends `value` as the heartbeat of the host `host` through the code both routes share.
fn send(rig: &Rig, host: &str, value: &Value) {
    let caller = BeatCaller {
        identity: who(host, "storage"),
        retiring: false,
    };
    let bytes = serde_json::to_vec(value).expect("JSON");
    beat(&rig.coordinator, caller, &bytes, None).expect("answered");
}

/// The report the fleet keeps of `host`.
fn kept_of_host(rig: &Rig, host: &str) -> Option<Kept> {
    let fleet = rig.coordinator.lock_fleet();
    fleet
        .node(&id(host))
        .and_then(|node| node.maintenance.clone())
}

/// How many reports the status counts as dropped.
fn dropped(rig: &Rig) -> Option<u64> {
    let status = rig.coordinator.status(Some(OPERATOR)).expect("status");
    status.extras_dropped.get(FACT).copied()
}

/// A heartbeat of an agent that announces `features` and carries the text `maintenance`.
fn heartbeat(features: &[&str], maintenance: Option<String>) -> Heartbeat {
    let mut beat = crate::testutil::beat("1.0");
    beat.agent_features = features.iter().map(|name| (*name).to_owned()).collect();
    beat.maintenance = maintenance;
    beat
}

/// Proves: the feature's name is `maintenance-v1` and the coordinator advertises it, so a
/// daemon's announcement is agreed.
///
/// Catches: a feature read and kept but not advertised, which no daemon would then announce.
#[test]
fn the_feature_is_advertised() {
    assert_eq!(FEATURE, "maintenance-v1");
    assert!(crate::features::FEATURES.contains(&FEATURE));
}

/// Catches: `maintenance-v1` advertised and not acted on: the report of a host that announces it
/// is kept in its node, and the same report of a host that does not announce it is not.
pub(crate) fn maintenance_v1_is_acted_on() {
    let rig = Rig::new("maintenance-acted-on");
    let text = text_of(&report(1, NOW));
    send(&rig, "d1", &body(&[FEATURE], Some(&text)));
    assert_eq!(kept_of_host(&rig, "d1"), Some(Kept(report(1, NOW))));
    send(&rig, "d2", &body(&[], Some(&text)));
    assert_eq!(kept_of_host(&rig, "d2"), None);
}

/// Proves: only a report that decodes and keeps every bound, from an agent that announces the
/// feature, is kept: no announcement, no report, text that is not base64, base64 that is not a
/// report, and a report over a bound (13 due items) are each none; the text a report is sent as
/// reads back as the same report.
///
/// Catches: a report kept from an agent that did not announce the feature, one that breaks a
/// bound kept, and an encoding that does not read back.
#[test]
fn a_report_is_kept_only_when_it_is_announced_and_whole() {
    let whole = report(2, NOW);
    let text = text_of(&whole);
    assert_eq!(decode(&text), Some(Kept(whole.clone())));
    assert_eq!(
        kept_of(&heartbeat(&[FEATURE], Some(text.clone()))),
        Some(Kept(whole))
    );
    assert_eq!(kept_of(&heartbeat(&["halt-v1"], Some(text))), None);
    assert_eq!(kept_of(&heartbeat(&[FEATURE], None)), None);
    assert_eq!(
        kept_of(&heartbeat(&[FEATURE], Some("not base64!".to_owned()))),
        None
    );
    let no_report = STANDARD.encode([0xff]);
    assert_eq!(kept_of(&heartbeat(&[FEATURE], Some(no_report))), None);
    let too_many = text_of(&report(13, NOW));
    assert_eq!(kept_of(&heartbeat(&[FEATURE], Some(too_many))), None);
    let at_the_bound = text_of(&report(12, NOW));
    assert!(kept_of(&heartbeat(&[FEATURE], Some(at_the_bound))).is_some());
}

/// Proves: a report is due when it lists an item or the daemon found more than it listed, and not
/// otherwise.
///
/// Catches: a host with 20 updates due, 12 listed and 8 left out, read as due only by its list,
/// and an empty report read as due.
#[test]
fn a_report_is_due_when_it_lists_or_leaves_out_an_item() {
    assert!(!Kept(report(0, NOW)).is_due());
    assert!(Kept(report(1, NOW)).is_due());
    let left_out = agent::MaintenanceReport {
        due_left_out: 8,
        ..report(0, NOW)
    };
    assert!(Kept(left_out).is_due());
    assert_eq!(Kept(report(1, NOW)).report().due.len(), 1);
    assert_eq!(Kept(report(1, NOW)).text(), text_of(&report(1, NOW)));
}

/// Proves: a heartbeat is settled before it is answered: a report of an agent that announces the
/// feature is kept; one that does not read, or breaks a bound, is counted as the dropped fact
/// `maintenance` and the report kept before stays; a heartbeat with none keeps the last one; one
/// from an agent that does not announce the feature carries and keeps none (and counts nothing),
/// and the report is not brought back by a later heartbeat that carries none.
///
/// Catches: a bad report that replaces a good one, a heartbeat without a report that forgets it,
/// a report kept for an agent that did not announce the feature, and a bad report not counted.
#[test]
fn a_heartbeat_keeps_the_last_report_that_was_whole() {
    let rig = Rig::new("maintenance-settle");
    let first = text_of(&report(1, NOW));
    let second = text_of(&report(2, NOW + 60));
    send(&rig, "d1", &body(&[FEATURE], Some(&first)));
    assert_eq!(kept_of_host(&rig, "d1"), Some(Kept(report(1, NOW))));
    assert_eq!(dropped(&rig), None);

    send(&rig, "d1", &body(&[FEATURE], None));
    assert_eq!(kept_of_host(&rig, "d1"), Some(Kept(report(1, NOW))));
    assert_eq!(dropped(&rig), None);

    send(&rig, "d1", &body(&[FEATURE], Some("not base64!")));
    assert_eq!(kept_of_host(&rig, "d1"), Some(Kept(report(1, NOW))));
    assert_eq!(dropped(&rig), Some(1));

    let too_many = text_of(&report(13, NOW + 120));
    send(&rig, "d1", &body(&[FEATURE], Some(&too_many)));
    assert_eq!(kept_of_host(&rig, "d1"), Some(Kept(report(1, NOW))));
    assert_eq!(dropped(&rig), Some(2));

    send(&rig, "d1", &body(&[FEATURE], Some(&second)));
    assert_eq!(kept_of_host(&rig, "d1"), Some(Kept(report(2, NOW + 60))));

    send(&rig, "d1", &body(&[], Some(&first)));
    assert_eq!(kept_of_host(&rig, "d1"), None);
    send(&rig, "d1", &body(&[FEATURE], None));
    assert_eq!(kept_of_host(&rig, "d1"), None);
    assert_eq!(dropped(&rig), Some(2));

    send(&rig, "d2", &body(&[FEATURE], Some("not base64!")));
    assert_eq!(kept_of_host(&rig, "d2"), None);
    assert_eq!(dropped(&rig), Some(3));
}

/// A fleet that holds the host `d1`, which announces `features` and whose node keeps `kept`.
fn fleet_of(features: &[&str], kept: Option<Kept>) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, crate::fleet::QUIET_START);
    let node = Node {
        role: "storage".to_owned(),
        features: features.iter().map(|name| (*name).to_owned()).collect(),
        agent: None,
        platform: hostkeeper_wire::Platform::LinuxX86_64,
        seen: Duration::ZERO,
        announced: Default::default(),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root: crate::trusted_root::TrustedRootSaid::default(),
        skewed_beats: 0,
        maintenance: kept,
    };
    fleet.note_node(id("d1"), node, 0);
    fleet
}

/// What `show_maintenance` makes of host `host` of `fleet` at `NOW`.
fn shown(fleet: &Fleet, host: &str) -> operator::Host {
    let mut shown = operator::Host::default();
    show_maintenance(fleet, &id(host), &mut shown, NOW);
    shown
}

/// Proves: a kept report is shown with the seconds since its last look and `maintenance_stale`
/// exactly when its last look is 0 or more than 7200 seconds before the coordinator's clock: a
/// look 60 s ago is not stale and has its age, one 7200 s ago is not stale, one 7201 s ago is
/// stale with its age, a look of 0 is stale with no age, a look up to 300 s ahead of the clock
/// has age 0 and is not stale, and one further ahead has age 0 and is stale.
///
/// Catches: an empty `due` of an old report read as nothing due, a stale limit one second off, an
/// age made of a look of 0 (decades), an age that wraps when the daemon's clock is ahead, and a
/// daemon whose clock runs far ahead shown fresh for as long as it runs ahead.
#[test]
fn a_kept_report_is_shown_with_its_age_and_whether_it_is_stale() {
    assert_eq!((STALE_AFTER_SECS, AHEAD_SECS), (7200, 300));
    for (looked, age, stale) in [
        (NOW - 60, Some(60), false),
        (NOW - 7200, Some(7200), false),
        (NOW - 7201, Some(7201), true),
        (0, None, true),
        (NOW + 30, Some(0), false),
        (NOW + 300, Some(0), false),
        (NOW + 301, Some(0), true),
        (NOW + 86_400, Some(0), true),
    ] {
        let fleet = fleet_of(&[FEATURE], Some(Kept(report(1, looked))));
        let host = shown(&fleet, "d1");
        assert_eq!(host.maintenance, Some(report(1, looked)), "look {looked}");
        assert_eq!(host.maintenance_look_age_secs, age, "look {looked}");
        assert_eq!(host.maintenance_stale, stale, "look {looked}");
    }
}

/// Proves: nothing is shown for a host that keeps no report, and for a host the fleet does not
/// hold.
///
/// Catches: a stale flag, or an age, on a host that has no report.
#[test]
fn nothing_is_shown_without_a_kept_report() {
    let none = fleet_of(&[FEATURE], None);
    assert_eq!(shown(&none, "d1"), operator::Host::default());
    assert_eq!(shown(&none, "d2"), operator::Host::default());
}
