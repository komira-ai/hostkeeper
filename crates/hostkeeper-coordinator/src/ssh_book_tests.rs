//! The book's rules, written once over [`World`] and run on the memory book here; the Postgres
//! book passes the same functions (`store/postgres/tests/ssh_book.rs`).

use std::sync::Arc;

use super::{
    ADDRESS_CHANGED, Book, BookError, Cause, Closing, HOSTKEY_CHANGED, HOSTKEY_PINNED, HostKeys,
    Memory, OpenRefused, Opened, Opening, PinRefused, Polled, TUNNEL_CLOSE, TUNNEL_OPEN,
    address_detail, close_detail, keys_detail, open_detail, same_set,
};
use crate::ssh_ledger::{self, CERT_SIGNED, DENIED, Issue, Ledger, REVOKE, Revocation};

/// One line of the audit: who, what, about which host, and the detail.
pub(crate) type Entry = (String, String, Option<String>, String);

/// What the book's tests need beside the book: certificates to open tunnels with, the audit to
/// read, and ways to age rows and to make the audit refuse a write.
pub(crate) trait World {
    fn book(&self) -> &dyn Book;
    /// A certificate for `operator` and `host` that is good for `ttl_secs`; its serial.
    fn issue(&self, operator: &str, host: &str, ttl_secs: u64) -> u64;
    /// Revokes the certificate.
    fn revoke(&self, serial: u64);
    /// Every line of the audit, in order.
    fn lines(&self) -> Vec<Entry>;
    /// The ids of the tunnels the book holds.
    fn tunnel_ids(&self) -> Vec<u64>;
    /// When the host was last heard from.
    fn seen(&self, host: &str) -> Option<u64>;
    /// Moves the host's last heartbeat `secs` back, or forward when `back` is false.
    fn shift_host(&self, host: &str, secs: u64, back: bool);
    /// Moves the time the tunnel was opened and last refreshed `secs` back.
    fn age_tunnel(&self, id: u64, secs: u64);
    /// From now on the audit takes no line, as a database whose audit table fails does.
    fn break_audit(&self);
    /// The audit takes lines again.
    fn fix_audit(&self);
}

pub(crate) fn entry(by: &str, action: &str, subject: &str, detail: impl Into<String>) -> Entry {
    (
        by.to_owned(),
        action.to_owned(),
        Some(subject.to_owned()),
        detail.into(),
    )
}

/// The lines the book wrote: the audit without the ledger's own.
pub(crate) fn book_lines(world: &dyn World) -> Vec<Entry> {
    world
        .lines()
        .into_iter()
        .filter(|line| ![CERT_SIGNED, REVOKE, DENIED].contains(&line.1.as_str()))
        .collect()
}

const HOST: &str = "node-07";

