//! `Console/Probe` over the scripted BMC tool and the timer double: what it finds in what the host
//! says, that it types nothing, how its read ends, and that every way it ends deactivates the
//! payload it activated, empties the table and writes one close line. No clock: the read's timers
//! fire when a test fires them, once the probe's progress (a timer armed) says it got so far.

use std::sync::Arc;
use std::time::Duration;

use hostkeeper_console::close::Close;
use hostkeeper_console::sol::SOL_DEACTIVATE;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleProbeClass as Class;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleProbeEnd as End;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;

use super::attach_support::Live;
use super::probe::{DEFAULT_MAX_BYTES, MAX_WAIT_SECS, MOST_BYTES, Window, unreached};
use super::stream_tests::{activated_then_deactivated, asked, serial, without_ended};
use super::support::{MAC, NODE, NODE_BMC_ANSWER, caller, session_id};

/// The read's longest wait and its quiet time, in the requests of these tests.
const WAIT: Duration = Duration::from_secs(60);
const QUIET: Duration = Duration::from_secs(5);

/// The tool's banner, which the bridge drops before the host's first bytes.
const BANNER: &[u8] = b"[SOL Session operational.  Use \xff? for help]\n";

/// A probe of `host` by `alice`, waiting [`WAIT`] at most and [`QUIET`] without a byte.
fn request(host: &str) -> proto::ProbeConsoleRequest {
    proto::ProbeConsoleRequest {
        host_id: host.to_owned(),
        reason: "probe it".to_owned(),
        wait_secs: 60,
        quiet_secs: 5,
        max_bytes: 0,
    }
}

/// Starts the probe of `request` by `by` in a task: its answer, once it ends.
fn start(
    live: &Live,
    by: &str,
    request: proto::ProbeConsoleRequest,
) -> tokio::task::JoinHandle<Result<proto::ProbeConsoleResponse, Refusal>> {
    let coordinator = Arc::clone(&live.coordinator);
    tokio::spawn(coordinator.console_probe(caller(by), request))
}

/// The finding of `class` shown by `evidence`.
fn finding(class: Class, evidence: &str) -> proto::ConsoleProbeFinding {
    proto::ConsoleProbeFinding {
        class: class.into(),
        evidence: evidence.to_owned(),
    }
}

/// The `console_close` reason of the first session, opened by `alice`, without its `ended_unix`.
fn close_reason(cause: &str, to_client: usize) -> String {
    format!(
        "session={}; kind=serial; opened_by=alice; cause={cause}; secs=0; to_device=0; \
         to_client={to_client}",
        session_id(1)
    )
}

/// Whether the console lists no session.
fn listed_none(live: &Live) -> bool {
    live.coordinator
        .console_list(&caller("alice"), &proto::ListConsoleSessionsRequest {})
        .expect("listed")
        .sessions
        .is_empty()
}

/// Proves: a probe of a host at its login prompt answers `login_prompt` with the prompt's line as
/// its evidence (the banner and the lines before it not a finding of their own), the bytes read
/// without the banner, its end `quiet` and its cause `client_left`; it arms the wait and the quiet
/// time it was asked for; the host was sent no byte at all; and the payload was activated, then
/// deactivated, the session is gone from the list, and the two lines are `console_open` with the
/// reason and `console_close` by the operator with no byte to the device.
///
/// Catches: a probe that presses Enter to wake the prompt (a login attempt on a live node), one
/// that leaves the payload active or the session listed, and a close line that is not the
/// operator's.
#[tokio::test]
async fn a_login_prompt_is_found_without_typing_and_the_session_is_cleaned_up() {
    let (live, timers) = Live::timed("probe-login");
    let mut host = serial(&live);
    let probing = start(&live, "alice", request(NODE));
    timers.armed(2).await;
    assert_eq!(timers.durations(), [WAIT, QUIET]);
    let said = b"\r\nUbuntu 24.04.1 LTS node-02 ttyS1\r\n\r\nnode-02 login: ";
    host.say(&[BANNER, said.as_slice()].concat()).await;
    timers.armed(3).await;
    timers.fire(QUIET);
    let answer = probing.await.expect("ended").expect("probed");
    assert_eq!(
        answer,
        proto::ProbeConsoleResponse {
            findings: vec![finding(Class::LoginPrompt, "node-02 login:")],
            from_boot: false,
            bytes_read: said.len() as u64,
            ended: End::Quiet.into(),
            cause: "client_left".to_owned(),
        }
    );
    assert_eq!(host.typed.recv().await, None, "nothing was typed");
    assert_eq!(asked(&live), activated_then_deactivated());
    assert!(listed_none(&live));
    let lines = live.lines();
    let actions: Vec<_> = lines
        .iter()
        .map(|(by, action, node, _)| (by.as_str(), action.as_str(), node.as_str()))
        .collect();
    assert_eq!(
        actions,
        [
            ("alice", "console_open", NODE),
            ("alice", "console_close", NODE)
        ]
    );
    assert_eq!(
        lines[0].3,
        format!("probe it; session={}; kind=serial", session_id(1))
    );
    assert_eq!(
        without_ended(&live.last_reason()),
        close_reason("client_left", said.len())
    );
}

