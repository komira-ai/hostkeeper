//! What is kept of a serial console report ([`crate::serial_console_report`]): its rules, the
//! last one kept, and the forgetting at a host's retirement. The operator's view after a gRPC
//! heartbeat is in `grpc/tests/serial_console.rs`.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1::RetireRequest;

use super::{MAX_DETAIL, STATES, SerialConsoleReports, kept};
use crate::first_serving_tests::{APPROVED, FIRST, Fixture, WEB};
use crate::marks_support::{operator, runs, who};
use crate::testutil::{OPERATOR, id};

/// A report of `port` in `state`.
fn report(port: &str, state: &str) -> agent::SerialConsoleReport {
    agent::SerialConsoleReport {
        port: port.to_owned(),
        state: state.to_owned(),
        detail: String::new(),
    }
}

/// A heartbeat that carries `report`.
fn carrying(report: Option<agent::SerialConsoleReport>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        serial_console: report,
        ..agent::HeartbeatRequest::default()
    }
}

/// Proves: a report that keeps the rules is kept as written, for each state of the closed set and
/// each port family, with a state of exactly 32 bytes accepted as `other` and a detail of exactly
/// 128 bytes kept whole.
#[test]
fn a_report_that_keeps_the_rules_is_kept_as_written() {
    for state in STATES {
        for port in ["ttyS0", "ttyS99", "ttyAMA3"] {
            let said = agent::SerialConsoleReport {
                detail: "update_grub".to_owned(),
                ..report(port, state)
            };
            assert_eq!(kept(&said), Some(said.clone()), "{port} {state}");
        }
    }
    let edge = agent::SerialConsoleReport {
        detail: "d".repeat(MAX_DETAIL),
        ..report("ttyS1", "failed")
    };
    assert_eq!(kept(&edge), Some(edge.clone()));
    assert_eq!(
        kept(&report("ttyS1", &"s".repeat(32))).expect("kept").state,
        "other"
    );
}

/// Proves: a state outside the closed set, including the empty one, is kept as `other` and the
/// rest of the report is kept.
#[test]
fn an_unknown_state_is_kept_as_other() {
    for state in ["", "rebooting", "APPLIED"] {
        let held = kept(&report("ttyS1", state)).expect("kept");
        assert_eq!(held, report("ttyS1", "other"), "{state:?}");
    }
}

/// Proves: a detail over 128 bytes is cut at a character boundary, never inside a character.
#[test]
fn a_long_detail_is_cut_at_a_character_boundary() {
    let long = agent::SerialConsoleReport {
        detail: "d".repeat(MAX_DETAIL + 1),
        ..report("ttyS1", "failed")
    };
    assert_eq!(kept(&long).expect("kept").detail, "d".repeat(MAX_DETAIL));
    let wide = agent::SerialConsoleReport {
        detail: format!("{}é", "d".repeat(MAX_DETAIL - 1)),
        ..report("ttyS1", "failed")
    };
    assert_eq!(
        kept(&wide).expect("kept").detail,
        "d".repeat(MAX_DETAIL - 1)
    );
}

/// Proves: a report is dropped for a port that is not `ttyS<n>` or `ttyAMA<n>` with `n` from 0 to
/// 99 written without a leading 0, and for a state of 33 bytes.
#[test]
fn a_report_that_breaks_a_rule_is_dropped() {
    for port in [
        "",
        "ttyS",
        "ttyS100",
        "ttyS01",
        "ttyUSB0",
        "/dev/ttyS0",
        "ttyS0 ",
        "tty0",
    ] {
        assert_eq!(kept(&report(port, "applied")), None, "{port:?}");
    }
    assert_eq!(kept(&report("ttyS0", &"s".repeat(33))), None);
}

/// Proves: a heartbeat that carries a report notes it as the host's last; one that breaks a rule
/// is dropped and counted under `serial_console` and the report kept before stays; a heartbeat
/// that carries none leaves it as it is; another host has none.
#[test]
fn the_last_report_is_kept_and_a_bad_one_is_dropped_counted_and_the_last_one_kept() {
    let f = Fixture::new("serial-console-report-hear");
    let coordinator = &f.rig.coordinator;
    let host = who("node-07", "daemon");
    let reports = &coordinator.serial_console_reports;
    let dropped = || {
        let status = coordinator.status(Some(OPERATOR)).expect("status");
        status.extras_dropped.get("serial_console").copied()
    };
    coordinator.hear_serial_console_report(&host, &carrying(None));
    assert_eq!(reports.report_of(&id("node-07")), None);
    let first = report("ttyS1", "reboot_pending");
    coordinator.hear_serial_console_report(&host, &carrying(Some(first.clone())));
    assert_eq!(reports.report_of(&id("node-07")), Some(first.clone()));
    assert_eq!(dropped(), None);
    coordinator.hear_serial_console_report(&host, &carrying(Some(report("nope", "applied"))));
    assert_eq!(reports.report_of(&id("node-07")), Some(first.clone()));
    assert_eq!(dropped(), Some(1));
    coordinator.hear_serial_console_report(&host, &carrying(None));
    assert_eq!(reports.report_of(&id("node-07")), Some(first));
    let second = report("ttyS1", "applied");
    coordinator.hear_serial_console_report(&host, &carrying(Some(second.clone())));
    assert_eq!(reports.report_of(&id("node-07")), Some(second));
    assert_eq!(reports.report_of(&id("node-08")), None);
}

/// Proves: a forgotten host has no report, and the others keep theirs.
#[test]
fn a_forgotten_host_has_no_report() {
    let reports = SerialConsoleReports::default();
    reports.note(&id("a"), report("ttyS0", "applied"));
    reports.note(&id("b"), report("ttyS0", "applied"));
    reports.forget(&id("a"));
    assert_eq!(reports.report_of(&id("a")), None);
    assert_eq!(
        reports.report_of(&id("b")),
        Some(report("ttyS0", "applied"))
    );
}

/// Proves: the report of a host an operator retires is forgotten with the host, and so is that of
/// a retiring host whose final report frees its slot; the report of a host that stays is kept.
#[test]
fn a_retired_host_has_no_report() {
    let f = Fixture::new("serial-console-report-retire");
    for (n, host) in ["db-1", "db-2", "db-3"].into_iter().enumerate() {
        f.approve(&format!("m-{n}"), host, APPROVED, false);
        f.beat(host, "storage", FIRST, vec![runs(WEB, "1.0")]);
        f.rig.coordinator.hear_serial_console_report(
            &who(host, "storage"),
            &carrying(Some(report("ttyS0", "applied"))),
        );
    }
    let reports = &f.rig.coordinator.serial_console_reports;
    assert!(
        ["db-1", "db-2", "db-3"]
            .iter()
            .all(|h| reports.report_of(&id(h)).is_some())
    );
    f.rig
        .coordinator
        .retire_host(
            &operator(),
            &RetireRequest {
                node_id: "db-1".to_owned(),
                reason: "gone".to_owned(),
            },
        )
        .expect("retired");
    assert_eq!(reports.report_of(&id("db-1")), None);
    f.enrollment
        .enrollment
        .store()
        .deny(&hostkeeper_enrollment::names::HostId::parse("db-2").expect("a host id"))
        .expect("denied");
    assert!(f.rig.coordinator.final_report(
        who("db-2", "storage"),
        crate::first_serving_tests::daemon(vec![runs(WEB, "1.0")])
    ));
    assert_eq!(reports.report_of(&id("db-2")), None);
    assert!(
        reports.report_of(&id("db-3")).is_some(),
        "a host that stays"
    );
}
