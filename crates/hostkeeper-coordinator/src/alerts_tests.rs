//! The alerts: each kind's derivation on a state built for it, with the time given; whom a
//! duplicate claim collides with; and the book that logs an alert once and keeps when it was
//! first observed. The tests over a coordinator are in `alerts_coordinator_tests.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hostkeeper_enrollment::enroll::{Claims, RecordedApproval, RequestState, RequestView};
use hostkeeper_enrollment::names::{HostId as EnrolledId, KeyHash};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::Health;

use super::{
    Alert, At, Book, EnrollmentFacts, Kind, Observed, Subject, Thresholds, Waiting, collides_with,
    derive,
};
use crate::fleet::{Fleet, Node, QUIET_START};
use crate::holds::{HoldMark, HoldScope, marks_of};
use crate::ops::{AuditEntry, Exclusion, Halt, HaltOrigin, OpsState, ServiceOps};
use crate::services::Services;
use crate::testutil::{LEGACY, Log, beat, id, kbf_services, secs, version};

/// The unix time of the coordinator's clock at 0.
const UNIX: u64 = 1_800_000_000;

/// The time `at` seconds after the coordinator's start.
fn at(seconds: u64) -> At {
    At {
        now: secs(seconds),
        unix: UNIX + seconds,
    }
}

/// A host noted as heard at `seen`.
fn node(seen: u64) -> Node {
    Node {
        role: "daemon".to_owned(),
        features: Vec::new(),
        agent: None,
        platform: Platform::LinuxX86_64,
        seen: secs(seen),
        announced: BTreeMap::from([(LEGACY.to_owned(), Health::Healthy)]),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root: crate::trusted_root::TrustedRootSaid::default(),
        skewed_beats: 0,
    }
}

/// A fleet started at 0 in which each of `hosts` heartbeated kbf at `seen`, running 1.0.
fn fleet_of(hosts: &[&str], seen: u64) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    for host in hosts {
        fleet.note_node(id(host), node(seen), UNIX);
        fleet.record_named_in(LEGACY, id(host), "daemon", beat("1.0"), secs(seen));
    }
    fleet
}

/// An enrollment store with nothing pending and a certificate a year from expiring.
fn quiet_store() -> EnrollmentFacts {
    EnrollmentFacts {
        server_not_after: Some(UNIX + 365 * 24 * 3600),
        ..EnrollmentFacts::default()
    }
}

/// The alerts of kbf's coordinator at 1000, with the default thresholds.
fn derived(fleet: &Fleet, ops: &OpsState, enrollment: &EnrollmentFacts) -> Vec<Alert> {
    derive(
        fleet,
        ops,
        &kbf_services(),
        enrollment,
        at(1_000),
        &Thresholds::default(),
    )
}

/// The ops state with `marks` as kbf's.
fn marked(marks: ServiceOps) -> OpsState {
    let mut ops = OpsState::default();
    ops.services.insert(LEGACY.to_owned(), marks);
    ops
}

fn subject(kind: Kind, service: &str, host: &str, version: &str) -> Subject {
    Subject {
        kind,
        service: service.to_owned(),
        host: host.to_owned(),
        version: version.to_owned(),
        request: String::new(),
        other_host: String::new(),
    }
}

/// A pending request with the id of `byte` repeated.
fn waiting(byte: &str, duplicate_claim: bool, other_host: Option<&str>) -> Waiting {
    Waiting {
        request_id: byte.repeat(32),
        posted_unix: UNIX + 10,
        renewal_of: None,
        duplicate_claim,
        other_host: other_host.map(str::to_owned),
    }
}

// ---- one derivation per kind ------------------------------------------------------------

/// Proves: a fleet of live hosts at a good version, nothing marked, nothing pending and a
/// certificate far from its expiry needs no person: the list is empty.
///
/// Catches: an alert raised by a healthy fleet, which an empty list from a derivation that does
/// nothing could not tell apart from this one; the other tests of this file each raise one.
#[test]
fn a_healthy_fleet_has_no_alert() {
    let fleet = fleet_of(&["d1", "d2"], 990);
    assert_eq!(
        derived(&fleet, &OpsState::default(), &quiet_store()),
        Vec::new()
    );
}