/// Proves: a host that says nothing is `silent` with no evidence and no byte read, the read ends
/// at its quiet time, and the payload is still deactivated and the session gone.
///
/// Catches: a probe that calls a host with no output anything but silent, or that skips the
/// clean-up when it read nothing.
#[tokio::test]
async fn a_host_that_says_nothing_is_silent() {
    let (live, timers) = Live::timed("probe-silent");
    let mut host = serial(&live);
    let probing = start(&live, "alice", request(NODE));
    timers.armed(2).await;
    timers.fire(QUIET);
    let answer = probing.await.expect("ended").expect("probed");
    assert_eq!(answer.findings, [finding(Class::Silent, "")]);
    assert_eq!((answer.bytes_read, answer.ended()), (0, End::Quiet));
    assert_eq!(host.typed.recv().await, None);
    assert_eq!(asked(&live), activated_then_deactivated());
    assert!(listed_none(&live));
}

/// Proves: a probe that reads a boot from before the kernel's first line answers `from_boot`, the
/// firmware's text before that line (its screen-control sequences taken out) as
/// `firmware_output`, the first kernel line as `kernel_output` and the prompt as `login_prompt`,
/// in the order they came, over pieces read one after another; each piece re-arms the quiet time.
///
/// Catches: a probe that claims firmware output without the kernel's first line, or misses it in
/// a boot it saw whole.
#[tokio::test]
async fn a_boot_read_whole_shows_the_firmware_the_kernel_and_the_prompt() {
    let (live, timers) = Live::timed("probe-boot");
    let host = serial(&live);
    let probing = start(&live, "alice", request(NODE));
    timers.armed(2).await;
    let pieces: [&[u8]; 3] = [
        b"\x1b[2J\x1b[1;1HPress <DEL> to run setup\r\n",
        b"[    0.000000] Linux version 6.8.0-49-generic\r\n[    1.204211] ata1: SATA link up\r\n",
        b"node-02 login: ",
    ];
    for (at, piece) in pieces.iter().enumerate() {
        host.say(piece).await;
        timers.armed(3 + at).await;
    }
    timers.fire(QUIET);
    let answer = probing.await.expect("ended").expect("probed");
    assert_eq!(
        answer.findings,
        [
            finding(Class::FirmwareOutput, "Press <DEL> to run setup"),
            finding(
                Class::KernelOutput,
                "[    0.000000] Linux version 6.8.0-49-generic"
            ),
            finding(Class::LoginPrompt, "node-02 login:"),
        ]
    );
    assert!(answer.from_boot);
    assert_eq!(
        answer.bytes_read,
        pieces.iter().map(|piece| piece.len() as u64).sum::<u64>()
    );
}