fn keys(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn opening(operator: &str, host: &str, serial: u64, per_operator: u64) -> Opening {
    Opening {
        operator: operator.to_owned(),
        host_id: host.to_owned(),
        serial,
        instance: "inst-a".to_owned(),
        live_secs: 120,
        per_operator,
    }
}

fn closing(id: u64, cause: Cause) -> Closing {
    Closing {
        id,
        cause,
        bytes_up: 100,
        bytes_down: 2000,
    }
}

fn hear(world: &dyn World, host: &str, address: &str, reported: &[&str]) {
    world
        .book()
        .heard(host, address, &keys(reported))
        .expect("heard");
}

/// A certificate for the operator and the host, and a tunnel opened with it, under
/// `per_operator`.
fn open_under(
    world: &dyn World,
    operator: &str,
    host: &str,
    per_operator: u64,
) -> Result<Opened, OpenRefused> {
    let serial = world.issue(operator, host, 3600);
    world
        .book()
        .open(&opening(operator, host, serial, per_operator))
}

fn open_ok(world: &dyn World, operator: &str, host: &str) -> Opened {
    open_under(world, operator, host, 4).expect("opened")
}

/// The detail of a close line without its trailing `seconds_open`, and that number.
fn split_seconds(detail: &str) -> (String, u64) {
    let (head, tail) = detail
        .split_once(",\"seconds_open\":")
        .expect("a close line names the seconds open");
    let seconds = tail.trim_end_matches('}').parse().expect("a number");
    (head.to_owned(), seconds)
}

/// Proves: an open of a good certificate for a host that was heard from records one tunnel, gives
/// its id and the address the host was heard from, and writes the `ssh_tunnel_open` line by the
/// operator for the host, its detail the tunnel, the serial, the address and the instance, by
/// exact text.
///
/// Catches: a tunnel without its line, and a line whose detail names another address or serial.
pub(crate) fn an_open_records_the_tunnel_and_its_line(world: &dyn World) {
    hear(world, HOST, "192.0.2.7", &[]);
    let serial = world.issue("alice", HOST, 3600);
    let opened = world
        .book()
        .open(&opening("alice", HOST, serial, 4))
        .expect("opened");
    assert_eq!(opened.address, "192.0.2.7");
    assert_eq!(world.tunnel_ids(), vec![opened.id]);
    assert_eq!(
        book_lines(world),
        vec![entry(
            "alice",
            TUNNEL_OPEN,
            HOST,
            format!(
                "{{\"tunnel\":{},\"serial\":{serial},\"address\":\"192.0.2.7\",\"instance\":\"inst-a\"}}",
                opened.id
            )
        )]
    );
}

/// Proves: an open is refused for the first defect in this order: a certificate that is none, is
/// another operator's, is for another host, expired or revoked (`BadSerial`, even when the host
/// was never heard from or is stale); a host never heard from (`NoAddress`); a heartbeat older
/// than `live_secs` (`HostNotLive`); and each refusal records nothing and writes no line.
///
/// Catches: a check made out of order (a stale host reported for a certificate that is not the
/// caller's), and a refusal that leaves a tunnel or a line behind.
pub(crate) fn an_open_is_refused_for_the_first_defect_in_the_documented_order(world: &dyn World) {
    let book = world.book();
    assert_eq!(
        book.open(&opening("alice", HOST, 999, 4)),
        Err(OpenRefused::BadSerial)
    );
    let own = world.issue("alice", HOST, 3600);
    assert_eq!(
        book.open(&opening("alice", HOST, own, 4)),
        Err(OpenRefused::NoAddress)
    );
    hear(world, HOST, "192.0.2.7", &[]);
    let bobs = world.issue("bob", HOST, 3600);
    let other_host = world.issue("alice", "node-08", 3600);
    let expired = world.issue("alice", HOST, 0);
    let revoked = world.issue("alice", HOST, 3600);
    world.revoke(revoked);
    for bad in [bobs, other_host, expired, revoked] {
        assert_eq!(
            book.open(&opening("alice", HOST, bad, 4)),
            Err(OpenRefused::BadSerial),
            "serial {bad}"
        );
    }
    world.shift_host(HOST, 500, true);
    assert_eq!(
        book.open(&opening("alice", HOST, bobs, 4)),
        Err(OpenRefused::BadSerial)
    );
    assert_eq!(
        book.open(&opening("alice", HOST, own, 4)),
        Err(OpenRefused::HostNotLive)
    );
    hear(world, HOST, "192.0.2.7", &[]);
    assert!(world.tunnel_ids().is_empty());
    assert!(book_lines(world).is_empty());
    assert!(book.open(&opening("alice", HOST, own, 4)).is_ok());
}

/// Proves: one operator holds one tunnel to a host (a second open to it is `TooMany`) and at most
/// `per_operator` in all (the next to another host is `TooMany`); another operator is not held by
/// either; a stale host is `HostNotLive` before the limit is looked at; and a closed tunnel frees
/// its place.
///
/// Catches: a limit that counts another operator's tunnels, one that lets a second tunnel to a
/// host through, and a limit checked before the host's liveness.
pub(crate) fn the_limits_are_one_tunnel_per_operator_per_host_and_a_count_per_operator(
    world: &dyn World,
) {
    for host in [HOST, "node-08", "node-09"] {
        hear(world, host, "192.0.2.7", &[]);
    }
    let first = open_under(world, "alice", HOST, 2).expect("opened");
    assert_eq!(
        open_under(world, "alice", HOST, 2),
        Err(OpenRefused::TooMany)
    );
    open_under(world, "alice", "node-08", 2).expect("opened");
    assert_eq!(
        open_under(world, "alice", "node-09", 2),
        Err(OpenRefused::TooMany)
    );
    open_under(world, "bob", HOST, 2).expect("another operator's tunnel to the same host");
    assert_eq!(world.tunnel_ids().len(), 3);
    assert_eq!(book_lines(world).len(), 3);

    world.shift_host("node-09", 500, true);
    assert_eq!(
        open_under(world, "alice", "node-09", 2),
        Err(OpenRefused::HostNotLive)
    );
    hear(world, "node-09", "192.0.2.7", &[]);
    world
        .book()
        .close(&closing(first.id, Cause::Eof))
        .expect("closed");
    open_under(world, "alice", "node-09", 2).expect("a place was freed");
    open_under(world, "alice", HOST, 3).expect("and the host is free again");
}

/// Proves: closing a tunnel removes it and writes the `ssh_tunnel_close` line by its operator
/// for its host with the cause, the bytes each way and the seconds it was open (at least those
/// it was aged by), by exact text; closing it again, as a tunnel the book no longer holds, is
/// not an error and writes nothing.
///
/// Catches: a close that leaves the tunnel, one that writes its line twice, and a detail that
/// names the wrong cause or bytes.
pub(crate) fn a_close_removes_the_tunnel_and_writes_its_line_once(world: &dyn World) {
    hear(world, HOST, "192.0.2.7", &[]);
    let opened = open_ok(world, "alice", HOST);
    world.age_tunnel(opened.id, 30);
    world
        .book()
        .close(&closing(opened.id, Cause::Eof))
        .expect("closed");
    assert!(world.tunnel_ids().is_empty());
    let lines = book_lines(world);
    assert_eq!(lines.len(), 2);
    let (by, action, subject, detail) = lines[1].clone();
    assert_eq!(
        (by.as_str(), action.as_str(), subject.as_deref()),
        ("alice", TUNNEL_CLOSE, Some(HOST))
    );
    let (head, seconds) = split_seconds(&detail);
    assert_eq!(
        head,
        format!(
            "{{\"tunnel\":{},\"cause\":\"eof\",\"bytes_up\":100,\"bytes_down\":2000",
            opened.id
        )
    );
    assert!(seconds >= 30, "{seconds}");
    world
        .book()
        .close(&closing(opened.id, Cause::Error))
        .expect("a tunnel already gone");
    assert_eq!(book_lines(world), lines);
}

/// Proves: a poll of an instance with its tunnels' ids says which of them were revoked and which
/// the book does not hold (an id of none, or of one that was reaped), counts none reaped while
/// no other instance's tunnel is stale, refreshes its own tunnels so another instance's poll
/// does not reap them, and reports nothing of another instance's tunnel as revoked.
///
/// Catches: a poll that reports another instance's tunnel as its own, one that does not refresh
/// (so the reaper closes a live tunnel), and one that writes a line when nothing closed.
pub(crate) fn a_poll_reports_the_revoked_and_the_gone_and_refreshes_its_own(world: &dyn World) {
    hear(world, HOST, "192.0.2.7", &[]);
    let alice_serial = world.issue("alice", HOST, 3600);
    let first = world
        .book()
        .open(&opening("alice", HOST, alice_serial, 4))
        .expect("opened");
    let second = open_ok(world, "bob", HOST);
    world.revoke(alice_serial);
    let lines = book_lines(world).len();
    assert_eq!(
        world
            .book()
            .poll("inst-a", &[first.id, second.id, 9999], 120),
        Ok(Polled {
            revoked: vec![first.id],
            gone: vec![9999],
            reaped: 0
        })
    );
    assert_eq!(
        world.book().poll("inst-b", &[first.id], 120),
        Ok(Polled::default()),
        "a tunnel of another instance is not this one's to close"
    );
    assert_eq!(book_lines(world).len(), lines);

    world.age_tunnel(first.id, 400);
    assert_eq!(
        world.book().poll("inst-a", &[first.id], 120),
        Ok(Polled {
            revoked: vec![first.id],
            gone: vec![],
            reaped: 0
        }),
        "an instance's own stale tunnel is refreshed, not reaped"
    );
    assert_eq!(
        world.book().poll("inst-b", &[], 120),
        Ok(Polled::default()),
        "the refresh was seen"
    );
    assert_eq!(world.tunnel_ids(), vec![first.id, second.id]);
}

/// Proves: a poll closes, as `instance_lost`, each tunnel of another instance not refreshed for
/// `stale_secs`, with one line each by the tunnel's operator, its detail without byte counts,
/// in the order of the tunnels' ids; counts them; and the instance that held them then finds
/// them gone.
///
/// Catches: a reaper that closes a tunnel that is fresh, a line without
/// the operator's name, and a reaped tunnel the owner is not told is gone.
pub(crate) fn a_poll_reaps_the_stale_tunnels_of_other_instances(world: &dyn World) {
    for host in [HOST, "node-08"] {
        hear(world, host, "192.0.2.7", &[]);
    }
    let alice = open_ok(world, "alice", HOST);
    let bob = open_ok(world, "bob", HOST);
    let carol = open_ok(world, "carol", "node-08");
    world.age_tunnel(alice.id, 400);
    world.age_tunnel(carol.id, 400);
    assert_eq!(
        world.book().poll("inst-b", &[], 120),
        Ok(Polled {
            revoked: vec![],
            gone: vec![],
            reaped: 2
        })
    );
    assert_eq!(world.tunnel_ids(), vec![bob.id]);
    let lines = book_lines(world);
    assert_eq!(lines.len(), 5);
    for (line, who, host, id) in [
        (&lines[3], "alice", HOST, alice.id),
        (&lines[4], "carol", "node-08", carol.id),
    ] {
        let (head, seconds) = split_seconds(&line.3);
        assert_eq!(
            (line.0.as_str(), line.1.as_str(), line.2.as_deref()),
            (who, TUNNEL_CLOSE, Some(host))
        );
        assert_eq!(
            head,
            format!("{{\"tunnel\":{id},\"cause\":\"instance_lost\"")
        );
        assert!(seconds >= 400, "{seconds}");
    }
    assert_eq!(
        world
            .book()
            .poll("inst-a", &[alice.id, bob.id, carol.id], 120),
        Ok(Polled {
            revoked: vec![],
            gone: vec![alice.id, carol.id],
            reaped: 0
        })
    );
}

/// Proves: a poll closes, as `instance_lost` with one line by the tunnel's operator, a row of
/// its own instance that went stale and is not among the ids it holds (the row of a close that
/// could not be written), and counts it; it reports nothing gone, and leaves a row of its own
/// that is stale and among its ids, which it refreshes.
///
/// Catches: a reaper that skips the polling instance's own rows, which would keep the operator
/// from the host until the instance stopped, and one that reaps a row the instance holds.
pub(crate) fn a_poll_reaps_its_own_stale_row_that_it_does_not_hold_and_keeps_the_ones_it_does(
    world: &dyn World,
) {
    for host in [HOST, "node-08"] {
        hear(world, host, "192.0.2.7", &[]);
    }
    let held = open_ok(world, "alice", HOST);
    let forgotten = open_ok(world, "bob", "node-08");
    world.age_tunnel(held.id, 400);
    world.age_tunnel(forgotten.id, 400);
    assert_eq!(
        world.book().poll("inst-a", &[held.id], 120),
        Ok(Polled {
            revoked: vec![],
            gone: vec![],
            reaped: 1
        })
    );
    assert_eq!(world.tunnel_ids(), vec![held.id]);
    let lines = book_lines(world);
    let line = &lines[lines.len() - 1];
    let (head, seconds) = split_seconds(&line.3);
    assert_eq!(
        (line.0.as_str(), line.1.as_str(), line.2.as_deref()),
        ("bob", TUNNEL_CLOSE, Some("node-08"))
    );
    assert_eq!(
        head,
        format!("{{\"tunnel\":{},\"cause\":\"instance_lost\"", forgotten.id)
    );
    assert!(seconds >= 400, "{seconds}");
    assert_eq!(
        world.book().poll("inst-a", &[held.id], 120),
        Ok(Polled::default()),
        "the row held was refreshed by the first poll"
    );
    assert_eq!(world.tunnel_ids(), vec![held.id]);
}

/// Proves: a heartbeat writes `ssh_address_changed` (by the coordinator, for the host, the
/// address before and after) only when the host comes from another address than its last
/// heartbeat did: not for the first heartbeat, not for the same address again.
///
/// Catches: a line for every heartbeat, a line for the first one, and a change that is not noted.
pub(crate) fn a_heartbeat_from_another_address_writes_one_line(world: &dyn World) {
    hear(world, HOST, "192.0.2.7", &[]);
    hear(world, HOST, "192.0.2.7", &[]);
    assert!(book_lines(world).is_empty());
    hear(world, HOST, "192.0.2.9", &[]);
    hear(world, HOST, "192.0.2.9", &[]);
    assert_eq!(
        book_lines(world),
        vec![entry(
            "coordinator",
            ADDRESS_CHANGED,
            HOST,
            "{\"before\":\"192.0.2.7\",\"after\":\"192.0.2.9\"}"
        )]
    );
    let opened = open_ok(world, "alice", HOST);
    assert_eq!(opened.address, "192.0.2.9");
}

/// Proves: a host that reports host keys other than its pin writes `ssh_hostkey_changed` once
/// for each change of what it reports: not without a pin, not for the pinned set (in any order),
/// not again while it reports the same other set (in any order), and again when it reports the
/// pinned set and then another; the line names the pinned and the reported set.
///
/// Catches: a line for every heartbeat of a changed host, a line when there is no pin, and a
/// change back to the pinned set noted as a change.
pub(crate) fn a_changed_host_key_writes_one_line_per_change_and_only_with_a_pin(world: &dyn World) {
    hear(world, HOST, "192.0.2.7", &["a"]);
    hear(world, HOST, "192.0.2.7", &["b"]);
    assert!(book_lines(world).is_empty(), "no pin, no line");
    world
        .book()
        .pin("carol", HOST, &keys(&["b"]))
        .expect("pinned");
    let pinned = book_lines(world).len();
    hear(world, HOST, "192.0.2.7", &["b"]);
    hear(world, HOST, "192.0.2.7", &["c"]);
    hear(world, HOST, "192.0.2.7", &["c"]);
    assert_eq!(
        book_lines(world)[pinned..],
        [entry(
            "coordinator",
            HOSTKEY_CHANGED,
            HOST,
            "{\"pinned\":[\"b\"],\"reported\":[\"c\"]}"
        )]
    );
    hear(world, HOST, "192.0.2.7", &["c", "d"]);
    hear(world, HOST, "192.0.2.7", &["d", "c"]);
    assert_eq!(book_lines(world).len(), pinned + 2);
    hear(world, HOST, "192.0.2.7", &["b"]);
    assert_eq!(book_lines(world).len(), pinned + 2, "back to the pin");
    hear(world, HOST, "192.0.2.7", &["c"]);
    assert_eq!(book_lines(world).len(), pinned + 3);
    assert_eq!(
        world.book().host_keys(HOST).map(|keys| keys.state()),
        Ok("changed")
    );
}

/// Proves: a heartbeat that changes the address and the keys at once writes both lines, the
/// address first; a heartbeat that reports no keys keeps the set reported before (host keys
/// still say it, and a pin is not `changed`) and writes no key line; and a host heard first with
/// no report has an empty reported set.
///
/// Catches: a heartbeat without a report that wipes the reported set (so the next pin is
/// refused), and two changes that write one line.
pub(crate) fn an_empty_report_keeps_the_reported_set_and_two_changes_write_two_lines(
    world: &dyn World,
) {
    hear(world, HOST, "192.0.2.7", &[]);
    assert_eq!(world.book().host_keys(HOST), Ok(HostKeys::default()));
    hear(world, HOST, "192.0.2.7", &["a"]);
    world
        .book()
        .pin("carol", HOST, &keys(&["a"]))
        .expect("pinned");
    let before = book_lines(world).len();
    hear(world, HOST, "192.0.2.7", &[]);
    let kept = world.book().host_keys(HOST).expect("read");
    assert_eq!(kept.reported, keys(&["a"]));
    assert_eq!(kept.state(), "pinned");
    assert_eq!(book_lines(world).len(), before);
    hear(world, HOST, "192.0.2.9", &["b"]);
    let lines = book_lines(world);
    assert_eq!(
        lines[before..]
            .iter()
            .map(|line| line.1.as_str())
            .collect::<Vec<_>>(),
        [ADDRESS_CHANGED, HOSTKEY_CHANGED]
    );
}

/// Proves: a pin is refused (`NotReported`, no line, no pin) for a host never heard from, one
/// heard with no report, and fingerprints that are not, as a set, the reported ones (fewer,
/// more); it is accepted for the reported set in another order, writing the
/// `ssh_hostkey_pinned` line by the operator naming the pinned and the reported set, and then
/// `host_keys` says `pinned`; a later pin of a new reported set replaces it; a host that
/// reports another set is `changed`.
///
/// Catches: a pin of keys the host never reported (trust on the operator's typing alone), one
/// that is refused for an order, and a pin that stays when its line was not written.
pub(crate) fn a_pin_is_taken_only_for_the_reported_set_and_replaces_an_earlier_one(
    world: &dyn World,
) {
    let book = world.book();
    assert_eq!(
        book.pin("carol", HOST, &keys(&["a"])),
        Err(PinRefused::NotReported)
    );
    hear(world, HOST, "192.0.2.7", &[]);
    assert_eq!(
        book.pin("carol", HOST, &keys(&["a"])),
        Err(PinRefused::NotReported)
    );
    hear(world, HOST, "192.0.2.7", &["a", "b"]);
    for wrong in [keys(&["a"]), keys(&["a", "b", "c"]), keys(&[])] {
        assert_eq!(
            book.pin("carol", HOST, &wrong),
            Err(PinRefused::NotReported),
            "{wrong:?}"
        );
    }
    assert_eq!(book.host_keys(HOST).expect("read").pinned, None);
    assert!(book_lines(world).is_empty());

    assert_eq!(book.pin("carol", HOST, &keys(&["b", "a"])), Ok(()));
    let pinned = book.host_keys(HOST).expect("read");
    assert_eq!(pinned.pinned, Some(keys(&["b", "a"])));
    assert_eq!(pinned.reported, keys(&["a", "b"]));
    assert_eq!(pinned.state(), "pinned");
    assert_eq!(
        book_lines(world),
        vec![entry(
            "carol",
            HOSTKEY_PINNED,
            HOST,
            "{\"pinned\":[\"b\",\"a\"],\"reported\":[\"a\",\"b\"]}"
        )]
    );

    hear(world, HOST, "192.0.2.7", &["c"]);
    assert_eq!(book.host_keys(HOST).expect("read").state(), "changed");
    assert_eq!(book.pin("dave", HOST, &keys(&["c"])), Ok(()));
    let replaced = book.host_keys(HOST).expect("read");
    assert_eq!(replaced.pinned, Some(keys(&["c"])));
    assert_eq!(replaced.state(), "pinned");
}

/// Proves: with the audit refusing writes, an open, a close, a pin, a heartbeat that has a line
/// to write and a poll that has a tunnel to close each fail (`Unwritten` or `BookError`) and
/// change nothing: no tunnel appears or goes, the pin is not set, the address is the one before;
/// once the audit takes lines again the same calls go through; a heartbeat with no line to
/// write does not need the audit.
///
/// Catches: a tunnel that is open without the line that says it was opened, and a change of the
/// book that stays when its line was refused.
pub(crate) fn a_refused_audit_write_changes_nothing(world: &dyn World) {
    let book = world.book();
    hear(world, HOST, "192.0.2.7", &["a"]);
    let serial = world.issue("alice", HOST, 3600);
    world.break_audit();
    assert!(matches!(
        book.open(&opening("alice", HOST, serial, 4)),
        Err(OpenRefused::Unwritten(_))
    ));
    assert!(world.tunnel_ids().is_empty());
    assert!(matches!(
        book.pin("carol", HOST, &keys(&["a"])),
        Err(PinRefused::Unwritten(_))
    ));
    assert_eq!(book.host_keys(HOST).expect("read").pinned, None);
    assert!(matches!(
        book.heard(HOST, "192.0.2.9", &keys(&["a"])),
        Err(BookError(_))
    ));
    book.heard(HOST, "192.0.2.7", &keys(&["a"]))
        .expect("a heartbeat with no line to write");

    world.fix_audit();
    assert!(book_lines(world).is_empty());
    let opened = book
        .open(&opening("alice", HOST, serial, 4))
        .expect("opened");
    assert_eq!(
        opened.address, "192.0.2.7",
        "the refused heartbeat left no trace"
    );
    world.break_audit();
    assert_eq!(
        book.poll("inst-a", &[opened.id], 120),
        Ok(Polled::default()),
        "a poll with nothing to close needs no line"
    );
    world.age_tunnel(opened.id, 400);
    assert!(matches!(
        book.close(&closing(opened.id, Cause::Eof)),
        Err(BookError(_))
    ));
    assert!(matches!(book.poll("inst-b", &[], 120), Err(BookError(_))));
    assert_eq!(world.tunnel_ids(), vec![opened.id]);
    world.fix_audit();
    assert_eq!(
        book.poll("inst-b", &[], 120).map(|polled| polled.reaped),
        Ok(1)
    );
    assert!(world.tunnel_ids().is_empty());
}

/// Proves: the heartbeat of a host that was never heard from, and the keys of a host never heard
/// from, are read as no address, no pin and no report.
///
/// Catches: a read of an unknown host that fails instead of answering empty.
pub(crate) fn an_unknown_host_has_no_keys(world: &dyn World) {
    assert_eq!(world.book().host_keys("nobody"), Ok(HostKeys::default()));
    assert_eq!(world.seen("nobody"), None);
}

/// Proves: a heartbeat never moves a host's last heartbeat time back: a row whose time is ahead
/// of the clock keeps it when the host is heard from, and a time behind moves up.
///
/// Catches: an upsert that overwrites the time, so a heartbeat handled late by a slow instance
/// makes a live host look stale.
pub(crate) fn the_last_heartbeat_time_never_goes_back(world: &dyn World) {
    hear(world, HOST, "192.0.2.7", &[]);
    world.shift_host(HOST, 1000, false);
    let ahead = world.seen(HOST).expect("heard");
    hear(world, HOST, "192.0.2.7", &[]);
    assert_eq!(world.seen(HOST), Some(ahead));
    world.shift_host(HOST, 5000, true);
    let behind = world.seen(HOST).expect("heard");
    hear(world, HOST, "192.0.2.7", &[]);
    assert!(world.seen(HOST) > Some(behind));
}

/// The memory twin and its ledger as a [`World`].
struct MemoryWorld {
    ledger: Arc<ssh_ledger::Memory>,
    book: Memory,
}

impl MemoryWorld {
    fn new() -> MemoryWorld {
        let ledger = Arc::new(ssh_ledger::Memory::at(1_800_000_000));
        MemoryWorld {
            book: Memory::over(Arc::clone(&ledger)),
            ledger,
        }
    }
}

impl World for MemoryWorld {
    fn book(&self) -> &dyn Book {
        &self.book
    }

    fn issue(&self, operator: &str, host: &str, ttl_secs: u64) -> u64 {
        self.ledger
            .issue(&Issue {
                operator,
                host_id: host,
                fingerprint: "SHA256:abc",
                ttl_secs,
                reason: "",
            })
            .expect("issued")
            .serial
    }

    fn revoke(&self, serial: u64) {
        self.ledger
            .revoke("carol", Revocation::Serial(serial))
            .expect("revoked");
    }

    fn lines(&self) -> Vec<Entry> {
        self.ledger
            .audit()
            .into_iter()
            .map(|row| {
                (
                    row.line.by,
                    row.line.action.to_owned(),
                    row.line.subject,
                    row.line.detail,
                )
            })
            .collect()
    }

    fn tunnel_ids(&self) -> Vec<u64> {
        self.book.tunnel_ids()
    }

    fn seen(&self, host: &str) -> Option<u64> {
        self.book.seen(host)
    }

    fn shift_host(&self, host: &str, secs: u64, back: bool) {
        self.book.shift_host(host, secs, back);
    }

    fn age_tunnel(&self, id: u64, secs: u64) {
        self.book.age_tunnel(id, secs);
    }

    fn break_audit(&self) {
        self.ledger.refuse_next(usize::MAX);
    }

    fn fix_audit(&self) {
        self.ledger.refuse_next(0);
    }
}

macro_rules! on_memory {
    ($($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                super::$name(&MemoryWorld::new());
            }
        )*
    };
}