/// Proves: a halt a host raised is `halt` naming the service, the host and the target, with the
/// check; an operator's halt names no host; a halt of a service the rollout file does not
/// define, or a service with marks and no halt, is none.
///
/// Catches: a halted fleet the alerts miss, an operator's halt blamed on a host, an alert of a
/// service the coordinator does not manage.
#[test]
fn a_halted_service_is_a_halt() {
    let fleet = fleet_of(&["d1"], 990);
    let mut ops = marked(ServiceOps {
        halt: Some(Halt {
            node_id: "d1".to_owned(),
            check: "disk_full".to_owned(),
            target: version("2.0"),
            report: None,
            origin: HaltOrigin::Hook,
        }),
        ..ServiceOps::default()
    });
    ops.services.insert(
        "gone".to_owned(),
        ServiceOps {
            halt: Some(Halt {
                node_id: "d9".to_owned(),
                check: "disk_full".to_owned(),
                target: version("2.0"),
                report: None,
                origin: HaltOrigin::Hook,
            }),
            ..ServiceOps::default()
        },
    );
    assert_eq!(
        derived(&fleet, &ops, &quiet_store()),
        vec![Alert {
            subject: subject(Kind::Halt, "kbf", "d1", "2.0"),
            message: "kbf is halted at 2.0 by d1, check disk_full: nothing is ordered for it \
                      until an operator clears the halt"
                .to_owned(),
        }]
    );

    let operators = marked(ServiceOps {
        halt: Some(Halt {
            node_id: String::new(),
            check: crate::halt::OPERATOR_CHECK.to_owned(),
            target: version("2.0"),
            report: None,
            origin: HaltOrigin::Operator,
        }),
        ..ServiceOps::default()
    });
    assert_eq!(
        derived(&fleet, &operators, &quiet_store()),
        vec![Alert {
            subject: subject(Kind::Halt, "kbf", "", "2.0"),
            message: "kbf is halted at 2.0 by an operator: nothing is ordered for it until an \
                      operator clears the halt"
                .to_owned(),
        }]
    );

    let unhalted = marked(ServiceOps {
        bad: vec![version("0.9")],
        ..ServiceOps::default()
    });
    assert_eq!(derived(&fleet, &unhalted, &quiet_store()), Vec::new());
}

/// Proves: a pending request that is no duplicate claim is `waiting_for_operator`, naming the
/// request, and a renewal's names the host it renews.
///
/// Catches: a host that asked to enroll left with no one told, and a renewal not told apart.
#[test]
fn a_pending_request_waits_for_an_operator() {
    let mut store = quiet_store();
    let mut renewal = waiting("cd", false, None);
    renewal.renewal_of = Some("db-1".to_owned());
    store.waiting = vec![waiting("ab", false, None), renewal];
    let alerts = derived(&fleet_of(&[], 0), &OpsState::default(), &store);
    assert_eq!(
        alerts,
        vec![
            Alert {
                subject: Subject {
                    request: "ab".repeat(32),
                    ..subject(Kind::WaitingForOperator, "", "", "")
                },
                message: format!(
                    "request {} (a host's enrollment, received at unix {}) waits for an \
                     operator's approval",
                    "ab".repeat(32),
                    UNIX + 10
                ),
            },
            Alert {
                subject: Subject {
                    request: "cd".repeat(32),
                    ..subject(Kind::WaitingForOperator, "", "db-1", "")
                },
                message: format!(
                    "request {} (the renewal of db-1, received at unix {}) waits for an \
                     operator's approval",
                    "cd".repeat(32),
                    UNIX + 10
                ),
            },
        ]
    );
}

/// Proves: a host the enrollment store marks duplicate is `duplicate`, critical.
///
/// Catches: a cloned disk ordered nothing with no one told.
#[test]
fn a_host_two_daemons_report_is_a_duplicate() {
    let mut store = quiet_store();
    store.duplicates = BTreeSet::from([id("d1")]);
    let alerts = derived(&fleet_of(&["d1"], 990), &OpsState::default(), &store);
    assert_eq!(
        alerts,
        vec![Alert {
            subject: subject(Kind::Duplicate, "", "d1", ""),
            message: "two live daemons report the identity of d1 (a cloned disk): it is \
                      ordered nothing until a new key replaces it"
                .to_owned(),
        }]
    );
    assert_eq!(
        alerts[0].subject.kind.severity(),
        proto::AlertSeverity::Critical
    );
}

