//! The fleet's side of `Hosts.Retry` ([`crate::fleet::retry`]): when a report set aside is taken
//! out of a heartbeat and when it stops being, what a call with nothing to set aside does, the
//! order the fleet shows as a host's current one, what a forgotten host leaves, and that a
//! failure an operator acknowledged keeps its ack while its report is set aside. Every time is an
//! explicit duration; the fleet is built directly.

use std::time::Duration;

use hostkeeper_wire::wire::{Health, Heartbeat, Outcome, ServiceBeat, UpdateResult};
use hostkeeper_wire::{HostId, Platform};

use crate::acks::stale_acks;
use crate::fleet::retry::Outstanding;
use crate::fleet::{Fleet, ORDER_AGE};
use crate::marks_support::{WEB, runs};
use crate::ops::{FailureAck, ServiceOps};
use crate::reconcile::Change;
use crate::testutil::{id, role, version};

/// A failed update of `web` to 2.0 at the try `order_try`.
fn failed(order_try: u64) -> UpdateResult {
    let mut report = UpdateResult::new(version("2.0"), Outcome::Failed);
    report.order_try = Some(order_try);
    report
}

/// `web` at 1.0, with `last` as its verdict, updating to 2.0 when `updating`.
fn web(last: Option<UpdateResult>, updating: bool) -> Heartbeat {
    let mut one: ServiceBeat = runs(WEB, "1.0");
    one.last_update = last;
    one.updating = updating.then(|| version("2.0"));
    one.as_heartbeat(Platform::LinuxX86_64)
}

