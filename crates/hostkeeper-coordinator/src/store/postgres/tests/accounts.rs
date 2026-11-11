//! The operator's users and API tokens over the Postgres store (`docs/design/operator-login.md`,
//! section 9; migration `0010`): the tables `users` and `api_tokens`, what a commit writes to
//! them, how a load reads them, what a commit of unknown outcome gives back to memory, and the
//! release before this one on a database this release migrated.

use std::sync::Arc;

use super::{SPENT_NOW, TestDatabase, off_runtime};
use crate::accounts::{AccountStore, Accounts, AccountsError};
use crate::store::conformance_tests::rich_accounts;
use crate::store::postgres::session::Session;
use crate::store::postgres::{Landing, Owned, TIMEOUTS, load_all, migrate};
use crate::store::{Backend, Commit, Shared};

fn commit(store: &impl Backend, before: &Accounts, after: &Accounts) {
    store
        .commit(&Commit {
            accounts: Some((before, after)),
            ..Commit::default()
        })
        .expect("committed");
}

fn names(db: &TestDatabase, table: &str) -> Vec<String> {
    db.rows(&format!("SELECT name FROM {table} ORDER BY name"))
        .into_iter()
        .map(|row| row[0].clone().unwrap_or_default())
        .collect()
}

fn with_session<R: Send + 'static>(
    db: &TestDatabase,
    f: impl FnOnce(&mut Session) -> R + Send + 'static,
) -> R {
    let config = db.dsn().config(TIMEOUTS);
    off_runtime(move || {
        let mut session = Session::connect(
            &config,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            false,
        )
        .expect("a session");
        f(&mut session)
    })
}

/// Proves: the accounts committed are one row of `users` per user and one of `api_tokens` per
/// token, keyed by name, whose `record` is the JSON the accounts file holds for it without the
/// name; a database that holds none has neither; and a start on the database gives the accounts
/// back whole.
///
/// Catches: a password hash or a digest written anywhere but its own row, a name kept inside the
/// record, or a user that a restart forgets.
#[test]
fn the_accounts_are_a_row_per_user_and_per_token_and_a_restart_reads_them_back() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    assert_eq!(loaded.accounts, Accounts::default());
    assert_eq!(names(&db, "users"), Vec::<String>::new());
    let held = rich_accounts();
    commit(&store, &Accounts::default(), &held);
    assert_eq!(names(&db, "users"), ["alice", "zed"]);
    assert_eq!(names(&db, "api_tokens"), ["backup", "ci-ops"]);
    let stored = db.rows("SELECT record::text FROM users WHERE name = 'zed'");
    let json: serde_json::Value =
        serde_json::from_str(stored[0][0].as_deref().expect("a record")).expect("JSON");
    assert_eq!(
        json,
        serde_json::to_value(held.user("zed").expect("zed")).expect("JSON")
    );
    assert!(json.get("name").is_none(), "{json}");
    store.stop();
    let (store, loaded) = db.open("again").expect("the store opens again");
    assert_eq!(loaded.accounts, held);
    store.stop();
}

/// Proves: a commit writes only the rows that differ: a user changed leaves the other user's row
/// and the tokens' rows as they were (a mark put into each by hand is still there), a token
/// revoked deletes its row, and a user deleted deletes its row.
///
/// Catches: a commit that rewrites every credential at every change.
#[test]
fn a_commit_writes_only_the_rows_that_changed() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    let held = rich_accounts();
    commit(&store, &Accounts::default(), &held);
    db.run("UPDATE users SET record = jsonb_set(record, '{mark}', '1')")
        .expect("a mark");
    db.run("UPDATE api_tokens SET record = jsonb_set(record, '{mark}', '1')")
        .expect("a mark");
    let changed = held
        .with_user_edited("zed", |u| u.last_login_at = Some(1_800_009_999))
        .expect("edited");
    commit(&store, &held, &changed);
    assert_eq!(
        db.rows("SELECT name, record ? 'mark' FROM users ORDER BY name"),
        [["alice", "t"], ["zed", "f"]]
            .map(|row| row.map(|text| Some(text.to_owned())).to_vec())
            .to_vec()
    );
    assert_eq!(
        db.rows("SELECT record ? 'mark' FROM api_tokens"),
        vec![vec![Some("t".to_owned())]; 2]
    );
    let revoked = changed.without_token("ci-ops").expect("revoked");
    commit(&store, &changed, &revoked);
    assert_eq!(names(&db, "api_tokens"), ["backup"]);
    let deleted = revoked.without_user("alice").expect("deleted");
    commit(&store, &revoked, &deleted);
    assert_eq!(names(&db, "users"), ["zed"]);
    store.stop();
    let (store, loaded) = db.open("again").expect("the store opens again");
    assert_eq!(
        loaded.accounts.user("zed").map(|u| u.last_login_at),
        Some(Some(1_800_009_999))
    );
    assert!(loaded.accounts.user("alice").is_none());
    store.stop();
}