/// Proves: a pending duplicate claim is `duplicate_claim_pending` and not also
/// `waiting_for_operator`; it names the request and the enrolled host whose identity it claims,
/// and says "another host or request" when no enrolled host is known.
///
/// Catches: a claim that would take over a host's identity listed as an ordinary request, or
/// listed twice, and a claim that names the wrong host.
#[test]
fn a_pending_duplicate_claim_names_both_hosts() {
    let mut store = quiet_store();
    store.waiting = vec![waiting("ab", true, Some("db-1")), waiting("cd", true, None)];
    let alerts = derived(&fleet_of(&[], 0), &OpsState::default(), &store);
    assert_eq!(
        alerts,
        vec![
            Alert {
                subject: Subject {
                    request: "ab".repeat(32),
                    other_host: "db-1".to_owned(),
                    ..subject(Kind::DuplicateClaimPending, "", "", "")
                },
                message: format!(
                    "request {} claims an identity db-1 has: approve it only with replace, or \
                     leave it",
                    "ab".repeat(32)
                ),
            },
            Alert {
                subject: Subject {
                    request: "cd".repeat(32),
                    ..subject(Kind::DuplicateClaimPending, "", "", "")
                },
                message: format!(
                    "request {} claims an identity another host or request has: approve it \
                     only with replace, or leave it",
                    "cd".repeat(32)
                ),
            },
        ]
    );
}

/// Proves: a canary failure the reconcile noted is `failed_canary` naming the canary host, the
/// service and the version, while the version is marked bad; unmarked, or with no marks of the
/// service at all, it is none.
///
/// Catches: a failed canary missing from the alerts (the gap the Fleet page had), one named
/// without its host, and one that outlives the operator's unmark.
#[test]
fn a_failed_canary_names_its_host_while_its_version_is_bad() {
    let mut fleet = fleet_of(&["d1", "d2"], 990);
    fleet.note_failed_canary(LEGACY, &version("2.0"), "d1");
    let bad = marked(ServiceOps {
        bad: vec![version("2.0")],
        ..ServiceOps::default()
    });
    assert_eq!(
        derived(&fleet, &bad, &quiet_store()),
        vec![Alert {
            subject: subject(Kind::FailedCanary, "kbf", "d1", "2.0"),
            message: "d1, a canary of kbf, failed 2.0: it is marked bad and rolled back".to_owned(),
        }]
    );
    let unmarked = marked(ServiceOps::default());
    assert_eq!(derived(&fleet, &unmarked, &quiet_store()), Vec::new());
    assert_eq!(
        derived(&fleet, &OpsState::default(), &quiet_store()),
        Vec::new()
    );
}

/// Proves: a held host that runs a version marked bad is `held_on_bad_version` naming the host,
/// the service and the version; an unheld host on it, and a held host on a good version, are
/// none.
///
/// Catches: a hold that keeps a host on a bad version with no one told, and an alert for a host
/// the rollout moves off it anyway.
#[test]
fn a_held_host_on_a_bad_version_is_held_on_bad_version() {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    for host in ["d1", "d2", "d3"] {
        fleet.note_node(id(host), node(990), UNIX);
    }
    fleet.record_named_in(LEGACY, id("d1"), "daemon", beat("2.0"), secs(990));
    fleet.record_named_in(LEGACY, id("d2"), "daemon", beat("2.0"), secs(990));
    fleet.record_named_in(LEGACY, id("d3"), "daemon", beat("1.0"), secs(990));
    let mut ops = marked(ServiceOps {
        bad: vec![version("2.0")],
        ..ServiceOps::default()
    });
    for host in ["d1", "d3"] {
        ops.holds.push(HoldMark {
            service: LEGACY.to_owned(),
            scope: HoldScope::Host(host.to_owned()),
            reason: "disk swap".to_owned(),
            by: "operator".to_owned(),
            at_unix: UNIX,
        });
    }
    fleet.set_marks(marks_of(&ops));
    assert_eq!(
        derived(&fleet, &ops, &quiet_store()),
        vec![Alert {
            subject: subject(Kind::HeldOnBadVersion, "kbf", "d1", "2.0"),
            message: "an operator holds d1 on kbf 2.0, which is marked bad: it is not ordered \
                      off it until the hold is released"
                .to_owned(),
        }]
    );
}

/// An `exclude_abandon` audit entry of `service` and `node`, with `reason`.
fn abandon(service: Option<&str>, node: Option<&str>, reason: Option<&str>) -> AuditEntry {
    let mut entry = AuditEntry::new("operator", "exclude_abandon");
    entry.service = service.map(str::to_owned);
    entry.node_id = node.map(str::to_owned);
    entry.reason = reason.map(str::to_owned);
    entry
}

