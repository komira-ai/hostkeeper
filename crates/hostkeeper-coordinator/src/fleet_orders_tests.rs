//! When an order holds its host's update slot ([`crate::fleet::Fleet::order_holds`]): within
//! `ORDER_AGE`, and past it while the host is silent; and what ends it.

use std::time::Duration;

use hostkeeper_wire::Platform;
use hostkeeper_wire::wire::Health;

use crate::fleet::{DOWN_AFTER, Fleet, Node, ORDER_AGE, QUIET_START};
use crate::store::decision::{OrderRow, Rows};
use crate::testutil::{beat, id, role, secs, version};

/// When `s1` was ordered.
const T: Duration = Duration::from_secs(100);

/// A fleet that heard `s1` running 1.0 of `web` at `T` and ordered it 2.0 then.
fn ordered() -> Fleet {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    fleet.record_in("web", id("s1"), role("storage"), beat("1.0"), T);
    fleet.order("web", id("s1"), version("2.0"), T);
    fleet
}

/// Whether the order of `s1` holds at `now`, as each reader of it says, all agreeing.
fn held(fleet: &Fleet, now: Duration) -> bool {
    let host = fleet.get_in("web", &id("s1")).expect("s1 is heard");
    let ordered = fleet.is_ordered("web", &id("s1"), now);
    assert_eq!(fleet.holds_slot("web", &id("s1"), host, now), ordered);
    assert_eq!(fleet.is_up("web", &id("s1"), host, now), !ordered);
    assert_eq!(fleet.is_busy_besides("other", &id("s1"), now), ordered);
    assert_eq!(fleet.outstanding("web", &id("s1"), now).is_some(), ordered);
    ordered
}

/// Proves: an ordered host that sends nothing after the order keeps its slot at `ORDER_AGE` and
/// long after (it is not up, busy with that update for any other service, and its order is still
/// outstanding); one heard idle within `DOWN_AFTER` of `now` frees it at `ORDER_AGE`, and not a
/// second before; one last heard exactly `DOWN_AFTER` before `now` is silent and keeps it.
///
/// Catches: an order freed by its age alone, so that a host that heard it and started, and whose
/// beats are lost, has its slot given to a second host (two updates live in the farm); or a
/// liveness bound off by one second either way.
#[test]
fn a_silent_ordered_host_keeps_its_slot_past_order_age_and_a_beating_one_frees_it() {
    let silent = ordered();
    assert!(held(&silent, T + ORDER_AGE - secs(1)));
    assert!(held(&silent, T + ORDER_AGE), "silent at its age");
    assert!(held(&silent, T + secs(900)), "silent long after");

    let at = T + ORDER_AGE;
    let mut beating = ordered();
    beating.record_in("web", id("s1"), role("storage"), beat("1.0"), T + secs(60));
    assert!(
        held(&beating, T + secs(60) + DOWN_AFTER),
        "silent since 60 s"
    );
    beating.record_in(
        "web",
        id("s1"),
        role("storage"),
        beat("1.0"),
        at - DOWN_AFTER + secs(1),
    );
    assert!(held(&beating, at - secs(1)), "beating, within its age");
    assert!(!held(&beating, at), "beating, at its age");

    let mut quiet_since = ordered();
    quiet_since.record_in(
        "web",
        id("s1"),
        role("storage"),
        beat("1.0"),
        at - DOWN_AFTER,
    );
    assert!(held(&quiet_since, at), "last heard DOWN_AFTER ago");
}

