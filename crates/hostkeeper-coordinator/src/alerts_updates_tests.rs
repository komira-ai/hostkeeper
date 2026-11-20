//! The alert `updates_due`: one warning for each host group that has hosts with updates due.

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::{Health, Heartbeat};

use crate::alerts::{
    Alert, At, Book, EnrollmentFacts, Kind, Observed, Subject, Thresholds, alert_proto, derive,
    rows,
};
use crate::fleet::{Fleet, Node, QUIET_START};
use crate::maintenance::{FEATURE, Kept, kept_of, text_of};
use crate::ops::OpsState;
use crate::testutil::{LEGACY, Log, beat, id, kbf_services, secs};

/// The unix time of the coordinator's clock at 0.
const UNIX: u64 = 1_800_000_000;

/// A report that lists `due` due items and says `left_out` more were left out.
fn report(due: usize, left_out: u32) -> agent::MaintenanceReport {
    agent::MaintenanceReport {
        due: vec![agent::DueItem::default(); due],
        due_left_out: left_out,
        ..agent::MaintenanceReport::default()
    }
}

/// The report the coordinator keeps of a beat that announces the feature and carries `report`.
fn kept(report: &agent::MaintenanceReport) -> Kept {
    let heartbeat = Heartbeat {
        agent_features: vec![FEATURE.to_owned()],
        maintenance: Some(text_of(report)),
        ..beat("1.0")
    };
    kept_of(&heartbeat).expect("a report that keeps every bound")
}

/// A host heard at 0 whose daemon announces the feature and whose kept report is `report`.
fn node(report: Option<&agent::MaintenanceReport>) -> Node {
    Node {
        role: "daemon".to_owned(),
        features: vec![FEATURE.to_owned()],
        agent: None,
        platform: Platform::LinuxX86_64,
        seen: secs(0),
        announced: BTreeMap::from([(LEGACY.to_owned(), Health::Healthy)]),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root: crate::trusted_root::TrustedRootSaid::default(),
        skewed_beats: 0,
        maintenance: report.map(kept),
    }
}

/// A fleet of `hosts`, each noted as heard.
fn fleet_of(hosts: Vec<(&str, Node)>) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    for (host, node) in hosts {
        fleet.note_node(id(host), node, UNIX);
    }
    fleet
}

/// A fleet of d1 to d`count`, every host announcing the feature and keeping `due`.
fn all_due(count: usize, due: &agent::MaintenanceReport) -> Fleet {
    fleet_of(
        (1..=count)
            .map(|n| (["d1", "d2", "d3", "d4"][n - 1], node(Some(due))))
            .collect(),
    )
}

/// The `updates_due` alerts of `fleet` when the enrollment store puts each host of `groups` in
/// its group (the empty name: in none); without `groups`, of a coordinator that holds no group.
fn updates(fleet: &Fleet, groups: Option<&[(&str, &str)]>) -> Vec<Alert> {
    let facts = EnrollmentFacts {
        groups: groups.map(|groups| {
            groups
                .iter()
                .map(|(host, group)| (id(host), (*group).to_owned()))
                .collect()
        }),
        ..EnrollmentFacts::default()
    };
    derive(
        fleet,
        &OpsState::default(),
        &kbf_services(),
        &facts,
        At {
            now: secs(10),
            unix: UNIX + 10,
        },
        &Thresholds::default(),
    )
    .into_iter()
    .filter(|alert| alert.subject.kind == Kind::UpdatesDue)
    .collect()
}

/// The alert `updates_due` of `group` with `message`.
fn alert(group: &str, message: &str) -> Alert {
    Alert {
        subject: Subject {
            group: group.to_owned(),
            ..Subject::of(Kind::UpdatesDue)
        },
        message: message.to_owned(),
    }
}

/// Proves: hosts with updates due are counted in one alert for each host group, in group order,
/// each with the exact count and text, "1 host" for one and "N hosts" for more; a host whose report
/// lists nothing due is not counted, and a group with only such hosts has no alert.
#[test]
fn hosts_with_updates_due_are_counted_in_one_alert_for_each_group() {
    let due = report(2, 0);
    let nothing = report(0, 0);
    let fleet = fleet_of(vec![
        ("d1", node(Some(&due))),
        ("d2", node(Some(&due))),
        ("d3", node(Some(&due))),
        ("d4", node(Some(&nothing))),
    ]);
    assert_eq!(
        updates(
            &fleet,
            Some(&[
                ("d1", "mac-ci"),
                ("d2", "mac-ci"),
                ("d3", "linux-ci"),
                ("d4", "mac-ci")
            ])
        ),
        vec![
            alert("linux-ci", "Updates due on 1 host of linux-ci"),
            alert("mac-ci", "Updates due on 2 hosts of mac-ci"),
        ]
    );
    assert_eq!(
        updates(
            &fleet,
            Some(&[
                ("d1", "mac-ci"),
                ("d2", "mac-ci"),
                ("d3", "mac-ci"),
                ("d4", "linux-ci")
            ])
        ),
        vec![alert("mac-ci", "Updates due on 3 hosts of mac-ci")]
    );
}

