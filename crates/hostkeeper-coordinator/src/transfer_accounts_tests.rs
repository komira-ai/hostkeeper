//! `import` and `export` of the operator's users and API tokens: the accounts file of a file store
//! and the tables `users` and `api_tokens` of a database (docs/design/operator-login.md section 9).

use super::export;
use super::tests::{Store, clock, count, export_flags, imported, loaded};
use crate::accounts::{AccountStore, Accounts, accounts_path, parse_file};
use crate::backup_accounts_tests::put_accounts;
use crate::lock::FileLock;
use crate::store::conformance_tests::rich_accounts;
use crate::store::postgres::tests::TestDatabase;

/// Proves: importing a file store that holds an accounts file writes its users as rows of `users`
/// and its tokens as rows of `api_tokens` (the report counts each), and a start on the database
/// loads the accounts a start on the files loads.
///
/// Catches: an import that leaves the accounts file behind, so the database would hold nobody who
/// can sign in.
#[test]
fn import_writes_the_accounts_file_as_rows() {
    let store = Store::new("transfer-accounts-import");
    put_accounts(&store.paths.state_file);
    let database = TestDatabase::new();
    let report = imported(&store, &database).expect("imported");
    assert!(report.contains("  users: 2 rows"), "{report}");
    assert!(report.contains("  api_tokens: 2 rows"), "{report}");
    assert_eq!(count(&database, "users"), 2);
    assert_eq!(count(&database, "api_tokens"), 2);
    assert_eq!(loaded(&database).accounts, rich_accounts());
    let files =
        AccountStore::open(&accounts_path(&store.paths.state_file)).expect("the file reads");
    assert_eq!(files.snapshot(), loaded(&database).accounts);
}

/// Proves: exporting a database that holds accounts writes the accounts file beside the state file,
/// which reads as the accounts the database held and is the file the import came from, and the
/// report counts the rows; a database that holds none is exported with no accounts file.
///
/// Catches: an export that drops the users, or writes an empty file for a state without any.
#[test]
fn export_writes_the_accounts_file_only_when_the_database_holds_accounts() {
    let from = Store::new("transfer-accounts-export-from");
    put_accounts(&from.paths.state_file);
    let database = TestDatabase::new();
    imported(&from, &database).expect("imported");
    let into = Store::new("transfer-accounts-export-into");
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();
    assert!(report.contains("  users: 2 rows"), "{report}");
    assert!(report.contains("  api_tokens: 2 rows"), "{report}");
    let written = std::fs::read(accounts_path(&into.paths.state_file)).expect("the file");
    assert_eq!(
        parse_file(&accounts_path(&into.paths.state_file), &written).expect("reads"),
        loaded(&database).accounts
    );
    assert_eq!(
        std::fs::read(accounts_path(&from.paths.state_file)).expect("the original"),
        written
    );

    let plain = Store::recorded("transfer-accounts-none-from");
    let empty = TestDatabase::new();
    imported(&plain, &empty).expect("imported");
    assert_eq!(loaded(&empty).accounts, Accounts::default());
    let out = Store::new("transfer-accounts-none-into");
    export(&export_flags(&empty, &out), &FileLock, &clock()).expect("exported");
    assert!(!accounts_path(&out.paths.state_file).exists());
}

/// Proves: an accounts file that is bad is refused by an import naming the file.
///
/// Catches: an import that carries a file it cannot read into the database as no users.
#[test]
fn import_refuses_an_accounts_file_it_cannot_read() {
    let store = Store::new("transfer-accounts-bad");
    put_accounts(&store.paths.state_file);
    std::fs::write(accounts_path(&store.paths.state_file), b"{ not json").expect("damaged");
    let database = TestDatabase::new();
    let refused = imported(&store, &database).expect_err("refused");
    assert!(
        refused
            .to_string()
            .contains(&accounts_path(&store.paths.state_file).display().to_string()),
        "{refused}"
    );
}
