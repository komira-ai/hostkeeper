//! The files of the MDM tables: what a start refuses, what a later release's file may carry, which
//! files a change writes, and what the debug text of a row keeps out of a log.

use std::fs;

use super::files::FileMdm;
use super::{Command, Enrollment};
use super::{
    CommandState, EnrollmentState, IdentityState, Issuance, MdmError, MdmStore, NewCommand,
};
use crate::testutil::scratch;

fn text(e: Result<FileMdm, MdmError>) -> String {
    match e {
        Err(MdmError::Store(why)) => why,
        other => format!("not refused: {:?}", other.map(|_| ())),
    }
}

/// Proves: a start is refused, naming the file, for a file that does not parse, a file of a schema
/// this release does not read, a family that is a directory, and a directory that cannot be made;
/// and a family with no file is empty.
///
/// Catches: a start that reads a file of another schema as its own, or takes a damaged file as an
/// empty table.
#[test]
fn a_start_is_refused_for_a_file_it_cannot_read() {
    let dir = scratch("mdm-files-refused");
    let root = dir.path();
    let path = |name: &str| root.join(name);

    fs::create_dir_all(path("broken")).expect("a directory");
    fs::write(path("broken").join("commands.json"), b"{not json").expect("a file");
    let why = text(FileMdm::open(&path("broken")));
    assert!(
        why.starts_with(&format!(
            "{}: ",
            path("broken").join("commands.json").display()
        )),
        "{why}"
    );

    fs::create_dir_all(path("later")).expect("a directory");
    fs::write(
        path("later").join("enrollments.json"),
        br#"{"schema": 2, "rows": []}"#,
    )
    .expect("a file");
    assert_eq!(
        text(FileMdm::open(&path("later"))),
        format!(
            "{}: schema 2 is not one this release reads",
            path("later").join("enrollments.json").display()
        )
    );

    fs::create_dir_all(path("dir").join("abm_state.json")).expect("a directory in a file's place");
    let why = text(FileMdm::open(&path("dir")));
    assert!(
        why.starts_with(&format!(
            "{}: ",
            path("dir").join("abm_state.json").display()
        )),
        "{why}"
    );

    fs::write(path("file"), b"x").expect("a file");
    let why = text(FileMdm::open(&path("file").join("below")));
    assert!(
        why.starts_with(&format!("{}: ", path("file").join("below").display())),
        "{why}"
    );

    let empty = FileMdm::open(&path("fresh")).expect("a directory with no files");
    assert_eq!(empty.abm_devices(), Ok(Vec::new()));
    assert_eq!(empty.abm_state(), Ok(None));
}

