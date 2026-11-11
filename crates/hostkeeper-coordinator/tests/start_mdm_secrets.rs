//! `serve`'s start over the key of the MDM secrets (`--mdm-secrets-key-file`,
//! `docs/design/coordinator-mdm.md` section 4.9), with the files and with the database: the key
//! file is made when it is not there and nothing is sealed, a missing key file, or no flag, with a
//! sealed secret refuses the start with the exact text, and so does a key that opens none of them
//! or a key file that is not one.
//!
//! The database tests make a database of their own on the server `HOSTKEEPER_TEST_POSTGRES_URL`
//! names, and fail, saying how to start one, when it is not set; they never skip.

mod support;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;

use hostkeeper_coordinator::config::{Parts, StartError};
use hostkeeper_coordinator::mdm_secrets::{Secret, SecretKind};
use support::database::Database;
use support::serve::{Files, build_given, flags_of, refusal, without};

fn with_key(files: &Files, name: &str) -> Vec<OsString> {
    let mut args = files.args();
    args.extend([
        OsString::from("--mdm-secrets-key-file"),
        files.path(name).into_os_string(),
    ]);
    args
}

fn start(args: Vec<OsString>) -> Parts {
    build_given(&flags_of(args)).expect("a start")
}

fn store(parts: &Parts, kind: SecretKind, text: &str) {
    parts
        .coordinator
        .mdm_setup()
        .put_secret(kind, &Secret::new(text.as_bytes().to_vec()), None, 1)
        .expect("a secret is stored");
}

fn mode(files: &Files, name: &str) -> u32 {
    std::fs::metadata(files.path(name))
        .expect("the file")
        .permissions()
        .mode()
        & 0o777
}

const MISSING: &str = "--mdm-secrets-key-file: the file is not there and the store holds sealed MDM secrets; restore the key file the secrets were sealed under (a new key would orphan every one)";
const NOT_GIVEN: &str = "--mdm-secrets-key-file is not given and the store holds sealed MDM secrets; give the key file they were sealed under";

/// Proves: with no flag the coordinator starts, keeps no master key, makes no key file and no
/// directory of setup files, and its setup refuses an upload.
///
/// Catches: a coordinator that needs a key to start, or one that writes setup files for a farm
/// that does not use MDM.
#[test]
fn serve_without_the_flag_keeps_no_secret_and_writes_nothing() {
    let files = Files::new("start-mdm-secrets-none");
    let parts = start(files.args());
    assert!(!parts.coordinator.mdm_setup().has_key());
    assert!(!files.path("desired.json.mdm-setup").exists());
    assert!(
        !parts.coordinator.runs_abm(),
        "no ABM client without a master key"
    );
    assert!(
        parts
            .coordinator
            .mdm_setup()
            .put_secret(SecretKind::PushKey, &Secret::new(vec![1]), None, 1)
            .is_err()
    );
}

/// Proves: with the flag and no file, the start makes the key file (mode 0600), a secret is sealed
/// under it into the setup directory beside the state file, and the next start reads the same file
/// and opens the secret; the key file is not rewritten.
///
/// Catches: a key made anew at each start, which would orphan what the last one sealed, or one made
/// readable by others.
#[test]
fn serve_makes_the_key_file_once_and_opens_the_secrets_it_sealed() {
    let files = Files::new("start-mdm-secrets-made");
    let args = with_key(&files, "mdm.key");
    let parts = start(args.clone());
    assert!(parts.coordinator.mdm_setup().has_key());
    assert_eq!(mode(&files, "mdm.key"), 0o600);
    store(&parts, SecretKind::PushKey, "the push key");
    drop(parts);
    let key_text = std::fs::read_to_string(files.path("mdm.key")).expect("the key");
    assert!(
        files
            .path("desired.json.mdm-setup")
            .join("secrets.json")
            .exists()
    );

    let again = start(args);
    assert_eq!(
        std::fs::read_to_string(files.path("mdm.key")).expect("the key"),
        key_text
    );
    assert_eq!(
        again
            .coordinator
            .mdm_setup()
            .open(SecretKind::PushKey)
            .expect("opens"),
        Some(Secret::new(b"the push key".to_vec()))
    );
}

