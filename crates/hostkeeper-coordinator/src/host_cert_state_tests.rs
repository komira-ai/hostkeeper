//! What `Hosts.Get` and `Hosts.List` answer of a host's certificate and clock: the
//! `cert_not_after_unix` and `last_renew_error` of its last heartbeat, and the count of its
//! heartbeats whose time was further than the skew bound from the coordinator's wall clock.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::Heartbeat;

use crate::fleet::CLOCK_SKEW_AFTER;
use crate::marks_support::start;
use crate::marks_support::{DB, NEW_DAEMON, WEB, ask, beat_of, operator, rig as marks_rig, runs};
use crate::testutil::{Rig, ops_of};

/// What the ops store's wall clock reads when the coordinator's clock reads 0.
const UNIX: u64 = 1_800_000_000;

/// A rig of `web` and `db` past its quiet start, whose wall clock reads [`UNIX`] on from the
/// coordinator's clock, with heartbeats counted skewed past `skew` (the default when `None`).
fn rig(test: &str, skew: Option<Duration>) -> Rig {
    let mut rig = marks_rig(test, "");
    let seconds = Arc::clone(&rig.clock);
    let unix: crate::audit_file::UnixClock =
        Arc::new(move || UNIX + seconds.load(Ordering::SeqCst));
    let coordinator = start(&rig.dir, &rig.clock, "").with_ops(ops_of(&rig.dir).with_unix(unix));
    rig.coordinator = match skew {
        Some(after) => coordinator.with_clock_skew(after),
        None => coordinator,
    };
    rig
}

/// The coordinator's wall clock now.
fn unix_now(rig: &Rig) -> u64 {
    UNIX + rig.clock.load(Ordering::SeqCst)
}

/// A heartbeat of a daemon running `web` and `db` 1.0 that says the time `sent`.
fn beat(sent: Option<u64>) -> Heartbeat {
    let mut beat = beat_of(NEW_DAEMON, vec![runs(WEB, "1.0"), runs(DB, "1.0")]);
    beat.sent_unix = sent;
    beat
}

fn get(rig: &Rig, node_id: &str) -> proto::Host {
    rig.coordinator
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: node_id.to_owned(),
            },
        )
        .expect("the host")
        .host
        .expect("a host")
}

fn listed(rig: &Rig) -> Vec<proto::Host> {
    rig.coordinator
        .list_hosts(&operator(), proto::ListHostsRequest::default())
        .expect("a page")
        .hosts
}

/// What `Hosts.Get` says of the host's certificate and clock.
fn said(host: &proto::Host) -> (Option<u64>, Option<&str>, u64) {
    (
        host.cert_not_after_unix,
        host.last_renew_error.as_deref(),
        host.clock_skew_beats,
    )
}

/// Catches a certificate end or renewal error not stored, stored from another host, kept past a
/// heartbeat that no longer says it, or answered by one call and not the other: each host's last
/// heartbeat is what `Hosts.Get` and `Hosts.List` answer, and a heartbeat without the fields
/// (a host whose renewal succeeded, or an agent that predates them) answers none.
#[test]
fn get_and_list_answer_the_certificate_of_the_last_heartbeat() {
    let rig = rig("cert-state-get", None);
    let mut failing = beat(Some(unix_now(&rig)));
    failing.cert_not_after_unix = Some(UNIX + 3_600);
    failing.last_renew_error = Some("renewal refused: host_excluded".to_owned());
    ask(&rig, "d1", "daemon", failing);
    let mut fine = beat(Some(unix_now(&rig)));
    fine.cert_not_after_unix = Some(UNIX + 604_800);
    ask(&rig, "d2", "daemon", fine);

    let d1 = get(&rig, "d1");
    assert_eq!(
        said(&d1),
        (
            Some(UNIX + 3_600),
            Some("renewal refused: host_excluded"),
            0
        )
    );
    assert_eq!(said(&get(&rig, "d2")), (Some(UNIX + 604_800), None, 0));
    let hosts = listed(&rig);
    assert_eq!(hosts.len(), 2);
    assert_eq!(said(&hosts[0]), said(&d1));
    assert_eq!(said(&hosts[1]), (Some(UNIX + 604_800), None, 0));

    let mut renewed = beat(Some(unix_now(&rig)));
    renewed.cert_not_after_unix = Some(UNIX + 604_800);
    ask(&rig, "d1", "daemon", renewed);
    assert_eq!(said(&get(&rig, "d1")), (Some(UNIX + 604_800), None, 0));
    ask(&rig, "d1", "daemon", beat(None));
    assert_eq!(
        said(&get(&rig, "d1")),
        (None, None, 0),
        "an old agent's beat"
    );
}

