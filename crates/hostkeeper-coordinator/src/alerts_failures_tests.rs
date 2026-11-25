//! The alerts of a host's failed update and of a host's own halt ([`super::failures`]), derived
//! from a fleet of the rollout file's service (`kbf`), each with what the host said ([`Said`]).

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::{Class, Health, LocalHalt, Outcome, Phase, Token, UpdateResult};

use super::{Alert, At, EnrollmentFacts, Kind, Observed, Said, Subject, Thresholds, derive};
use crate::fleet::{Fleet, Node, QUIET_START};
use crate::holds::marks_of;
use crate::ops::{Exclusion, OpsState, ServiceOps};
use crate::testutil::{LEGACY, beat, id, kbf_services, secs, version};

/// The unix time of the coordinator's clock at 0.
const UNIX: u64 = 1_800_000_000;

/// A host noted as heard at 990, that lists kbf.
fn node() -> Node {
    Node {
        role: "daemon".to_owned(),
        features: Vec::new(),
        agent: None,
        platform: Platform::LinuxX86_64,
        seen: secs(990),
        announced: BTreeMap::from([(LEGACY.to_owned(), Health::Healthy)]),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root: crate::trusted_root::TrustedRootSaid::default(),
        skewed_beats: 0,
        maintenance: None,
    }
}

/// A fleet in which each host heartbeated kbf at 990 as `beats` says.
fn fleet_of(beats: Vec<(&str, hostkeeper_wire::wire::Heartbeat)>) -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    for (host, heartbeat) in beats {
        fleet.note_node(id(host), node(), UNIX);
        fleet.record_named_in(LEGACY, id(host), "daemon", heartbeat, secs(990));
    }
    fleet
}

/// A host that runs `running` of kbf and failed `target` with `reason`, its try 2, saying
/// `detail`.
fn failed(
    running: &str,
    target: &str,
    reason: &str,
    detail: Option<&str>,
) -> hostkeeper_wire::wire::Heartbeat {
    let mut heartbeat = beat(running);
    heartbeat.last_update = Some(UpdateResult {
        target: version(target),
        outcome: Outcome::Failed,
        phase: Some(Phase::BeforeCommit),
        class: Some(Class::Environment),
        reason: Token::parse(reason),
        order_id: None,
        order_try: Some(2),
        detail: detail.map(str::to_owned),
    });
    heartbeat
}

/// A host that runs 1.0 of kbf and holds a halt of it of its own, of generation `generation`,
/// written `age_secs` ago when it says.
fn halted(generation: u64, age_secs: Option<u64>) -> hostkeeper_wire::wire::Heartbeat {
    let mut heartbeat = beat("1.0");
    heartbeat.local_halt = Some(LocalHalt {
        reason: Token::parse("tainted"),
        generation,
        age_secs,
    });
    heartbeat
}

/// The alerts of `kind` at 1000, kbf wanted at `wanted` (none: no desired version).
fn of_kind(fleet: &Fleet, ops: &OpsState, wanted: Option<&str>, kind: Kind) -> Vec<Alert> {
    let enrollment = EnrollmentFacts {
        server_not_after: Some(UNIX + 365 * 24 * 3600),
        wanted: wanted
            .map(|at| BTreeMap::from([(LEGACY.to_owned(), version(at))]))
            .unwrap_or_default(),
        ..EnrollmentFacts::default()
    };
    let at = At {
        now: secs(1_000),
        unix: UNIX + 1_000,
    };
    derive(
        fleet,
        ops,
        &kbf_services(),
        &enrollment,
        at,
        &Thresholds::default(),
    )
    .into_iter()
    .filter(|alert| alert.subject.kind == kind)
    .collect()
}

fn subject(kind: Kind, host: &str, version: &str) -> Subject {
    Subject {
        service: LEGACY.to_owned(),
        host: host.to_owned(),
        version: version.to_owned(),
        ..Subject::of(kind)
    }
}

