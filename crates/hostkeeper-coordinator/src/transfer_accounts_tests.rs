//! `export` of the operator's users and API tokens: the tables `users` and `api_tokens` of a
//! database become the accounts file of a file store (docs/design/operator-login.md section 9).

use super::export;
use super::tests::{Store, clock, count, export_flags, loaded, seeded};
use crate::accounts::{AccountStore, Accounts, accounts_path, parse_file};
use crate::backup_accounts_tests::put_accounts;
use crate::lock::FileLock;
use crate::store::conformance_tests::rich_accounts;
use crate::store::postgres::tests::TestDatabase;

/// Proves: a database holding users and API tokens (filled from the accounts file of a recorded
/// file store, the two rows each of `users` and `api_tokens`) is exported with the accounts file
/// beside the state file, which reads as the accounts the database held and is byte for byte the
/// file the database was filled from, and the report counts each table's rows.
///
/// Catches: an export that drops the users and tokens, so a database moved back to files would
/// leave nobody who can sign in.
#[test]
fn export_writes_the_users_and_tokens_as_the_accounts_file() {
    let from = Store::new("transfer-accounts-export-from");
    put_accounts(&from.paths.state_file);
    let database = TestDatabase::new();
    seeded(&from, &database);
    assert_eq!(count(&database, "users"), 2);
    assert_eq!(count(&database, "api_tokens"), 2);
    assert_eq!(loaded(&database).accounts, rich_accounts());

    let into = Store::new("transfer-accounts-export-into");
    let report = export(&export_flags(&database, &into), &FileLock, &clock())
        .expect("exported")
        .to_string();

    assert!(report.contains("  users: 2 rows"), "{report}");
    assert!(report.contains("  api_tokens: 2 rows"), "{report}");
    let written = std::fs::read(accounts_path(&into.paths.state_file)).expect("the file");
    assert_eq!(
        parse_file(&accounts_path(&into.paths.state_file), &written).expect("reads"),
        rich_accounts()
    );
    assert_eq!(
        std::fs::read(accounts_path(&from.paths.state_file)).expect("the original"),
        written
    );
    let opened =
        AccountStore::open(&accounts_path(&into.paths.state_file)).expect("a store opens it");
    assert_eq!(opened.snapshot(), rich_accounts());
}

/// Proves: a database that holds no user and no token is exported with no accounts file, and the
/// report counts both tables as 0 rows.
///
/// Catches: an export that writes an empty accounts file for a state without any, which a start
/// on files would then read as a deliberate wipe.
#[test]
fn export_writes_no_accounts_file_for_a_database_without_accounts() {
    let plain = Store::recorded("transfer-accounts-none-from");
    let empty = TestDatabase::new();
    seeded(&plain, &empty);
    assert_eq!(loaded(&empty).accounts, Accounts::default());

    let out = Store::new("transfer-accounts-none-into");
    let report = export(&export_flags(&empty, &out), &FileLock, &clock())
        .expect("exported")
        .to_string();

    assert!(report.contains("  users: 0 rows"), "{report}");
    assert!(report.contains("  api_tokens: 0 rows"), "{report}");
    assert!(!accounts_path(&out.paths.state_file).exists());
}
