//! The UI's terminal after the upgrade, against a real SSH server standing for the host: the
//! browser is a WebSocket over an in-memory pipe, the coordinator is the tunnels' rig with a
//! sign-in, and the host is the `russh` server of `ssh_host_tests` on a socket of port 0 of
//! loopback that the tunnel dials. No test asserts a time: the first message's patience runs on
//! tokio's paused clock, and every other wait is for a message, an event of the tunnels or the
//! end of a pipe.

use std::sync::atomic::Ordering;

use hostkeeper_proto::hostkeeper::operator::v1::SshRefusal;

use super::run;
use crate::ssh_book::{Book as _, Cause, TUNNEL_CLOSE, TUNNEL_OPEN};
use crate::ssh_cert::fingerprint;
use crate::ssh_client::key_from_seed;
use crate::ssh_host_tests::{
    Behavior, Browser, HOST, Kind, Seen, Sent, Site, host_fingerprint, steps,
};
use crate::ssh_ledger::{Ledger as _, Revocation};
use crate::ssh_sign::refusal_token;
use crate::ssh_sign::tests::Tune;
use crate::websocket::tests::client_frame;

fn pinned() -> Option<Vec<String>> {
    Some(vec![host_fingerprint()])
}

/// A site whose host behaves, with its key pinned.
async fn site(name: &str) -> Site {
    Site::new(name, Kind::Ssh(Behavior::good()), pinned()).await
}

/// A browser for `site` that has sent its first message and been told the terminal is open.
async fn open_terminal(site: &Site) -> Browser {
    let mut browser = site.connect(HOST);
    browser.text(&site.hello(80, 24)).await;
    browser.status("signing").await;
    browser.status("connecting").await;
    browser.status("opened").await;
    browser
}

/// Whether the ledger holds no certificate, tunnel or refusal of the site: the lines it holds
/// are the pin's.
fn untouched(site: &Site) -> bool {
    site.rig.ledger.certs().is_empty()
        && site
            .rig
            .ledger
            .audit()
            .iter()
            .all(|row| row.line.action == "ssh_hostkey_pinned")
}

/// The close frame a browser sends.
fn close_frame() -> Vec<u8> {
    client_frame(8, &[3, 232])
}

