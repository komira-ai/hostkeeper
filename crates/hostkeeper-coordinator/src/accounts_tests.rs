//! The users and API tokens the store keeps: the rules of a name, a hash and a digest, the last
//! enabled user, the file they are written to, and the store that commits each change before
//! memory holds it. No clock and no timing: a time is a number a test gives.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use super::{
    AccountStore, Accounts, AccountsError, ApiToken, User, accounts_path, encode_file, parse_file,
};
use crate::store::conformance_tests::rich_accounts;
use crate::store::{Fake, Shared};
use crate::testutil::scratch;
use crate::tokens::TokenNameError;

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaA";

fn user(name: &str) -> User {
    User {
        name: name.to_owned(),
        hash: HASH.to_owned(),
        enabled: true,
        created_by: "coordinator".to_owned(),
        created_at: 1_800_000_000,
        ..User::default()
    }
}

fn token(name: &str, fill: char) -> ApiToken {
    ApiToken {
        name: name.to_owned(),
        sha256: fill.to_string().repeat(64),
        created_by: "alice".to_owned(),
        created_at: 1_800_000_100,
    }
}

fn with(users: &[&str]) -> Accounts {
    users.iter().fold(Accounts::default(), |held, name| {
        held.with_user(user(name)).expect("a user")
    })
}

/// Proves: a user or a token is added only under a name of the token grammar (1 to 32 bytes of
/// `a-z`, `0-9` and `-`) that the audit does not use for its own changes, and users and tokens
/// share one namespace: a name that is a user's is refused for a token and the other way round,
/// and a name used twice is refused whichever it is.
///
/// Catches: a user and a token that both answer to `alice`, so the audit's `by` means two things.
#[test]
fn a_name_is_one_name_of_the_token_grammar_for_a_user_or_a_token() {
    let held = with(&["alice"])
        .with_token(token("ci-ops", 'a'))
        .expect("a token");
    for bad in ["", "Alice", "al_ice", &"a".repeat(33)] {
        assert!(
            matches!(
                held.with_user(user(bad)),
                Err(AccountsError::Name {
                    source: TokenNameError::Grammar,
                    ..
                })
            ),
            "{bad:?}"
        );
    }
    for reserved in ["coordinator", "ci"] {
        assert!(matches!(
            held.with_user(user(reserved)),
            Err(AccountsError::Name {
                source: TokenNameError::Reserved(_),
                ..
            })
        ));
        assert!(matches!(
            held.with_token(token(reserved, 'b')),
            Err(AccountsError::Name {
                source: TokenNameError::Reserved(_),
                ..
            })
        ));
    }
    assert_eq!(
        held.with_user(user("alice"))
            .expect_err("a user twice")
            .to_string(),
        "the name alice is already a user's or a token's"
    );
    assert!(matches!(
        held.with_user(user("ci-ops")),
        Err(AccountsError::Taken(name)) if name == "ci-ops"
    ));
    assert!(matches!(
        held.with_token(token("alice", 'b')),
        Err(AccountsError::Taken(name)) if name == "alice"
    ));
    assert!(matches!(
        held.with_token(token("ci-ops", 'b')),
        Err(AccountsError::Taken(name)) if name == "ci-ops"
    ));
    assert!(held.with_user(user("bob-2")).is_ok());
}

/// Proves: a user record holds a PHC string and nothing else in the hash's place: text that does
/// not start with `$`, such as a plaintext password, and a string over 512 bytes are refused, on
/// creation and when an edit leaves one.
///
/// Catches: a plaintext password written into the store because a caller passed it for its hash.
#[test]
fn a_hash_that_is_not_a_phc_string_is_refused() {
    let held = with(&["alice"]);
    for bad in ["hunter2hunter2", "", &format!("${}", "a".repeat(512))] {
        let refused = held.with_user(User {
            hash: bad.to_owned(),
            ..user("bob")
        });
        assert_eq!(
            refused.expect_err("refused").to_string(),
            "the password hash of bob is not a PHC string of at most 512 bytes"
        );
        let edited = held.with_user_edited("alice", |u| u.hash = bad.to_owned());
        assert!(matches!(edited, Err(AccountsError::Hash(name)) if name == "alice"));
    }
}