/// A change of `action` (`include`, `exclude`) of kbf on `node`.
fn plain(action: &str, node: &str) -> AuditEntry {
    let mut entry = AuditEntry::new("operator", action);
    entry.service = Some(LEGACY.to_owned());
    entry.node_id = Some(node.to_owned());
    entry
}

/// Proves: an `exclude_abandon` the ops state keeps is `abandoned_while_updating` while its host
/// stays excluded, with the operator's reason; once the host is included again it is none; an
/// abandon followed by an include and then a plain exclude of the pair is none (the host is
/// excluded, but not because it was abandoned); and so are an entry of another action, of a
/// service the coordinator does not manage, and an entry that names no service or no host.
///
/// Catches: a host abandoned mid-update with no one told, an alert that stands after the
/// operator took the host back, and a later plain exclusion reported as an abandon.
#[test]
fn a_host_abandoned_while_updating_stands_while_excluded() {
    let fleet = fleet_of(&[], 0);
    let mut ops = OpsState::default();
    ops.audit = vec![
        AuditEntry::new("operator", "exclude"),
        abandon(
            Some("kbf"),
            Some("d1"),
            Some("host gone; possibly_mid_update"),
        ),
        abandon(Some("kbf"), Some("d2"), None),
        abandon(Some("gone"), Some("d1"), None),
        abandon(None, Some("d1"), None),
        abandon(Some("kbf"), None, None),
        abandon(Some("kbf"), Some("d3"), None),
        abandon(Some("kbf"), Some("d4"), None),
        plain("include", "d4"),
        plain("exclude", "d4"),
    ];
    for host in ["d1", "d2", "d4"] {
        ops.excluded.push(Exclusion {
            service: LEGACY.to_owned(),
            node_id: host.to_owned(),
            reason: "host gone".to_owned(),
        });
    }
    ops.excluded.push(Exclusion {
        service: "gone".to_owned(),
        node_id: "d1".to_owned(),
        reason: "host gone".to_owned(),
    });
    assert_eq!(
        derived(&fleet, &ops, &quiet_store()),
        vec![
            Alert {
                subject: subject(Kind::AbandonedWhileUpdating, "kbf", "d1", ""),
                message: "an operator abandoned d1 while it may have been updating kbf (host \
                          gone; possibly_mid_update): its update slot was freed, and it stays \
                          excluded until included"
                    .to_owned(),
            },
            Alert {
                subject: subject(Kind::AbandonedWhileUpdating, "kbf", "d2", ""),
                message: "an operator abandoned d2 while it may have been updating kbf (no \
                          reason): its update slot was freed, and it stays excluded until \
                          included"
                    .to_owned(),
            },
        ]
    );
    ops.excluded.clear();
    assert_eq!(derived(&fleet, &ops, &quiet_store()), Vec::new());
}

/// Proves: by the clock given, a host whose last heartbeat is exactly the silent threshold old
/// is `silent_host`, and one heard a second later is none.
///
/// Catches: a host gone quiet with no one told, a threshold off by one, and a silence measured
/// by any clock but the one given.
#[test]
fn a_host_silent_for_the_threshold_is_silent_and_one_just_inside_is_not() {
    let thresholds = Thresholds {
        silent_after: secs(300),
        ..Thresholds::default()
    };
    let mut fleet = fleet_of(&["d1"], 700);
    fleet.note_node(id("d2"), node(701), UNIX);
    let alerts = derive(
        &fleet,
        &OpsState::default(),
        &kbf_services(),
        &quiet_store(),
        at(1_000),
        &thresholds,
    );
    assert_eq!(
        alerts,
        vec![Alert {
            subject: subject(Kind::SilentHost, "", "d1", ""),
            message: "d1 has not heartbeated for 300 s (the threshold is 300 s)".to_owned(),
        }]
    );
    let earlier = derive(
        &fleet,
        &OpsState::default(),
        &kbf_services(),
        &quiet_store(),
        at(999),
        &thresholds,
    );
    assert_eq!(earlier, Vec::new(), "d1 is 299 s silent at 999");
}

