//! `Ssh.Tunnel`, `Ssh.HostKeys` and `Ssh.PinHostKeys` as the coordinator answers them, and what a
//! heartbeat tells the book: who is refused and how, which refusals leave an audit line, and what
//! is recorded when a tunnel could not be opened. The book is the memory one; the Postgres one
//! passes the same rules beside the store.

use futures::StreamExt;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::SshRefusal as Refusal;
use std::net::IpAddr;

use super::{MAX_PINNED, fingerprint_ok};
use crate::operator::Caller;
use crate::ssh_book::{Book, Cause, Opening, TUNNEL_CLOSE, TUNNEL_OPEN, close_detail};
use crate::ssh_ledger::denied_detail;
use crate::ssh_sign::tests::{Tune, world};
use crate::ssh_tunnel::tests::{Client, INSTANCE, Rig, caller, data, open};

const FP_A: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const FP_B: &str = "SHA256:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const FP_C: &str = "SHA256:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

/// The lines of the refusals the ledger holds: who, about which host, and the detail.
fn denied(rig: &Rig) -> Vec<(String, Option<String>, String)> {
    rig.ledger
        .audit()
        .into_iter()
        .filter(|row| row.line.action == "ssh_denied")
        .map(|row| (row.line.by, row.line.subject, row.line.detail))
        .collect()
}

/// The line of refusal `why` of `by` about `host`.
fn line(by: &str, host: &str, why: &str) -> (String, Option<String>, String) {
    (by.to_owned(), Some(host.to_owned()), denied_detail(why))
}

