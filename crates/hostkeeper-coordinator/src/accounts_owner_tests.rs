//! Who an API token belongs to: the owner a record holds, or its maker for a record written before
//! tokens had owners; the user a token acts as, which must be enabled; and a user's deletion,
//! which takes their tokens with it. No clock: a time is a number a test gives.

use std::path::Path;

use super::{Accounts, AccountsError, ApiToken, User, encode_file, parse_file};

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

fn token(name: &str, owner: &str, fill: char) -> ApiToken {
    ApiToken {
        name: name.to_owned(),
        sha256: fill.to_string().repeat(64),
        owner: owner.to_owned(),
        created_by: owner.to_owned(),
        created_at: 1_800_000_100,
    }
}

/// Proves: a token record with no `owner` (one written before tokens had owners) reads as owned
/// by `created_by`, whose name it then acts as, and the accounts are written back with that owner
/// in the record; a record with an owner keeps it, whoever made it.
///
/// Catches: a token made in the UI before this release (its maker a user) that stops working, or
/// acts as its own name, after the upgrade; and an owner lost on the next write.
#[test]
fn a_record_without_an_owner_reads_as_owned_by_its_maker_and_is_written_with_one() {
    let text = format!(
        r#"{{"users": {{"erin": {{"hash": "{HASH}", "enabled": true, "created_by": "coordinator",
                              "created_at": 1}},
                      "dana": {{"hash": "{HASH}", "enabled": true, "created_by": "erin",
                               "created_at": 2}}}},
            "tokens": {{"devbox": {{"sha256": "{}", "created_by": "erin", "created_at": 3}},
                       "robot": {{"sha256": "{}", "owner": "dana", "created_by": "erin",
                                 "created_at": 4}}}}}}"#,
        "a".repeat(64),
        "b".repeat(64)
    );
    let held = parse_file(Path::new("accounts"), text.as_bytes()).expect("reads");
    assert_eq!(
        held.token("devbox")
            .map(|t| (t.owner.as_str(), t.created_by.as_str())),
        Some(("erin", "erin"))
    );
    assert_eq!(
        held.token("robot")
            .map(|t| (t.owner.as_str(), t.created_by.as_str())),
        Some(("dana", "erin"))
    );
    assert_eq!(
        held.acting_owner("devbox").map(|u| u.name.as_str()),
        Some("erin")
    );
    assert_eq!(
        held.acting_owner("robot").map(|u| u.name.as_str()),
        Some("dana")
    );
    let written = String::from_utf8(encode_file(&held).expect("encodes")).expect("text");
    assert!(
        written.contains(&format!(
            "\"devbox\": {{\n      \"sha256\": \"{}\",\n      \"owner\": \"erin\",\n",
            "a".repeat(64)
        )),
        "{written}"
    );
    assert_eq!(
        parse_file(Path::new("accounts"), written.as_bytes()).expect("reads"),
        held
    );
}

/// Proves: a token acts as its owner while the owner is an enabled user, and as no one once the
/// owner is disabled (and again once enabled); a token whose owner is no user (a record a tokens
/// file token made before tokens had owners) acts as no one, and a name that is no token's acts as
/// no one either.
///
/// Catches: a disabled person's script that keeps working, and a token that falls back to acting
/// as its own name, or its maker's, when its owner is not a user.
#[test]
fn a_token_acts_as_its_owner_only_while_the_owner_is_an_enabled_user() {
    let held = Accounts::default()
        .with_user(user("erin"))
        .and_then(|held| held.with_user(user("zed")))
        .and_then(|held| held.with_token(token("devbox", "erin", 'a')))
        .expect("accounts");
    assert_eq!(
        held.acting_owner("devbox").map(|u| u.name.as_str()),
        Some("erin")
    );
    let disabled = held.with_enabled("erin", false).expect("erin is disabled");
    assert_eq!(disabled.acting_owner("devbox"), None);
    let enabled = disabled
        .with_enabled("erin", true)
        .expect("erin is enabled");
    assert_eq!(
        enabled.acting_owner("devbox").map(|u| u.name.as_str()),
        Some("erin")
    );
    assert_eq!(held.acting_owner("nothing"), None);

    let text = format!(
        r#"{{"users": {{"erin": {{"hash": "{HASH}", "enabled": true, "created_by": "coordinator",
                              "created_at": 1}}}},
            "tokens": {{"legacy": {{"sha256": "{}", "created_by": "ops-file", "created_at": 3}}}}}}"#,
        "c".repeat(64)
    );
    let legacy = parse_file(Path::new("accounts"), text.as_bytes()).expect("reads");
    assert_eq!(
        legacy.token("legacy").map(|t| t.owner.as_str()),
        Some("ops-file")
    );
    assert_eq!(legacy.acting_owner("legacy"), None);
}