/// Proves: the tables refuse a name that is not 1 to 32 bytes of `a-z`, `0-9` and `-`, and a record
/// that is not JSON.
///
/// Catches: a name written by hand that the file's reader would refuse, so the two stores would
/// disagree about who exists.
#[test]
fn the_tables_refuse_what_the_accounts_cannot_hold() {
    let db = TestDatabase::new();
    let (store, _) = db.open("coordinator").expect("the store opens");
    for sql in [
        "INSERT INTO users (name, record) VALUES ('Alice', '{}')",
        "INSERT INTO users (name, record) VALUES ('', '{}')",
        "INSERT INTO api_tokens (name, record) VALUES ('ci_ops', '{}')",
        "INSERT INTO api_tokens (name, record) VALUES ('a-name-of-more-than-thirty-two-bytes', '{}')",
        "INSERT INTO users (name, record) VALUES ('alice', NULL)",
    ] {
        assert!(db.run(sql).is_err(), "{sql}");
    }
    db.run("INSERT INTO users (name, record) VALUES ('alice', '{}')")
        .expect("a name of the grammar");
    store.stop();
}

/// Proves: a row whose record does not read, a name the accounts refuse, and a token that shares a
/// user's name each stop the start, with the text that names the table and the record.
///
/// Catches: a start that serves accounts it cannot read, or an error that does not say which row.
#[test]
fn a_row_that_does_not_read_stops_the_start_naming_the_record() {
    let db = TestDatabase::new();
    let (store, _) = db.open("first").expect("the store opens");
    store.stop();
    db.run("INSERT INTO users (name, record) VALUES ('alice', '{\"hash\": \"x\"}')")
        .expect("a row");
    let refused = db.open("second").err().expect("refused");
    assert_eq!(
        refused.to_string(),
        "database:accounts: the record alice: missing field `enabled`; the coordinator does not guess which users and tokens exist, so fix or remove the file to start"
    );
    db.run("DELETE FROM users").expect("cleared");
    let record = r#"{"hash": "$argon2id$v=19$m=1,t=1,p=1$c2FsdA$aGFzaA", "enabled": true, "created_by": "x", "created_at": 1}"#;
    let token = format!(
        r#"{{"sha256": "{}", "created_by": "x", "created_at": 1}}"#,
        "a".repeat(64)
    );
    db.run(&format!(
        "INSERT INTO users (name, record) VALUES ('alice', '{record}'); \
         INSERT INTO api_tokens (name, record) VALUES ('alice', '{token}')"
    ))
    .expect("rows");
    let refused = db.open("third").err().expect("refused");
    assert_eq!(
        refused.to_string(),
        "database:accounts: the name alice is already a user's or a token's; the coordinator does not guess which users and tokens exist, so fix or remove the file to start"
    );
}