fn keys(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Why `who`'s call that opens a tunnel with `first` is refused; none when it is not.
async fn refused(rig: &Rig, who: &Caller, first: proto::TunnelFrame) -> Option<Refusal> {
    rig.try_open(who, first).await.0.err()
}

/// Proves: a tunnel is refused in the design's order before any frame is answered: no
/// credential, CI's token, a coordinator without tunnels, a draining instance, a first frame that
/// is not `open` (or no frame, or a failed stream), a host id that is none, an operator the file
/// does not list, a host that is not enrolled, a host the file does not permit the operator, and
/// a host whose record cannot be read; the ones after the caller is known and the host id read
/// leave an audit line, and nothing is dialled, recorded or registered.
///
/// Catches: a check that moves behind a costlier one (a certificate looked up for a caller the
/// file does not list), a refusal that leaves no line, and one that leaves a tunnel behind.
#[tokio::test]
async fn a_tunnel_is_refused_in_order_before_anything_is_recorded() {
    let rig = Rig::new("access-refusals", |_| {}).await;
    let serial = rig.cert("operator", "node-07");
    assert_eq!(
        refused(&rig, &Caller::Anonymous, open("node-07", serial)).await,
        Some(Refusal::Unauthorized)
    );
    assert_eq!(
        refused(&rig, &Caller::Ci, open("node-07", serial)).await,
        Some(Refusal::Forbidden)
    );
    assert_eq!(
        refused(&rig, &caller("operator"), data(b"x")).await,
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        refused(
            &rig,
            &caller("operator"),
            proto::TunnelFrame { frame: None }
        )
        .await,
        Some(Refusal::InvalidRequest)
    );
    let (client, incoming) = Client::new();
    drop(client);
    let ended = rig
        .coordinator
        .ssh_tunnel_open(&caller("operator"), incoming, None)
        .await;
    assert_eq!(ended.err(), Some(Refusal::InvalidRequest));
    let (client, incoming) = Client::new();
    client.fail().await;
    let failed = rig
        .coordinator
        .ssh_tunnel_open(&caller("operator"), incoming, None)
        .await;
    assert_eq!(failed.err(), Some(Refusal::InvalidRequest));
    assert_eq!(denied(&rig), Vec::new());

    assert_eq!(
        refused(&rig, &caller("operator"), open("Not A Host", serial)).await,
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        refused(&rig, &caller("mallory"), open("node-07", serial)).await,
        Some(Refusal::NotPermitted)
    );
    assert_eq!(
        refused(&rig, &caller("operator"), open("node-99", serial)).await,
        Some(Refusal::UnknownHost)
    );
    assert_eq!(
        refused(&rig, &caller("bob"), open("node-07", serial)).await,
        Some(Refusal::NotPermitted)
    );
    assert_eq!(
        denied(&rig),
        vec![
            line("mallory", "node-07", "not_permitted"),
            line("operator", "node-99", "unknown_host"),
            line("bob", "node-07", "not_permitted"),
        ]
    );
    rig.records.fail_at(rig.records.count() + 1);
    assert_eq!(
        refused(&rig, &caller("operator"), open("node-07", serial)).await,
        Some(Refusal::StoreFailed)
    );
    assert_eq!(denied(&rig).len(), 3);
    assert_eq!(rig.tunnels.count(), 0);
    assert_eq!(rig.book.inner.tunnel_ids(), Vec::<u64>::new());
    assert_eq!(rig.tunnel_lines(), Vec::new());

    rig.tunnels.drain().await;
    assert_eq!(
        refused(&rig, &caller("operator"), open("node-07", serial)).await,
        Some(Refusal::Draining)
    );
}

/// Proves: a coordinator that holds the signing but no tunnels refuses `Tunnel`, `HostKeys` and
/// `PinHostKeys` `not_configured`, whoever calls it as an operator.
///
/// Catches: a stream that is opened with no book to record it in.
#[tokio::test]
async fn a_coordinator_without_tunnels_refuses_with_not_configured() {
    let world = world("access-plain", None, Tune::default());
    let (client, incoming) = Client::new();
    client.send(open("node-07", 1)).await;
    let answer = world
        .coordinator
        .ssh_tunnel_open(&caller("operator"), incoming, None)
        .await;
    assert_eq!(answer.err(), Some(Refusal::NotConfigured));
    let host_keys = world.coordinator.ssh_host_keys(
        &caller("operator"),
        &proto::HostKeysRequest {
            host_id: "node-07".to_owned(),
        },
    );
    assert_eq!(host_keys.err(), Some(Refusal::NotConfigured));
    let pinned = world.coordinator.ssh_pin_host_keys(
        &caller("operator"),
        &proto::PinHostKeysRequest {
            host_id: "node-07".to_owned(),
            fingerprints: keys(&[FP_A]),
        },
    );
    assert_eq!(pinned.err(), Some(Refusal::NotConfigured));
}

/// Proves: the book's refusals reach the caller as their own: a serial that is not the caller's
/// (`bad_serial`), a host never heard from (`no_address`), one not heard within the live time
/// (`host_not_live`), a second tunnel to the same host (`too_many_tunnels`) each leave an audit
/// line; a book that cannot write (or whose call panics) is `audit_unavailable` with no line; the
/// instance's own cap is `too_many_tunnels` with none; and nothing refused leaves a slot taken.
///
/// Catches: a book refusal that is answered as another, one that leaves no line, and a slot that
/// a refused open does not give back.
#[tokio::test]
async fn the_books_refusals_reach_the_caller_and_leave_their_lines() {
    let mut rig = Rig::new("access-book-refusals", |limits| limits.max_tunnels = 2).await;
    let serial = rig.cert("operator", "node-07");
    let operator = caller("operator");
    let carol = caller("carol");
    assert_eq!(
        refused(&rig, &operator, open("node-07", serial + 99)).await,
        Some(Refusal::BadSerial)
    );
    assert_eq!(
        refused(&rig, &operator, open("node-08", serial)).await,
        Some(Refusal::BadSerial)
    );
    let nuc = rig.cert("carol", "nuc");
    assert_eq!(
        refused(&rig, &carol, open("nuc", nuc)).await,
        Some(Refusal::NoAddress)
    );
    rig.book.inner.shift_host("node-07", 1000, true);
    assert_eq!(
        refused(&rig, &operator, open("node-07", serial)).await,
        Some(Refusal::HostNotLive)
    );
    rig.book.inner.shift_host("node-07", 1000, false);
    assert_eq!(
        denied(&rig),
        vec![
            line("operator", "node-07", "bad_serial"),
            line("operator", "node-08", "bad_serial"),
            line("carol", "nuc", "no_address"),
            line("operator", "node-07", "host_not_live"),
        ]
    );

    let held = rig.tunnel("operator", "node-07").await;
    let again = rig.cert("operator", "node-07");
    assert_eq!(
        refused(&rig, &operator, open("node-07", again)).await,
        Some(Refusal::TooManyTunnels)
    );
    assert_eq!(denied(&rig).len(), 5);
    assert_eq!(
        denied(&rig)[4],
        line("operator", "node-07", "too_many_tunnels")
    );

    let second = rig.tunnel("bob", "node-08").await;
    let third = rig.cert("operator", "node-08");
    assert_eq!(
        refused(&rig, &operator, open("node-08", third)).await,
        Some(Refusal::TooManyTunnels)
    );
    assert_eq!(denied(&rig).len(), 5, "the instance's cap leaves no line");
    let (first_id, second_id) = (held.id, second.id);
    drop((held, second));
    rig.closed(first_id).await;
    rig.closed(second_id).await;

    rig.book.fail("open");
    assert_eq!(
        refused(&rig, &operator, open("node-07", again)).await,
        Some(Refusal::AuditUnavailable)
    );
    rig.book.mend("open");
    rig.book.fail("panic_open");
    assert_eq!(
        refused(&rig, &operator, open("node-07", again)).await,
        Some(Refusal::AuditUnavailable)
    );
    assert_eq!(denied(&rig).len(), 5);
    assert_eq!(rig.tunnels.count(), 0);
}

/// Proves: a host that cannot be dialled is refused `dial_failed`, the book's row is closed with
/// the cause `error` and no bytes (so the operator can try again at once), no line of the
/// refusal is written, and the instance's slot is free.
///
/// Catches: a tunnel row left open for a connection that was never made, which would hold the
/// operator out of the host until the instance stopped.
#[tokio::test]
async fn a_host_that_cannot_be_dialled_is_refused_and_its_row_closed() {
    let closed_port = {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("a port");
        closed.local_addr().expect("an address").port()
    };
    let rig = Rig::new("access-dial-failed", |limits| {
        limits.port = closed_port;
        limits.max_tunnels = 1;
    })
    .await;
    let serial = rig.cert("operator", "node-07");
    for _ in 0..2 {
        let answer = rig
            .try_open(&caller("operator"), open("node-07", serial))
            .await
            .0;
        assert_eq!(answer.err(), Some(Refusal::DialFailed));
    }
    assert_eq!(rig.book.inner.tunnel_ids(), Vec::<u64>::new());
    assert_eq!(rig.tunnels.count(), 0);
    assert_eq!(denied(&rig), Vec::new());
    let lines = rig.tunnel_lines();
    assert_eq!(
        lines.iter().map(|line| line.1).collect::<Vec<_>>(),
        [TUNNEL_OPEN, TUNNEL_CLOSE, TUNNEL_OPEN, TUNNEL_CLOSE]
    );
    assert_eq!(lines[1].3, close_detail(1, Cause::Error, Some((0, 0)), 0));
    assert_eq!(lines[3].3, close_detail(2, Cause::Error, Some((0, 0)), 0));
}

/// Proves: a caller that gives up while the tunnel is being recorded and dialled (its call is
/// dropped) leaves no row in the book: the row the book made is closed with the cause `error`, and
/// the slot is given back.
///
/// Catches: a tunnel recorded for a caller that is gone, which would keep the operator from the
/// host (one tunnel per host) until the instance stopped, as the reaper never closes its own.
#[tokio::test]
async fn a_caller_that_gives_up_midway_leaves_no_row_behind() {
    let rig = Rig::new("access-cancelled", |_| {}).await;
    let mut closes = rig.book.watch_closes();
    let serial = rig.cert("operator", "node-07");
    let (client, incoming) = Client::new();
    client.send(open("node-07", serial)).await;
    let operator = caller("operator");
    let mut call = Box::pin(rig.coordinator.ssh_tunnel_open(&operator, incoming, None));
    assert!(futures::poll!(call.as_mut()).is_pending());
    drop(call);
    let closing = closes.recv().await.expect("the row is closed");
    assert_eq!((closing.id, closing.cause), (1, Cause::Error));
    assert_eq!(rig.book.inner.tunnel_ids(), Vec::<u64>::new());
    assert_eq!(rig.tunnels.count(), 0);
    let slot = rig.tunnels.reserve().expect("the slot is free");
    drop(slot);
}

/// Proves: an opened tunnel answers `opened` with the login user of the host (the Linux
/// account), and a client that sends nothing after `open` and drops its stream is closed `eof`.
///
/// Catches: an `opened` frame that names no user or a registry that keeps a tunnel whose client
/// left.
#[tokio::test]
async fn an_opened_tunnel_answers_the_login_user() {
    let mut rig = Rig::new("access-opened", |_| {}).await;
    let live = rig.tunnel("operator", "node-07").await;
    assert_eq!(rig.tunnels.count(), 1);
    drop(live.client);
    assert_eq!(rig.closed(live.id).await, Cause::Eof);
    let rest: Vec<_> = live.out.collect().await;
    assert_eq!(rest.len(), 1);
}

/// Proves: `HostKeys` answers what the book holds: no pin and no keys for a host nothing was
/// pinned for, the pinned set with the state `pinned` once pinned, and `changed` (the pinned set
/// still what is answered) when the host reports others; the caller's checks are `SignUserCert`'s
/// (no credential, CI, a host id that is none, an operator the file does not list, an unknown
/// host, a host not permitted, a record that cannot be read), the audited ones leaving a line; a
/// book that cannot be read is `store_failed`.
///
/// Catches: a state that is wrong after a change of the reported keys, a pin that is read for a
/// caller the file does not permit, and a refusal with no line.
#[tokio::test]
async fn host_keys_answers_the_pinned_set_and_its_state_to_a_permitted_operator() {
    let rig = Rig::new("access-host-keys", |_| {}).await;
    let ask = |who: Caller, host: &str| {
        rig.coordinator.ssh_host_keys(
            &who,
            &proto::HostKeysRequest {
                host_id: host.to_owned(),
            },
        )
    };
    assert_eq!(
        ask(Caller::Anonymous, "node-07").err(),
        Some(Refusal::Unauthorized)
    );
    assert_eq!(ask(Caller::Ci, "node-07").err(), Some(Refusal::Forbidden));
    assert_eq!(
        ask(caller("operator"), "").err(),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        ask(caller("mallory"), "node-07").err(),
        Some(Refusal::NotPermitted)
    );
    assert_eq!(
        ask(caller("operator"), "node-99").err(),
        Some(Refusal::UnknownHost)
    );
    assert_eq!(
        ask(caller("bob"), "node-07").err(),
        Some(Refusal::NotPermitted)
    );
    rig.records.fail_at(rig.records.count() + 1);
    assert_eq!(
        ask(caller("operator"), "node-07").err(),
        Some(Refusal::StoreFailed)
    );
    assert_eq!(
        denied(&rig),
        vec![
            line("mallory", "node-07", "not_permitted"),
            line("operator", "node-99", "unknown_host"),
            line("bob", "node-07", "not_permitted"),
        ]
    );

    let none = ask(caller("operator"), "node-07").expect("answered");
    assert_eq!(
        (none.fingerprints, none.state.as_str()),
        (Vec::new(), "no_pin")
    );
    rig.book
        .inner
        .heard("node-07", "127.0.0.1", &keys(&[FP_A, FP_B]))
        .expect("heard");
    let pin = proto::PinHostKeysRequest {
        host_id: "node-07".to_owned(),
        fingerprints: keys(&[FP_B, FP_A]),
    };
    let pinned = rig
        .coordinator
        .ssh_pin_host_keys(&caller("operator"), &pin)
        .expect("pinned");
    assert_eq!(pinned.state, "pinned");
    assert_eq!(pinned.fingerprints, keys(&[FP_B, FP_A]));
    rig.book
        .inner
        .heard("node-07", "127.0.0.1", &keys(&[FP_C]))
        .expect("heard");
    let changed = ask(caller("operator"), "node-07").expect("answered");
    assert_eq!(changed.state, "changed");
    assert_eq!(changed.fingerprints, keys(&[FP_B, FP_A]));
    rig.book.fail("host_keys");
    assert_eq!(
        ask(caller("operator"), "node-07").err(),
        Some(Refusal::StoreFailed)
    );
}

/// Proves: `PinHostKeys` pins exactly the set the host's last heartbeat reported and writes the
/// `ssh_hostkey_pinned` line under the operator's name; fingerprints that are not that set, and
/// any pin for a host that reported none, are refused `not_reported` with a line of the refusal
/// and no pin; a malformed fingerprint or more than eight are `invalid_request` with no line;
/// a book that cannot write is `audit_unavailable`; the operators file's checks are those of
/// `HostKeys`.
///
/// Catches: a pin of keys the host did not report (which would trust what an operator typed),
/// a pin with no audit line, and a pin the file does not permit the operator.
#[tokio::test]
async fn pin_host_keys_pins_the_reported_set_and_refuses_anything_else() {
    let rig = Rig::new("access-pin", |_| {}).await;
    let pin = |who: Caller, host: &str, fingerprints: &[&str]| {
        rig.coordinator.ssh_pin_host_keys(
            &who,
            &proto::PinHostKeysRequest {
                host_id: host.to_owned(),
                fingerprints: keys(fingerprints),
            },
        )
    };
    assert_eq!(
        pin(Caller::Anonymous, "node-07", &[FP_A]).err(),
        Some(Refusal::Unauthorized)
    );
    assert_eq!(
        pin(Caller::Ci, "node-07", &[FP_A]).err(),
        Some(Refusal::Forbidden)
    );
    assert_eq!(
        pin(caller("operator"), "node-07", &["SHA256:short"]).err(),
        Some(Refusal::InvalidRequest)
    );
    let nine = [FP_A; MAX_PINNED + 1];
    assert_eq!(
        pin(caller("operator"), "node-07", &nine).err(),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        pin(caller("operator"), "", &[FP_A]).err(),
        Some(Refusal::InvalidRequest)
    );
    assert_eq!(
        pin(caller("mallory"), "node-07", &[FP_A]).err(),
        Some(Refusal::NotPermitted)
    );
    assert_eq!(
        pin(caller("operator"), "node-99", &[FP_A]).err(),
        Some(Refusal::UnknownHost)
    );
    rig.records.fail_at(rig.records.count() + 1);
    assert_eq!(
        pin(caller("operator"), "node-07", &[FP_A]).err(),
        Some(Refusal::StoreFailed)
    );
    assert_eq!(denied(&rig).len(), 2);

    // The host reported nothing yet: nothing can be pinned.
    assert_eq!(
        pin(caller("operator"), "node-07", &[FP_A]).err(),
        Some(Refusal::NotReported)
    );
    rig.book
        .inner
        .heard("node-07", "127.0.0.1", &keys(&[FP_A, FP_B]))
        .expect("heard");
    assert_eq!(
        pin(caller("operator"), "node-07", &[FP_A]).err(),
        Some(Refusal::NotReported)
    );
    assert_eq!(
        pin(caller("operator"), "node-07", &[]).err(),
        Some(Refusal::NotReported)
    );
    assert_eq!(
        denied(&rig)[2..].to_vec(),
        vec![
            line("operator", "node-07", "not_reported"),
            line("operator", "node-07", "not_reported"),
            line("operator", "node-07", "not_reported"),
        ]
    );
    rig.book.fail("pin");
    assert_eq!(
        pin(caller("operator"), "node-07", &[FP_A, FP_B]).err(),
        Some(Refusal::AuditUnavailable)
    );
    rig.book.mend("pin");
    let done = pin(caller("operator"), "node-07", &[FP_B, FP_A]).expect("pinned");
    assert_eq!(
        (done.state.as_str(), done.fingerprints),
        ("pinned", keys(&[FP_B, FP_A]))
    );
    let pinned: Vec<_> = rig
        .ledger
        .audit()
        .into_iter()
        .filter(|row| row.line.action == crate::ssh_book::HOSTKEY_PINNED)
        .map(|row| (row.line.by, row.line.subject))
        .collect();
    assert_eq!(
        pinned,
        vec![("operator".to_owned(), Some("node-07".to_owned()))]
    );
}

/// Proves: a fingerprint is `SHA256:` and 43 base64 characters, and nothing else.
///
/// Catches: a pin of text that is no fingerprint, which no host could ever report.
#[test]
fn a_fingerprint_is_sha256_and_forty_three_base64_characters() {
    assert!(fingerprint_ok(FP_A));
    assert!(fingerprint_ok(&format!("SHA256:{}+/", "a".repeat(41))));
    for bad in [
        "",
        "SHA256:",
        "MD5:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        &format!("SHA256:{}", "A".repeat(42)),
        &format!("SHA256:{}", "A".repeat(44)),
        &format!("SHA256:{}=", "A".repeat(42)),
        &format!("SHA256:{}-", "A".repeat(42)),
    ] {
        assert!(!fingerprint_ok(bad), "{bad}");
    }
}

/// Proves: a heartbeat tells the book where the host was heard from (a mapped IPv6 address is the
/// IPv4 address) and the host keys it reported; a heartbeat from a connection with no address says
/// nothing; and a book that cannot take it is not an error of the heartbeat.
///
/// Catches: an address that is stored in its mapped spelling (which no dial can use) and a book
/// failure that surfaces to the host.
#[tokio::test]
async fn a_heartbeat_tells_the_book_its_address_and_keys_and_a_failing_book_does_not_matter() {
    let rig = Rig::new("access-hear", |_| {}).await;
    let mapped: IpAddr = "::ffff:192.0.2.9".parse().expect("an address");
    rig.coordinator
        .hear_ssh("node-07", Some(mapped), keys(&[FP_A]))
        .await;
    let held = rig.book.inner.host_keys("node-07").expect("keys");
    assert_eq!(held.reported, keys(&[FP_A]));
    let serial = rig.cert("operator", "node-07");
    let opened = rig
        .book
        .inner
        .open(&Opening {
            operator: "operator".to_owned(),
            host_id: "node-07".to_owned(),
            serial,
            instance: INSTANCE.to_owned(),
            live_secs: 120,
            per_operator: 4,
        })
        .expect("recorded");
    assert_eq!(opened.address, "192.0.2.9");

    rig.coordinator
        .hear_ssh("node-09", None, keys(&[FP_B]))
        .await;
    assert_eq!(rig.book.inner.seen("node-09"), None);
    let from: IpAddr = "192.0.2.1".parse().expect("an address");
    rig.book.fail("heard");
    rig.coordinator
        .hear_ssh("node-09", Some(from), Vec::new())
        .await;
    assert_eq!(rig.book.inner.seen("node-09"), None);
    rig.book.mend("heard");
    rig.coordinator
        .hear_ssh("node-09", Some(from), Vec::new())
        .await;
    assert!(rig.book.inner.seen("node-09").is_some());

    let plain = world("access-hear-plain", None, Tune::default());
    plain
        .coordinator
        .hear_ssh("node-09", Some(from), Vec::new())
        .await;
}