/// Proves: a token's digest is 64 lowercase hex digits, and no two tokens share one: each other
/// spelling (63 digits, an upper-case letter, a letter past `f`, none) is refused naming the
/// token, and a second token with a taken digest is refused naming both.
///
/// Catches: one secret that answers to two names, or a digest no bearer can ever match.
#[test]
fn a_token_digest_is_64_lowercase_hex_digits_and_belongs_to_one_token() {
    let held = Accounts::default()
        .with_token(token("ci-ops", 'a'))
        .expect("a token");
    for bad in [
        "a".repeat(63),
        "A".repeat(64),
        "g".repeat(64),
        String::new(),
    ] {
        let refused = held.with_token(ApiToken {
            sha256: bad,
            ..token("backup", 'b')
        });
        assert_eq!(
            refused.expect_err("refused").to_string(),
            "the sha256 of the token backup is not 64 lowercase hex digits"
        );
    }
    assert_eq!(
        held.with_token(token("backup", 'a'))
            .expect_err("the same digest")
            .to_string(),
        "the tokens backup and ci-ops have the same sha256"
    );
    assert!(held.with_token(token("backup", 'b')).is_ok());
}

/// Proves: the last enabled user cannot be disabled or deleted, however many disabled users
/// remain, and either is allowed once another user is enabled; enabling a disabled user, and
/// disabling one that is already disabled, never trip the rule.
///
/// Catches: a farm locked out of its own UI by an admin who disabled or removed the last way in.
#[test]
fn the_last_enabled_user_cannot_be_disabled_or_deleted() {
    let held = with(&["alice", "zed"])
        .with_enabled("alice", false)
        .expect("one of two is disabled");
    assert_eq!(held.enabled_users(), 1);
    for refused in [held.with_enabled("zed", false), held.without_user("zed")] {
        let why = refused.expect_err("the last enabled user");
        assert!(matches!(why, AccountsError::LastEnabled));
        assert_eq!(
            why.to_string(),
            "the last enabled user cannot be disabled or deleted"
        );
    }
    assert_eq!(
        held.with_enabled("alice", false).expect("already disabled"),
        held
    );
    assert_eq!(
        held.without_user("alice")
            .expect("a disabled one")
            .users()
            .count(),
        1
    );
    let both = held.with_enabled("alice", true).expect("enabled again");
    assert_eq!(both.enabled_users(), 2);
    assert_eq!(
        both.with_enabled("zed", false)
            .expect("not the last")
            .enabled_users(),
        1
    );
    assert_eq!(
        both.without_user("zed")
            .expect("not the last")
            .enabled_users(),
        1
    );
}

/// Proves: a user or a token that is not there is refused by name for each change, and an edit
/// changes the fields it is given and never the name the record is kept under.
///
/// Catches: an edit that renames a record away from its key, which a load would then read as
/// another user.
#[test]
fn an_edit_changes_the_record_and_never_the_name() {
    let held = with(&["alice"]);
    let edited = held
        .with_user_edited("alice", |u| {
            u.must_change = true;
            u.initial_expires_at = Some(1_800_003_600);
            u.name = "mallory".to_owned();
        })
        .expect("edited");
    let alice = edited.user("alice").expect("still alice");
    assert_eq!(alice.name, "alice");
    assert!(alice.must_change);
    assert_eq!(alice.initial_expires_at, Some(1_800_003_600));
    assert!(edited.user("mallory").is_none());
    assert!(
        held.user("alice").is_some_and(|u| !u.must_change),
        "the original is untouched"
    );
    for refused in [
        held.with_user_edited("bob", |_| {}),
        held.with_enabled("bob", true),
        held.without_user("bob"),
    ] {
        assert_eq!(
            refused.expect_err("none").to_string(),
            "there is no user named bob"
        );
    }
    assert_eq!(
        held.without_token("ci-ops").expect_err("none").to_string(),
        "there is no API token named ci-ops"
    );
    let with_token = held.with_token(token("ci-ops", 'a')).expect("a token");
    assert!(
        with_token
            .without_token("ci-ops")
            .expect("removed")
            .token("ci-ops")
            .is_none()
    );
}