/// Catches a skew counted at or inside the bound, not counted past it, counted one way only,
/// counted for a heartbeat that says no time, measured against the coordinator's monotonic
/// clock instead of its wall clock, or not kept from one heartbeat to the next: of the
/// heartbeats at the bound (either way), one second past it (either way), on time, and with no
/// time, two count; and the count moves with the wall clock, not the coordinator's own.
#[test]
fn a_heartbeat_counts_as_skewed_only_past_the_bound_either_way() {
    let rig = rig("cert-state-skew", None);
    let bound = CLOCK_SKEW_AFTER.as_secs();
    assert_eq!(bound, 120);
    let skewed = |rig: &Rig, offset: i64, expected: u64| {
        let sent = unix_now(rig).checked_add_signed(offset).expect("a time");
        ask(rig, "d1", "daemon", beat(Some(sent)));
        assert_eq!(get(rig, "d1").clock_skew_beats, expected, "{offset:+}");
    };
    skewed(&rig, 0, 0);
    skewed(&rig, 120, 0);
    skewed(&rig, -120, 0);
    skewed(&rig, 121, 1);
    skewed(&rig, -121, 2);
    ask(&rig, "d1", "daemon", beat(None));
    assert_eq!(get(&rig, "d1").clock_skew_beats, 2, "no time is no skew");
    // The coordinator's clock moves on; a host whose clock moved with it is on time.
    rig.clock.fetch_add(1_000, Ordering::SeqCst);
    skewed(&rig, 0, 2);
    // Another host's count is its own.
    ask(&rig, "d2", "daemon", beat(Some(UNIX)));
    assert_eq!(get(&rig, "d2").clock_skew_beats, 1, "1000 s behind");
    assert_eq!(get(&rig, "d1").clock_skew_beats, 2);
    let counts: Vec<u64> = listed(&rig).iter().map(|h| h.clock_skew_beats).collect();
    assert_eq!(counts, [2, 1]);
}

/// Catches `--clock-skew-secs` not reaching the count: with a bound of 10 seconds, 10 off is
/// on time and 11 off is skewed.
#[test]
fn the_skew_bound_is_the_one_the_coordinator_is_given() {
    let rig = rig("cert-state-skew-bound", Some(Duration::from_secs(10)));
    ask(&rig, "d1", "daemon", beat(Some(unix_now(&rig) + 10)));
    assert_eq!(get(&rig, "d1").clock_skew_beats, 0);
    ask(&rig, "d1", "daemon", beat(Some(unix_now(&rig) + 11)));
    assert_eq!(get(&rig, "d1").clock_skew_beats, 1);
}

/// The value of `--clock-skew-secs` a `serve` given `extra` reads.
fn skew_flag(extra: &[&str]) -> u64 {
    use clap::Parser as _;

    use crate::config::{Cli, Command};

    let mut args = vec!["hostkeeper-coordinator", "serve"];
    for flag in [
        "--heartbeat-listen",
        "--private-listen",
        "--tls-cert",
        "--tls-key",
        "--client-ca",
        "--state-file",
        "--oidc-config",
        "--operator-token-file",
        "--rollout-file",
    ] {
        args.push(flag);
        args.push(if flag.ends_with("listen") {
            "192.0.2.1:1"
        } else {
            "unread"
        });
    }
    args.extend_from_slice(extra);
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("flags").command else {
        panic!("serve")
    };
    flags.clock_skew_secs
}

/// Catches `--clock-skew-secs` without its default of [`CLOCK_SKEW_AFTER`], or not taken as
/// given: 120 when absent, the number given otherwise.
#[test]
fn the_clock_skew_flag_defaults_to_two_minutes_and_takes_a_value() {
    assert_eq!(skew_flag(&[]), CLOCK_SKEW_AFTER.as_secs());
    assert_eq!(skew_flag(&["--clock-skew-secs", "30"]), 30);
}