/// A fleet that heard `beat` of `web` from `d1` at one second.
fn heard(beat: Heartbeat) -> (Fleet, HostId) {
    let mut fleet = Fleet::new(Duration::ZERO, Duration::ZERO);
    let host = id("d1");
    fleet.record_in(WEB, host.clone(), role("daemon"), beat, secs(1));
    (fleet, host)
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// The report the fleet keeps of `web` from `host`.
fn kept(fleet: &Fleet, host: &HostId) -> Option<UpdateResult> {
    fleet
        .get_in(WEB, host)
        .and_then(|h| h.beat.last_update.clone())
}

/// Proves: a report set aside leaves the kept beat at once and is taken out of each later beat
/// that carries it, updating or not, while its try still moves the next one past it; a beat with
/// no report keeps it set aside; the same report heard again once the host was seen updating is
/// the new order's verdict, kept, and ends the setting aside; and any other report ends it too.
///
/// Catches: a stale report that comes back into the beat (`failed_here` again), a try that goes
/// back to 1 once the report is taken out, a host whose new failure equal to the old one (a
/// daemon that names no try) is set aside for ever, and a setting aside that outlives a new
/// report.
#[test]
fn a_report_is_taken_out_of_each_beat_until_the_host_reports_again() {
    let mut old = UpdateResult::new(version("2.0"), Outcome::Failed);
    old.order_try = None;
    let (mut fleet, host) = heard(web(Some(old.clone()), false));
    assert_eq!(fleet.retryable(WEB, &host), Some((old.clone(), false)));
    fleet.set_aside(WEB, &host);
    assert_eq!(kept(&fleet, &host), None);
    assert_eq!(fleet.set_aside_of(WEB, &host), Some(&old));
    assert_eq!(fleet.retryable(WEB, &host), Some((old.clone(), true)));

    let mut hear = |beat: Heartbeat| {
        fleet.record_in(WEB, host.clone(), role("daemon"), beat, secs(2));
        (kept(&fleet, &host), fleet.set_aside_of(WEB, &host).cloned())
    };
    assert_eq!(
        hear(web(Some(old.clone()), false)),
        (None, Some(old.clone()))
    );
    assert_eq!(hear(web(None, false)), (None, Some(old.clone())));
    assert_eq!(
        hear(web(Some(old.clone()), true)),
        (None, Some(old.clone()))
    );
    assert_eq!(hear(web(None, true)), (None, Some(old.clone())));
    assert_eq!(
        hear(web(Some(old.clone()), false)),
        (Some(old.clone()), None),
        "heard again after updating: the new order's verdict"
    );

    let (mut fleet, host) = heard(web(Some(failed(3)), false));
    fleet.set_aside(WEB, &host);
    assert_eq!(fleet.next_try(WEB, &host), 4);
    fleet.record_in(
        WEB,
        host.clone(),
        role("daemon"),
        web(Some(failed(3)), false),
        secs(2),
    );
    assert_eq!(fleet.next_try(WEB, &host), 4, "its try still counts");
    fleet.record_in(WEB, host.clone(), role("daemon"), web(None, true), secs(3));
    assert_eq!(fleet.set_aside_of(WEB, &host), Some(&failed(3)));
    fleet.record_in(
        WEB,
        host.clone(),
        role("daemon"),
        web(Some(failed(4)), false),
        secs(4),
    );
    assert_eq!(fleet.set_aside_of(WEB, &host), None);
    assert_eq!(kept(&fleet, &host), Some(failed(4)));
}

/// Proves: a host whose report is no failure, or whose failure is of the version it runs, has
/// nothing to retry, and setting aside there changes nothing; nor does it for a host or a
/// service the fleet has no word of. An excluded host's failure, kept outside the service's
/// hosts, can be set aside.
///
/// Catches: a success or a running version's failure taken out of the beat, and a host the
/// service no longer counts that can never be retried.
#[test]
fn only_a_failure_of_a_version_the_host_does_not_run_is_set_aside() {
    let done = UpdateResult::new(version("1.0"), Outcome::Succeeded);
    let (mut fleet, host) = heard(web(Some(done.clone()), false));
    assert_eq!(fleet.retryable(WEB, &host), None);
    fleet.set_aside(WEB, &host);
    assert_eq!(kept(&fleet, &host), Some(done));
    assert_eq!(fleet.set_aside_of(WEB, &host), None);
    assert_eq!(fleet.retryable(WEB, &id("d9")), None);
    fleet.set_aside(WEB, &id("d9"));
    assert_eq!(fleet.retryable("db", &host), None);

    let (fleet, host) = heard(web(None, false));
    assert_eq!(fleet.retryable(WEB, &host), None);

    let mut runs_it: ServiceBeat = runs(WEB, "2.0");
    runs_it.health = Health::Unhealthy;
    runs_it.last_update = Some(failed(1));
    let (fleet, host) = heard(runs_it.as_heartbeat(Platform::LinuxX86_64));
    assert_eq!(fleet.retryable(WEB, &host), None);
    let mut staged: ServiceBeat = runs(WEB, "2.0");
    staged.health = Health::Absent;
    staged.last_update = Some(failed(1));
    let (fleet, host) = heard(staged.as_heartbeat(Platform::LinuxX86_64));
    assert_eq!(fleet.retryable(WEB, &host), Some((failed(1), false)));

    let (mut fleet, host) = heard(web(Some(failed(1)), false));
    fleet.forget_in(WEB, &host);
    assert!(fleet.get_in(WEB, &host).is_none(), "kept outside");
    assert_eq!(fleet.retryable(WEB, &host), Some((failed(1), false)));
    fleet.set_aside(WEB, &host);
    assert_eq!(fleet.set_aside_of(WEB, &host), Some(&failed(1)));
}

/// Proves: the current order shows its try and target, the id the coordinator named for that
/// target (none when it named none, or named it for another target), an uninstall's own id, and
/// nothing once the order has aged out.
///
/// Catches: an order shown with the id of an earlier order to another version, an uninstall
/// shown as an update, and an aged-out order still shown as current.
#[test]
fn the_current_order_shows_its_id_try_and_target_until_it_ages_out() {
    let (mut fleet, host) = heard(web(None, false));
    assert_eq!(fleet.outstanding(WEB, &host, secs(1)), None);
    fleet.name_order(WEB, &host, &version("3.0"), "update-old".to_owned());
    fleet.order(WEB, host.clone(), version("2.0"), secs(1));
    let shown = |order_id: Option<&str>| Outstanding {
        order_id: order_id.map(str::to_owned),
        order_try: 1,
        target: version("2.0"),
        uninstall: false,
    };
    assert_eq!(fleet.outstanding(WEB, &host, secs(1)), Some(shown(None)));
    fleet.name_order(WEB, &host, &version("2.0"), "update-new".to_owned());
    assert_eq!(
        fleet.outstanding(WEB, &host, secs(1)),
        Some(shown(Some("update-new")))
    );
    assert_eq!(
        fleet.outstanding(WEB, &host, secs(1) + ORDER_AGE),
        None,
        "aged out"
    );

    let (mut fleet, host) = heard(web(None, false));
    fleet.name_order(WEB, &host, &version("1.0"), "update-x".to_owned());
    fleet.order_uninstall(
        WEB,
        host.clone(),
        version("1.0"),
        "uninstall-4".to_owned(),
        secs(1),
    );
    assert_eq!(
        fleet.outstanding(WEB, &host, secs(1)),
        Some(Outstanding {
            order_id: Some("uninstall-4".to_owned()),
            order_try: 1,
            target: version("1.0"),
            uninstall: true,
        })
    );
}

/// Proves: a host the fleet forgets leaves nothing set aside or named behind, so a host that
/// comes back with the same id is not ordered again over its own report.
///
/// Catches: a setting aside that outlives the host it was for.
#[test]
fn a_forgotten_host_leaves_nothing_set_aside() {
    let (mut fleet, host) = heard(web(Some(failed(1)), false));
    fleet.set_aside(WEB, &host);
    fleet.name_order(WEB, &host, &version("2.0"), "update-new".to_owned());
    assert!(fleet.release_if_final(&host, secs(2)));
    fleet.record_in(
        WEB,
        host.clone(),
        role("daemon"),
        web(Some(failed(1)), false),
        secs(3),
    );
    assert_eq!(fleet.set_aside_of(WEB, &host), None);
    assert_eq!(kept(&fleet, &host), Some(failed(1)));
    fleet.order(WEB, host.clone(), version("2.0"), secs(3));
    assert_eq!(
        fleet
            .outstanding(WEB, &host, secs(3))
            .and_then(|o| o.order_id),
        None
    );
}

/// Proves: a failure an operator acknowledged keeps its ack while its report is set aside (the
/// host still makes that report; only the rules no longer read it), and the ack is dropped once
/// the host reports anything else.
///
/// Catches: an ack dropped because the report was set aside, which a restart of the coordinator,
/// forgetting what was set aside, would turn back into a failed host that holds the plan.
#[test]
fn an_ack_stands_while_its_report_is_set_aside() {
    let (mut fleet, host) = heard(web(Some(failed(1)), false));
    let marks = ServiceOps {
        failure_acked: vec![FailureAck {
            node_id: "d1".to_owned(),
            report: failed(1),
        }],
        ..ServiceOps::default()
    };
    assert_eq!(stale_acks(WEB, &marks, &fleet), Vec::<Change>::new());
    fleet.set_aside(WEB, &host);
    assert_eq!(stale_acks(WEB, &marks, &fleet), Vec::<Change>::new());
    fleet.record_in(
        WEB,
        host.clone(),
        role("daemon"),
        web(Some(failed(2)), false),
        secs(2),
    );
    assert_eq!(
        stale_acks(WEB, &marks, &fleet),
        vec![Change::DropAck("d1".to_owned())]
    );
}

/// Proves: for a report set aside that names no try, a second order given since (the first went
/// unanswered and aged out) makes the same report, heard again, the new order's verdict: it is
/// kept and the setting aside ends. For a report that names its try, the same sequence leaves it
/// set aside: the new try's verdict would differ from it.
///
/// Catches: a daemon that names no try and fails the new order before a heartbeat says it is
/// updating, whose failure is set aside for ever and which is ordered again at every order age.
#[test]
fn a_report_with_no_try_ends_once_a_second_order_was_given() {
    let mut no_try = failed(1);
    no_try.order_try = None;
    for (report, ended) in [(no_try, true), (failed(1), false)] {
        let (mut fleet, host) = heard(web(Some(report.clone()), false));
        fleet.set_aside(WEB, &host);
        let order = |fleet: &mut Fleet, at: Duration| {
            fleet.order(WEB, host.clone(), version("2.0"), at);
            fleet.name_order(WEB, &host, &version("2.0"), "update-x".to_owned());
        };
        order(&mut fleet, secs(1));
        order(&mut fleet, secs(2));
        fleet.record_in(
            WEB,
            host.clone(),
            role("daemon"),
            web(Some(report.clone()), false),
            secs(3),
        );
        assert!(
            fleet.set_aside_of(WEB, &host).is_some(),
            "a resend is no second order"
        );
        order(&mut fleet, secs(1) + ORDER_AGE);
        fleet.record_in(
            WEB,
            host.clone(),
            role("daemon"),
            web(Some(report.clone()), false),
            secs(200),
        );
        assert_eq!(
            fleet.set_aside_of(WEB, &host).is_none(),
            ended,
            "{report:?}"
        );
        assert_eq!(kept(&fleet, &host).is_some(), ended, "{report:?}");
    }
}