/// Proves: the read ends once `max_bytes` came, at that count, whatever the host sends after it,
/// and only those bytes are classified; with no limit named it is 64 KiB.
///
/// Catches: a probe that reads past its limit, or classifies bytes it did not count.
#[tokio::test]
async fn the_read_ends_at_its_byte_count() {
    let (live, timers) = Live::timed("probe-bytes");
    let host = serial(&live);
    let probing = start(
        &live,
        "alice",
        proto::ProbeConsoleRequest {
            max_bytes: 4,
            ..request(NODE)
        },
    );
    timers.armed(2).await;
    host.say(b"abcdefgh login:").await;
    let answer = probing.await.expect("ended").expect("probed");
    assert_eq!(answer.findings, [finding(Class::OtherOutput, "abcd")]);
    assert_eq!((answer.bytes_read, answer.ended()), (4, End::MaxBytes));
    assert_eq!(answer.cause, "client_left");
    assert_eq!(asked(&live), activated_then_deactivated());
    assert_eq!(DEFAULT_MAX_BYTES, 65_536);
}

/// Proves: a host that keeps talking ends the read at `wait_secs`, its end `wait`, with what it
/// read so far.
///
/// Catches: a probe that never ends on a host whose output never stops (a boot loop).
#[tokio::test]
async fn the_read_ends_at_its_wait() {
    let (live, timers) = Live::timed("probe-wait");
    let host = serial(&live);
    let probing = start(&live, "alice", request(NODE));
    timers.armed(2).await;
    host.say(b"[ 4211.000001] eth0: link up\r\n").await;
    timers.armed(3).await;
    timers.fire(WAIT);
    let answer = probing.await.expect("ended").expect("probed");
    assert_eq!(
        answer.findings,
        [finding(Class::KernelOutput, "[ 4211.000001] eth0: link up")]
    );
    assert!(!answer.from_boot);
    assert_eq!(answer.ended(), End::Wait);
    assert_eq!(asked(&live), activated_then_deactivated());
}

/// Proves: a read that fails (the BMC tool ends mid-read without the BMC's word) still answers
/// what came before, its end `stream_ended` with the cause `device_unreachable`, and the payload
/// is deactivated, the session gone and its close line by `coordinator`.
///
/// Catches: a failed read that leaves the payload active (the next open is `bmc_sol_busy` until
/// someone deactivates it by hand) or the session listed.
#[tokio::test]
async fn a_failed_read_still_closes_and_deactivates() {
    let (live, timers) = Live::timed("probe-lost");
    let host = serial(&live);
    let probing = start(&live, "alice", request(NODE));
    timers.armed(2).await;
    host.say(b"node-02 login: ").await;
    timers.armed(3).await;
    host.exit("Error: no response from BMC\n");
    let answer = probing.await.expect("ended").expect("probed");
    assert_eq!(
        answer.findings,
        [finding(Class::LoginPrompt, "node-02 login:")]
    );
    assert_eq!(answer.ended(), End::StreamEnded);
    assert_eq!(answer.cause, "device_unreachable");
    assert_eq!(asked(&live), activated_then_deactivated());
    assert!(listed_none(&live));
    let (by, action, ..) = live.lines().pop().expect("a line");
    assert_eq!(
        (by.as_str(), action.as_str()),
        ("coordinator", "console_close")
    );
}

/// Proves: a payload another session holds is refused `bmc_sol_busy` with nothing activated and
/// nothing deactivated, a BMC that does not answer `device_unreachable`; either way no timer is
/// armed (nothing was read), the session is gone and its one close line by `coordinator` names
/// the cause.
///
/// Catches: a probe that takes a lab user's SOL session, or answers `silent` for a host it never
/// reached.
#[tokio::test]
async fn a_device_not_reached_is_refused_and_its_session_closed() {
    let (live, timers) = Live::timed("probe-sol-busy");
    live.fixture.tool.session(&[NODE_BMC_ANSWER, " 01 01 00\n"]);
    let refused = start(&live, "alice", request(NODE)).await.expect("ended");
    assert_eq!(refused, Err(Refusal::BmcSolBusy));
    assert_eq!(live.fixture.tool.unused(), 0);
    assert!(!asked(&live).contains(&SOL_DEACTIVATE.to_owned()));
    assert!(timers.durations().is_empty());
    assert!(listed_none(&live));
    assert!(live.last_reason().contains("cause=bmc_sol_busy;"));

    let (live, timers) = Live::timed("probe-unreachable");
    let no_session = "Error: Unable to establish IPMI v2 / RMCP+ session\n";
    live.fixture.tool.end_at_start(no_session);
    live.fixture.tool.end_at_start(no_session);
    let refused = start(&live, "alice", request(NODE)).await.expect("ended");
    assert_eq!(refused, Err(Refusal::DeviceUnreachable));
    assert!(!asked(&live).contains(&SOL_DEACTIVATE.to_owned()));
    assert!(timers.durations().is_empty());
    assert!(listed_none(&live));
    let (by, ..) = live.lines().pop().expect("a line");
    assert_eq!(by, "coordinator");
    assert!(live.last_reason().contains("cause=device_unreachable;"));
}