/// The alerts of a certificate that expires at `not_after`, at 1000, with a window of 600 s.
fn cert_alerts(not_after: Option<u64>) -> Vec<Alert> {
    let store = EnrollmentFacts {
        server_not_after: not_after,
        ..EnrollmentFacts::default()
    };
    let thresholds = Thresholds {
        cert_window_secs: 600,
        ..Thresholds::default()
    };
    derive(
        &fleet_of(&[], 0),
        &OpsState::default(),
        &Services::default(),
        &store,
        at(1_000),
        &thresholds,
    )
}

/// Proves: the certificate the coordinator serves its hosts is `cert_near_expiry` when it
/// expires inside the window (one second inside), and once it has expired; at the window's
/// edge, and with no enrollment store, it is none.
///
/// Catches: a renewal failing unseen until the hosts are refused, and a window off by one.
#[test]
fn a_cert_inside_the_window_is_near_expiry_and_one_outside_is_not() {
    let now = UNIX + 1_000;
    assert_eq!(
        cert_alerts(Some(now + 599)),
        vec![Alert {
            subject: subject(Kind::CertNearExpiry, "", "", ""),
            message: format!(
                "the certificate the coordinator serves its hosts expires at unix {}, in 599 s: \
                 its renewal is failing (see the coordinator's log)",
                now + 599
            ),
        }]
    );
    assert_eq!(cert_alerts(Some(now + 600)), Vec::new());
    assert_eq!(
        cert_alerts(Some(now)),
        vec![Alert {
            subject: subject(Kind::CertNearExpiry, "", "", ""),
            message: format!(
                "the certificate the coordinator serves its hosts expired at unix {now}: its \
                 renewal is failing (see the coordinator's log)"
            ),
        }]
    );
    assert_eq!(cert_alerts(None), Vec::new());
}

/// The alerts of a push certificate that expires at `not_after`, at 1000, with the thresholds
/// `days`.
fn push_alerts(not_after: Option<u64>, days: &[u32]) -> Vec<Alert> {
    let store = EnrollmentFacts {
        push_cert_not_after: not_after,
        push_alert_days: days.to_vec(),
        ..EnrollmentFacts::default()
    };
    derive(
        &fleet_of(&[], 0),
        &OpsState::default(),
        &Services::default(),
        &store,
        at(1_000),
        &Thresholds::default(),
    )
}

/// Proves: the push certificate raises one alert once its expiry is within the largest of the
/// `push_alert_days` thresholds, its version is the smallest threshold reached (so that reaching
/// 14 days is a new alert after 60), it is `expired` once the certificate has, and none for a
/// certificate beyond every threshold, with no certificate held, or when no threshold is set and it
/// has not expired.
///
/// Catches: a renewal missed because the reminder never came, and a reminder that says nothing new
/// at 14 days.
#[test]
fn the_push_certificate_alerts_at_each_threshold_and_when_it_expired() {
    let now = UNIX + 1_000;
    let day = 86_400;
    let version = |alerts: &[Alert]| {
        alerts
            .iter()
            .map(|a| a.subject.version.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(push_alerts(Some(now + 61 * day), &[60, 14]), Vec::new());
    assert_eq!(
        version(&push_alerts(Some(now + 60 * day - 1), &[60, 14])),
        ["60"]
    );
    assert_eq!(
        version(&push_alerts(Some(now + 30 * day), &[60, 14])),
        ["60"]
    );
    assert_eq!(
        version(&push_alerts(Some(now + 14 * day - 1), &[60, 14])),
        ["14"]
    );
    assert_eq!(version(&push_alerts(Some(now + 1), &[60, 14])), ["14"]);
    assert_eq!(version(&push_alerts(Some(now), &[60, 14])), ["expired"]);
    assert_eq!(version(&push_alerts(Some(now - day), &[])), ["expired"]);
    assert_eq!(push_alerts(Some(now + day), &[]), Vec::new());
    assert_eq!(push_alerts(None, &[60, 14]), Vec::new());
    let alerts = push_alerts(Some(now + 10 * day), &[60, 14]);
    assert_eq!(alerts[0].subject.kind, Kind::PushCertNearExpiry);
    assert_eq!(
        alerts[0].message,
        format!(
            "the MDM push certificate expires at unix {}, in 864000 s: renew it from the MDM setup \
             (a push certificate that expires stops every push to the Macs)",
            now + 10 * day
        )
    );
}

/// Proves: the alert is derived from the setup's push certificate in use and the setting
/// `push_alert_days`, on a coordinator: none with no certificate, one when the certificate expires
/// inside a threshold, none again when the setting asks for fewer days, and a certificate that
/// was replaced is judged by its newest version.
///
/// Catches: an expiry read off a replaced version, and a setting that does not change when the alert
/// comes.
#[tokio::test]
async fn a_coordinator_alerts_on_the_push_certificate_it_holds() {
    use crate::operator_mdm_push_tests::{DAY, TOPIC, alice, rig};
    use hostkeeper_mdm_push::request::pkcs1_pem;
    use hostkeeper_mdm_push::testkit::{fixture_key, push_certificate_pem};

    let rig = rig("alerts-push-certificate");
    let alerts = |rig: &crate::operator_mdm_push_tests::Rig| {
        rig.world
            .coordinator
            .list_alerts(&alice(), proto::ListAlertsRequest::default())
            .expect("listed")
            .alerts
            .into_iter()
            .filter(|alert| alert.kind == proto::AlertKind::PushCertNearExpiry as i32)
            .collect::<Vec<_>>()
    };
    assert!(alerts(&rig).is_empty(), "no certificate, no alert");
    let key = fixture_key(0);
    let now = rig.now();
    let nearly = push_certificate_pem(key, TOPIC, now - DAY, now + 20 * DAY);
    rig.certificate(&nearly, pkcs1_pem(key).expect("pem").expose())
        .expect("stored");
    let found = alerts(&rig);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].version, "60");
    assert_eq!(found[0].severity, proto::AlertSeverity::Warning as i32);
    rig.world
        .coordinator
        .mdm_put_setting(
            &alice(),
            &proto::PutSettingRequest {
                name: "push_alert_days".to_owned(),
                value: "10".to_owned(),
            },
        )
        .expect("set");
    assert!(
        alerts(&rig).is_empty(),
        "20 days left is outside a threshold of 10"
    );
    let renewed = push_certificate_pem(key, TOPIC, now - DAY, now + 5 * DAY);
    rig.certificate(&renewed, pkcs1_pem(key).expect("pem").expose())
        .expect("stored");
    assert_eq!(
        alerts(&rig)[0].version,
        "10",
        "the newest certificate's expiry is read"
    );
}

