//! `Console/Open`, `Console/List` and `Console/Close`: the one session per host, its audit lines
//! and its window, with no device contacted.

use std::time::Duration;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;

use super::support::{
    Fixture, MAC, NODE, NODE_BMC_ADDRESS, caller, file_address, line, session_id,
};
use crate::operator::Caller;
use crate::store::audit_failure;

fn open(host: &str, reason: &str) -> proto::OpenConsoleRequest {
    proto::OpenConsoleRequest {
        host_id: host.to_owned(),
        reason: reason.to_owned(),
    }
}

fn close(host: &str) -> proto::CloseConsoleRequest {
    proto::CloseConsoleRequest {
        host_id: host.to_owned(),
    }
}

fn list(fixture: &Fixture, who: &str) -> Vec<proto::ConsoleSession> {
    fixture
        .rig
        .coordinator
        .console_list(&caller(who), &proto::ListConsoleSessionsRequest {})
        .expect("listed")
        .sessions
}

/// Proves: `Open` tells the page how long to wait for a serial console's first byte, the console's
/// quiet time (the default 10 seconds, and `--console-quiet-s` once set) in `quiet_secs`, and 0
/// for a Mac's screen, whatever the quiet time is.
///
/// Catches: a quiet time the page is never told, or one told for a screen that has no serial
/// port.
#[test]
fn open_tells_a_serial_page_how_long_to_wait_for_a_first_byte_and_a_screen_zero() {
    let mut fixture = Fixture::new("console-open-quiet");
    let quiet = |fixture: &Fixture, host: &str| {
        let answer = fixture
            .rig
            .coordinator
            .console_open(&caller("alice"), &open(host, "look"))
            .expect("opened");
        fixture
            .rig
            .coordinator
            .console_close(&caller("alice"), &close(host))
            .expect("closed");
        answer.quiet_secs
    };
    assert_eq!(quiet(&fixture, NODE), 10);
    assert_eq!(quiet(&fixture, MAC), 0);
    fixture.rig.coordinator.console.quiet = Duration::from_secs(45);
    assert_eq!(quiet(&fixture, NODE), 45);
    assert_eq!(quiet(&fixture, MAC), 0);
}