/// Proves: with a sealed secret in the setup files, a start whose key file is gone is refused with
/// the exact text and makes no new key, a start with no flag is refused with the exact text, a
/// start under another key is refused naming the kind it does not open, and a key file of mode 0644
/// or of the wrong content is refused naming the flag.
///
/// Catches: a silent new key over sealed secrets, which would orphan every one of them.
#[test]
fn serve_refuses_sealed_secrets_it_cannot_open() {
    let files = Files::new("start-mdm-secrets-refused");
    let args = with_key(&files, "mdm.key");
    let parts = start(args.clone());
    store(&parts, SecretKind::PushKey, "the push key");
    drop(parts);

    std::fs::rename(files.path("mdm.key"), files.path("kept.key")).expect("moved");
    assert_eq!(refusal(args.clone()).to_string(), MISSING);
    assert!(!files.path("mdm.key").exists(), "no new key was made");
    assert_eq!(refusal(files.args()).to_string(), NOT_GIVEN);

    files.put("other.key", &"ab".repeat(32), 0o600);
    let why = refusal(with_key(&files, "other.key"));
    assert!(
        matches!(&why, StartError::MdmSecretsUnopened(kinds) if kinds == "push_key"),
        "{why}"
    );
    assert_eq!(
        why.to_string(),
        "--mdm-secrets-key-file: the key does not open the sealed secrets of push_key: it is not the key they were sealed under"
    );

    files.put(
        "open.key",
        &std::fs::read_to_string(files.path("kept.key")).expect("kept"),
        0o644,
    );
    assert_eq!(
        refusal(with_key(&files, "open.key")).to_string(),
        "--mdm-secrets-key-file: its mode must be 0600 or 0400"
    );
    files.put("short.key", "abcd", 0o600);
    assert_eq!(
        refusal(with_key(&files, "short.key")).to_string(),
        "--mdm-secrets-key-file: the file must hold 64 hex digits, the 256-bit key"
    );

    std::fs::rename(files.path("kept.key"), files.path("mdm.key")).expect("restored");
    assert!(start(args).coordinator.mdm_setup().has_key());
}

/// Proves: a setup file that does not read refuses the start naming the file, and the key file is
/// not made before it is read.
///
/// Catches: a damaged secrets file taken as no secret, over which a new key would be made.
#[test]
fn serve_refuses_a_damaged_setup_file() {
    let files = Files::new("start-mdm-secrets-damaged");
    std::fs::create_dir_all(files.path("desired.json.mdm-setup")).expect("a directory");
    files.put("desired.json.mdm-setup/secrets.json", "{not json", 0o600);
    let why = refusal(with_key(&files, "mdm.key"));
    assert!(matches!(&why, StartError::MdmSetupStore(_)), "{why}");
    assert!(
        why.to_string().starts_with(&format!(
            "the MDM setup store failed: {}",
            files
                .path("desired.json.mdm-setup")
                .join("secrets.json")
                .display()
        )),
        "{why}"
    );
    assert!(!files.path("mdm.key").exists());
}

fn database_args(files: &Files, database: &Database, key: Option<&str>) -> Vec<OsString> {
    files.put("database.dsn", &database.dsn_text(), 0o600);
    let mut args = without(&files.args(), "--state-file");
    args.extend([
        OsString::from("--database-dsn-file"),
        files.path("database.dsn").into_os_string(),
        OsString::from("--halt-marker-file"),
        files.path("halt.marker").into_os_string(),
    ]);
    if let Some(key) = key {
        args.extend([
            OsString::from("--mdm-secrets-key-file"),
            files.path(key).into_os_string(),
        ]);
    }
    args
}

