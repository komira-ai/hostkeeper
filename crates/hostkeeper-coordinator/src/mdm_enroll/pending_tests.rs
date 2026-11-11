//! The list of waiting requests, the declined serials and the limit on a peer.

use std::net::IpAddr;

use super::pending::{Held1, Pending, RateLimit};

fn hold(pending: &Pending, serial: &str, udid: &str, now: i64, max: usize) -> Held1 {
    pending.hold(serial, udid, Some("Mac14,13"), "why", now, max)
}

/// Proves: a serial that posts again is one row whose count grows, whose first time stays and whose
/// UDID and product follow the last post; a serial the list does not hold is new.
///
/// Catches: a row per post, which a Mac's retries would grow without bound.
#[test]
fn a_serial_that_posts_again_is_one_row() {
    let pending = Pending::default();
    assert_eq!(hold(&pending, "A1", "u1", 10, 8), Held1::New);
    assert_eq!(pending.hold("A1", "u2", None, "again", 20, 8), Held1::Seen);
    let rows = pending.list();
    assert_eq!(rows.len(), 1);
    let (serial, row) = &rows[0];
    assert_eq!(serial, "A1");
    assert_eq!((row.first_unix, row.last_unix, row.count), (10, 20, 2));
    assert_eq!((row.udid.as_str(), row.reason.as_str()), ("u2", "again"));
    assert_eq!(row.product, None);
}

/// Proves: with the list full, a new serial replaces the one with the oldest last post that no
/// operator confirmed, and when every row is confirmed it is refused as full.
///
/// Catches: an attacker's serials pushing out the request an operator is about to click, and a
/// list that grows without a bound.
#[test]
fn a_full_list_drops_the_oldest_unconfirmed_row_and_never_a_confirmed_one() {
    let pending = Pending::default();
    hold(&pending, "A1", "u", 10, 2);
    hold(&pending, "A2", "u", 20, 2);
    assert_eq!(hold(&pending, "A3", "u", 30, 2), Held1::New);
    let serials: Vec<String> = pending.list().into_iter().map(|(s, _)| s).collect();
    assert_eq!(serials, ["A2", "A3"]);
    pending.confirm("A2", 1_000);
    pending.confirm("A3", 1_000);
    assert_eq!(hold(&pending, "A4", "u", 40, 2), Held1::Full);
    // A confirmation that ran out is a row like any other.
    assert_eq!(hold(&pending, "A5", "u", 2_000, 2), Held1::New);
    let serials: Vec<String> = pending.list().into_iter().map(|(s, _)| s).collect();
    assert_eq!(serials, ["A3", "A5"]);
}

/// Proves: a confirmation is made for a serial that waits and for no other, is spent once by the
/// UDID that waited before its time, and is not moved to another UDID by a later post.
///
/// Catches: a confirmation that admits any Mac that claims the serial, or a second time.
#[test]
fn a_confirmation_is_spent_once_by_the_udid_that_waited() {
    let pending = Pending::default();
    assert_eq!(pending.confirm("A1", 100), None);
    hold(&pending, "A1", "u1", 10, 8);
    let row = pending.confirm("A1", 100).expect("the row waits");
    assert_eq!(row.confirmed_until, Some(100));
    // A post of another UDID leaves a confirmed row as it is.
    assert_eq!(pending.hold("A1", "u2", None, "x", 50, 8), Held1::Seen);
    assert_eq!(pending.list()[0].1.udid, "u1");
    assert!(!pending.spend("A1", "u2", 60));
    assert!(
        !pending.spend("A1", "u1", 100),
        "the time itself is past it"
    );
    assert!(pending.spend("A1", "u1", 99));
    assert!(!pending.spend("A1", "u1", 99), "once");
    assert!(pending.list().is_empty());
    assert!(!pending.spend("nobody", "u1", 0));
}

/// Proves: a confirmation that ran out does not stop the row following the Mac's UDID again.
///
/// Catches: a stale confirmation that pins a row to the first UDID forever.
#[test]
fn an_expired_confirmation_lets_the_row_follow_the_udid() {
    let pending = Pending::default();
    hold(&pending, "A1", "u1", 10, 8);
    pending.confirm("A1", 100);
    pending.hold("A1", "u2", None, "x", 200, 8);
    let row = &pending.list()[0].1;
    assert_eq!((row.udid.as_str(), row.confirmed_until), ("u2", None));
}

/// Proves: a serial's decline is audited the first time and again only after the window, and the
/// table of them forgets the oldest past its bound.
///
/// Catches: one audit row per post of a Mac that is not ours, which fills the audit.
#[test]
fn a_decline_is_audited_once_a_window() {
    let pending = Pending::default();
    assert!(pending.first_decline("A1", 0, 60, 2));
    assert!(!pending.first_decline("A1", 59, 60, 2));
    assert!(pending.first_decline("A1", 60, 60, 2));
    assert!(pending.first_decline("A2", 61, 60, 2));
    assert!(
        pending.first_decline("A3", 62, 60, 2),
        "A1 was the oldest and goes"
    );
    assert!(
        pending.first_decline("A1", 63, 60, 2),
        "so it is first again"
    );
}

/// Proves: a peer is counted per minute and per address, the count starts again with the minute,
/// and requests with no address share one count.
///
/// Catches: a limit that one noisy address uses up for the others, or never lifts.
#[test]
fn a_peer_is_limited_per_minute() {
    let limit = RateLimit::default();
    let (a, b): (IpAddr, IpAddr) = ("192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap());
    assert!(limit.allow(Some(a), 0, 2));
    assert!(limit.allow(Some(a), 10, 2));
    assert!(!limit.allow(Some(a), 20, 2));
    assert!(limit.allow(Some(b), 20, 2));
    assert!(limit.allow(Some(a), 60, 2), "the next minute");
    assert!(limit.allow(None, 61, 1));
    assert!(!limit.allow(None, 62, 1));
}