/// Proves: opening a Linux server's console answers a serial session with a 64-digit id and the
/// attach window, writes one `console_open` line with the session and its kind after the reason,
/// shows the session in `List` (without its id), and contacts no device; a Mac's is a screen.
///
/// Catches: an open that reaches the BMC before a WebSocket attaches, a session missing from
/// `List`, and an id that `List` hands to another operator.
#[test]
fn an_open_session_is_listed_and_audited_without_contacting_the_device() {
    let fixture = Fixture::new("console-open");
    let coordinator = &fixture.rig.coordinator;
    fixture.rig.at(2_010);
    let serial = coordinator.console_open(&caller("bob"), &open(NODE, " boot order "));
    assert_eq!(
        serial,
        Ok(proto::OpenConsoleResponse {
            kind: proto::ConsoleKind::Serial.into(),
            session: session_id(1),
            expires_in_secs: 30,
            quiet_secs: 10,
            address: Some(file_address(NODE_BMC_ADDRESS)),
        })
    );
    let screen = coordinator.console_open(&caller("alice"), &open(MAC, "login stuck"));
    assert_eq!(
        screen.map(|answer| answer.kind()),
        Ok(proto::ConsoleKind::Screen)
    );
    let listed = list(&fixture, "bob");
    assert_eq!(
        listed
            .iter()
            .map(|one| (
                one.host_id.as_str(),
                one.kind(),
                one.opened_by.as_str(),
                one.attached,
                one.reason.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (
                MAC,
                proto::ConsoleKind::Screen,
                "alice",
                false,
                "login stuck"
            ),
            (NODE, proto::ConsoleKind::Serial, "bob", false, "boot order"),
        ]
    );
    assert!(listed.iter().all(|one| one.opened_unix > 0));
    assert_eq!(
        fixture.lines(),
        [
            line(
                "bob",
                "console_open",
                NODE,
                &format!("boot order; session={}; kind=serial", session_id(1))
            ),
            line(
                "alice",
                "console_open",
                MAC,
                &format!("login stuck; session={}; kind=screen", session_id(2))
            ),
        ]
    );
    assert_eq!(fixture.tool.seen(), []);
}

/// Proves: a second open of a host with a session is `console_busy`, an open past
/// `--console-max-sessions` is `too_many_sessions`, each audited as `console_denied`; after the
/// first is closed, the host opens again.
///
/// Catches: two operators handed one serial port, and a table that grows without bound.
#[test]
fn a_second_session_for_a_host_is_busy_and_names_the_holder() {
    let fixture = Fixture::with("console-open-busy", 1, Duration::from_secs(30));
    let coordinator = &fixture.rig.coordinator;
    coordinator
        .console_open(&caller("alice"), &open(NODE, "first"))
        .expect("opened");
    assert_eq!(
        coordinator.console_open(&caller("bob"), &open(NODE, "second")),
        Err(Refusal::ConsoleBusy)
    );
    assert_eq!(
        coordinator.console_open(&caller("bob"), &open(MAC, "third")),
        Err(Refusal::TooManySessions)
    );
    let holders: Vec<String> = list(&fixture, "bob")
        .into_iter()
        .map(|one| one.opened_by)
        .collect();
    assert_eq!(holders, ["alice"]);
    assert_eq!(
        coordinator.console_close(&caller("bob"), &close(NODE)),
        Ok(proto::CloseConsoleResponse {})
    );
    assert!(
        coordinator
            .console_open(&caller("bob"), &open(NODE, "again"))
            .is_ok()
    );
    let lines = fixture.lines();
    assert_eq!(
        lines[1..3],
        [
            line(
                "bob",
                "console_denied",
                NODE,
                "second; call=open; cause=console_busy"
            ),
            line(
                "bob",
                "console_denied",
                MAC,
                "third; call=open; cause=too_many_sessions"
            ),
        ]
    );
    assert_eq!(
        lines[3],
        line(
            "bob",
            "console_close",
            NODE,
            &format!(
                "session={}; kind=serial; opened_by=alice; cause=closed; ended_unix={}",
                session_id(1),
                fixture.rig.coordinator.ops().unix_now()
            )
        )
    );
}

/// Proves: a session not attached within its window ends at the first call after it, which writes
/// its `console_close` by `coordinator` with the time it ended; one a moment younger stays.
///
/// Catches: an unattached session that holds its host until the coordinator restarts.
#[test]
fn an_unattached_session_ends_at_its_window() {
    let fixture = Fixture::with("console-open-window", 8, Duration::from_secs(30));
    let coordinator = &fixture.rig.coordinator;
    coordinator
        .console_open(&caller("alice"), &open(NODE, "watch the boot"))
        .expect("opened");
    let opened_unix = list(&fixture, "alice")[0].opened_unix;
    fixture.rig.at(2_029);
    assert_eq!(list(&fixture, "alice").len(), 1);
    fixture.rig.at(2_030);
    assert_eq!(list(&fixture, "alice"), []);
    assert_eq!(
        fixture.lines()[1],
        line(
            "coordinator",
            "console_close",
            NODE,
            &format!(
                "session={}; kind=serial; opened_by=alice; cause=not_attached; ended_unix={}",
                session_id(1),
                opened_unix + 30
            )
        )
    );
    assert_eq!(
        coordinator.console_close(&caller("alice"), &close(NODE)),
        Err(Refusal::NoSession)
    );
}

/// Proves: `Close` of a host with no session is `no_session`, audited; a bad host id is
/// `invalid_request`, unaudited.
///
/// Catches: a close that pretends to have ended something.
#[test]
fn closing_a_host_without_a_session_is_no_session() {
    let fixture = Fixture::new("console-close-none");
    let coordinator = &fixture.rig.coordinator;
    assert_eq!(
        coordinator.console_close(&caller("bob"), &close(NODE)),
        Err(Refusal::NoSession)
    );
    assert_eq!(
        coordinator.console_close(&caller("bob"), &close("Bad Id")),
        Err(Refusal::InvalidRequest)
    );
    assert_eq!(
        fixture.lines(),
        [line(
            "bob",
            "console_denied",
            NODE,
            "call=close; cause=no_session"
        )]
    );
}

/// Proves: `Open` needs a reason like `Power`, refuses a host it cannot find or address as
/// `Power` does (a Mac's data MAC not in the targets file is `no_address`), and refuses callers
/// outside the operators file without a line.
///
/// Catches: a session opened with no reason, or for a host with no known device.
#[test]
fn an_open_is_checked_like_a_power_call() {
    let fixture = Fixture::new("console-open-checks");
    fixture.record("mac-02", &["a8:5e:45:00:00:09"], "", "C02X2");
    fixture.enroll("m-mac2", &[], "", "C02X2", "mac-02");
    fixture.record("mac-03", &[], "", "C02X3");
    fixture.enroll("m-mac3", &[], "", "C02X3", "mac-03");
    let coordinator = &fixture.rig.coordinator;
    let alice = caller("alice");
    for (request, refusal) in [
        (open(NODE, ""), Refusal::ReasonRequired),
        (open(NODE, &"r".repeat(257)), Refusal::ReasonTooLong),
        (open(NODE, "a\u{7}b"), Refusal::InvalidRequest),
        (open("NOT A HOST", "x"), Refusal::InvalidRequest),
    ] {
        assert_eq!(coordinator.console_open(&alice, &request), Err(refusal));
    }
    assert_eq!(fixture.lines(), []);
    for (host, refusal) in [
        ("node-99", Refusal::HostNotFound),
        ("mac-02", Refusal::NoAddress),
        ("mac-03", Refusal::NoAddress),
    ] {
        assert_eq!(
            coordinator.console_open(&alice, &open(host, "x")),
            Err(refusal),
            "{host}"
        );
    }
    for (who, refusal) in [
        (Caller::Ci, Refusal::Forbidden),
        (Caller::Anonymous, Refusal::Unauthorized),
        (caller("carol"), Refusal::NoConsoleAccess),
    ] {
        assert_eq!(
            coordinator.console_open(&who, &open(NODE, "x")),
            Err(refusal)
        );
        assert_eq!(
            coordinator.console_list(&who, &proto::ListConsoleSessionsRequest {}),
            Err(refusal)
        );
        assert_eq!(coordinator.console_close(&who, &close(NODE)), Err(refusal));
    }
    assert_eq!(fixture.lines().len(), 3);
}

/// Proves: with no audit line there is no session (`audit_unavailable`), and a random source that
/// fails is `store_failed`; neither leaves a session behind.
///
/// Catches: a session handed out unaudited, and one with an id of zeros.
#[test]
fn console_open_is_not_made_when_the_audit_line_cannot_be_appended() {
    let fixture = Fixture::new("console-open-no-audit");
    fixture.fake.refuse(|_| Some(audit_failure()));
    let coordinator = &fixture.rig.coordinator;
    assert_eq!(
        coordinator.console_open(&caller("alice"), &open(NODE, "x")),
        Err(Refusal::AuditUnavailable)
    );
    assert_eq!(list(&fixture, "alice"), []);

    let mut fixture = Fixture::new("console-open-no-random");
    let console =
        super::support::console(&fixture.files, &fixture.tool, 8, Duration::from_secs(30))
            .with_random(Box::new(|_| false));
    let coordinator = std::mem::replace(
        &mut fixture.rig.coordinator,
        crate::marks_support::start(&fixture.rig.dir, &fixture.rig.clock, ""),
    );
    fixture.rig.coordinator = coordinator.with_console(console);
    assert_eq!(
        fixture
            .rig
            .coordinator
            .console_open(&caller("alice"), &open(NODE, "x")),
        Err(Refusal::StoreFailed)
    );
    assert_eq!(list(&fixture, "alice"), []);
}