/// Proves: a setup that cannot be read raises no alert of its own and does not fail the list.
///
/// Catches: an alert list that fails because the MDM setup store is down.
#[tokio::test]
async fn a_setup_that_cannot_be_read_does_not_fail_the_alerts() {
    use crate::operator_mdm_push_tests::{alice, flaky_rig};
    use std::sync::atomic::Ordering;

    let (rig, store) = flaky_rig("alerts-push-unreadable");
    store.reads_fail.store(true, Ordering::SeqCst);
    let listed = rig
        .world
        .coordinator
        .list_alerts(&alice(), proto::ListAlertsRequest::default())
        .expect("listed");
    assert!(listed.alerts.is_empty());
}

/// Proves: alerts of several kinds come in the order of their kinds, then by service and host,
/// whatever order the derivations found them in; each kind has its log token and severity.
///
/// Catches: an answer whose order moves from one call to the next, and a log token respelled
/// (an operator's log search would miss it).
#[test]
fn alerts_come_by_kind_then_service_and_host() {
    let mut fleet = fleet_of(&["d2", "d1"], 0);
    fleet.note_failed_canary(LEGACY, &version("2.0"), "d2");
    let ops = marked(ServiceOps {
        bad: vec![version("2.0")],
        halt: Some(Halt {
            node_id: "d2".to_owned(),
            check: "disk_full".to_owned(),
            target: version("2.0"),
            report: None,
            origin: HaltOrigin::Hook,
        }),
        ..ServiceOps::default()
    });
    let mut store = quiet_store();
    store.duplicates = BTreeSet::from([id("d2"), id("d1")]);
    let kinds: Vec<(Kind, String)> = derived(&fleet, &ops, &store)
        .into_iter()
        .map(|alert| (alert.subject.kind, alert.subject.host))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (Kind::Halt, "d2".to_owned()),
            (Kind::Duplicate, "d1".to_owned()),
            (Kind::Duplicate, "d2".to_owned()),
            (Kind::FailedCanary, "d2".to_owned()),
            (Kind::SilentHost, "d1".to_owned()),
            (Kind::SilentHost, "d2".to_owned()),
        ]
    );
    let all = [
        Kind::Halt,
        Kind::WaitingForOperator,
        Kind::Duplicate,
        Kind::DuplicateClaimPending,
        Kind::FailedCanary,
        Kind::HeldOnBadVersion,
        Kind::AbandonedWhileUpdating,
        Kind::SilentHost,
        Kind::CertNearExpiry,
        Kind::StoreUnreachable,
        Kind::TrustedRoot,
        Kind::PushCertNearExpiry,
    ];
    assert_eq!(
        all.map(Kind::log_token),
        [
            "halt",
            "waiting_for_operator",
            "duplicate",
            "duplicate_claim",
            "canary_failed",
            "held_on_bad_version",
            "exclude_abandon",
            "silent_host",
            "cert_near_expiry",
            "store_unreachable",
            "trusted_root",
            "push_cert_near_expiry",
        ]
    );
    assert_eq!(
        all.map(|kind| kind.proto() as i32),
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
    );
    let critical: Vec<Kind> = all
        .into_iter()
        .filter(|kind| kind.severity() == proto::AlertSeverity::Critical)
        .collect();
    assert_eq!(
        critical,
        vec![
            Kind::Halt,
            Kind::Duplicate,
            Kind::FailedCanary,
            Kind::StoreUnreachable
        ]
    );
}