/// Proves: over the database the key file is made when nothing is sealed and a secret is a row
/// that the next start opens; with a sealed row, a missing key file and no flag are each refused
/// with the exact text, and a key that opens none of the rows is refused naming the kind.
///
/// Catches: a database coordinator that makes a new key over sealed rows.
#[test]
fn serve_over_the_database_keeps_the_same_rules() {
    let files = Files::new("start-mdm-secrets-database");
    let database = Database::new();
    let args = database_args(&files, &database, Some("mdm.key"));
    let parts = start(args.clone());
    assert_eq!(mode(&files, "mdm.key"), 0o600);
    store(&parts, SecretKind::TlsKey, "the tls key");
    drop(parts);
    database.lock_is_free();

    let opened = start(args.clone());
    assert_eq!(
        opened
            .coordinator
            .mdm_setup()
            .open(SecretKind::TlsKey)
            .expect("opens"),
        Some(Secret::new(b"the tls key".to_vec()))
    );
    drop(opened);
    database.lock_is_free();

    assert_eq!(
        refusal(database_args(&files, &database, None)).to_string(),
        NOT_GIVEN
    );
    database.lock_is_free();
    files.put("other.key", &"cd".repeat(32), 0o600);
    let why = refusal(database_args(&files, &database, Some("other.key")));
    assert!(
        matches!(&why, StartError::MdmSecretsUnopened(kinds) if kinds == "tls_key"),
        "{why}"
    );
    database.lock_is_free();
    std::fs::remove_file(files.path("mdm.key")).expect("removed");
    assert_eq!(refusal(args).to_string(), MISSING);
    assert!(!files.path("mdm.key").exists());
}

/// Proves: a database whose setup table cannot be read refuses the start, saying that the setup
/// store failed and naming the table, and makes no key file: a read that failed is not "no sealed
/// secret".
///
/// Catches: a start that takes a failed read for an empty store and makes a new key over secrets it
/// could not see.
#[test]
fn serve_refuses_a_database_whose_secrets_it_cannot_read() {
    let files = Files::new("start-mdm-secrets-unreadable");
    let database = Database::new();
    let args = database_args(&files, &database, Some("mdm.key"));
    drop(start(database_args(&files, &database, None)));
    database.lock_is_free();
    support::database::client(Some(database.name()))
        .batch_execute("SET search_path TO hostkeeper; DROP TABLE mdm_secrets")
        .expect("the table is dropped");

    let why = refusal(args);
    assert!(matches!(&why, StartError::MdmSetupStore(_)), "{why}");
    let text = why.to_string();
    assert!(
        text.starts_with("the MDM setup store failed: ") && text.contains("mdm_secrets"),
        "{text}"
    );
    assert!(!files.path("mdm.key").exists());
}

/// Proves: with files the coordinator runs the ABM client when `--mdm-secrets-key-file` is given,
/// with the tables in the directory beside the state file (`<state>.mdm`) that the start makes.
///
/// Catches: a farm that does not use MDM given an ABM client, and an MDM farm whose ABM sync is
/// never started.
#[test]
fn serve_runs_the_abm_client_over_the_files_when_there_is_a_key_file() {
    let files = Files::new("start-mdm-abm-files");
    let parts = start(with_key(&files, "mdm.key"));
    assert!(parts.coordinator.runs_abm());
    assert!(files.path("desired.json.mdm").is_dir());
}

/// Proves: over the database the coordinator runs the ABM client, with the flag or without it.
///
/// Catches: a database farm whose ABM sync is never started.
#[test]
fn serve_runs_the_abm_client_over_the_database() {
    let files = Files::new("start-mdm-abm-database");
    let database = Database::new();
    for key in [None, Some("mdm.key")] {
        let parts = start(database_args(&files, &database, key));
        assert!(parts.coordinator.runs_abm());
        drop(parts);
        database.lock_is_free();
    }
}

/// Proves: a file of the MDM tables that does not read refuses the start naming the file, before
/// the coordinator serves anything.
///
/// Catches: a damaged table file taken as an empty table, which the ABM sync would then fill from
/// scratch and the approval of Macs would trust.
#[test]
fn serve_refuses_a_damaged_mdm_table_file() {
    let files = Files::new("start-mdm-abm-damaged");
    std::fs::create_dir_all(files.path("desired.json.mdm")).expect("a directory");
    files.put("desired.json.mdm/abm_devices.json", "{not json", 0o600);
    let why = refusal(with_key(&files, "mdm.key"));
    assert!(matches!(&why, StartError::MdmSetupStore(_)), "{why}");
    assert!(
        why.to_string().contains(
            &files
                .path("desired.json.mdm")
                .join("abm_devices.json")
                .display()
                .to_string()
        ),
        "{why}"
    );
}