/// Proves: a host whose last report of kbf is a failure of its desired version is one
/// `failed_update` alert, with the full message, the report's reason and detail and its try, and
/// a host that failed with no detail says none; a host that runs the version, or reports nothing,
/// raises none.
///
/// Catches: a failed update nobody is told of, or one whose reason the operator must look up host
/// by host.
#[test]
fn a_failed_update_of_the_desired_version_is_an_alert_with_what_the_host_said() {
    let fleet = fleet_of(vec![
        (
            "d1",
            failed(
                "1.0",
                "2.0",
                "config_missing",
                Some("there is no tierstore.toml"),
            ),
        ),
        ("d2", failed("1.0", "2.0", "busy", None)),
        ("d3", beat("2.0")),
        ("d4", beat("1.0")),
    ]);

    assert_eq!(
        of_kind(
            &fleet,
            &OpsState::default(),
            Some("2.0"),
            Kind::FailedUpdate
        ),
        vec![
            Alert {
                subject: subject(Kind::FailedUpdate, "d1", "2.0"),
                message: "d1 failed kbf 2.0: config_missing (\"there is no tierstore.toml\"); it \
                          is not ordered 2.0 again until an operator retries it (Hosts.Retry), \
                          holds or excludes it, or another version is set"
                    .to_owned(),
                said: Said {
                    reason: "config_missing".to_owned(),
                    detail: "there is no tierstore.toml".to_owned(),
                    tries: 2,
                    since_unix: 0,
                },
            },
            Alert {
                subject: subject(Kind::FailedUpdate, "d2", "2.0"),
                message: "d2 failed kbf 2.0: busy; it is not ordered 2.0 again until an operator \
                          retries it (Hosts.Retry), holds or excludes it, or another version is set"
                    .to_owned(),
                said: Said {
                    reason: "busy".to_owned(),
                    detail: String::new(),
                    tries: 2,
                    since_unix: 0,
                },
            },
        ]
    );
}

/// Proves: a failed update stops being an alert when it no longer stands: another version is
/// wanted, none is, the host reports a success, or an operator excluded the host from the service.
/// A failure that names no reason says `unspecified`.
///
/// Catches: an alert that outlives its failure, or a failure of an old version alerting forever.
#[test]
fn a_failed_update_clears_when_the_failure_no_longer_stands() {
    let fleet = fleet_of(vec![("d1", failed("1.0", "2.0", "no_such_token!", None))]);
    let ops = OpsState::default();
    let alerts = of_kind(&fleet, &ops, Some("2.0"), Kind::FailedUpdate);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].said.reason, "unspecified");

    assert_eq!(
        of_kind(&fleet, &ops, Some("3.0"), Kind::FailedUpdate),
        vec![]
    );
    assert_eq!(of_kind(&fleet, &ops, None, Kind::FailedUpdate), vec![]);

    let mut succeeded = beat("2.0");
    succeeded.last_update = Some(UpdateResult::new(version("2.0"), Outcome::Succeeded));
    let reached = fleet_of(vec![("d1", succeeded)]);
    assert_eq!(
        of_kind(&reached, &ops, Some("2.0"), Kind::FailedUpdate),
        vec![]
    );

    let mut excluded = OpsState::default();
    excluded.excluded.push(Exclusion {
        service: LEGACY.to_owned(),
        node_id: "d1".to_owned(),
        reason: "host gone".to_owned(),
    });
    let mut fleet = fleet;
    fleet.set_marks(marks_of(&excluded));
    assert_eq!(
        of_kind(&fleet, &excluded, Some("2.0"), Kind::FailedUpdate),
        vec![]
    );
}

