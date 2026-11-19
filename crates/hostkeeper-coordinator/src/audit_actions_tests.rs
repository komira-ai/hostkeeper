//! The audit's `subject`, `source` and `api_token`, and the login actions: the exact lines the file holds, and
//! a reader of the release before.

use std::fs;
use std::sync::Arc;

use serde::Deserialize;

use crate::audit_actions::{self, AuditOnly, about, from_peer};
use crate::audit_file::{self, AuditLine};
use crate::ops::{AuditEntry, OpsStore, Write};
use crate::testutil::scratch;
use crate::tokens::Operator;

const AT: u64 = 1_800_000_000;

/// The audit line reader of the release before: `detail` without `subject` or `source`, and not
/// strict about keys it does not know.
#[derive(Deserialize)]
struct PreviousLine {
    seq: u64,
    by: String,
    action: String,
    detail: PreviousDetail,
}

#[derive(Deserialize)]
struct PreviousDetail {
    reason: Option<String>,
}

/// Proves: an entry with a subject and one with a source are lines with `subject` and `source`
/// after `reason` in `detail`, by exact equality, and an entry with neither has no such key.
///
/// Catches: a subject or source that is dropped, renamed or written as null, or a password or
/// session id that finds its way into a line.
#[test]
fn subject_and_source_are_the_last_keys_of_the_detail_line() {
    let dir = scratch("audit-subject");
    let file = dir.path().join("ops.json");
    let store = OpsStore::open(&file)
        .expect("opened")
        .with_unix(Arc::new(|| AT));
    let mut reset = about("alice", audit_actions::PASSWORD_RESET, "bob");
    reset.reason = Some("forgotten".to_owned());
    for entry in [
        reset,
        from_peer(
            "coordinator",
            audit_actions::LOGIN_FAILED,
            "192.0.2.7:51234",
        ),
        AuditEntry::new("alice", audit_actions::LOGOUT),
    ] {
        store
            .change(Write::BeforeMemory, entry, |_| {})
            .expect("a change");
    }
    let wanted = concat!(
        r#"{"seq":1,"at_unix":1800000000,"by":"alice","action":"password_reset","detail":"#,
        r#"{"reason":"forgotten","subject":"bob"}}"#,
        "\n",
        r#"{"seq":2,"at_unix":1800000000,"by":"coordinator","action":"login_failed","detail":"#,
        r#"{"source":"192.0.2.7:51234"}}"#,
        "\n",
        r#"{"seq":3,"at_unix":1800000000,"by":"alice","action":"logout","detail":{}}"#,
        "\n"
    );
    assert_eq!(
        fs::read_to_string(audit_file::path_of(&file)).expect("read"),
        wanted
    );
}

/// Proves: a line with `subject` and `source` reads back into an entry equal to the one it was
/// made from, and the release before reads the same line, keeping its own keys.
///
/// Catches: a subject or source lost on reading back, or a line the release before refuses.
#[test]
fn a_line_with_subject_and_source_reads_back_and_the_release_before_reads_it() {
    let mut entry = about("alice", audit_actions::USER_CREATE, "bob");
    entry.seq = 4;
    entry.source = Some("192.0.2.7:51234".to_owned());
    entry.reason = Some("new hire".to_owned());
    let line = AuditLine::of(&entry, AT);
    let text = serde_json::to_string(&line).expect("serialised");

    let back: AuditLine = serde_json::from_str(&text).expect("read back");
    assert_eq!(back.into_logged().entry, entry);

    let old: PreviousLine = serde_json::from_str(&text).expect("the release before reads it");
    assert_eq!(old.seq, 4);
    assert_eq!(old.by, "alice");
    assert_eq!(old.action, "user_create");
    assert_eq!(old.detail.reason.as_deref(), Some("new hire"));
}

/// Proves: a line the release before wrote, with no `subject`, `source` or `api_token`, reads as
/// an entry with none of them.
///
/// Catches: a new key that is required of an old file.
#[test]
fn a_line_without_subject_or_source_reads_with_neither() {
    let text = r#"{"seq":1,"at_unix":1,"by":"a","action":"exclude","detail":{"service":"kbf"}}"#;
    let line: AuditLine = serde_json::from_str(text).expect("read");
    let entry = line.into_logged().entry;
    assert_eq!(entry.subject, None);
    assert_eq!(entry.source, None);
    assert_eq!(entry.api_token, None);
    assert_eq!(entry.service.as_deref(), Some("kbf"));
}

