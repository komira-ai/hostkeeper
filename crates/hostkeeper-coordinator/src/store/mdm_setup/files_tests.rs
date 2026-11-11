//! The files of the MDM setup: what a start refuses, what a later release's file may carry, which
//! files a change writes, and what the files and the debug text of a row keep out.

use std::fs;

use super::files::{FileSetup, encode_file};
use super::{Sealed, SecretRow, SetupError, SetupStore};
use crate::testutil::scratch;

fn text(e: Result<FileSetup, SetupError>) -> String {
    match e {
        Err(SetupError::Store(why)) => why,
        other => format!("not refused: {:?}", other.map(|_| ())),
    }
}

fn row(kind: &str, version: u32, at: i64) -> SecretRow {
    SecretRow {
        kind: kind.to_owned(),
        version,
        sealed: Sealed(vec![
            0xde, 0xad, 0xbe, 0xef, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
            19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
        ]),
        fingerprint: "ab".repeat(32),
        not_after_unix: None,
        created_unix: at,
        superseded_unix: None,
    }
}

/// Proves: a start is refused, naming the file, for a file that does not parse, a file of a schema
/// this release does not read, a sealed field that is not hex, a family that is a directory and a
/// directory that is a file; a directory that is not there holds nothing and is not made by the
/// start.
///
/// Catches: a start that reads a file of another schema as its own, or takes a damaged file as an
/// empty table, which would let the next upload overwrite sealed secrets.
#[test]
fn a_start_is_refused_for_a_file_it_cannot_read() {
    let dir = scratch("mdm-setup-refused");
    let root = dir.path();
    let path = |name: &str| root.join(name);
    let begin = |name: &str, file: &str| format!("{}: ", path(name).join(file).display());

    for (name, file, body) in [
        ("broken", "secrets.json", &b"{not json"[..]),
        ("broken-settings", "settings.json", b"{not json"),
        ("nothex", "secrets.json", br#"{"schema":1,"rows":[{"kind":"k","version":1,"sealed":"zz","fingerprint":"f","not_after_unix":null,"created_unix":1,"superseded_unix":null}]}"#),
    ] {
        fs::create_dir_all(path(name)).expect("a directory");
        fs::write(path(name).join(file), body).expect("a file");
        let why = text(FileSetup::open(&path(name)));
        assert!(why.starts_with(&begin(name, file)), "{name}: {why}");
    }

    for (file, body) in [
        ("secrets.json", r#"{"schema": 2, "rows": []}"#),
        ("settings.json", r#"{"schema": 2, "settings": {}}"#),
    ] {
        let name = format!("later-{file}");
        fs::create_dir_all(path(&name)).expect("a directory");
        fs::write(path(&name).join(file), body).expect("a file");
        assert_eq!(
            text(FileSetup::open(&path(&name))),
            format!(
                "{}: schema 2 is not one this release reads",
                path(&name).join(file).display()
            )
        );
    }

    for file in ["secrets.json", "settings.json"] {
        let name = format!("dir-{file}");
        fs::create_dir_all(path(&name).join(file)).expect("a directory in a file's place");
        let why = text(FileSetup::open(&path(&name)));
        assert!(why.starts_with(&begin(&name, file)), "{why}");
    }

    let fresh = FileSetup::open(&path("fresh")).expect("a directory that is not there");
    assert_eq!(fresh.secrets(), Ok(Vec::new()));
    assert_eq!(fresh.settings(), Ok(Default::default()));
    assert!(!path("fresh").exists(), "a start makes no directory");
}

/// Proves: a change makes the directory and writes the file of the family it changed, no other,
/// holds the sealed bytes as hex and nothing else of the value, and a change that changes nothing
/// writes nothing.
///
/// Catches: a start that makes a directory for every coordinator whether or not it uses MDM, a
/// store that rewrites both files at each change, and one that writes plaintext.
#[test]
fn a_change_writes_the_family_it_changed() {
    let dir = scratch("mdm-setup-written");
    let root = dir.path().join("setup");
    let store = FileSetup::open(&root).expect("a store");
    store.set_setting("tls_renew_days", "9").expect("a setting");
    assert!(root.join("settings.json").exists());
    assert!(!root.join("secrets.json").exists(), "no secret was written");
    store.put_secret(&row("tls_key", 1, 5)).expect("a secret");
    let written = fs::read_to_string(root.join("secrets.json")).expect("the file");
    assert!(
        written.contains(&hex::encode(&row("tls_key", 1, 5).sealed.0)),
        "{written}"
    );

    fs::remove_file(root.join("settings.json")).expect("removed");
    store.put_secret(&row("tls_key", 2, 6)).expect("a secret");
    assert!(
        !root.join("settings.json").exists(),
        "the settings file was not rewritten"
    );
    let before = fs::read(root.join("secrets.json")).expect("the file");
    assert_eq!(store.prune_superseded(0), Ok(0));
    assert_eq!(
        fs::read(root.join("secrets.json")).expect("the file"),
        before
    );

    let blocked = dir.path().join("blocked");
    let below = FileSetup::open(&blocked.join("setup")).expect("nothing to read");
    fs::write(&blocked, b"a file").expect("a file where the directory is to be");
    let why = below
        .set_setting("x", "y")
        .expect_err("the directory cannot be made");
    assert!(
        matches!(&why, SetupError::Store(text) if text.starts_with(&format!("{}: ", blocked.join("setup").display()))),
        "{why}"
    );
    assert_eq!(
        below.settings(),
        Ok(Default::default()),
        "a failed change is not made"
    );
}

/// Proves: a file of a later release that carries a field this release does not know is read, and a
/// change by this release writes the field back with its row, while the rows this release made
/// carry none.
///
/// Catches: a rollback that opens the files of the next release and, at its first change, drops
/// what it did not know (B8).
#[test]
fn a_field_of_a_later_release_is_written_back_with_its_row() {
    let dir = scratch("mdm-setup-later");
    let root = dir.path().join("setup");
    fs::create_dir_all(&root).expect("a directory");
    let later = format!(
        r#"{{"schema":1,"rows":[{{"kind":"tls_key","version":1,"sealed":"{}","fingerprint":"{}","not_after_unix":null,"created_unix":1,"superseded_unix":null,"rotated_by":"a later release"}}]}}"#,
        hex::encode(&row("tls_key", 1, 1).sealed.0),
        "ab".repeat(32)
    );
    fs::write(root.join("secrets.json"), later).expect("a file");
    let store = FileSetup::open(&root).expect("a later release's file opens");
    store.put_secret(&row("tls_key", 2, 2)).expect("a change");
    let file: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("secrets.json")).expect("the file"))
            .expect("it parses");
    assert_eq!(file["rows"][0]["rotated_by"], "a later release");
    assert_eq!(file["rows"][1].get("rotated_by"), None);
    assert_eq!(file["rows"][0]["superseded_unix"], 2);
}

/// Proves: a store in memory alone reads empty, keeps what it is given and writes no file.
///
/// Catches: a volatile store that reaches the disk.
#[test]
fn a_volatile_store_keeps_its_rows_in_memory() {
    let store = FileSetup::volatile();
    store.put_secret(&row("tls_key", 1, 1)).expect("a secret");
    store.set_setting("tls_renew_days", "3").expect("a setting");
    assert_eq!(store.secrets().expect("rows").len(), 1);
    assert_eq!(store.settings().expect("rows").len(), 1);
}

/// Proves: when the directory sync of a file fails after the file is in place, the change is kept,
/// the call answers its result, and a restart finds the change.
///
/// Catches: a store whose memory and file part after a failed directory sync.
#[test]
fn a_directory_sync_that_fails_after_the_file_is_in_place_keeps_the_change() {
    fn refuse(_: &std::path::Path) -> std::io::Result<()> {
        Err(std::io::Error::other("no sync"))
    }
    let log = crate::testutil::Log::default();
    let _guard = log.install();
    let dir = scratch("mdm-setup-unsynced");
    let root = dir.path().join("setup");
    let store = FileSetup::open_syncing(&root, refuse).expect("a store");
    store
        .put_secret(&row("tls_key", 1, 1))
        .expect("an unsynced change is kept");
    assert_eq!(store.secrets().expect("rows").len(), 1);
    assert!(
        log.text().contains(
            "an MDM setup file is in place but its directory was not synced; the change is kept"
        ),
        "{}",
        log.text()
    );
    let restarted = FileSetup::open(&root).expect("a restart");
    assert_eq!(restarted.secrets().expect("rows").len(), 1);
}

/// Proves: a file that cannot be encoded is an error naming why, and not a file; the debug text
/// of a row shows the length of its sealed bytes and not a byte of them.
///
/// Catches: an encoder that writes a half-made file, and a derived `Debug` that prints a sealed
/// value into a log line.
#[test]
fn an_unencodable_file_is_an_error_and_a_row_prints_no_bytes() {
    struct Unwritable;
    impl serde::Serialize for Unwritable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("cannot be written"))
        }
    }
    assert_eq!(
        encode_file(&Unwritable),
        Err(SetupError::Store("cannot be written".to_owned()))
    );
    assert_eq!(
        format!("{:?}", row("tls_key", 1, 1).sealed),
        "Sealed(32 bytes)"
    );
    assert_eq!(
        format!("{:?}", row("tls_key", 1, 7)),
        "SecretRow { kind: \"tls_key\", version: 1, sealed: Sealed(32 bytes), fingerprint: \"abababababababababababababababababababababababababababababababab\", not_after_unix: None, created_unix: 7, superseded_unix: None }"
    );
}