/// Proves: a file of a later release that carries a field this release does not know is read, the
/// field skipped, and a change then writes the family back as this release knows it.
///
/// Catches: a start that refuses the files of a release that added a field (B8: formats change
/// additively, so a rollback of one release reads the files of the next).
#[test]
fn a_field_of_a_later_release_is_skipped() {
    let dir = scratch("mdm-files-later");
    let root = dir.path().join("mdm");
    let store = FileMdm::open(&root).expect("the files open");
    store
        .replace_live(&Enrollment::new("e1", "S1", "u", 1))
        .expect("an enrollment");
    let file = root.join("enrollments.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&file).expect("the file")).expect("it parses");
    value["note"] = serde_json::json!("a later release");
    value["rows"][0]["wake_policy"] = serde_json::json!({"floor": 3600});
    fs::write(&file, serde_json::to_vec(&value).expect("it encodes"))
        .expect("the file is rewritten");

    let again = FileMdm::open(&root).expect("a later release's file opens");
    assert_eq!(again.enrollment("e1"), store.enrollment("e1"));
    again.record_contact("e1", 5).expect("a change");
    let written: serde_json::Value =
        serde_json::from_slice(&fs::read(&file).expect("the file")).expect("it parses");
    assert_eq!(written["rows"][0].get("wake_policy"), None);
}

/// Proves: a change writes the families it changed and no other: a family file removed from
/// beside the store stays removed while a change touches only commands.
///
/// Catches: a store that rewrites every family at each change (an enqueue would write the ABM
/// device list), which costs a write of the largest file for each command.
#[test]
fn a_change_writes_only_the_families_it_changed() {
    let dir = scratch("mdm-files-written");
    let root = dir.path().join("mdm");
    let store = FileMdm::open(&root).expect("the files open");
    store
        .replace_live(&Enrollment::new("e1", "S1", "u", 1))
        .expect("an enrollment");
    assert!(root.join("abm_devices.json").exists());
    fs::remove_file(root.join("abm_devices.json")).expect("the file is removed");
    store
        .enqueue(&NewCommand {
            id: "c1".to_owned(),
            enrollment_id: "e1".to_owned(),
            command_type: "ProfileList".to_owned(),
            payload: Vec::new(),
            created_unix: 2,
        })
        .expect("a command");
    assert!(!root.join("abm_devices.json").exists());
    assert!(root.join("commands.json").exists());
}

/// Proves: an enrollment without an identity checks out with its commands cancelled, and a push or
/// a contact recorded for an enrollment that is not held changes nothing.
///
/// Catches: a check-out that leaves commands queued under an ended enrollment, or one that makes a
/// row of an unknown enrollment.
#[test]
fn a_check_out_without_an_identity_cancels_commands_and_unknown_ids_change_nothing() {
    let dir = scratch("mdm-files-checkout");
    let store = FileMdm::open(&dir.path().join("mdm")).expect("the files open");
    store
        .replace_live(&Enrollment::new("e1", "S1", "u", 1))
        .expect("an enrollment");
    store
        .enqueue(&NewCommand {
            id: "c1".to_owned(),
            enrollment_id: "e1".to_owned(),
            command_type: "ProfileList".to_owned(),
            payload: Vec::new(),
            created_unix: 2,
        })
        .expect("a command");
    store.record_push("nobody", 3).expect("a no-op");
    store.record_contact("nobody", 3).expect("a no-op");
    assert_eq!(store.enrollment("nobody"), Ok(None));
    assert_eq!(store.check_out("e1", 4), Ok(true));
    assert_eq!(
        store
            .command("c1")
            .expect("it reads")
            .map(|c| (c.state, c.done_unix)),
        Some((CommandState::Cancelled, Some(4)))
    );
    assert_eq!(
        store.enrollment("e1").expect("it reads").map(|e| e.state),
        Some(EnrollmentState::CheckedOut)
    );
}

/// Proves: the debug text of an enrollment, a command and an issuance names what identifies them
/// and leaves out the push token, the magic, the payload and the certificate, by exact text.
///
/// Catches: a log line that carries the secret a push is made with, or a whole plist.
#[test]
fn debug_text_keeps_the_push_token_and_payloads_out() {
    let mut e = Enrollment::new("e1", "S1", "u", 1);
    e.push_token = Some(b"secret-token".to_vec());
    e.push_magic = Some("secret-magic".to_owned());
    assert_eq!(
        format!("{e:?}"),
        r#"Enrollment { enrollment_id: "e1", serial: "S1", state: Live, .. }"#
    );
    let issued = Issuance {
        cert_sha256: "abc".to_owned(),
        cert_der: b"der-bytes".to_vec(),
        not_after_unix: 7,
    };
    assert_eq!(
        format!("{issued:?}"),
        r#"Issuance { cert_sha256: "abc", not_after_unix: 7, .. }"#
    );
    let c = Command {
        id: "c1".to_owned(),
        seq: 1,
        enrollment_id: "e1".to_owned(),
        command_type: "InstallProfile".to_owned(),
        payload: b"<plist>secret</plist>".to_vec(),
        state: CommandState::Queued,
        deferred: false,
        deliveries: 0,
        created_unix: 1,
        sent_unix: None,
        done_unix: None,
        response: None,
    };
    assert_eq!(
        format!("{c:?}"),
        r#"Command { id: "c1", enrollment_id: "e1", command_type: "InstallProfile", state: Queued, .. }"#
    );
}

/// Proves: each state spells as the tables and the files hold it and is read back from that
/// spelling, and a spelling that is none of them is read as none.
///
/// Catches: a state whose spelling in the database and in the files differ, or that reads a
/// misspelling as another state.
#[test]
fn every_state_spells_as_the_tables_hold_it() {
    for (state, text) in [
        (EnrollmentState::Live, "live"),
        (EnrollmentState::Replaced, "replaced"),
        (EnrollmentState::CheckedOut, "checked_out"),
    ] {
        assert_eq!(
            (state.as_str(), EnrollmentState::parse(text)),
            (text, Some(state))
        );
        assert_eq!(
            serde_json::to_string(&state).expect("it encodes"),
            format!("\"{text}\"")
        );
    }
    for (state, text) in [
        (CommandState::Queued, "queued"),
        (CommandState::Sent, "sent"),
        (CommandState::Acked, "acked"),
        (CommandState::Errored, "errored"),
        (CommandState::Cancelled, "cancelled"),
    ] {
        assert_eq!(
            (state.as_str(), CommandState::parse(text)),
            (text, Some(state))
        );
        assert_eq!(
            serde_json::to_string(&state).expect("it encodes"),
            format!("\"{text}\"")
        );
    }
    for (state, text) in [
        (IdentityState::Challenged, "challenged"),
        (IdentityState::Issued, "issued"),
        (IdentityState::Retired, "retired"),
        (IdentityState::Dead, "dead"),
    ] {
        assert_eq!(
            (state.as_str(), IdentityState::parse(text)),
            (text, Some(state))
        );
        assert_eq!(
            serde_json::to_string(&state).expect("it encodes"),
            format!("\"{text}\"")
        );
    }
    assert_eq!(EnrollmentState::parse("Live"), None);
    assert_eq!(CommandState::parse(""), None);
    assert_eq!(IdentityState::parse("live"), None);
}