/// Proves: a commit of the accounts whose outcome was not known and that landed is laid over
/// memory by an account store, row by row: a user it created is taken, one memory changed since
/// stays as memory has it; a landing that did not land, or had no accounts, gives nothing.
///
/// Catches: a store that keeps memory as it was before a change the database holds, so a user
/// created in a commit that landed could not sign in until a restart.
#[test]
fn an_account_store_takes_a_landed_change_and_leaves_what_it_had_no_part_in() {
    let held = rich_accounts();
    let none = Accounts::default();
    let landed = |commit: &Commit<'_>, landed: bool| Landing {
        number: 1,
        landed,
        change: Some(Owned::of(commit)),
    };
    let change = Commit {
        accounts: Some((&none, &held)),
        ..Commit::default()
    };
    let backend: Shared = Arc::new(crate::store::Fake::new());

    let store = AccountStore::over(Arc::clone(&backend), none.clone());
    store.take_landed(&landed(&Commit::default(), true));
    store.take_landed(&landed(&change, false));
    assert_eq!(store.snapshot(), none, "nothing landed, nothing taken");
    store.take_landed(&landed(&change, true));
    assert_eq!(store.snapshot(), held);

    let store = AccountStore::over(Arc::clone(&backend), none.clone());
    store
        .change(|memory| {
            memory.with_user(crate::accounts::User {
                name: "zed".to_owned(),
                hash: "$argon2id$v=19$m=1,t=1,p=1$bWVt$aGFzaA".to_owned(),
                enabled: true,
                created_by: "alice".to_owned(),
                created_at: 1,
                ..crate::accounts::User::default()
            })
        })
        .expect("memory changed since");
    store.take_landed(&landed(&change, true));
    let taken = store.snapshot();
    assert_eq!(
        taken.user("zed").map(|u| u.hash.as_str()),
        Some("$argon2id$v=19$m=1,t=1,p=1$bWVt$aGFzaA"),
        "a row memory changed since stays as memory has it"
    );
    assert!(taken.user("alice").is_some() && taken.token("ci-ops").is_some());
}

/// Proves: `0010` does not stop the release before it (the migrations `0001` to `0009`) on a
/// database this release migrated and holding accounts: its runner takes the database, because
/// `0010` says `may-run`, and runs nothing, and its load of every table it knows reads the state as
/// it did; and this release starts on a database the release before migrated, runs `0010` and `0011` (the MDM tables)
/// and holds no account.
///
/// Catches: accounts that stop the previous coordinator from starting, which backing out of this
/// release would need the database restored for, or a migration this release does not run on a
/// database of the release before.
#[test]
fn the_release_before_starts_on_a_database_with_0010_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("the store opens");
    commit(&store, &Accounts::default(), &rich_accounts());
    store.stop();

    let (ran, loaded_before) = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..9]).expect("the release before's set");
        let ran = migrate::apply(session, &before)
            .expect("its runner takes the database and does not refuse 0010");
        (ran, load_all(session, SPENT_NOW).expect("the state reads"))
    });
    assert_eq!(ran, Vec::<i32>::new());
    assert_eq!(loaded_before.accounts, rich_accounts());
    assert_eq!(names(&db, "users"), ["alice", "zed"]);

    let older = TestDatabase::new();
    let ran = with_session(&older, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..9]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates")
    });
    assert_eq!(ran, (1..=9).collect::<Vec<i32>>());
    let (store, loaded) = older.open("this release").expect("this release starts");
    assert_eq!(loaded.accounts, Accounts::default());
    store.stop();
    assert_eq!(
        older.rows("SELECT version FROM schema_version ORDER BY version"),
        (1..=11)
            .map(|n| vec![Some(n.to_string())])
            .collect::<Vec<_>>()
    );
}

/// Proves: an account store over the Postgres store keeps its changes in the database: a user
/// created through it is a row, and the error of a store that cannot write is an I/O error that
/// leaves the accounts as they were.
///
/// Catches: a change taken as made when the database refused it.
#[test]
fn an_account_store_over_postgres_commits_through_the_database() {
    let db = TestDatabase::new();
    let (store, loaded) = db.open("coordinator").expect("the store opens");
    let backend: Shared = Arc::new(store.clone());
    let accounts = AccountStore::over(backend, loaded.accounts);
    let held = rich_accounts();
    accounts.change(|_| Ok(held.clone())).expect("created");
    assert_eq!(names(&db, "users"), ["alice", "zed"]);
    db.run(
        "CREATE FUNCTION test_refuse() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'the test refuses this commit'; END $$; \
         CREATE TRIGGER test_refuse BEFORE UPDATE ON lease FOR EACH ROW \
         EXECUTE FUNCTION test_refuse()",
    )
    .expect("the refusal is set");
    let refused = accounts.change(|memory| memory.without_user("alice"));
    assert!(matches!(refused, Err(AccountsError::Io(_))), "{refused:?}");
    assert_eq!(accounts.snapshot(), held);
    assert_eq!(names(&db, "users"), ["alice", "zed"]);
    store.stop();
}