/// Proves: the file is exactly the JSON below: a record for each user and token keyed by name, the
/// name not repeated in it, an optional field written only when it holds something, and a newline
/// at the end; and reading it gives the accounts back.
///
/// Catches: a change of the file's shape, which the release before this one's reader, and the
/// next one's, would have to follow.
#[test]
fn the_file_holds_exactly_the_records_below_and_reads_back() {
    let held = with(&["alice"])
        .with_token(token("ci-ops", 'c'))
        .and_then(|held| held.with_user_edited("alice", |u| u.last_login_at = Some(1_800_000_500)))
        .expect("accounts");
    let text = String::from_utf8(encode_file(&held).expect("encodes")).expect("text");
    let expected = format!(
        r#"{{
  "users": {{
    "alice": {{
      "hash": "{HASH}",
      "enabled": true,
      "must_change": false,
      "initial_void": false,
      "created_by": "coordinator",
      "created_at": 1800000000,
      "last_login_at": 1800000500
    }}
  }},
  "tokens": {{
    "ci-ops": {{
      "sha256": "{}",
      "created_by": "alice",
      "created_at": 1800000100
    }}
  }}
}}
"#,
        "c".repeat(64)
    );
    assert_eq!(text, expected);
    assert_eq!(
        parse_file(Path::new("accounts"), text.as_bytes()).expect("reads"),
        held
    );
    assert_eq!(
        String::from_utf8(encode_file(&Accounts::default()).expect("encodes")).expect("text"),
        "{}\n"
    );
}

/// Proves: a file whose keys this build does not know is read as it knows them (a later build may
/// add a key to the file, to a user or to a token), a record that leaves out every optional field
/// reads with each at its default, and the rich accounts survive a round trip.
///
/// Catches: a start refused on a file a later release wrote, which backing out to this release
/// would then not survive.
#[test]
fn a_key_this_build_does_not_know_is_skipped_and_an_optional_one_may_be_absent() {
    let text = format!(
        r#"{{"future": 1,
            "users": {{"alice": {{"hash": "{HASH}", "enabled": false, "created_by": "x",
                                  "created_at": 5, "mfa": "totp"}}}},
            "tokens": {{"ci-ops": {{"sha256": "{}", "created_by": "alice", "created_at": 6,
                                    "expires": 7}}}}}}"#,
        "d".repeat(64)
    );
    let held = parse_file(Path::new("accounts"), text.as_bytes()).expect("reads");
    assert_eq!(
        held.user("alice"),
        Some(&User {
            name: "alice".to_owned(),
            hash: HASH.to_owned(),
            enabled: false,
            created_by: "x".to_owned(),
            created_at: 5,
            ..User::default()
        })
    );
    assert_eq!(held.token("ci-ops").map(|t| t.created_at), Some(6));
    let rich = rich_accounts();
    let again = parse_file(Path::new("accounts"), &encode_file(&rich).expect("encodes"));
    assert_eq!(again.expect("reads"), rich);
}