/// Catches a `trusted_root` alert for a host whose root needs no attention or is unknown, one
/// that does not name its host, its reason or its age, and one host's reason put on another:
/// of five live hosts, the stale, missing and unparseable ones each raise one warning naming the
/// host, the reason and the age it said (none for the missing one), in host order, and the hosts
/// whose root needs no attention or said nothing of it raise none.
#[test]
fn trusted_root_names_each_host_whose_root_needs_attention() {
    use crate::trusted_root::TrustedRootSaid;
    use hostkeeper_proto::hostkeeper::agent::v1::TrustedRootReason;

    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    let roots = [
        ("d1", Some(91 * 86_400), TrustedRootReason::Stale),
        ("d2", Some(60), TrustedRootReason::None),
        ("d3", None, TrustedRootReason::Missing),
        ("d4", None, TrustedRootReason::Unspecified),
        ("d5", Some(0), TrustedRootReason::Unparseable),
    ];
    for (host, age_secs, reason) in roots {
        let mut said = node(990);
        said.trusted_root = TrustedRootSaid { age_secs, reason };
        fleet.note_node(id(host), said, UNIX);
    }
    let alerts: Vec<(Subject, proto::AlertSeverity, String)> =
        derived(&fleet, &OpsState::default(), &quiet_store())
            .into_iter()
            .map(|alert| {
                let severity = alert.subject.kind.severity();
                (alert.subject, severity, alert.message)
            })
            .collect();
    let then = "update hostkeeper on it, or copy a current root to it";
    assert_eq!(
        alerts,
        [
            (
                subject(Kind::TrustedRoot, "", "d1", ""),
                proto::AlertSeverity::Warning,
                format!(
                    "d1 reports its Sigstore trusted root stale (7862400 s old): its package \
                     checks go on, and refuse every package signed after a Sigstore key rotation \
                     the root does not hold; {then}"
                ),
            ),
            (
                subject(Kind::TrustedRoot, "", "d3", ""),
                proto::AlertSeverity::Warning,
                format!(
                    "d3 reports its Sigstore trusted root missing (no age): its package checks \
                     fail; {then}"
                ),
            ),
            (
                subject(Kind::TrustedRoot, "", "d5", ""),
                proto::AlertSeverity::Warning,
                format!(
                    "d5 reports its Sigstore trusted root unparseable (0 s old): its package \
                     checks fail; {then}"
                ),
            ),
        ]
    );
}

// ---- whom a duplicate claim collides with -----------------------------------------------

fn claims(machine_id: &str, macs: &[&str]) -> Claims {
    Claims {
        machine_id: machine_id.to_owned(),
        addresses: macs.iter().map(|mac| (*mac).to_owned()).collect(),
        ..Claims::default()
    }
}

fn request(byte: &str, claims: Claims) -> RequestView {
    RequestView {
        request_id: KeyHash::from_hex(&byte.repeat(32)).expect("a key hash"),
        state: RequestState::DuplicateClaim,
        claims,
        image: String::new(),
        posted_unix: UNIX,
        renewal_of: None,
        round: 0,
        host_id: None,
        mac_nonce_presented: false,
        expiry: None,
    }
}