/// Proves: a `halted` refusal, which a host that holds a halt of its own answers every order
/// with, is no failed update: the host's halt is the alert (`host_halt`).
///
/// Catches: one stopped host listed twice, once as a failure it never ran.
#[test]
fn a_halted_refusal_is_the_hosts_halt_and_no_failed_update() {
    let mut refused = halted(3, Some(60));
    refused.last_update = Some(UpdateResult {
        reason: Token::parse("halted"),
        ..UpdateResult::new(version("2.0"), Outcome::Failed)
    });
    let fleet = fleet_of(vec![("d1", refused)]);
    let ops = OpsState::default();

    assert_eq!(
        of_kind(&fleet, &ops, Some("2.0"), Kind::FailedUpdate),
        vec![]
    );
    assert_eq!(of_kind(&fleet, &ops, Some("2.0"), Kind::HostHalt).len(), 1);
}

/// Proves: a host that holds a halt of its own is one `host_halt` alert whether or not the
/// service has a desired version, with the full message, the halt's reason, and when the host
/// wrote it (the observation's unix time less the age it reports); one under a generation above
/// any the coordinator issued says it is a halt of an earlier coordinator; one that reports no age
/// says none and has no time.
///
/// Catches: a halt a host keeps for hours after its cause is gone, which nobody is told of, or
/// told of with no way to see how old it is.
#[test]
fn a_hosts_own_halt_is_an_alert_with_its_reason_and_age() {
    let fleet = fleet_of(vec![
        ("d1", halted(3, Some(9 * 3_600 + 120))),
        ("d2", halted(7, Some(45))),
        ("d3", halted(1, None)),
        ("d4", beat("1.0")),
    ]);
    let mut ops = OpsState::default();
    ops.services.insert(
        LEGACY.to_owned(),
        ServiceOps {
            halt_generation: 5,
            ..ServiceOps::default()
        },
    );

    assert_eq!(
        of_kind(&fleet, &ops, None, Kind::HostHalt),
        vec![
            Alert {
                subject: subject(Kind::HostHalt, "d1", "1.0"),
                message: "d1 holds its own halt of kbf (tainted, generation 3, written 9 h 2 \
                          min ago): it refuses every order of the service until an operator \
                          clears the halt (Releases.ClearHalt)"
                    .to_owned(),
                said: Said {
                    reason: "tainted".to_owned(),
                    detail: String::new(),
                    tries: 0,
                    since_unix: UNIX + 1_000 - (9 * 3_600 + 120),
                },
            },
            Alert {
                subject: subject(Kind::HostHalt, "d2", "1.0"),
                message: "d2 holds its own halt of kbf (tainted, generation 7, written 45 s \
                          ago); its generation is above any this coordinator issued, so it is a \
                          halt of an earlier coordinator, whose cause may be gone: it refuses \
                          every order of the service until an operator clears the halt \
                          (Releases.ClearHalt)"
                    .to_owned(),
                said: Said {
                    reason: "tainted".to_owned(),
                    detail: String::new(),
                    tries: 0,
                    since_unix: UNIX + 1_000 - 45,
                },
            },
            Alert {
                subject: subject(Kind::HostHalt, "d3", "1.0"),
                message: "d3 holds its own halt of kbf (tainted, generation 1): it refuses \
                          every order of the service until an operator clears the halt \
                          (Releases.ClearHalt)"
                    .to_owned(),
                said: Said {
                    reason: "tainted".to_owned(),
                    detail: String::new(),
                    tries: 0,
                    since_unix: 0,
                },
            },
        ]
    );
}

/// Proves: the age of a host's halt is said in its two largest units, from seconds to days.
///
/// Catches: a nine-hour halt said as 32400 seconds, or an age cut to the wrong unit.
#[test]
fn the_age_of_a_halt_is_said_in_its_largest_units() {
    for (age, said) in [
        (0, "0 s"),
        (59, "59 s"),
        (60, "1 min"),
        (3_599, "59 min"),
        (3_600, "1 h 0 min"),
        (86_399, "23 h 59 min"),
        (86_400, "1 d 0 h"),
        (3 * 86_400 + 5 * 3_600 + 7, "3 d 5 h"),
    ] {
        let fleet = fleet_of(vec![("d1", halted(1, Some(age)))]);
        let alerts = of_kind(&fleet, &OpsState::default(), None, Kind::HostHalt);
        assert!(
            alerts[0].message.contains(&format!("written {said} ago")),
            "{age}: {}",
            alerts[0].message
        );
    }
}