/// Proves: a device that fails after its payload was activated but before the host said a byte
/// is refused as a device not reached, not answered `silent`: another session taking the payload
/// is `bmc_sol_busy` and that payload is not deactivated; a BMC that stops answering is
/// `device_unreachable` and the payload is deactivated; either way the session is gone and its
/// close line by `coordinator` names the cause.
///
/// Catches: a probe that reports a host it never read as a console that says nothing.
#[tokio::test]
async fn a_device_that_fails_before_any_byte_is_refused_not_silent() {
    let (live, timers) = Live::timed("probe-taken");
    let host = serial(&live);
    let probing = start(&live, "alice", request(NODE));
    timers.armed(2).await;
    host.exit("Error sending SOL data: FAIL\nSOL session closed by BMC\n");
    assert_eq!(probing.await.expect("ended"), Err(Refusal::BmcSolBusy));
    assert!(!asked(&live).contains(&SOL_DEACTIVATE.to_owned()));
    assert!(listed_none(&live));
    assert!(live.last_reason().contains("cause=bmc_sol_busy;"));

    let (live, timers) = Live::timed("probe-gone");
    let host = serial(&live);
    let probing = start(&live, "alice", request(NODE));
    timers.armed(2).await;
    host.exit("Error: no response from BMC\n");
    assert_eq!(
        probing.await.expect("ended"),
        Err(Refusal::DeviceUnreachable)
    );
    assert_eq!(asked(&live), activated_then_deactivated());
    assert!(listed_none(&live));
    let (by, ..) = live.lines().pop().expect("a line");
    assert_eq!(by, "coordinator");
}

/// Proves: the causes that name a device's failure are its six, each read back as its close, and
/// no other cause is one: a client that left, an operator's close, the limits, the drain, a
/// revoked token and a Mac's causes.
///
/// Catches: a probe that an operator's close turns into a device refusal, or a device failure
/// answered as a reading.
#[test]
fn only_a_devices_own_causes_are_its_failure() {
    let failures: Vec<_> = Close::ALL
        .into_iter()
        .filter(|close| super::probe::device_failure(close.token()) == Some(*close))
        .collect();
    assert_eq!(
        failures,
        [
            Close::DeviceUnreachable,
            Close::BmcSolBusy,
            Close::BmcBusy,
            Close::BmcAuthFailed,
            Close::AddressMismatch,
            Close::BmcFailed,
        ]
    );
    for other in ["client_left", "device_closed", "protocol_error"] {
        assert_eq!(super::probe::device_failure(other), None, "{other}");
    }
    assert!(
        Close::ALL
            .into_iter()
            .filter(|close| !failures.contains(close))
            .all(|close| super::probe::device_failure(close.token()).is_none())
    );
}

/// Proves: an operator's `Close` of the host while the probe reads ends the read
/// (`stream_ended`, `closed_by_operator`), deactivates the payload, and the close line is the
/// closer's.
///
/// Catches: a probe that holds a host another operator closed.
#[tokio::test]
async fn an_operators_close_ends_the_probe() {
    let (live, timers) = Live::timed("probe-closed");
    let _host = serial(&live);
    let probing = start(&live, "alice", request(NODE));
    timers.armed(2).await;
    live.coordinator
        .console_close(
            &caller("bob"),
            &proto::CloseConsoleRequest {
                host_id: NODE.to_owned(),
            },
        )
        .expect("closed");
    let answer = probing.await.expect("ended").expect("probed");
    assert_eq!(
        (answer.ended(), answer.cause.as_str()),
        (End::StreamEnded, "closed_by_operator")
    );
    assert_eq!(answer.findings, [finding(Class::Silent, "")]);
    assert_eq!(asked(&live), activated_then_deactivated());
    let (by, action, ..) = live.lines().pop().expect("a line");
    assert_eq!((by.as_str(), action.as_str()), ("bob", "console_close"));
}