/// Proves: a file that is not JSON of the shape, or breaks a rule of the accounts (a plaintext
/// hash, a name that is not a name, one name for a user and a token, a digest used twice), is
/// refused with the file's path and the reason, and the start is told what to do.
///
/// Catches: a bad file read as no users, which the next change would overwrite, locking everyone
/// out without a word.
#[test]
fn a_file_that_breaks_a_rule_is_refused_naming_the_file_and_the_reason() {
    let digest = "e".repeat(64);
    let record =
        format!(r#"{{"hash": "{HASH}", "enabled": true, "created_by": "x", "created_at": 1}}"#);
    let token_record = format!(r#"{{"sha256": "{digest}", "created_by": "x", "created_at": 1}}"#);
    let cases = [
        (
            "not json".to_owned(),
            "expected ident at line 1 column 2".to_owned(),
        ),
        (
            r#"{"users": {"alice": {"hash": "x"}}}"#.to_owned(),
            "missing field `enabled` at line 1 column 33".to_owned(),
        ),
        (
            format!(
                r#"{{"users": {{"alice": {}}}}}"#,
                record.replace(HASH, "hunter2")
            ),
            "the password hash of alice is not a PHC string of at most 512 bytes".to_owned(),
        ),
        (
            format!(r#"{{"users": {{"Alice": {record}}}}}"#),
            "Alice: a token name is 1 to 32 bytes of a-z, 0-9 and -".to_owned(),
        ),
        (
            format!(r#"{{"users": {{"alice": {record}}}, "tokens": {{"alice": {token_record}}}}}"#),
            "the name alice is already a user's or a token's".to_owned(),
        ),
        (
            format!(r#"{{"tokens": {{"a": {token_record}, "b": {token_record}}}}}"#),
            "the tokens a and b have the same sha256".to_owned(),
        ),
    ];
    for (text, why) in cases {
        let refused = parse_file(Path::new("/srv/hk/desired.json.accounts"), text.as_bytes())
            .expect_err("refused");
        assert_eq!(
            refused.to_string(),
            format!(
                "/srv/hk/desired.json.accounts: {why}; the coordinator does not guess which users and tokens exist, so fix or remove the file to start"
            ),
            "{text}"
        );
    }
}

/// Proves: a change is made durable before memory holds it. A backend that refuses it leaves the
/// accounts as they were, the error is the backend's, and a change the edit itself refuses
/// commits nothing; a change that leaves the accounts as they were writes nothing.
///
/// Catches: memory that runs ahead of the store, so a user created in a failed write signs in
/// until the next restart and then does not exist.
#[test]
fn a_change_is_durable_before_memory_holds_it_and_a_refused_one_changes_nothing() {
    let fake = Fake::new();
    let backend: Shared = Arc::new(fake.clone());
    let store = AccountStore::over(Arc::clone(&backend), Accounts::default());
    store
        .change(|held| held.with_user(user("alice")))
        .expect("created");
    assert_eq!(fake.commits(), 1);
    assert_eq!(fake.accounts(), with(&["alice"]));
    assert_eq!(store.snapshot(), with(&["alice"]));

    store
        .change(|held| Ok(held.clone()))
        .expect("nothing to change");
    assert_eq!(
        fake.commits(),
        1,
        "a change that changes nothing writes nothing"
    );

    let refused = store.change(|held| held.with_user(user("alice")));
    assert!(matches!(refused, Err(AccountsError::Taken(_))));
    assert_eq!(fake.commits(), 1, "a refused edit commits nothing");

    fake.fail();
    let failed = store.change(|held| held.with_user(user("bob")));
    assert!(matches!(failed, Err(AccountsError::Io(_))), "{failed:?}");
    assert_eq!(store.snapshot(), with(&["alice"]));
    assert_eq!(fake.accounts(), with(&["alice"]));
    fake.heal();
    store
        .change(|held| held.with_user(user("bob")))
        .expect("created after the heal");
    assert_eq!(store.snapshot(), with(&["alice", "bob"]));
}

/// Proves: a store over the file writes `<state-file>.accounts` with mode 0600, beside no other
/// file, and a store opened again on it holds the same accounts; a store opened on a state file
/// with no accounts file holds none and writes nothing, and a file that is bad stops the open
/// naming it.
///
/// Catches: password hashes in a file other users can read, a restart that forgets a user, or a
/// start that needs a file the release before never wrote.
#[test]
fn the_file_store_writes_a_private_file_beside_the_state_file_and_reads_it_at_start() {
    let dir = scratch("accounts-file");
    let path = accounts_path(&dir.path().join("desired.json"));
    assert_eq!(path, dir.path().join("desired.json.accounts"));
    assert_eq!(
        AccountStore::open(&path).expect("no file").snapshot(),
        Accounts::default()
    );
    assert!(!path.exists(), "opening writes nothing");

    let store = AccountStore::open(&path).expect("opens");
    store
        .change(|held| {
            held.with_user(user("alice"))
                .and_then(|h| h.with_token(token("ci-ops", 'a')))
        })
        .expect("created");
    assert_eq!(
        fs::metadata(&path).expect("written").permissions().mode() & 0o777,
        0o600
    );
    let names: Vec<_> = fs::read_dir(dir.path())
        .expect("a directory")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert_eq!(
        names,
        ["desired.json.accounts"],
        "no temporary file is left"
    );
    let again = AccountStore::open(&path).expect("opens again");
    assert_eq!(again.snapshot(), store.snapshot());

    fs::write(&path, b"{ not json").expect("damaged");
    let refused = AccountStore::open(&path).expect_err("refused");
    assert!(
        refused
            .to_string()
            .starts_with(&format!("{}: ", path.display())),
        "{refused}"
    );
}

/// Proves: a file store whose write cannot be made (the temporary file's name is a directory)
/// refuses the change with an I/O error and holds the accounts as they were, in memory and in the
/// file.
///
/// Catches: a full disk or a bad directory that is answered as a created user.
#[test]
fn a_file_that_cannot_be_written_leaves_the_accounts_as_they_were() {
    let dir = scratch("accounts-file-fails");
    let path = dir.path().join("desired.json.accounts");
    let store = AccountStore::open(&path).expect("opens");
    store
        .change(|held| held.with_user(user("alice")))
        .expect("created");
    let before = fs::read(&path).expect("written");
    fs::create_dir(dir.path().join("desired.json.accounts.new")).expect("in the way");
    let refused = store.change(|held| held.with_user(user("bob")));
    assert!(matches!(refused, Err(AccountsError::Io(_))), "{refused:?}");
    assert_eq!(store.snapshot(), with(&["alice"]));
    assert_eq!(fs::read(&path).expect("still there"), before);
}

/// Proves: accounts built from stored records take each record's name from its key, and a record
/// that does not read, or a name that is not a name, is refused naming the place of the rows.
///
/// Catches: a load of rows that gives a user a name other than its key's.
#[test]
fn records_are_read_under_their_keys_and_refused_by_the_place_that_held_them() {
    let record = serde_json::json!({
        "hash": HASH, "enabled": true, "created_by": "x", "created_at": 1
    });
    let held = Accounts::of_records(
        Path::new("database/accounts"),
        [("alice".to_owned(), record.clone())],
        std::iter::empty(),
    )
    .expect("reads");
    assert_eq!(held.user("alice").map(|u| u.name.as_str()), Some("alice"));
    let refused = Accounts::of_records(
        Path::new("database/accounts"),
        [("alice".to_owned(), serde_json::json!({"hash": HASH}))],
        std::iter::empty(),
    )
    .expect_err("a record that does not read");
    assert!(
        refused
            .to_string()
            .starts_with("database/accounts: the record alice: "),
        "{refused}"
    );
    let refused = Accounts::of_records(
        Path::new("database/accounts"),
        [("Alice".to_owned(), record)],
        std::iter::empty::<(String, serde_json::Value)>(),
    )
    .expect_err("a name that is not a name");
    assert!(
        refused
            .to_string()
            .starts_with("database/accounts: Alice: "),
        "{refused}"
    );
    let mut users = BTreeMap::new();
    users.insert("alice".to_owned(), user("alice"));
    assert!(Accounts::new(users, BTreeMap::new()).is_ok());
}