/// Proves: the three kinds of a host off its desired version are critical and go out with their
/// log tokens, and `Alerts.List` answers what the host said in the alert's own fields.
///
/// Catches: a failed update listed as a warning that a pager filters out, or its reason only in
/// the message's text.
#[test]
fn both_kinds_go_out_critical_with_what_the_host_said() {
    assert_eq!(
        (Kind::FailedUpdate.log_token(), Kind::HostHalt.log_token()),
        ("failed_update", "host_halt")
    );
    let fleet = fleet_of(vec![("d1", failed("1.0", "2.0", "busy", Some("held")))]);
    let alert = of_kind(
        &fleet,
        &OpsState::default(),
        Some("2.0"),
        Kind::FailedUpdate,
    )
    .pop()
    .expect("an alert");
    let listed = super::alert_proto(Observed {
        alert,
        first_unix: UNIX + 900,
    });
    assert_eq!(
        (
            listed.kind,
            listed.severity,
            listed.host_id.as_str(),
            listed.version.as_str(),
            listed.reason.as_str(),
            listed.detail.as_str(),
            listed.tries,
            listed.since_unix,
            listed.first_observed_unix,
        ),
        (
            i32::from(proto::AlertKind::FailedUpdate),
            i32::from(proto::AlertSeverity::Critical),
            "d1",
            "2.0",
            "busy",
            "held",
            2,
            0,
            UNIX + 900,
        )
    );
    assert_eq!(Kind::HostHalt.proto(), proto::AlertKind::HostHalt);
    assert_eq!(Kind::HostHalt.severity(), proto::AlertSeverity::Critical);
    assert_eq!(
        (
            Kind::RepeatedDeferral.log_token(),
            Kind::RepeatedDeferral.proto(),
            Kind::RepeatedDeferral.severity(),
        ),
        (
            "repeated_deferral",
            proto::AlertKind::RepeatedDeferral,
            proto::AlertSeverity::Critical,
        )
    );
}

/// Proves: a host's own halt that names no reason is listed with the reason `unspecified`.
///
/// Catches: a halt with no reason dropped from the alerts, or listed with an empty reason a
/// pager cannot match.
#[test]
fn a_hosts_own_halt_with_no_reason_says_unspecified() {
    let mut heartbeat = halted(0, Some(5));
    heartbeat.local_halt.as_mut().expect("a halt").reason = None;
    let fleet = fleet_of(vec![("d1", heartbeat)]);
    let alerts = of_kind(&fleet, &OpsState::default(), None, Kind::HostHalt);
    assert_eq!(
        alerts
            .iter()
            .map(|alert| (alert.said.reason.as_str(), alert.message.as_str()))
            .collect::<Vec<_>>(),
        vec![(
            "unspecified",
            "d1 holds its own halt of kbf (unspecified, generation 0, written 5 s ago): it \
             refuses every order of the service until an operator clears the halt \
             (Releases.ClearHalt)"
        )]
    );
}

/// Proves: a host whose last heartbeat no longer lists the service raises neither kind for it,
/// whatever the fleet kept of the service from an earlier beat.
///
/// Catches: an alert of a service the host dropped, which nobody can clear.
#[test]
fn a_service_the_host_no_longer_lists_raises_no_alert() {
    let mut heartbeat = failed("1.0", "2.0", "busy", None);
    heartbeat.local_halt = halted(0, Some(5)).local_halt;
    let mut fleet = fleet_of(vec![("d1", heartbeat)]);
    let mut gone = node();
    gone.announced.clear();
    fleet.note_node(id("d1"), gone, UNIX);
    let ops = OpsState::default();

    assert_eq!(
        of_kind(&fleet, &ops, Some("2.0"), Kind::FailedUpdate),
        vec![]
    );
    assert_eq!(of_kind(&fleet, &ops, Some("2.0"), Kind::HostHalt), vec![]);
}