/// Proves: a terminal opened with a good first message signs a certificate for the host as the
/// session's operator (one row of the ledger, for the host, with the reason `coordinator UI
/// terminal` and the fingerprint of the terminal's own key), opens the tunnel and logs in over it
/// as the login user with exactly that certificate (the host saw its serial, its key id
/// `operator/<serial>` and the pty `xterm-256color` of the first message's size), tells
/// `signing`, `connecting` and `opened` with the login user, then carries the host's greeting
/// (sent before the shell's reply), what is typed (echoed back), standard error, a window change
/// (the host saw the new size) and 40,000 bytes of output in messages of at most 16 KiB, and ends
/// `exit` when the shell does; the audit holds the tunnel's open and close under the operator.
///
/// Catches: a terminal that is not the design's sequence, a key that is not the one the
/// certificate was made for, a greeting lost between the shell request and its reply, a resize
/// that is not told to the host, an output message past 16 KiB, a last message that is not
/// `exit`, and a tunnel with no close row.
#[tokio::test]
async fn a_terminal_carries_bytes_both_ways_resizes_and_ends_with_the_shell() {
    let mut site = site("term-happy").await;
    let mut browser = site.connect(HOST);
    browser.text(&site.hello(80, 24)).await;
    browser.status("signing").await;
    browser.status("connecting").await;
    assert_eq!(browser.status("opened").await.as_deref(), Some("root"));
    browser.output_until(b"welcome\r\n").await;
    browser.binary(b"ls\r").await;
    browser.output_until(b"ls\r").await;
    browser.binary(b"err\r").await;
    browser.output_until(b"stderr\r\n").await;
    browser
        .text(r#"{"type":"resize","cols":100,"rows":30}"#)
        .await;
    browser.output_until(b"winch 100x30\r\n").await;
    browser.binary(b"big\r").await;
    assert!(browser.output_count(40_000).await >= 3);
    browser.binary(b"exit\r").await;
    browser.closed("exit").await;

    let certs = site.rig.ledger.certs();
    let (serial, cert) = certs.iter().next().expect("one certificate");
    assert_eq!(certs.len(), 1);
    assert_eq!(
        (
            cert.operator.as_str(),
            cert.host_id.as_str(),
            cert.reason.as_str()
        ),
        ("operator", HOST, "coordinator UI terminal")
    );
    let key = key_from_seed(&[7; 32]);
    assert_eq!(
        cert.fingerprint,
        fingerprint(&key.public_key().to_bytes().expect("a blob"))
    );
    assert_eq!(
        site.seen.recv().await,
        Some(Seen::Login(
            "root".to_owned(),
            *serial,
            format!("operator/{serial}"),
            true
        ))
    );
    assert_eq!(
        site.seen.recv().await,
        Some(Seen::Pty("xterm-256color".to_owned(), 80, 24))
    );
    let id = site.opened().await;
    site.rig.closed(id).await;
    let lines = site.rig.tunnel_lines();
    let actions: Vec<(&str, &str)> = lines
        .iter()
        .map(|(by, action, _, _)| (by.as_str(), *action))
        .collect();
    assert_eq!(
        actions,
        [("operator", TUNNEL_OPEN), ("operator", TUNNEL_CLOSE)]
    );
}

/// Proves: a first message that is not `{csrf, cols, rows}` of the session's CSRF token and a size
/// in range ends the socket `bad_request` with nothing else sent and nothing done (no certificate
/// is signed): a wrong token, a missing one, text that is not JSON, a binary first message, 0 or
/// 501 columns, 0 or 201 rows, a text message longer than a message may be, a close frame and a
/// protocol error (an unmasked frame).
///
/// Catches: a terminal that works for a page that does not hold the session's token, and one that
/// does anything before the first message is right.
#[tokio::test]
async fn a_first_message_that_is_not_the_sessions_is_refused_with_nothing_done() {
    let site = site("term-hello").await;
    let good = site.hello(80, 24);
    let wrong = good.replace(&site.csrf, "not-the-token");
    let cases: Vec<Vec<u8>> = vec![
        client_frame(1, wrong.as_bytes()),
        client_frame(1, br#"{"cols":80,"rows":24}"#),
        client_frame(1, b"hello"),
        client_frame(2, good.as_bytes()),
        client_frame(1, site.hello(0, 24).as_bytes()),
        client_frame(1, site.hello(501, 24).as_bytes()),
        client_frame(1, site.hello(80, 0).as_bytes()),
        client_frame(1, site.hello(80, 201).as_bytes()),
        client_frame(1, "a".repeat(2000).as_bytes()),
        close_frame(),
        vec![0x81, 0x01, b'x'],
    ];
    for (at, frame) in cases.into_iter().enumerate() {
        let mut browser = site.connect(HOST);
        browser.raw(&frame).await;
        let before = browser.closed("bad_request").await;
        assert!(before.is_empty(), "case {at}: {before:?}");
    }
    assert!(site.rig.ledger.certs().is_empty());
    assert!(untouched(&site));
}

/// Proves: a browser that sends no first message gets `bad_request` once the listener's patience
/// has passed (on the paused clock) and the socket ends.
///
/// Catches: a socket that waits for ever for a first message, which anyone who is signed in could
/// hold open.
#[tokio::test(start_paused = true)]
async fn a_first_message_that_does_not_come_in_the_patience_ends_the_socket() {
    let site = site("term-patience").await;
    let mut browser = site.connect(HOST);
    assert!(browser.closed("bad_request").await.is_empty());
    assert!(site.rig.ledger.certs().is_empty());
}

/// Proves: a session that ended between the upgrade and the first message gets no terminal: the
/// first message is right (the token is the one the session had) and the answer is
/// `token_revoked`, with no certificate signed.
///
/// Catches: a terminal opened for a user who signed out while the socket was opening.
#[tokio::test]
async fn a_session_that_ended_before_the_first_message_gets_no_terminal() {
    let site = site("term-ended").await;
    let mut browser = site.connect(HOST);
    let sessions = &site.rig.coordinator.login().expect("a sign-in").sessions;
    assert!(sessions.end(&site.session.0));
    browser.text(&site.hello(80, 24)).await;
    assert!(browser.closed("token_revoked").await.is_empty());
    assert!(site.rig.ledger.certs().is_empty());
}

/// Proves: a ping gets a pong with its payload, at any time, and does not end the terminal.
///
/// Catches: a proxy's ping that is not answered and so cuts the socket.
#[tokio::test]
async fn a_ping_is_answered_with_a_pong() {
    let site = site("term-ping").await;
    let mut browser = site.connect(HOST);
    browser.raw(&client_frame(9, b"hi")).await;
    assert_eq!(browser.next().await, Sent::Pong(b"hi".to_vec()));
    browser.text("not a first message").await;
    browser.closed("bad_request").await;
}

/// Proves: the refusals of the checks before anything is signed end the socket with the refusal's
/// own token, with no status before it and no certificate: a host outside the operator's classes
/// (`not_permitted`), one that is not enrolled (`unknown_host`) and a host id that is none
/// (`invalid_request`), the two audited as `ssh_denied`; a host with no pinned key set
/// (`no_pin`); a book that cannot be read, and one whose read does not finish
/// (both `store_failed`).
///
/// Catches: a terminal that signs a certificate before the operator is known to be allowed and the
/// host key pinned, and a refusal told in words that are not the token the page switches on.
#[tokio::test]
async fn the_refusals_before_signing_end_the_socket_with_their_token() {
    let site = site("term-refusals").await;
    for (host, cause) in [
        ("nuc", "not_permitted"),
        ("node-99", "unknown_host"),
        ("Not A Host", "invalid_request"),
    ] {
        let mut browser = site.connect(host);
        browser.text(&site.hello(80, 24)).await;
        assert!(browser.closed(cause).await.is_empty(), "{host}");
    }
    let denied: Vec<String> = site
        .rig
        .ledger
        .audit()
        .into_iter()
        .filter(|row| row.line.action == "ssh_denied")
        .map(|row| row.line.detail)
        .collect();
    assert_eq!(
        denied,
        [
            r#"{"why":"not_permitted"}"#.to_owned(),
            r#"{"why":"unknown_host"}"#.to_owned()
        ]
    );
    for (fault, cause) in [
        ("host_keys", "store_failed"),
        ("panic_host_keys", "store_failed"),
    ] {
        site.rig.book.fail(fault);
        let mut browser = site.connect(HOST);
        browser.text(&site.hello(80, 24)).await;
        assert!(browser.closed(cause).await.is_empty(), "{fault}");
        site.rig.book.mend(fault);
    }
    let unpinned = Site::new("term-no-pin", Kind::Ssh(Behavior::good()), None).await;
    let mut browser = unpinned.connect(HOST);
    browser.text(&unpinned.hello(80, 24)).await;
    assert!(browser.closed("no_pin").await.is_empty());
    assert!(site.rig.ledger.certs().is_empty());
    assert!(unpinned.rig.ledger.certs().is_empty());
}

/// Proves: a coordinator whose random source gives no bytes cannot make the terminal's key: the
/// socket ends `error` after `signing`, with no certificate.
///
/// Catches: a terminal that goes on with a key made from nothing.
#[tokio::test]
async fn a_random_source_that_fails_ends_the_socket_before_anything_is_signed() {
    let tune = Tune {
        random_fails: true,
        ..Tune::default()
    };
    let site = Site::build(
        "term-no-random",
        Kind::Ssh(Behavior::good()),
        (HOST, "operator"),
        pinned(),
        tune,
    )
    .await;
    let mut browser = site.connect(HOST);
    browser.text(&site.hello(80, 24)).await;
    let before = browser.closed("error").await;
    assert_eq!(steps(&before), ["signing"]);
    assert!(site.rig.ledger.certs().is_empty());
}

/// Proves: a host whose sshd presents a key outside the pinned set ends the terminal
/// `host_key_mismatch` after `signing` and `connecting`, and the host never saw a login (the
/// certificate was not shown); the certificate was signed and the tunnel opened and closed, both
/// audited.
///
/// Catches: a client that shows its certificate to whatever answers on the host's address, which a
/// host that changed address would hand to another machine.
#[tokio::test]
async fn a_host_key_outside_the_pin_ends_the_terminal_before_the_certificate_is_shown() {
    let mut site = Site::new(
        "term-key-mismatch",
        Kind::Ssh(Behavior::good()),
        Some(vec![fingerprint(b"another host's key")]),
    )
    .await;
    let mut browser = site.connect(HOST);
    browser.text(&site.hello(80, 24)).await;
    let before = browser.closed("host_key_mismatch").await;
    assert_eq!(steps(&before), ["signing", "connecting"]);
    assert!(site.seen.try_recv().is_err(), "the host saw no login");
    assert_eq!(site.rig.ledger.certs().len(), 1);
    let id = site.opened().await;
    site.rig.closed(id).await;
    assert_eq!(site.rig.tunnel_lines().len(), 2);
}

/// Proves: a host that reports other keys than the pinned ones (`changed`) is still reached by the
/// pinned set: the terminal opens and runs, and the host's key is the pinned one.
///
/// Catches: a terminal that trusts what the host's heartbeat reports instead of the operator's pin.
#[tokio::test]
async fn a_changed_report_does_not_change_the_pin_the_terminal_trusts() {
    let site = site("term-changed").await;
    site.rig
        .book
        .inner
        .heard(HOST, "127.0.0.1", &[fingerprint(b"a key it reports now")])
        .expect("heard");
    let mut browser = open_terminal(&site).await;
    browser.binary(b"exit\r").await;
    browser.closed("exit").await;
}

/// Proves: a host that does not trust the CA that signed the certificate refuses the login
/// (`auth_refused`, and the host saw the login refused), and one that refuses the pty, one that
/// refuses the shell (`shell_refused`), one that refuses to open the channel (`shell_refused`), one
/// and one that closes the channel in answer to the shell request (`error`: the tunnel has said
/// nothing) each end the terminal with their own cause after
/// `signing` and `connecting`, never `opened`.
///
/// Catches: a refusal of the host told as another one, and a terminal that says `opened` for a
/// shell that is not there.
#[tokio::test]
async fn a_host_that_refuses_the_login_the_pty_or_the_shell_is_told_as_such() {
    let cases = [
        (
            Behavior {
                trusts_ca: false,
                ..Behavior::good()
            },
            "auth_refused",
        ),
        (
            Behavior {
                refuse_pty: true,
                ..Behavior::good()
            },
            "shell_refused",
        ),
        (
            Behavior {
                refuse_shell: true,
                ..Behavior::good()
            },
            "shell_refused",
        ),
        (
            Behavior {
                close_on_shell: true,
                ..Behavior::good()
            },
            "error",
        ),
        (
            Behavior {
                refuse_channel: true,
                ..Behavior::good()
            },
            "shell_refused",
        ),
    ];
    for (at, (behavior, cause)) in cases.into_iter().enumerate() {
        let mut site = Site::new(
            &format!("term-host-refuses-{at}"),
            Kind::Ssh(behavior),
            pinned(),
        )
        .await;
        let mut browser = site.connect(HOST);
        browser.text(&site.hello(80, 24)).await;
        let before = browser.closed(cause).await;
        assert_eq!(steps(&before), ["signing", "connecting"], "{cause}");
        if cause == "auth_refused" {
            let Some(Seen::Login(user, _, _, accepted)) = site.seen.recv().await else {
                panic!("the host saw a login");
            };
            assert_eq!((user.as_str(), accepted), ("root", false));
        }
        let id = site.opened().await;
        site.rig.closed(id).await;
    }
}

/// Proves: a host that closes the connection at once ends the terminal with the tunnel's own
/// cause, `host_closed`, after `signing` and `connecting`.
///
/// Catches: a connection that broke told as a refusal of the host or as no cause at all.
#[tokio::test]
async fn a_host_that_hangs_up_ends_the_terminal_with_the_tunnels_cause() {
    let site = Site::new("term-hang-up", Kind::HangUp, pinned()).await;
    let mut browser = site.connect(HOST);
    browser.text(&site.hello(80, 24)).await;
    let before = browser.closed("host_closed").await;
    assert_eq!(steps(&before), ["signing", "connecting"]);
}

/// Proves: a browser that closes the socket (or types, or breaks the protocol) while the host has
/// not answered ends the terminal at once: what it typed is dropped, a close frame ends it with
/// nothing told, a protocol error is told `bad_request`; either way the tunnel ends and the book
/// holds its close.
///
/// Catches: a terminal and a tunnel that stay up for a host that never answers after the person
/// has gone, until the idle timeout.
#[tokio::test]
async fn a_browser_that_leaves_while_the_host_is_silent_ends_the_tunnel() {
    let mut site = Site::new("term-stalled", Kind::Stall, pinned()).await;
    let mut browser = site.connect(HOST);
    browser.text(&site.hello(80, 24)).await;
    browser.status("signing").await;
    browser.status("connecting").await;
    let id = site.opened().await;
    browser.binary(b"typed too early").await;
    browser.text("so was this").await;
    browser.raw(&close_frame()).await;
    assert_eq!(browser.next().await, Sent::Close(1000));
    assert_eq!(browser.next().await, Sent::End);
    assert_eq!(site.rig.closed(id).await, Cause::Eof);

    let mut browser = site.connect(HOST);
    browser.text(&site.hello(80, 24)).await;
    browser.status("signing").await;
    browser.status("connecting").await;
    let id = site.opened().await;
    browser.raw(&[0x81, 0x01, b'x']).await;
    browser.closed("bad_request").await;
    assert_eq!(site.rig.closed(id).await, Cause::Eof);
}

/// Proves: a second terminal for a host the operator already holds a tunnel to is refused with the
/// tunnel's own refusal (`too_many_tunnels`) after its certificate was signed and `connecting`
/// told; the first terminal runs on.
///
/// Catches: the UI's terminals going round the limit of one tunnel per operator per host.
#[tokio::test]
async fn a_second_terminal_to_the_same_host_is_refused_by_the_tunnels_rules() {
    let site = site("term-second").await;
    let mut first = open_terminal(&site).await;
    let mut second = site.connect(HOST);
    second.text(&site.hello(80, 24)).await;
    let before = second.closed("too_many_tunnels").await;
    assert_eq!(steps(&before), ["signing", "connecting"]);
    assert_eq!(site.rig.ledger.certs().len(), 2);
    first.binary(b"still here\r").await;
    first.output_until(b"still here\r").await;
}

/// Proves: the browser closing the socket after the shell is open ends the SSH session and the
/// tunnel: the book holds the close, with a cause of the client's end going away, and the browser
/// is told nothing more.
///
/// Catches: a tunnel left open to a host after the person closed the page.
#[tokio::test]
async fn the_browser_closing_the_socket_ends_the_tunnel() {
    let mut site = site("term-browser-closes").await;
    let mut browser = open_terminal(&site).await;
    browser.output_until(b"welcome\r\n").await;
    browser.raw(&close_frame()).await;
    assert_eq!(browser.next().await, Sent::Close(1000));
    assert_eq!(browser.next().await, Sent::End);
    let id = site.opened().await;
    let cause = site.rig.closed(id).await;
    assert!(matches!(cause, Cause::Eof | Cause::HostClosed), "{cause:?}");
    let lines = site.rig.tunnel_lines();
    assert_eq!(lines.last().map(|line| line.1), Some(TUNNEL_CLOSE));
}

/// Proves: a revoked certificate, an operator taken out of the operators file and a session that
/// ended (signed out) each close the terminal at the next poll with the tunnel's cause (`revoked`,
/// `not_permitted`, `token_revoked`), told as the last message.
///
/// Catches: a terminal that outlives the certificate, the permission or the sign-in it was made
/// under.
#[tokio::test]
async fn a_revoked_certificate_a_removed_operator_and_an_ended_session_close_the_terminal() {
    let mut site = site("term-poll-ends").await;
    let mut browser = open_terminal(&site).await;
    let serial = *site
        .rig
        .ledger
        .certs()
        .keys()
        .next()
        .expect("a certificate");
    site.rig
        .ledger
        .revoke("operator", Revocation::Serial(serial))
        .expect("revoked");
    site.rig.poll().await;
    browser.closed("revoked").await;
    let id = site.opened().await;
    site.rig.closed(id).await;

    let mut browser = open_terminal(&site).await;
    std::fs::write(
        &site.rig.operators,
        "[[operator]]\nname = \"bob\"\nhosts = [\"node-08\"]\n",
    )
    .expect("a new operators file");
    site.rig.poll().await;
    browser.closed("not_permitted").await;
}

/// Proves: a terminal's credential is its login session and not a token: the operator of a session
/// who holds no token at all keeps their terminal through a poll; the poll closes it
/// `token_revoked` when the session is signed out, and when the session is past its idle deadline
/// (the poll asks without using the session, so it does not keep it alive).
///
/// Catches: a poll that kills every terminal of a user without a token, and a session that a poll
/// keeps alive for ever.
#[tokio::test]
async fn a_terminal_lives_as_long_as_its_session_and_needs_no_token() {
    let mut site = Site::with(
        "term-session-credential",
        Kind::Ssh(Behavior::good()),
        ("node-08", "bob"),
        pinned(),
    )
    .await;
    assert!(!site.rig.coordinator.tokens().has_name("bob"));
    let mut browser = site.connect("node-08");
    browser.text(&site.hello(80, 24)).await;
    browser.status("signing").await;
    browser.status("connecting").await;
    browser.status("opened").await;
    site.rig.poll().await;
    browser.binary(b"alive\r").await;
    browser.output_until(b"alive\r").await;
    let sessions = &site.rig.coordinator.login().expect("a sign-in").sessions;
    assert!(sessions.end(&site.session.0));
    site.rig.poll().await;
    browser.closed("token_revoked").await;
    let id = site.opened().await;
    site.rig.closed(id).await;

    let mut idle = Site::with(
        "term-session-idle",
        Kind::Ssh(Behavior::good()),
        ("node-08", "bob"),
        pinned(),
    )
    .await;
    let mut browser = idle.connect("node-08");
    browser.text(&idle.hello(80, 24)).await;
    browser.status("signing").await;
    browser.status("connecting").await;
    browser.status("opened").await;
    idle.rig.clock.fetch_add(1800, Ordering::SeqCst);
    idle.rig.poll().await;
    browser.closed("token_revoked").await;
    let id = idle.opened().await;
    idle.rig.closed(id).await;
}

/// Proves: what is typed in a terminal is a use of its session: a session that would have idled
/// out is still live at the poll after typing, and goes at its idle deadline from the last typing.
///
/// Catches: a terminal that is cut at the session's idle deadline while the person is typing in
/// it.
#[tokio::test]
async fn typing_in_a_terminal_keeps_its_session_from_idling_out() {
    let site = site("term-typing").await;
    let mut browser = open_terminal(&site).await;
    let sessions = &site.rig.coordinator.login().expect("a sign-in").sessions;
    site.rig.clock.fetch_add(1700, Ordering::SeqCst);
    browser.binary(b"a\r").await;
    browser.output_until(b"a\r").await;
    site.rig.clock.fetch_add(1700, Ordering::SeqCst);
    assert!(sessions.is_live(&site.session.1));
    site.rig.poll().await;
    browser.binary(b"b\r").await;
    browser.output_until(b"b\r").await;
    site.rig.clock.fetch_add(1800, Ordering::SeqCst);
    assert!(!sessions.is_live(&site.session.1));
}

/// Proves: after the shell is open, a resize outside 1..500 by 1..200, a text message that is not
/// a resize, a type that is not `resize` and a binary message of more than 16 KiB each end the
/// terminal `bad_request`.
///
/// Catches: a window size taken from a page without a range, and a message of any size reaching
/// the shell.
#[tokio::test]
async fn a_message_the_terminal_does_not_take_ends_it_bad_request() {
    let mut site = site("term-bad-input").await;
    let too_big = vec![0_u8; 16 * 1024 + 1];
    let cases: Vec<Vec<u8>> = vec![
        client_frame(1, br#"{"type":"resize","cols":0,"rows":24}"#),
        client_frame(1, br#"{"type":"resize","cols":80,"rows":201}"#),
        client_frame(1, br#"{"type":"scroll","cols":80,"rows":24}"#),
        client_frame(1, b"typed as text"),
        client_frame(2, &too_big),
    ];
    for frame in cases {
        let mut browser = open_terminal(&site).await;
        browser.raw(&frame).await;
        browser.closed("bad_request").await;
        let id = site.opened().await;
        site.rig.closed(id).await;
    }
}

/// Proves: a browser whose connection takes no write (it is gone) ends the terminal when the
/// first output cannot be sent, whether the output is the greeting the shell sent before its reply
/// or what the shell says after it; the tunnel ends.
///
/// Catches: a terminal that goes on carrying output to a browser it cannot reach.
#[tokio::test]
async fn output_that_cannot_be_sent_ends_the_terminal() {
    let early = Behavior::good();
    let late = Behavior {
        greets: false,
        speaks_after: true,
        ..Behavior::good()
    };
    for (behavior, name) in [(early, "term-broken-early"), (late, "term-broken-late")] {
        let mut site = Site::new(name, Kind::Ssh(behavior), pinned()).await;
        let mut browser = site.connect_broken(site.start(HOST));
        browser.text(&site.hello(80, 24)).await;
        let id = site.opened().await;
        (&mut browser.serving).await.expect("the terminal ended");
        site.rig.closed(id).await;
    }
}

/// Proves: a terminal whose upgrade was never handed over ends without doing anything.
///
/// Catches: a task that waits for ever for a connection that will not come.
#[tokio::test]
async fn an_upgrade_that_is_not_handed_over_ends_the_task() {
    let site = site("term-no-upgrade").await;
    let mut request = hyper::Request::new(());
    let upgrade = hyper::upgrade::on(&mut request);
    run(site.start(HOST), upgrade).await;
    assert!(site.rig.ledger.certs().is_empty());
}

/// Proves: the token a refusal is told by is its name in lower case (`not_permitted`), as the
/// audit writes it, and the reason of the terminal's certificate is the words the design names.
///
/// Catches: a page that switches on a token the coordinator does not send.
#[test]
fn a_refusal_is_told_by_its_lower_case_name() {
    assert_eq!(refusal_token(SshRefusal::NotPermitted), "not_permitted");
    assert_eq!(
        refusal_token(SshRefusal::TooManyTunnels),
        "too_many_tunnels"
    );
    assert_eq!(super::REASON, "coordinator UI terminal");
}