mod shared {
    use super::MemoryWorld;

    on_memory!(
        an_open_records_the_tunnel_and_its_line,
        an_open_is_refused_for_the_first_defect_in_the_documented_order,
        the_limits_are_one_tunnel_per_operator_per_host_and_a_count_per_operator,
        a_close_removes_the_tunnel_and_writes_its_line_once,
        a_poll_reports_the_revoked_and_the_gone_and_refreshes_its_own,
        a_poll_reaps_the_stale_tunnels_of_other_instances,
        a_poll_reaps_its_own_stale_row_that_it_does_not_hold_and_keeps_the_ones_it_does,
        a_heartbeat_from_another_address_writes_one_line,
        a_changed_host_key_writes_one_line_per_change_and_only_with_a_pin,
        an_empty_report_keeps_the_reported_set_and_two_changes_write_two_lines,
        a_pin_is_taken_only_for_the_reported_set_and_replaces_an_earlier_one,
        a_refused_audit_write_changes_nothing,
        an_unknown_host_has_no_keys,
        the_last_heartbeat_time_never_goes_back,
    );
}

/// Proves: the token of each cause is the one `Close.cause` carries: eof, idle, max_session,
/// token_revoked, revoked, not_permitted, host_closed, coordinator_stopping, instance_lost,
/// error.
///
/// Catches: a token changed on one side of the wire, which the client reads as another cause.
#[test]
fn each_cause_has_its_token() {
    let all = [
        (Cause::Eof, "eof"),
        (Cause::Idle, "idle"),
        (Cause::MaxSession, "max_session"),
        (Cause::TokenRevoked, "token_revoked"),
        (Cause::Revoked, "revoked"),
        (Cause::NotPermitted, "not_permitted"),
        (Cause::HostClosed, "host_closed"),
        (Cause::CoordinatorStopping, "coordinator_stopping"),
        (Cause::InstanceLost, "instance_lost"),
        (Cause::Error, "error"),
    ];
    for (cause, token) in all {
        assert_eq!(cause.as_str(), token);
    }
}

