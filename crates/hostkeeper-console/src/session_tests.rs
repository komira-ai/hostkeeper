//! The console sessions' table, on a time the test passes.

use std::time::Duration;

use super::{
    AttachRefused, Attachment, BadSessionId, Bounds, Kind, Limit, Refused, SESSION_ID_BYTES,
    Session, SessionId, Sessions,
};

const WINDOW: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(1_800);
const TOTAL: Duration = Duration::from_secs(28_800);

/// A table of at most `max` sessions, attached within `window`, with the default limits.
fn table(max: usize, window: Duration) -> Sessions {
    Sessions::new(Bounds {
        max,
        window,
        idle: IDLE,
        total: TOTAL,
    })
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn session(host_id: &str, at: u64) -> Session {
    Session {
        id: SessionId::from_bytes([at as u8; SESSION_ID_BYTES]),
        host_id: host_id.to_owned(),
        kind: Kind::Serial,
        opened_by: "alice".to_owned(),
        opened_unix: 1_000 + at,
        opened_at: Duration::from_secs(at),
        reason: "boot order".to_owned(),
        attachment: None,
    }
}

fn hosts(sessions: &Sessions) -> Vec<&str> {
    sessions.iter().map(|one| one.host_id.as_str()).collect()
}

/// Proves: a second session of a host is refused busy while the first stands, and a session of
/// another host is refused when the table holds its most; once the first is closed, both admit
/// again.
///
/// Catches: two operators given one host's serial port (a BMC has one payload, D11), and a table
/// that grows past `--console-max-sessions`.
#[test]
fn a_second_session_for_a_host_is_busy_and_names_the_holder() {
    let mut sessions = table(1, WINDOW);
    assert_eq!(sessions.admit("node-02"), Ok(()));
    sessions.insert(session("node-02", 0));
    assert_eq!(sessions.admit("node-02"), Err(Refused::Busy));
    assert_eq!(sessions.admit("node-03"), Err(Refused::Full));
    let holder: Vec<&str> = sessions.iter().map(|s| s.opened_by.as_str()).collect();
    assert_eq!(holder, ["alice"]);
    assert_eq!(sessions.close("node-02"), Some(session("node-02", 0)));
    assert_eq!(sessions.close("node-02"), None);
    assert_eq!(sessions.admit("node-02"), Ok(()));
    assert_eq!(sessions.admit("node-03"), Ok(()));
}

/// Proves: a session not attached ends exactly at the end of its window, not a moment before; an
/// attached one does not; the ended ones are answered and the rest kept, by host id.
///
/// Catches: an unattached session that holds its host for ever, one ended early, and an attached
/// session cut by the attach window.
#[test]
fn an_unattached_session_ends_at_its_window() {
    let mut sessions = table(8, WINDOW);
    assert_eq!(sessions.window(), WINDOW);
    sessions.insert(session("node-03", 10));
    sessions.insert(session("node-01", 0));
    let mut attached = session("node-02", 0);
    attached.attachment = Some(Attachment {
        at: secs(1),
        active_at: secs(1),
        moved: 0,
    });
    sessions.insert(attached);
    let almost = WINDOW - Duration::from_nanos(1);
    assert_eq!(sessions.expire(almost), Vec::new());
    assert_eq!(hosts(&sessions), ["node-01", "node-02", "node-03"]);
    assert_eq!(sessions.expire(WINDOW), vec![session("node-01", 0)]);
    assert_eq!(hosts(&sessions), ["node-02", "node-03"]);
    let late = Duration::from_secs(3_600);
    assert_eq!(sessions.expire(late), vec![session("node-03", 10)]);
    assert_eq!(hosts(&sessions), ["node-02"]);
    let mut far = table(8, Duration::MAX);
    far.insert(session("node-04", 5));
    assert_eq!(far.expire(late), Vec::new());
}

/// Proves: a session's id is written as 64 lower-case hex digits, and printed the same; each kind
/// has its audit name.
///
/// Catches: an id shortened or in upper case, which the WebSocket's path would not match.
#[test]
fn a_session_id_is_64_lower_case_hex_digits() {
    let mut bytes = [0u8; SESSION_ID_BYTES];
    bytes[0] = 0xab;
    bytes[31] = 0x0f;
    let id = SessionId::from_bytes(bytes);
    let text = id.to_string();
    assert_eq!(text.len(), 64);
    assert_eq!(text, format!("ab{}0f", "0".repeat(60)));
    assert_eq!(format!("{id:?}"), format!("SessionId({text})"));
    assert_eq!(
        [Kind::Serial, Kind::Screen].map(Kind::as_str),
        ["serial", "screen"]
    );
}

/// Proves: the operator who opened a session attaches to it once, inside its window: a second
/// attach is refused `Attached`, another operator's `NotOpener` before and after the attach (it
/// learns nothing of the session's state), an id no session has and a
/// window that has passed (even before the expiry ends it) `NoSession`; an attached session is
/// kept past its window and counts against the most sessions.
///
/// Catches: a session id replayed for a second stream, another operator's token on a session, an
/// attach after the window, and an attached session that frees its slot.
#[test]
fn a_websocket_attaches_once_by_its_opener_within_the_window() {
    let mut sessions = table(1, WINDOW);
    let first = session("node-02", 0);
    let id = first.id;
    sessions.insert(first);
    let unknown = SessionId::from_bytes([0xee; SESSION_ID_BYTES]);
    assert_eq!(
        sessions.attach(&unknown, "alice", secs(1)).err(),
        Some(AttachRefused::NoSession)
    );
    assert_eq!(
        sessions.attach(&id, "bob", secs(1)).err(),
        Some(AttachRefused::NotOpener)
    );
    assert_eq!(
        sessions.attach(&id, "alice", WINDOW).err(),
        Some(AttachRefused::NoSession)
    );
    let attached = sessions.attach(&id, "alice", secs(29)).expect("attached");
    assert_eq!(
        attached.attachment,
        Some(Attachment {
            at: secs(29),
            active_at: secs(29),
            moved: 0,
        })
    );
    assert_eq!(
        sessions.attach(&id, "alice", secs(29)).err(),
        Some(AttachRefused::Attached)
    );
    assert_eq!(
        sessions.attach(&id, "bob", secs(29)).err(),
        Some(AttachRefused::NotOpener)
    );
    assert_eq!(sessions.expire(secs(3_600)), Vec::new());
    assert_eq!(sessions.admit("node-03"), Err(Refused::Full));
}

/// Proves: an attached session is due `Idle` once no byte crossed for the idle limit since the
/// last time the caller saw one cross, and not a moment before; bytes that cross push the idle
/// limit out, the same count told again does not; it is due `Total` at the total limit after its
/// attach whatever crossed, the total limit first when both are reached; an unattached session is
/// never due, and telling the table of a host with no session changes nothing.
///
/// Catches: a stream that idles out while in use, one that never idles out, and a busy stream
/// held past `--console-total-s`.
#[test]
fn idle_and_total_timers_close_a_session() {
    let mut sessions = Sessions::new(Bounds {
        max: 8,
        window: WINDOW,
        idle: secs(100),
        total: secs(1_000),
    });
    let first = session("node-02", 0);
    let id = first.id;
    sessions.insert(first);
    sessions.insert(session("node-03", 0));
    sessions.attach(&id, "alice", secs(10)).expect("attached");
    assert_eq!(sessions.due(secs(109)), Vec::new());
    assert_eq!(
        sessions.due(secs(110)),
        vec![("node-02".to_owned(), Limit::Idle)]
    );
    sessions.observe("node-02", 5, secs(60));
    assert_eq!(sessions.due(secs(159)), Vec::new());
    sessions.observe("node-02", 5, secs(150));
    assert_eq!(
        sessions.due(secs(160)),
        vec![("node-02".to_owned(), Limit::Idle)]
    );
    sessions.observe("node-03", 9, secs(150));
    sessions.observe("node-09", 9, secs(150));
    for at in (200..1_000).step_by(50) {
        sessions.observe("node-02", at, secs(at));
        assert_eq!(sessions.due(secs(at)), Vec::new(), "at {at}");
    }
    assert_eq!(
        sessions.due(secs(1_010)),
        vec![("node-02".to_owned(), Limit::Total)]
    );
    assert_eq!(
        sessions.due(secs(5_000)),
        vec![("node-02".to_owned(), Limit::Total)]
    );
    assert_eq!(
        sessions.get("node-03").and_then(|s| s.attachment.clone()),
        None
    );
}

/// Proves: removing a session by its id removes that session only: once a host's session was
/// closed and another opened, the first's id removes nothing and the second stays.
///
/// Catches: the end of an old stream removing the session an operator opened after it.
#[test]
fn a_stream_removes_its_own_session_and_no_later_one() {
    let mut sessions = table(8, WINDOW);
    let old = session("node-02", 1);
    let old_id = old.id;
    sessions.insert(old.clone());
    assert_eq!(sessions.close("node-02"), Some(old));
    let new = session("node-02", 2);
    sessions.insert(new.clone());
    assert_eq!(sessions.remove(&old_id), None);
    assert_eq!(sessions.get("node-02"), Some(&new));
    assert_eq!(sessions.remove(&new.id), Some(new));
    assert_eq!(sessions.get("node-02"), None);
    assert_eq!(sessions.bounds().max, 8);
}

/// Proves: a session id parses from exactly 64 lower-case hex digits, back to the id that printed
/// them; one digit fewer or more, an upper-case digit, a letter past `f` and a sign are refused.
///
/// Catches: a path that names a session in another spelling, which would make one session
/// reachable by two paths.
#[test]
fn a_session_id_parses_only_64_lower_case_hex_digits() {
    let mut bytes = [0u8; SESSION_ID_BYTES];
    bytes[0] = 0x9a;
    bytes[31] = 0xf0;
    let id = SessionId::from_bytes(bytes);
    assert_eq!(SessionId::parse(&id.to_string()), Ok(id));
    let good = id.to_string();
    for bad in [
        good[1..].to_owned(),
        format!("{good}0"),
        good.to_uppercase(),
        format!("g{}", &good[1..]),
        format!("+{}", &good[1..]),
        String::new(),
    ] {
        assert_eq!(SessionId::parse(&bad), Err(BadSessionId), "{bad}");
    }
    assert_eq!(
        BadSessionId.to_string(),
        "a session id is 64 lower-case hex digits"
    );
}