/// Proves: a refused probe is refused as `Open` is, with the same audit: an operator the file does
/// not name is `no_console_access` with no line; a host not enrolled, a host whose console is
/// busy, and a Mac (`no_bmc_mac`, a probe reads a serial console only) are each the refusal `Open`
/// gives, with a `console_denied` line equal to `Open`'s; a CI caller is `forbidden`; nothing is
/// opened and the device is not contacted.
///
/// Catches: a probe that is a way around the operators file, or whose refusals the audit does not
/// show as `Open`'s are shown.
#[tokio::test]
async fn a_refused_probe_is_refused_and_audited_as_open_is() {
    let (live, _timers) = Live::timed("probe-denied");
    let open = |by: &str, host: &str| {
        live.coordinator.console_open(
            &caller(by),
            &proto::OpenConsoleRequest {
                host_id: host.to_owned(),
                reason: "probe it".to_owned(),
            },
        )
    };
    for (by, host, refusal) in [
        ("carol", NODE, Refusal::NoConsoleAccess),
        ("alice", "node-99", Refusal::HostNotFound),
        ("alice", MAC, Refusal::NoBmcMac),
    ] {
        let before = live.lines().len();
        let probed = start(&live, by, request(host)).await.expect("ended");
        assert_eq!(probed, Err(refusal), "{by} {host}");
        let probe_lines = live.lines().split_off(before);
        if host == MAC {
            // A Mac is a screen, which `Open` opens; its probe is refused as `Power` is.
            assert_eq!(
                probe_lines,
                [super::support::line(
                    "alice",
                    "console_denied",
                    MAC,
                    "probe it; call=open; cause=no_bmc_mac"
                )]
            );
            continue;
        }
        assert_eq!(open(by, host).map(drop), Err(refusal), "{by} {host}");
        let open_lines = live.lines().split_off(before + probe_lines.len());
        assert_eq!(probe_lines, open_lines, "{by} {host}");
    }
    assert!(listed_none(&live));
    assert!(live.fixture.dials.dialled().is_empty());

    open("alice", NODE).expect("opened");
    let before = live.lines().len();
    let probed = start(&live, "alice", request(NODE)).await.expect("ended");
    assert_eq!(probed, Err(Refusal::ConsoleBusy));
    let probe_lines = live.lines().split_off(before);
    assert_eq!(open("alice", NODE).map(drop), Err(Refusal::ConsoleBusy));
    assert_eq!(
        probe_lines,
        live.lines().split_off(before + probe_lines.len())
    );
    assert!(live.fixture.tool.seen().is_empty());

    let ci =
        Arc::clone(&live.coordinator).console_probe(crate::operator::Caller::Ci, request(NODE));
    assert_eq!(ci.await, Err(Refusal::Forbidden));
}

/// Proves: a window out of its bounds is `invalid_request` with no line and no session (a wait of
/// 0 or above 900 seconds, more than 1 MiB), but only once the caller is let in: an operator the
/// file does not name is `no_console_access` first; the bounds themselves are taken; a quiet time
/// or a byte count of 0 is the coordinator's quiet time or 64 KiB.
///
/// Catches: a probe that holds a call open without end, or tells a stranger what a good request
/// looks like.
#[tokio::test]
async fn a_window_out_of_its_bounds_is_invalid() {
    let (live, _timers) = Live::timed("probe-window");
    for bad in [
        proto::ProbeConsoleRequest {
            wait_secs: 0,
            ..request(NODE)
        },
        proto::ProbeConsoleRequest {
            wait_secs: MAX_WAIT_SECS + 1,
            ..request(NODE)
        },
        proto::ProbeConsoleRequest {
            max_bytes: MOST_BYTES + 1,
            ..request(NODE)
        },
    ] {
        let refused = start(&live, "alice", bad.clone()).await.expect("ended");
        assert_eq!(refused, Err(Refusal::InvalidRequest), "{bad:?}");
        let stranger = start(&live, "carol", bad).await.expect("ended");
        assert_eq!(stranger, Err(Refusal::NoConsoleAccess));
    }
    assert!(live.lines().is_empty());
    assert!(listed_none(&live));

    let quiet = Duration::from_secs(10);
    let widest = proto::ProbeConsoleRequest {
        wait_secs: MAX_WAIT_SECS,
        quiet_secs: 0,
        max_bytes: MOST_BYTES,
        ..request(NODE)
    };
    assert_eq!(
        Window::of(&widest, quiet),
        Ok(Window {
            wait: Duration::from_secs(900),
            quiet,
            max_bytes: 1 << 20,
        })
    );
    let narrowest = proto::ProbeConsoleRequest {
        wait_secs: 1,
        quiet_secs: 1,
        max_bytes: 0,
        ..request(NODE)
    };
    assert_eq!(
        Window::of(&narrowest, quiet),
        Ok(Window {
            wait: Duration::from_secs(1),
            quiet: Duration::from_secs(1),
            max_bytes: 65_536,
        })
    );
}