/// Proves: the details of the audit lines are these exact texts, their keys in the order
/// declared (the text is hashed into the chain): an open, a close with bytes and one without
/// (a lost instance's), a changed address, and keys with a quote in a fingerprint escaped.
///
/// Catches: a detail whose key order or spelling changes (every line after it would hash
/// differently from the ones already written).
#[test]
fn the_details_are_exact_json() {
    assert_eq!(
        open_detail(7, 42, "192.0.2.7", "0123456789abcdef"),
        "{\"tunnel\":7,\"serial\":42,\"address\":\"192.0.2.7\",\"instance\":\"0123456789abcdef\"}"
    );
    assert_eq!(
        close_detail(7, Cause::MaxSession, Some((1, 2)), 3),
        "{\"tunnel\":7,\"cause\":\"max_session\",\"bytes_up\":1,\"bytes_down\":2,\"seconds_open\":3}"
    );
    assert_eq!(
        close_detail(7, Cause::InstanceLost, None, 3),
        "{\"tunnel\":7,\"cause\":\"instance_lost\",\"seconds_open\":3}"
    );
    assert_eq!(
        address_detail("192.0.2.7", "2001:db8::1"),
        "{\"before\":\"192.0.2.7\",\"after\":\"2001:db8::1\"}"
    );
    assert_eq!(
        keys_detail(&keys(&["a\"b"]), &keys(&["c", "d"])),
        "{\"pinned\":[\"a\\\"b\"],\"reported\":[\"c\",\"d\"]}"
    );
}

