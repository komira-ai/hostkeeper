//! The accounts' file of the file store (`<state-file>.accounts`): written whole when a commit
//! changes the accounts and only then, beside whatever else the commit writes, read at load, and
//! refused by name when it is bad. No clock, no timing.

use std::fs;
use std::path::Path;

use super::conformance_tests::{desired_at, rich_accounts};
use super::{Backend, BackendError, Commit, Files};
use crate::accounts::{Accounts, AccountsError, accounts_path};
use crate::testutil::{LEGACY, scratch};

fn files_over(dir: &Path) -> Files {
    let state = dir.join("desired.json");
    Files::default()
        .with_desired(&state, Some(LEGACY))
        .with_accounts(&accounts_path(&state))
}

/// Proves: a commit of the accounts alone writes `<state-file>.accounts` and no other file, a
/// commit of another part leaves it alone, and a load gives the accounts back with every other
/// part empty.
///
/// Catches: accounts written into the state file, which the coordinator before would refuse, or
/// a commit of something else that rewrites the credentials.
#[test]
fn a_commit_of_the_accounts_writes_their_file_alone_and_a_load_reads_it() {
    let dir = scratch("files-accounts-written");
    let files = files_over(dir.path());
    let held = rich_accounts();
    files
        .commit(&Commit {
            accounts: Some((&Accounts::default(), &held)),
            ..Commit::default()
        })
        .expect("committed");
    let names: Vec<_> = fs::read_dir(dir.path())
        .expect("a directory")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert_eq!(names, ["desired.json.accounts"]);
    let before = fs::read(dir.path().join("desired.json.accounts")).expect("the file");

    let (none, some) = (crate::desired::Held::default(), desired_at("2.0", 'a'));
    files
        .commit(&Commit {
            desired: Some((&none, &some)),
            ..Commit::default()
        })
        .expect("a desired state committed");
    assert_eq!(
        fs::read(dir.path().join("desired.json.accounts")).expect("the file"),
        before,
        "a commit of another part leaves the accounts' file as it was"
    );
    let loaded = files.load().expect("loads");
    assert_eq!(loaded.accounts, held);
    assert_eq!(loaded.desired, some);
}

/// Proves: a commit of the desired state and the accounts together writes both, and a commit of
/// the accounts that fails (the temporary file's name is a directory) is refused as an accounts
/// error and leaves the file as it was.
///
/// Catches: an accounts' commit taken as done when its file was not written.
#[test]
fn a_commit_of_the_desired_state_and_the_accounts_writes_both_and_a_failed_one_leaves_the_file() {
    let dir = scratch("files-accounts-together");
    let files = files_over(dir.path());
    let (none, some, held) = (
        crate::desired::Held::default(),
        desired_at("2.0", 'a'),
        rich_accounts(),
    );
    files
        .commit(&Commit {
            desired: Some((&none, &some)),
            accounts: Some((&Accounts::default(), &held)),
            ..Commit::default()
        })
        .expect("committed");
    assert!(dir.path().join("desired.json").exists());
    assert_eq!(files.load_accounts().expect("loads"), held);

    fs::create_dir(dir.path().join("desired.json.accounts.new")).expect("in the way");
    let refused = files.commit(&Commit {
        accounts: Some((&held, &Accounts::default())),
        ..Commit::default()
    });
    assert!(matches!(
        refused,
        Err(BackendError::Accounts(AccountsError::Io(_)))
    ));
    assert_eq!(files.load_accounts().expect("loads"), held);
}

/// Proves: with no accounts file a load holds no account, a file store given no accounts' file
/// takes a commit of accounts as done and loads none, and an accounts' file that is bad is refused
/// at load by its path.
///
/// Catches: a start that needs the new file, which the coordinator before never wrote, or a bad
/// file loaded as no users.
#[test]
fn a_load_without_the_accounts_file_holds_none_and_a_bad_one_is_refused() {
    let dir = scratch("files-accounts-absent");
    let files = files_over(dir.path());
    assert_eq!(files.load().expect("loads").accounts, Accounts::default());
    assert!(!dir.path().join("desired.json.accounts").exists());

    let held = rich_accounts();
    let unplaced = Files::default();
    unplaced
        .commit(&Commit {
            accounts: Some((&Accounts::default(), &held)),
            ..Commit::default()
        })
        .expect("taken as done");
    assert_eq!(
        unplaced.load().expect("loads").accounts,
        Accounts::default()
    );

    fs::write(dir.path().join("desired.json.accounts"), b"7").expect("a bad file");
    let refused = files.load().expect_err("refused");
    assert!(
        refused.to_string().starts_with(&format!(
            "{}: ",
            dir.path().join("desired.json.accounts").display()
        )),
        "{refused}"
    );
}