fn approved(byte: &str, host: &str, claims: Claims) -> RecordedApproval {
    RecordedApproval {
        request_id: KeyHash::from_hex(&byte.repeat(32)).expect("a key hash"),
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        host_group: String::new(),
        services: Vec::new(),
        replace: false,
        approver: "operator".to_owned(),
        approved_unix: UNIX,
        reason: "racked".to_owned(),
        claims,
        mac_nonce: false,
    }
}

/// Proves: a duplicate claim collides with the host it renews; else with the host of a recorded
/// approval that has its machine id, or one of its universally administered MACs; an approval of
/// the request itself, an empty machine id and a locally administered MAC match nothing.
///
/// Catches: a duplicate claim that names a host it does not share an identity with, or none
/// when it does.
#[test]
fn a_duplicate_claim_collides_with_the_host_whose_identity_it_shares() {
    let approvals = vec![
        approved("01", "own", claims("m-0", &[])),
        approved("02", "db-1", claims("m-1", &["0c:c4:7a:00:00:02"])),
        approved("03", "db-2", claims("", &["02:00:00:00:00:01"])),
    ];
    let mut renewal = request("09", claims("m-9", &[]));
    renewal.renewal_of = Some(EnrolledId::parse("db-7").expect("a host id"));
    assert_eq!(collides_with(&renewal, &approvals), Some("db-7".to_owned()));
    let by_machine = request("09", claims("m-1", &[]));
    assert_eq!(
        collides_with(&by_machine, &approvals),
        Some("db-1".to_owned())
    );
    let by_mac = request("09", claims("m-9", &["0c-c4-7a-00-00-02"]));
    assert_eq!(collides_with(&by_mac, &approvals), Some("db-1".to_owned()));
    let itself = request("01", claims("m-0", &[]));
    assert_eq!(collides_with(&itself, &approvals), None);
    let blank = request("09", claims("", &["02:00:00:00:00:01"]));
    assert_eq!(collides_with(&blank, &approvals), None);
}

// ---- the book -----------------------------------------------------------------------------

fn halt_alert(host: &str) -> Alert {
    Alert {
        subject: subject(Kind::Halt, "kbf", host, "2.0"),
        message: format!("kbf is halted by {host}"),
    }
}

/// Proves: an alert is logged once, at the first observation that finds it, as one error line
/// with its `alert=` field and what it concerns; later observations keep when it was first
/// observed and log nothing; one that ended is forgotten, and coming back it is logged again
/// and first observed anew.
///
/// Catches: an alert logged at every observation, never, or without its token; a first
/// observation time that moves; an ended alert that keeps its old time.
#[test]
fn an_alert_is_logged_once_when_it_appears_and_keeps_its_first_time() {
    let book = Book::default();
    let log = Log::default();
    let _guard = log.install();
    assert_eq!(
        book.observe_with(|| Ok::<_, ()>((vec![halt_alert("d1")], UNIX)))
            .expect("observed"),
        vec![Observed {
            alert: halt_alert("d1"),
            first_unix: UNIX
        }]
    );
    let lines: Vec<String> = log.text().lines().map(str::to_owned).collect();
    let [line] = lines.as_slice() else {
        panic!("one line: {}", log.text());
    };
    assert!(
        line.ends_with(
            r#"ERROR hostkeeper_coordinator::alerts: kbf is halted by d1 alert="halt" service=kbf node_id=d1 version=2.0 request_id= other_host_id="#
        ),
        "{line}"
    );

    let both = book
        .observe_with(|| Ok::<_, ()>((vec![halt_alert("d1"), halt_alert("d2")], UNIX + 15)))
        .expect("observed");
    assert_eq!(
        both.iter()
            .map(|one| (one.alert.subject.host.as_str(), one.first_unix))
            .collect::<Vec<_>>(),
        vec![("d1", UNIX), ("d2", UNIX + 15)]
    );
    assert_eq!(log.text().lines().count(), 2, "{}", log.text());

    assert_eq!(
        book.observe_with(|| Ok::<_, ()>((vec![halt_alert("d2")], UNIX + 30)))
            .expect("observed")
            .len(),
        1
    );
    let back = book
        .observe_with(|| Ok::<_, ()>((vec![halt_alert("d1"), halt_alert("d2")], UNIX + 45)))
        .expect("observed");
    assert_eq!(
        back.iter().map(|one| one.first_unix).collect::<Vec<_>>(),
        vec![UNIX + 45, UNIX + 15]
    );
    assert_eq!(
        log.text().matches("node_id=d1").count(),
        2,
        "{}",
        log.text()
    );
}
