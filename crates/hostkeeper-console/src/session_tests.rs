//! The console sessions' table, on a time the test passes.

use std::time::Duration;

use super::{Kind, Refused, SESSION_ID_BYTES, Session, SessionId, Sessions};

const WINDOW: Duration = Duration::from_secs(30);

fn session(host_id: &str, at: u64) -> Session {
    Session {
        id: SessionId::from_bytes([at as u8; SESSION_ID_BYTES]),
        host_id: host_id.to_owned(),
        kind: Kind::Serial,
        opened_by: "alice".to_owned(),
        opened_unix: 1_000 + at,
        opened_at: Duration::from_secs(at),
        reason: "boot order".to_owned(),
        attached: false,
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
    let mut sessions = Sessions::new(1, WINDOW);
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
    let mut sessions = Sessions::new(8, WINDOW);
    assert_eq!(sessions.window(), WINDOW);
    sessions.insert(session("node-03", 10));
    sessions.insert(session("node-01", 0));
    let mut attached = session("node-02", 0);
    attached.attached = true;
    sessions.insert(attached);
    let almost = WINDOW - Duration::from_nanos(1);
    assert_eq!(sessions.expire(almost), Vec::new());
    assert_eq!(hosts(&sessions), ["node-01", "node-02", "node-03"]);
    assert_eq!(sessions.expire(WINDOW), vec![session("node-01", 0)]);
    assert_eq!(hosts(&sessions), ["node-02", "node-03"]);
    let late = Duration::from_secs(3_600);
    assert_eq!(sessions.expire(late), vec![session("node-03", 10)]);
    assert_eq!(hosts(&sessions), ["node-02"]);
    let mut far = Sessions::new(8, Duration::MAX);
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