/// Proves: hosts the enrollment store puts in no group are counted in one alert that says so, apart
/// from the alert of the group the other hosts are in; a coordinator that holds no group counts
/// every host in none.
#[test]
fn hosts_in_no_group_give_one_alert_that_says_so() {
    let fleet = all_due(3, &report(1, 0));
    assert_eq!(
        updates(&fleet, Some(&[("d1", ""), ("d2", ""), ("d3", "mac-ci")])),
        vec![
            alert("", "Updates due on 2 hosts in no host group"),
            alert("mac-ci", "Updates due on 1 host of mac-ci"),
        ]
    );
    assert_eq!(
        updates(&all_due(1, &report(1, 0)), None),
        vec![alert("", "Updates due on 1 host in no host group")]
    );
}

/// Proves: a host that keeps no report counts nothing, and a due host the groups read before the
/// fleet do not name (its first report came in between the two reads) is not counted, rather than
/// counted in no group, until they name it.
#[test]
fn a_host_without_a_report_or_a_group_read_counts_nothing() {
    let due = report(1, 0);
    let fleet = fleet_of(vec![("d1", node(None)), ("d2", node(Some(&due)))]);
    assert_eq!(
        updates(&fleet, None),
        vec![alert("", "Updates due on 1 host in no host group")]
    );
    assert_eq!(updates(&fleet, Some(&[("d1", "mac-ci")])), vec![]);
    assert_eq!(
        updates(&fleet, Some(&[("d1", "mac-ci"), ("d2", "mac-ci")])),
        vec![alert("mac-ci", "Updates due on 1 host of mac-ci")]
    );
}

/// Proves: a report that lists no due item but says some were left out counts as due, and a report
/// that lists none and left none out does not.
#[test]
fn due_items_left_out_of_the_list_still_count() {
    assert_eq!(
        updates(&all_due(1, &report(0, 3)), None),
        vec![alert("", "Updates due on 1 host in no host group")]
    );
    assert_eq!(updates(&all_due(1, &report(0, 0)), None), vec![]);
}