/// Proves: each way a device is not reached is the refusal of the same name, an operator whose
/// entry went is `no_console_access`, and a cause a serial console's activation cannot end with
/// is `device_unreachable`.
///
/// Catches: a probe refused with a token that names another failure than the device's.
#[test]
fn a_device_not_reached_is_the_refusal_of_its_cause() {
    let refusals: Vec<_> = Close::ALL.into_iter().map(unreached).collect();
    assert_eq!(
        refusals,
        [
            Refusal::DeviceUnreachable,
            Refusal::BmcSolBusy,
            Refusal::BmcBusy,
            Refusal::BmcAuthFailed,
            Refusal::DeviceUnreachable,
            Refusal::DeviceUnreachable,
            Refusal::DeviceUnreachable,
            Refusal::NoConsoleAccess,
            Refusal::AddressMismatch,
            Refusal::DeviceUnreachable,
            Refusal::DeviceUnreachable,
            Refusal::DeviceUnreachable,
            Refusal::BmcFailed,
        ]
    );
}

/// Proves: on the runtime's clock (paused, so that the test moves it), the coordinator's timer of
/// 5 seconds is still pending a nanosecond before 5 seconds pass, and has passed once they have.
///
/// Catches: a timer that ignores its duration and passes at once, which would end every probe's
/// read at its start, and one that never passes, which would hold every probe to its wait.
#[tokio::test(start_paused = true)]
async fn the_runtimes_timer_passes_at_its_duration_and_not_before() {
    use futures::FutureExt as _;

    let mut timer = (super::sleeping())(QUIET);
    assert_eq!((&mut timer).now_or_never(), None, "at once");
    tokio::time::advance(QUIET - Duration::from_nanos(1)).await;
    assert_eq!((&mut timer).now_or_never(), None, "just before");
    tokio::time::advance(Duration::from_nanos(1)).await;
    assert_eq!(timer.now_or_never(), Some(()), "at its duration");
}

/// Proves: on a coordinator whose sessions must be attached at once (`--console-attach-s 0`), a
/// probe is refused `store_failed` and leaves nothing behind: its session ends with its
/// `not_attached` close line in the same call, the device is never contacted, and the list is
/// empty.
///
/// Catches: a probe that, refused, leaves a session listed that the sweep alone ends later.
#[tokio::test]
async fn a_session_that_cannot_be_attached_ends_at_once() {
    let live = Live::with("probe-no-window", 8, Duration::ZERO);
    let refused = start(&live, "alice", request(NODE)).await.expect("ended");
    assert_eq!(refused, Err(Refusal::StoreFailed));
    assert!(live.fixture.tool.seen().is_empty());
    let lines = live.lines();
    let actions: Vec<_> = lines
        .iter()
        .map(|(by, action, ..)| (by.as_str(), action.as_str()))
        .collect();
    assert_eq!(
        actions,
        [("alice", "console_open"), ("coordinator", "console_close")]
    );
    assert!(live.last_reason().contains("cause=not_attached;"));
    assert!(
        live.coordinator
            .console
            .lock_table()
            .sessions
            .get(NODE)
            .is_none()
    );
}
