//! A console stream after its upgrade, over an in-memory connection served by hyper, the scripted
//! BMC tool and the dial double: the bytes both ways, the clean-up, and each way a stream closes
//! with its code, its reason and its one close line.

use hostkeeper_console::bmc::MAC_READ;
use hostkeeper_console::ipmi::{SOL_ESCAPE, ShellError};
use hostkeeper_console::scripted::{Seen, SolHost};
use hostkeeper_console::sol::{PAYLOAD_STATUS, SOL_DEACTIVATE, SOL_INFO};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::attach_support::{ALICE, Client, Frame, Live, PEER};
use super::support::{MAC, MAC_ADDRESS, NODE, NODE_BMC_ANSWER, caller, session_id};
use crate::websocket::tests::{client_frame, raw_frame};

/// `sol info` with both rates at 115.2.
const INFO: &str =
    "Volatile Bit Rate (kbps)        : 115.2\nNon-Volatile Bit Rate (kbps)    : 115.2\n";

/// The tool's banner, as it comes before the host's first bytes.
const BANNER: &[u8] = b"[SOL Session operational.  Use \xff? for help]\n";

/// Scripts the activating session of `node-02` and the deactivating one after it.
fn serial(live: &Live) -> SolHost {
    let host = live
        .fixture
        .tool
        .sol_session(&[NODE_BMC_ANSWER, " 01 00 00\n", INFO]);
    live.fixture.tool.session(&[NODE_BMC_ANSWER, ""]);
    host
}

/// What the tool was asked, without the arguments of each open.
fn asked(live: &Live) -> Vec<String> {
    live.fixture
        .tool
        .seen()
        .into_iter()
        .map(|seen| match seen {
            Seen::Open { escape, .. } => format!("open escape={escape:?}"),
            Seen::Ask(command) => command,
            Seen::Closed => "closed".to_owned(),
        })
        .collect()
}