/// Proves: any word of the host counts as heard: its beat of another service, a beat of the
/// service the service does not count (`record_outside_in`), or its node's heartbeat alone; an
/// order to install a service the host does not list (`ordered_unseen`) holds past `ORDER_AGE`
/// while its node is silent and frees once the node beats.
///
/// Catches: a liveness read of the ordered service's own beat only, which keeps the slot of a host
/// that beats every other service and never heard the order (the farm's one slot held for good),
/// or frees the install order of a silent host.
#[test]
fn any_word_of_the_host_counts_as_heard() {
    let at = T + ORDER_AGE;
    let mut other = ordered();
    other.record_in("other", id("s1"), role("storage"), beat("1.0"), at);
    assert!(!held(&other, at), "a beat of another service");

    let mut outside = ordered();
    outside.record_outside_in("other", id("s1"), "storage", beat("1.0"), at);
    assert!(
        !held(&outside, at),
        "a beat the other service does not count"
    );

    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    fleet.note_node(id("s1"), node(T), 0);
    fleet.order("web", id("s1"), version("2.0"), T);
    let unseen = |fleet: &Fleet, now| {
        fleet
            .ordered_unseen("web", now)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(unseen(&fleet, at), [id("s1")], "its node is silent");
    assert_eq!(unseen(&fleet, T + secs(900)), [id("s1")]);
    fleet.note_node(id("s1"), node(at), 0);
    assert!(unseen(&fleet, at).is_empty(), "its node beats");
    assert!(!fleet.is_ordered("web", &id("s1"), at));
}

/// Proves: an order to a host the fleet has never heard holds for `ORDER_AGE` and no longer.
///
/// Catches: an order with no word of its host held for ever, which nothing but an abandon would
/// end.
#[test]
fn an_order_to_a_host_never_heard_holds_for_its_age() {
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    fleet.order("web", id("s1"), version("2.0"), T);
    assert!(fleet.is_ordered("web", &id("s1"), T + ORDER_AGE - secs(1)));
    assert!(!fleet.is_ordered("web", &id("s1"), T + ORDER_AGE));
}

/// Proves: `Hosts.Exclude` with `abandon` frees the slot of a silent ordered host at once, and
/// answers that it held it.
///
/// Catches: a silent host's slot that nothing can free once its order no longer ages out.
#[test]
fn abandon_frees_the_slot_of_a_silent_ordered_host() {
    let mut fleet = ordered();
    let now = T + secs(900);
    assert!(fleet.abandon_in("web", &id("s1"), now), "it held the slot");
    assert!(!fleet.is_ordered("web", &id("s1"), now));
    assert!(fleet.outstanding("web", &id("s1"), now).is_none());
    assert!(!fleet.is_busy_besides("other", &id("s1"), now));
}

/// The database's clock when the rows below were read.
const NOW_UNIX: i64 = 2_000_000_000;

/// The rows of a shared store holding `orders`.
fn rows(orders: Vec<OrderRow>) -> Rows {
    Rows {
        now_unix: NOW_UNIX,
        quiet_until: NOW_UNIX - 1,
        orders,
        ..Rows::default()
    }
}

/// The row of an order of 2.0 of `web` to `host`, given `ago` seconds before the rows were read.
fn row(host: &str, ago: i64) -> OrderRow {
    OrderRow {
        host_id: host.to_owned(),
        service: "web".to_owned(),
        target: "2.0".to_owned(),
        uninstall_id: None,
        at_unix: NOW_UNIX - ago,
        order_try: 1,
    }
}

/// Proves: over a store other coordinators share, the order row of a silent host is kept by a
/// decision past `ORDER_AGE`; once an operator abandons the host the next take of the rows leaves
/// that row out and the decision ends it, and an order given to the host after the abandon is
/// taken in again; a retired host's rows end the same way.
///
/// Catches: an abandon undone by the next read of the rows, so that the order of a host gone for
/// good holds the farm's slot for ever; an abandon that also drops a later order of the host; a
/// decision that ends the row of a silent host's order at its age.
#[test]
fn an_abandoned_or_retired_hosts_order_row_is_ended_by_the_next_decision() {
    let now = secs(1000);
    let sent = std::collections::BTreeMap::new();
    let mut fleet = Fleet::new(Duration::ZERO, QUIET_START);
    fleet.record_in("web", id("s1"), role("storage"), beat("1.0"), secs(400));
    let read = rows(vec![row("s1", 500)]);
    let kept = fleet.take_rows(&read, now);
    assert!(fleet.is_ordered("web", &id("s1"), now), "silent since 400");
    let writes = fleet.writes_since(&kept, &sent, now, NOW_UNIX);
    assert!(writes.ended.is_empty(), "{writes:?}");

    fleet.abandon_in("web", &id("s1"), now);
    let kept = fleet.take_rows(&read, now);
    assert!(!fleet.is_ordered("web", &id("s1"), now), "abandoned");
    let writes = fleet.writes_since(&kept, &sent, now, NOW_UNIX);
    assert_eq!(writes.ended, [row("s1", 500)]);

    fleet.take_rows(&rows(Vec::new()), now);
    fleet.take_rows(&rows(vec![row("s1", 500)]), now);
    assert!(
        fleet.is_ordered("web", &id("s1"), now),
        "the mark went once no row was read"
    );
    fleet.abandon_in("web", &id("s1"), now);
    fleet.take_rows(&rows(vec![row("s1", -10)]), now);
    assert!(
        fleet.is_ordered("web", &id("s1"), now),
        "given after the abandon"
    );

    let mut retired = Fleet::new(Duration::ZERO, QUIET_START);
    retired.record_in("web", id("s2"), role("storage"), beat("1.0"), now);
    let read = rows(vec![row("s2", 500)]);
    retired.take_rows(&read, now);
    assert!(
        retired.release_if_final(&id("s2"), now),
        "its order aged out"
    );
    let kept = retired.take_rows(&read, now);
    let writes = retired.writes_since(&kept, &sent, now, NOW_UNIX);
    assert_eq!(writes.ended, [row("s2", 500)]);
}

/// A node heard at `seen` that announces `web`.
fn node(seen: Duration) -> Node {
    Node {
        role: "storage".to_owned(),
        features: Vec::new(),
        agent: None,
        platform: Platform::LinuxX86_64,
        seen,
        announced: [("web".to_owned(), Health::Healthy)].into(),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root: crate::trusted_root::TrustedRootSaid::default(),
        skewed_beats: 0,
        maintenance: None,
    }
}