/// Proves: a token is added only with an owner who is a user (a disabled one too), and the
/// refusal names the owner; a token added with no owner is owned by its maker, as a stored record
/// with none is; a refused one adds nothing.
///
/// Catches: a token owned by no one, which would be stored and refused at every call, or owned
/// by a name a user made later would inherit.
#[test]
fn a_token_is_added_only_for_an_owner_who_is_a_user() {
    let held = Accounts::default()
        .with_user(user("erin"))
        .and_then(|held| held.with_user(user("zed")))
        .and_then(|held| held.with_enabled("zed", false))
        .expect("accounts");
    let refused = held
        .with_token(token("robot", "bob", 'a'))
        .expect_err("bob is no user");
    assert!(matches!(&refused, AccountsError::NoUser(name) if name == "bob"));
    assert_eq!(refused.to_string(), "there is no user named bob");
    assert_eq!(
        held.with_token(token("robot", "", 'a'))
            .expect_err("no owner")
            .to_string(),
        "there is no user named "
    );
    assert!(held.with_token(token("robot", "zed", 'a')).is_ok());
    let by_maker = held
        .with_token(ApiToken {
            owner: String::new(),
            ..token("robot", "erin", 'a')
        })
        .expect("owned by its maker");
    assert_eq!(
        by_maker.token("robot").map(|t| t.owner.as_str()),
        Some("erin")
    );
    assert_eq!(held.tokens().count(), 0);
}

/// Proves: deleting a user removes every token they own and no other token; the tokens of a user
/// are listed by `tokens_of`; a user made later under the name owns none of them.
///
/// Catches: a deleted person's token that comes back to life when a user of the same name is
/// made, and a deletion that takes another user's token.
#[test]
fn deleting_a_user_removes_the_tokens_they_own_and_no_other() {
    let held = Accounts::default()
        .with_user(user("erin"))
        .and_then(|held| held.with_user(user("zed")))
        .and_then(|held| held.with_token(token("devbox", "zed", 'a')))
        .and_then(|held| held.with_token(token("ci-ops", "zed", 'b')))
        .and_then(|held| held.with_token(token("laptop", "erin", 'c')))
        .expect("accounts");
    let owned: Vec<&str> = held.tokens_of("zed").map(|t| t.name.as_str()).collect();
    assert_eq!(owned, ["ci-ops", "devbox"]);
    let deleted = held.without_user("zed").expect("zed is deleted");
    let left: Vec<&str> = deleted.tokens().map(|t| t.name.as_str()).collect();
    assert_eq!(left, ["laptop"]);
    let again = deleted.with_user(user("zed")).expect("a new zed");
    assert_eq!(again.tokens_of("zed").count(), 0);
    assert_eq!(again.acting_owner("devbox"), None);
}

/// Proves: a token stored before tokens had owners, whose maker was deleted before this release
/// (so it names a user who is gone, and acts as no one), is dropped when a user is made under that
/// name: the new user owns no token and the old secret acts as no one; another user's token stays.
///
/// Catches: a deleted person's old token that a new user of the same name inherits, with that
/// user's console and SSH rights.
#[test]
fn a_user_made_under_a_gone_makers_name_does_not_inherit_the_old_token() {
    let text = format!(
        r#"{{"users": {{"erin": {{"hash": "{HASH}", "enabled": true, "created_by": "coordinator",
                              "created_at": 1}}}},
            "tokens": {{"old": {{"sha256": "{}", "created_by": "gone", "created_at": 3}},
                       "kept": {{"sha256": "{}", "created_by": "erin", "created_at": 4}}}}}}"#,
        "a".repeat(64),
        "b".repeat(64)
    );
    let held = parse_file(Path::new("accounts"), text.as_bytes()).expect("reads");
    assert_eq!(held.acting_owner("old"), None);
    let again = held.with_user(user("gone")).expect("a new user");
    assert!(again.token("old").is_none());
    assert_eq!(again.acting_owner("old"), None);
    assert_eq!(again.tokens_of("gone").count(), 0);
    assert_eq!(
        again.acting_owner("kept").map(|u| u.name.as_str()),
        Some("erin")
    );
}

/// Proves: a record a release before rewrote without `owner` (after a rollback) reads as owned by
/// its maker, whoever owned it before; a maker who is no user leaves it acting as no one.
///
/// Catches: a rollback and roll-forward that leaves a token acting as someone it was never
/// tied to, other than its maker.
#[test]
fn a_record_rewritten_without_an_owner_is_owned_by_its_maker_again() {
    let text = format!(
        r#"{{"users": {{"erin": {{"hash": "{HASH}", "enabled": true, "created_by": "coordinator",
                              "created_at": 1}},
                      "dana": {{"hash": "{HASH}", "enabled": true, "created_by": "erin",
                               "created_at": 2}}}},
            "tokens": {{"robot": {{"sha256": "{}", "created_by": "dana", "created_at": 3}}}}}}"#,
        "a".repeat(64)
    );
    let held = parse_file(Path::new("accounts"), text.as_bytes()).expect("reads");
    assert_eq!(
        held.acting_owner("robot").map(|u| u.name.as_str()),
        Some("dana")
    );
}