/// Proves: two sets of fingerprints are the same when they hold the same fingerprints in any
/// order and with any repeats, and differ when one holds a fingerprint the other lacks, or one is
/// empty.
///
/// Catches: a pin refused for the order the host listed its keys in, and two different sets
/// taken for one.
#[test]
fn same_set_ignores_order_and_repeats() {
    assert!(same_set(&keys(&["a", "b"]), &keys(&["b", "a"])));
    assert!(same_set(&keys(&["a", "a", "b"]), &keys(&["b", "a"])));
    assert!(same_set(&[], &[]));
    assert!(!same_set(&keys(&["a"]), &keys(&["a", "b"])));
    assert!(!same_set(&keys(&["a", "b"]), &keys(&["a", "c"])));
    assert!(!same_set(&keys(&["a"]), &[]));
}

/// Proves: the state of the host keys is `no_pin` without a pin (whatever is reported), `pinned`
/// for a pin the host reports, in any order, or reports nothing against, and `changed` for a
/// pin and a different non-empty report.
///
/// Catches: a host that reports nothing (an older daemon) shown as changed, and a pin shown as
/// holding when the host reports other keys.
#[test]
fn the_state_of_the_host_keys_is_no_pin_pinned_or_changed() {
    let state = |pinned: Option<&[&str]>, reported: &[&str]| {
        HostKeys {
            pinned: pinned.map(keys),
            reported: keys(reported),
        }
        .state()
    };
    assert_eq!(state(None, &[]), "no_pin");
    assert_eq!(state(None, &["a"]), "no_pin");
    assert_eq!(state(Some(&["a", "b"]), &["b", "a"]), "pinned");
    assert_eq!(state(Some(&["a"]), &[]), "pinned");
    assert_eq!(state(Some(&["a"]), &["b"]), "changed");
}

/// Proves: the memory twin's own helpers move the rows they are told to, and are inert for a
/// host or a tunnel it does not hold.
///
/// Catches: a test helper that panics for a missing row, which would hide the failure it was
/// meant to show.
#[test]
fn the_memory_helpers_ignore_what_is_not_there() {
    let world = MemoryWorld::new();
    world.shift_host("nobody", 5, true);
    world.age_tunnel(1, 5);
    hear(&world, HOST, "192.0.2.7", &[]);
    let seen = world.seen(HOST).expect("heard");
    world.shift_host(HOST, 5, false);
    assert_eq!(world.seen(HOST), Some(seen + 5));
}