/// The commands of an activation and of the deactivation after it.
fn activated_then_deactivated() -> Vec<String> {
    [
        "open escape=None",
        MAC_READ,
        PAYLOAD_STATUS,
        SOL_INFO,
        "closed",
        &format!("open escape={:?}", Some(SOL_ESCAPE)),
        "closed",
        "open escape=None",
        MAC_READ,
        SOL_DEACTIVATE,
        "closed",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The `console_close` reason of the first session of `kind` opened by `alice`, without its
/// `ended_unix` (the audit's own clock).
fn close_reason(kind: &str, cause: &str, secs: u64, to_device: usize, to_client: usize) -> String {
    format!(
        "session={}; kind={kind}; opened_by=alice; cause={cause}; peer={PEER}; \
         secs={secs}; to_device={to_device}; to_client={to_client}",
        session_id(1)
    )
}

/// `reason` without its `ended_unix` detail.
pub(crate) fn without_ended(reason: &str) -> String {
    reason
        .split("; ")
        .filter(|detail| !detail.starts_with("ended_unix="))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Proves: a Mac's screen stream dials the Mac's address and carries every byte both ways
/// unchanged, binary and text messages of the client alike, the Mac's bytes as binary messages;
/// the session shows attached; a Mac that closes its connection ends the stream with a normal
/// close, `device_closed`, and one close line with the bytes each way.
///
/// Catches: a stream that changes RFB's bytes (0xff and all), drops a text message, or outlives
/// the Mac's connection.
#[tokio::test]
async fn bytes_cross_the_bridge_both_ways_unchanged() {
    let live = Live::new("stream-screen");
    let mut mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let mut client = live.attach(&session, ALICE).await;
    let every: Vec<u8> = (0..=255).collect();
    client.binary(&every).await;
    let mut got = vec![0u8; 256];
    mac.read_exact(&mut got).await.expect("the client's bytes");
    assert_eq!(got, every);
    client.text("RFB 003.008\n").await;
    let mut got = vec![0u8; 12];
    mac.read_exact(&mut got).await.expect("the text's bytes");
    assert_eq!(got, b"RFB 003.008\n");
    mac.write_all(&every).await.expect("written");
    assert_eq!(client.output(256).await, every);
    assert_eq!(live.fixture.dials.dialled(), [MAC_ADDRESS]);
    let listed = live
        .coordinator
        .console_list(&caller("alice"), &proto::ListConsoleSessionsRequest {})
        .expect("listed");
    assert!(listed.sessions[0].attached);
    drop(mac);
    client.closed(1000, "device_closed").await;
    live.gone(MAC).await;
    assert_eq!(
        without_ended(&live.last_reason()),
        close_reason("screen", "device_closed", 0, 268, 256)
    );
}

/// Proves: a serial stream activates SOL in one checked session, drops the tool's banner and the
/// escape byte, carries the rest both ways; when the client leaves, the tool is stopped, the
/// payload is deactivated in a session of its own after the MAC read, the session leaves the
/// table, and one close line by the operator says `client_left` with the peer, the seconds and
/// the bytes each way.
///
/// Catches: a payload left active when the tab closes (the host stays `bmc_sol_busy` until
/// someone deactivates it by hand), the banner or 0xff passed through, and a close line missing
/// what the audit needs.
#[tokio::test]
async fn sol_deactivates_when_the_browser_leaves() {
    let live = Live::new("stream-serial");
    let mut host = serial(&live);
    let session = live.open("alice", NODE);
    let mut client = live.attach(&session, ALICE).await;
    let mut said = BANNER.to_vec();
    said.extend_from_slice(b"login: ");
    host.say(&said).await;
    assert_eq!(client.output(7).await, b"login: ");
    client.binary(b"root\xff\r").await;
    assert_eq!(host.typed.recv().await, Some(b"root\r".to_vec()));
    live.fixture.rig.at(2_042);
    client.raw(&client_frame(8, &1000u16.to_be_bytes())).await;
    client.closed(1000, "client_left").await;
    live.gone(NODE).await;
    assert_eq!(asked(&live), activated_then_deactivated());
    let lines = live.lines();
    let (by, action, node, _) = lines.last().expect("a line");
    assert_eq!(
        (by.as_str(), action.as_str(), node.as_str()),
        ("alice", "console_close", NODE)
    );
    assert_eq!(
        without_ended(&live.last_reason()),
        close_reason("serial", "client_left", 42, 5, 7)
    );
}

/// Proves: a payload someone else holds when the stream attaches closes it 4002 `bmc_sol_busy`
/// with nothing activated and nothing deactivated; one that another session takes in the middle
/// (the tool ends saying the BMC closed its session) closes it 4002 too, and is not deactivated.
///
/// Catches: the coordinator taking a lab user's SOL session, or deactivating the payload of the
/// session that took it.
#[tokio::test]
async fn a_foreign_sol_holder_is_bmc_sol_busy_and_is_not_deactivated() {
    let live = Live::new("stream-sol-busy");
    live.fixture.tool.session(&[NODE_BMC_ANSWER, " 01 01 00\n"]);
    let session = live.open("alice", NODE);
    let mut client = live.attach(&session, ALICE).await;
    client.closed(4002, "bmc_sol_busy").await;
    live.gone(NODE).await;
    assert_eq!(live.fixture.tool.unused(), 0);
    assert!(!asked(&live).contains(&SOL_DEACTIVATE.to_owned()));

    let live = Live::new("stream-sol-taken");
    let host = live
        .fixture
        .tool
        .sol_session(&[NODE_BMC_ANSWER, " 01 00 00\n", INFO]);
    let session = live.open("alice", NODE);
    let mut client = live.attach(&session, ALICE).await;
    host.say(b"\r\n").await;
    assert_eq!(client.output(2).await, b"\r\n");
    host.exit("Error sending SOL data: FAIL\nSOL session closed by BMC\n");
    client.closed(4002, "bmc_sol_busy").await;
    live.gone(NODE).await;
    assert_eq!(asked(&live).last().map(String::as_str), Some("closed"));
    assert!(!asked(&live).contains(&SOL_DEACTIVATE.to_owned()));
    let (by, ..) = live.lines().pop().expect("a line");
    assert_eq!(by, "coordinator");
}

/// Proves: each failure after the upgrade is a close frame with its code and reason token and one
/// close line with the same cause, by `coordinator`: a BMC that does not answer 4001, a full one
/// 4003, a refused credential 4004, another MAC 4009, a tool that cannot start 4013, a tool that
/// ends mid-stream without the BMC's word 4001 (and its payload is then deactivated), a Mac's
/// refused port 4011 and a Mac that does not answer 4012.
///
/// Catches: a failure the browser cannot tell from another (a WebSocket cannot read an HTTP
/// status after the upgrade), and a failure that leaves no audit line.
#[tokio::test]
async fn a_failure_after_the_upgrade_is_a_close_code_and_reason() {
    const NO_SESSION: &str = "Error: Unable to establish IPMI v2 / RMCP+ session\n";
    type Script = fn(&Live);
    let cases: [(&str, Script, u16, &str); 5] = [
        (
            "unreachable",
            |live| {
                live.fixture.tool.end_at_start(NO_SESSION);
                live.fixture.tool.end_at_start(NO_SESSION);
            },
            4001,
            "device_unreachable",
        ),
        (
            "full",
            |live| {
                live.fixture
                    .tool
                    .end_at_start("insufficient resources for session\n")
            },
            4003,
            "bmc_busy",
        ),
        (
            "auth",
            |live| {
                live.fixture.tool.end_at_start(NO_SESSION);
                live.fixture.tool.end_at_start("RAKP 2 HMAC is invalid\n");
            },
            4004,
            "bmc_auth_failed",
        ),
        (
            "mismatch",
            |live| live.fixture.tool.session(&[" 11 0c c4 7a 00 00 09\n"]),
            4009,
            "address_mismatch",
        ),
        (
            "no tool",
            |live| live.fixture.tool.refuse(ShellError::Start),
            4013,
            "bmc_failed",
        ),
    ];
    for (name, script, code, reason) in cases {
        let live = Live::new(&format!("stream-fail-{}", name.replace(' ', "-")));
        script(&live);
        let session = live.open("alice", NODE);
        let mut client = live.attach(&session, ALICE).await;
        client.closed(code, reason).await;
        live.gone(NODE).await;
        let (by, action, ..) = live.lines().pop().expect("a line");
        assert_eq!(
            (by.as_str(), action.as_str()),
            ("coordinator", "console_close"),
            "{name}"
        );
        assert!(
            live.last_reason().contains(&format!("cause={reason};")),
            "{name}"
        );
        assert!(!asked(&live).contains(&SOL_DEACTIVATE.to_owned()), "{name}");
    }

    let live = Live::new("stream-fail-lost");
    let host = serial(&live);
    let session = live.open("alice", NODE);
    let mut client = live.attach(&session, ALICE).await;
    host.exit("Error: no response from BMC\n");
    client.closed(4001, "device_unreachable").await;
    live.gone(NODE).await;
    assert_eq!(asked(&live), activated_then_deactivated());

    for (kind, code, reason) in [
        (
            std::io::ErrorKind::ConnectionRefused,
            4011,
            "mac_screen_sharing_off",
        ),
        (
            std::io::ErrorKind::TimedOut,
            4012,
            "mac_unreachable_or_asleep",
        ),
        (
            std::io::ErrorKind::HostUnreachable,
            4012,
            "mac_unreachable_or_asleep",
        ),
    ] {
        let live = Live::new(&format!("stream-fail-mac-{code}-{kind:?}"));
        live.fixture.dials.fail(kind);
        let session = live.open("alice", MAC);
        let mut client = live.attach(&session, ALICE).await;
        client.closed(code, reason).await;
        live.gone(MAC).await;
        assert!(live.last_reason().contains(&format!("cause={reason};")));
    }
}

/// Proves: `Console/Close` of an attached session closes its stream 4007 `closed_by_operator`,
/// deactivates the payload, and the one close line is by the closer, not the opener; the session
/// is gone once the line is written, and the host can be opened again.
///
/// Catches: a close that answers but leaves the stream and the payload running, and a close line
/// that hides who closed it.
#[tokio::test]
async fn an_operators_close_ends_the_stream_with_its_code() {
    let live = Live::new("stream-close");
    let _host = serial(&live);
    let session = live.open("alice", NODE);
    let mut client = live.attach(&session, ALICE).await;
    live.coordinator
        .console_close(
            &caller("bob"),
            &proto::CloseConsoleRequest {
                host_id: NODE.to_owned(),
            },
        )
        .expect("closed");
    client.closed(4007, "closed_by_operator").await;
    live.gone(NODE).await;
    assert_eq!(asked(&live), activated_then_deactivated());
    let (by, action, ..) = live.lines().pop().expect("a line");
    assert_eq!((by.as_str(), action.as_str()), ("bob", "console_close"));
    assert!(live.last_reason().contains("cause=closed_by_operator;"));
    assert_eq!(live.open("bob", NODE), session_id(2));
}

/// Proves: a client whose connection drops with no close frame ends the stream `client_left`, its
/// payload deactivated; a client that breaks the protocol (a frame with a reserved bit) closes the stream 1002
/// `protocol_error`, and one that sends a message over 64 KiB closes it 1009 `message_too_big`,
/// the payload deactivated either way; a ping is answered with its payload while the stream runs.
///
/// Catches: a stream that keeps reading a broken client, buffers a message of any size, or never
/// answers the pings a client keeps its connection alive with.
#[tokio::test]
async fn a_client_that_breaks_the_protocol_is_closed_with_its_code() {
    let live = Live::new("stream-protocol");
    let _host = serial(&live);
    let session = live.open("alice", NODE);
    let mut client = live.attach(&session, ALICE).await;
    client.raw(&client_frame(9, b"alive")).await;
    assert_eq!(client.next().await, Frame::Pong(b"alive".to_vec()));
    client.raw(&raw_frame(0xc2, b"compressed")).await;
    client.closed(1002, "protocol_error").await;
    live.gone(NODE).await;
    assert_eq!(asked(&live), activated_then_deactivated());

    let live = Live::new("stream-dropped");
    let _host = serial(&live);
    let session = live.open("alice", NODE);
    let client = live.attach(&session, ALICE).await;
    drop(client);
    live.gone(NODE).await;
    assert!(live.last_reason().contains("cause=client_left;"));
    assert_eq!(asked(&live), activated_then_deactivated());

    let live = Live::new("stream-too-big");
    let mut mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let mut client = live.attach(&session, ALICE).await;
    let mut head = vec![0x82, 0x80 | 127];
    head.extend_from_slice(&(64 * 1024 + 1_u64).to_be_bytes());
    client.raw(&head).await;
    client.closed(1009, "message_too_big").await;
    live.gone(MAC).await;
    let mut rest = Vec::new();
    mac.read_to_end(&mut rest).await.expect("the Mac's end");
    assert_eq!(rest, b"");
}

/// Proves: a stream whose device is no longer found at its attach (the targets file lost its
/// address between the open and the attach) closes 4001; one whose operator lost access in that
/// time closes 4008; nothing is dialled or activated.
///
/// Catches: a device reached with an address or an operator the files no longer give.
#[tokio::test]
async fn a_device_or_operator_gone_by_the_attach_closes_the_stream() {
    let live = Live::new("stream-no-target");
    let session = live.open("alice", MAC);
    std::fs::write(&live.fixture.files.targets, "").expect("emptied");
    live.coordinator.console().files().reload();
    let mut client = live.attach(&session, ALICE).await;
    client.closed(4001, "device_unreachable").await;
    live.gone(MAC).await;
    assert_eq!(live.fixture.dials.dialled(), Vec::<String>::new());

    let live = Live::new("stream-no-entry");
    let session = live.open("alice", MAC);
    let attached =
        super::route::attach(&live.coordinator, &parse(&session), "alice", None).expect("attached");
    std::fs::write(
        &live.fixture.files.operators,
        "[[operator]]\nname = \"bob\"\n",
    )
    .expect("written");
    live.coordinator.console().files().reload();
    let (client, server) = tokio::io::duplex(1 << 16);
    tokio::spawn(super::stream::serve(attached, Box::new(server)));
    let mut client = Client { stream: client };
    client.closed(4008, "token_revoked").await;
    live.gone(MAC).await;
    assert_eq!(live.fixture.dials.dialled(), Vec::<String>::new());
    assert!(!live.last_reason().contains("peer="));
}

/// The session id `text`.
fn parse(text: &str) -> hostkeeper_console::session::SessionId {
    hostkeeper_console::session::SessionId::parse(text).expect("an id")
}

/// Proves: over the listener's own connection (TLS, HTTP/1.1), an upgraded stream whose client
/// reads nothing while the device sends far more than the connection holds is not cut when the
/// patience passes, many times over on the paused clock: the client then reads every byte, in
/// order. While it lives it holds its session, and with one session allowed an open of another
/// host is `too_many_sessions`.
///
/// Catches: a console cut after ten seconds of a browser tab in the background (the listener's
/// write deadline), and an upgraded connection outside the session cap.
#[tokio::test(start_paused = true)]
async fn an_upgraded_connection_leaves_the_patience_timer_and_counts_against_the_session_cap() {
    use rustls::pki_types::ServerName;

    use crate::http::Surface;
    use crate::listen::tests::{connector, tls_material};
    use crate::listen::{CLIENT_PATIENCE, connection};

    let live = Live::with("stream-patience", 1, std::time::Duration::from_secs(30));
    let mut mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let (source, ca) = tls_material();
    let (client, server) = tokio::io::duplex(4 * 1024);
    tokio::spawn(connection(
        server,
        source,
        Surface::Control,
        std::sync::Arc::clone(&live.coordinator),
        CLIENT_PATIENCE,
    ));
    let mut tls = connector(&ca, &[b"http/1.1"])
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .expect("a handshake");
    let mut headers = super::attach_support::good_headers(ALICE);
    headers[0].1 = "localhost".to_owned();
    headers[1].1 = "https://localhost".to_owned();
    let head = super::attach_support::send(
        &mut tls,
        &super::attach_support::upgrade(&session, &headers),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
    let screen: Vec<u8> = (0..256 * 1024).map(|at: usize| (at % 251) as u8).collect();
    let sent = screen.clone();
    let sending = tokio::spawn(async move {
        mac.write_all(&sent).await.expect("written");
        mac
    });
    tokio::time::sleep(CLIENT_PATIENCE * 6).await;
    let opened = live.coordinator.console_open(
        &caller("alice"),
        &proto::OpenConsoleRequest {
            host_id: NODE.to_owned(),
            reason: "a second one".to_owned(),
        },
    );
    assert_eq!(opened.err(), Some(proto::ConsoleRefusal::TooManySessions));
    let mut client = Client { stream: tls };
    assert_eq!(client.output(screen.len()).await, screen);
    drop(sending.await.expect("the Mac's end"));
    client.closed(1000, "device_closed").await;
    live.gone(MAC).await;
}

/// Proves: a deactivation that cannot be made is logged and does not stop the end: with a BMC
/// that now reports another MAC, nothing but the MAC read is sent and the session still ends with
/// its close line; with the host's address gone from the targets file, no session of the tool is
/// opened at all.
///
/// Catches: a stream whose clean-up failure leaves its session in the table (the host
/// `console_busy` for ever), and a deactivate sent to a BMC that is not the host's.
#[tokio::test]
async fn a_deactivation_that_cannot_be_made_still_ends_the_session() {
    let live = Live::new("stream-deactivate-mismatch");
    let _host = live
        .fixture
        .tool
        .sol_session(&[NODE_BMC_ANSWER, " 01 00 00\n", INFO]);
    live.fixture.tool.session(&[" 11 0c c4 7a 00 00 09\n"]);
    let session = live.open("alice", NODE);
    let mut client = live.attach(&session, ALICE).await;
    client.raw(&client_frame(8, &1000u16.to_be_bytes())).await;
    client.closed(1000, "client_left").await;
    live.gone(NODE).await;
    let commands = asked(&live);
    assert_eq!(
        commands[commands.len() - 3..],
        ["open escape=None", MAC_READ, "closed"]
    );
    assert!(live.last_reason().contains("cause=client_left;"));

    let live = Live::new("stream-deactivate-no-target");
    let _host = live
        .fixture
        .tool
        .sol_session(&[NODE_BMC_ANSWER, " 01 00 00\n", INFO]);
    let session = live.open("alice", NODE);
    let mut client = live.attach(&session, ALICE).await;
    std::fs::write(&live.fixture.files.targets, "").expect("emptied");
    live.coordinator.console().files().reload();
    client.raw(&client_frame(8, &1000u16.to_be_bytes())).await;
    client.closed(1000, "client_left").await;
    live.gone(NODE).await;
    assert_eq!(asked(&live).last().map(String::as_str), Some("closed"));
    assert_eq!(
        asked(&live)
            .iter()
            .filter(|c| c.starts_with("open"))
            .count(),
        2
    );
}

/// Proves: when a stream whose client reads nothing is closed (here by `Console/Close`) while the
/// Mac floods it, the end of the output and the close frame are waited for one grace each on the
/// paused clock and then given up: the session ends with its close line, and nothing holds the
/// connection any more, though the client never read (a write to it finds it closed).
///
/// Catches: a stream whose stuck writer keeps the client's connection open for ever after the
/// session ended, outside every limit of the listener and the session cap.
#[tokio::test(start_paused = true)]
async fn a_client_that_reads_nothing_is_cut_at_the_grace() {
    let live = Live::new("stream-grace");
    let mut mac = live.fixture.dials.connect();
    let attached = live.attach_unserved(MAC);
    let (mut client, server) = tokio::io::duplex(1024);
    tokio::spawn(super::stream::serve(attached, Box::new(server)));
    let flooding = tokio::spawn(async move {
        let _ = mac.write_all(&vec![7u8; 256 * 1024]).await;
        mac
    });
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    live.coordinator
        .console_close(
            &caller("bob"),
            &proto::CloseConsoleRequest {
                host_id: MAC.to_owned(),
            },
        )
        .expect("closed");
    live.gone(MAC).await;
    assert!(live.last_reason().contains("cause=closed_by_operator;"));
    // The aborted tasks are dropped at their next turn: then nothing holds the server's end.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let written = client.write_all(b"still there?").await;
    assert_eq!(
        written.map_err(|why| why.kind()),
        Err(std::io::ErrorKind::BrokenPipe)
    );
    flooding.abort();
}

/// Proves: for a Mac's screen session the coordinator reads and consumes nothing of the device's
/// stream before it bridges: the RFB greeting the Mac writes before the client sends anything is
/// the client's first binary message, byte for byte, and the client's reply reaches the Mac.
///
/// Catches: a stream that peeks at or swallows the Mac's greeting, which would break the
/// browser's RFB handshake.
#[tokio::test]
async fn the_stream_is_bridged_from_the_first_byte_unread() {
    let live = Live::new("stream-first-byte");
    let mut mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    mac.write_all(b"RFB 003.008\n").await.expect("written");
    let mut client = live.attach(&session, ALICE).await;
    assert_eq!(client.output(12).await, b"RFB 003.008\n");
    client.binary(b"RFB 003.008\n").await;
    let mut got = vec![0u8; 12];
    mac.read_exact(&mut got).await.expect("the client's bytes");
    assert_eq!(got, b"RFB 003.008\n");
}