/// Proves: an entry made by an operator calling with an API token is a line by the owner with
/// `api_token` the last key of `detail`, by exact equality; it reads back with the token, and the
/// release before reads it, keeping its own keys; the ops file's ring, which the release before
/// reads strictly, holds no `api_token`.
///
/// Catches: a token dropped from the file store's audit, the token named as the actor, and a ring
/// the release before would refuse after a rollback.
#[test]
fn a_line_made_with_an_api_token_is_by_the_owner_and_names_the_token_last() {
    let dir = scratch("audit-api-token");
    let file = dir.path().join("ops.json");
    let store = OpsStore::open(&file)
        .expect("opened")
        .with_unix(Arc::new(|| AT));
    let alice = Operator::by_token(
        "alice".parse().expect("a name"),
        "alice-bot".parse().expect("a name"),
    );
    let mut entry = AuditEntry::new(&alice, "exclude");
    entry.node_id = Some("s1".to_owned());
    entry.reason = Some("disk".to_owned());
    store
        .change(Write::BeforeMemory, entry, |_| {})
        .expect("a change");
    let text = fs::read_to_string(audit_file::path_of(&file)).expect("read");
    assert_eq!(
        text,
        concat!(
            r#"{"seq":1,"at_unix":1800000000,"by":"alice","action":"exclude","detail":"#,
            r#"{"node_id":"s1","reason":"disk","api_token":"alice-bot"}}"#,
            "\n"
        )
    );
    let back: AuditLine = serde_json::from_str(text.trim_end()).expect("read back");
    assert_eq!(
        back.into_logged().entry.api_token.as_deref(),
        Some("alice-bot")
    );
    let old: PreviousLine = serde_json::from_str(text.trim_end()).expect("the release before");
    assert_eq!((old.seq, old.by.as_str()), (1, "alice"));
    assert_eq!(old.detail.reason.as_deref(), Some("disk"));
    let ring = fs::read_to_string(&file).expect("the ops file");
    assert!(ring.contains("\"by\": \"alice\""), "{ring}");
    assert!(!ring.contains("api_token"), "{ring}");
}

/// Proves: the actions of the design's audit table are these exact strings, one for each.
///
/// Catches: a misspelt or renamed action, which the audit list's filter would then miss.
#[test]
fn the_login_actions_are_the_strings_of_the_design() {
    assert_eq!(
        [
            audit_actions::LOGIN,
            audit_actions::LOGIN_FAILED,
            audit_actions::LOGIN_LOCKED,
            audit_actions::LOGOUT,
            audit_actions::PASSWORD_CHANGED,
            audit_actions::PASSWORD_RESET,
            audit_actions::USER_BOOTSTRAP,
            audit_actions::INITIAL_PASSWORD_EXPIRED,
            audit_actions::USER_CREATE,
            audit_actions::USER_DISABLE,
            audit_actions::USER_ENABLE,
            audit_actions::USER_DELETE,
            audit_actions::API_TOKEN_CREATE,
            audit_actions::API_TOKEN_REVOKE,
        ],
        [
            "login",
            "login_failed",
            "login_locked",
            "logout",
            "password_changed",
            "password_reset",
            "user_bootstrap",
            "initial_password_expired",
            "user_create",
            "user_disable",
            "user_enable",
            "user_delete",
            "api_token_create",
            "api_token_revoke",
        ]
    );
}

/// Proves: the console's actions are the exact strings of the design (decision D4), one for
/// each, and they are the audit-only actions and no others: `AuditOnly` names each once, as its
/// string, and a change's action (`include`, `login`) is not one.
///
/// Catches: a misspelt or renamed console action, which the audit list's filter would then miss,
/// an audit-only action without its string, and a change's action that a store would leave out
/// of the ring it rebuilds at start.
#[test]
fn the_console_actions_are_the_strings_of_the_design() {
    let strings = AuditOnly::ALL.map(AuditOnly::as_str);
    assert_eq!(
        strings,
        [
            audit_actions::CONSOLE_OPEN,
            audit_actions::CONSOLE_CLOSE,
            audit_actions::CONSOLE_DENIED,
            audit_actions::POWER_ACTION,
            audit_actions::BOOT_DEVICE_SET,
            audit_actions::CONSOLE_COORDINATOR_START,
        ]
    );
    assert_eq!(
        strings,
        [
            "console_open",
            "console_close",
            "console_denied",
            "power_action",
            "boot_device_set",
            "console_coordinator_start",
        ]
    );
    assert!(
        strings
            .iter()
            .all(|action| AuditOnly::is_audit_only(action))
    );
    for change in ["include", "login", "console", "mdm_baseline", ""] {
        assert!(!AuditOnly::is_audit_only(change), "{change}");
    }
}

/// Proves: the helpers set only the field they name: `about` the subject, `from_peer` the source.
///
/// Catches: a subject written into the source field or the reverse.
#[test]
fn about_sets_the_subject_and_from_peer_the_source_alone() {
    let a = about("alice", "user_delete", "bob");
    assert_eq!(
        (a.subject.as_deref(), a.source.as_deref()),
        (Some("bob"), None)
    );
    let p = from_peer("coordinator", "login_locked", "192.0.2.7:1");
    assert_eq!(
        (p.subject.as_deref(), p.source.as_deref()),
        (None, Some("192.0.2.7:1"))
    );
}