/// Proves: the alert stands across observations while a host is due and is logged once with its
/// token `updates_due`; it clears when the next beats report nothing due, and when updates are due
/// again it is first observed anew and logged again.
#[test]
fn the_alert_clears_when_the_next_beats_report_nothing_due() {
    let book = Book::default();
    let log = Log::default();
    let _guard = log.install();
    let observe = |fleet: &Fleet, unix: u64| {
        book.observe_with(|| Ok::<_, ()>((updates(fleet, None), unix)))
            .expect("observed")
    };
    let due = all_due(1, &report(1, 0));
    let first = observe(&due, UNIX);
    assert_eq!(
        first,
        vec![Observed {
            alert: alert("", "Updates due on 1 host in no host group"),
            first_unix: UNIX,
        }]
    );
    assert_eq!(observe(&due, UNIX + 15)[0].first_unix, UNIX);
    assert_eq!(log.text().matches(r#"alert="updates_due""#).count(), 1);
    assert_eq!(observe(&all_due(1, &report(0, 0)), UNIX + 30), vec![]);
    assert_eq!(observe(&due, UNIX + 45)[0].first_unix, UNIX + 45);
    assert_eq!(log.text().matches(r#"alert="updates_due""#).count(), 2);
}

/// Proves: the alert goes out as kind `UPDATES_DUE` with severity `WARNING`, the group in
/// `host_group` and no host or service.
#[test]
fn the_alert_goes_out_as_updates_due_with_its_group() {
    let observed = Observed {
        alert: alert("mac-ci", "Updates due on 2 hosts of mac-ci"),
        first_unix: UNIX,
    };
    let sent = alert_proto(observed);
    assert_eq!(sent.kind(), proto::AlertKind::UpdatesDue);
    assert_eq!(sent.severity(), proto::AlertSeverity::Warning);
    assert_eq!(sent.host_group, "mac-ci");
    assert_eq!(
        (sent.host_id.as_str(), sent.service.as_str()),
        ("", ""),
        "{sent:?}"
    );
    assert_eq!(sent.message, "Updates due on 2 hosts of mac-ci");
    assert_eq!(Kind::UpdatesDue.log_token(), "updates_due");
}

/// Proves: the key of an alert in the database's rows puts the group in the service column, so the
/// alerts of two groups are two rows, and an alert of a service keeps the service there.
#[test]
fn the_row_key_holds_the_group_in_the_service_column() {
    let mac = rows::key(&alert("mac-ci", "m"));
    let linux = rows::key(&alert("linux-ci", "m"));
    assert_ne!(mac, linux);
    assert_eq!(mac[..2], ["updates_due".to_owned(), "mac-ci".to_owned()]);
    assert_eq!(linux[1], "linux-ci");
    let service = Alert {
        subject: Subject {
            service: "kbf".to_owned(),
            ..Subject::of(Kind::Halt)
        },
        message: "m".to_owned(),
    };
    assert_eq!(
        rows::key(&service)[..2],
        ["halt".to_owned(), "kbf".to_owned()]
    );
}

/// Proves: over a coordinator, `Alerts.List` answers one `UPDATES_DUE` warning for hosts that are
/// due when it holds no host group (they are in none), and none once the hosts' next beats report
/// nothing due.
#[tokio::test]
async fn alerts_list_answers_the_updates_due_of_the_coordinators_fleet() {
    let rig = crate::marks_support::rig("alerts-updates-due", "");
    let listed = |rig: &crate::testutil::Rig| -> Vec<proto::Alert> {
        rig.coordinator
            .list_alerts(
                &crate::marks_support::operator(),
                proto::ListAlertsRequest {},
            )
            .expect("listed")
            .alerts
            .into_iter()
            .filter(|alert| alert.kind() == proto::AlertKind::UpdatesDue)
            .collect()
    };
    let due = report(1, 0);
    {
        let mut fleet = rig.coordinator.lock_fleet();
        fleet.note_node(id("d1"), node(Some(&due)), UNIX);
        fleet.note_node(id("d2"), node(Some(&due)), UNIX);
    }
    let alerts = listed(&rig);
    let [one] = alerts.as_slice() else {
        panic!("one alert: {alerts:?}");
    };
    assert_eq!(one.severity(), proto::AlertSeverity::Warning);
    assert_eq!(one.host_group, "");
    assert_eq!(one.message, "Updates due on 2 hosts in no host group");
    rig.coordinator
        .lock_fleet()
        .note_node(id("d1"), node(Some(&report(0, 0))), UNIX);
    rig.coordinator
        .lock_fleet()
        .note_node(id("d2"), node(Some(&report(0, 0))), UNIX);
    assert_eq!(listed(&rig), vec![]);
}

/// Proves: over a coordinator that holds host groups, `Alerts.List` counts each due host in the
/// group the enrollment store records for it: one alert for the group with its name in
/// `host_group`, and one for the enrolled host in no group; a host that keeps a report with
/// nothing due is in no alert.
#[tokio::test]
async fn alerts_list_counts_each_due_host_in_its_recorded_group() {
    let farm = crate::group_decision_support::Farm::new("alerts-updates-due-groups");
    farm.group("mac-ci", &[], None);
    farm.host("g1", "mac-ci");
    farm.host("g2", "mac-ci");
    farm.host("g3", "mac-ci");
    farm.lone("n1");
    let coordinator = farm.world.coordinator();
    {
        let mut fleet = coordinator.lock_fleet();
        for host in ["g1", "g2", "n1"] {
            fleet.note_node(id(host), node(Some(&report(1, 0))), UNIX);
        }
        fleet.note_node(id("g3"), node(Some(&report(0, 0))), UNIX);
    }
    let alerts: Vec<(String, String)> = coordinator
        .list_alerts(
            &crate::marks_support::operator(),
            proto::ListAlertsRequest {},
        )
        .expect("listed")
        .alerts
        .into_iter()
        .filter(|alert| alert.kind() == proto::AlertKind::UpdatesDue)
        .map(|alert| (alert.host_group, alert.message))
        .collect();
    assert_eq!(
        alerts,
        vec![
            (
                String::new(),
                "Updates due on 1 host in no host group".to_owned()
            ),
            (
                "mac-ci".to_owned(),
                "Updates due on 2 hosts of mac-ci".to_owned()
            ),
        ]
    );
}
