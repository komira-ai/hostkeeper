//! `serve` with `--initial-admin-file` (`docs/design/operator-login.md`, section 4): the start rule
//! that takes the flag in place of a token flag or beside one, the user the file makes before the
//! first call is served, a restart that adopts nothing twice, and the files a start refuses.

mod support;

use std::ffi::OsString;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use clap::error::ErrorKind;
use hostkeeper_coordinator::config::{Cli, StartError};
use hostkeeper_coordinator::initial_admin::InitialAdminError;
use hostkeeper_coordinator::operator::Caller;
use support::serve::{Files, build_given, flags_of, refusal, with, without};

const PASSWORD: &str = "k7Pq9xWm3Rt5Vb2NcH8dZs4F";
const ALICE: &str = "alice-token-0123456789abcdef0123456789";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after 1970")
        .as_secs()
}

/// The text of an initial-admin file for `user`, good until `valid_until`.
fn file(id: &str, user: &str, valid_until: u64) -> String {
    format!(
        "id = \"{id}\"\nuser = \"{user}\"\npassword = \"{PASSWORD}\"\nvalid_until = {valid_until}\n"
    )
}

/// The arguments with no token flag, the initial-admin file (of `files`) added, and the cheapest
/// password hash the algorithm takes.
fn initial_only(files: &Files) -> Vec<OsString> {
    let mut args = without(&files.args(), "--operator-token-file");
    args.extend([
        "--initial-admin-file".into(),
        files.path("initial-admin.toml").into(),
        "--password-memory-kib".into(),
        "8".into(),
        "--password-iterations".into(),
        "1".into(),
    ]);
    args
}

/// Proves: with `--initial-admin-file` and no token flag the coordinator starts, holds the user
/// the file names with a hash of the password (never the password) and the duty to change it, and
/// no bearer is an operator's.
///
/// Catches: a start rule that still needs a token file, a user made without `must_change`, or a
/// password kept in plain text.
#[tokio::test]
async fn the_flag_alone_starts_the_coordinator_with_the_user_of_the_file() {
    let files = Files::new("ia-alone");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() + 600),
        0o400,
    );
    let parts = build_given(&flags_of(initial_only(&files))).expect("starts");
    let held = parts.coordinator.accounts().snapshot();
    let admin = held.user("admin").expect("the user");
    assert!(admin.enabled && admin.must_change);
    assert!(admin.hash.starts_with("$argon2id$v=19$m=8,t=1,p=1$"));
    assert!(!admin.hash.contains(PASSWORD));
    assert!(matches!(
        parts.coordinator.caller(Some(ALICE)).await,
        Caller::Anonymous
    ));
    let state = std::fs::read_to_string(files.path("desired.json.accounts")).expect("stored");
    assert!(
        state.contains("admin") && !state.contains(PASSWORD),
        "{state}"
    );
}

/// Proves: the flag beside `--tokens-file` (and beside `--operator-token-file`) starts, the
/// tokens still name their operators (B8: what started before means the same), and the user is
/// made.
///
/// Catches: the new flag displacing the tokens a farm already uses.
#[tokio::test]
async fn the_flag_beside_a_token_flag_keeps_the_tokens_working() {
    let files = Files::new("ia-beside");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() + 600),
        0o400,
    );
    let tokens = files.tokens(&[("alice", ALICE)]);
    let mut args = initial_only(&files);
    args.extend(["--tokens-file".into(), tokens.into()]);
    let parts = build_given(&flags_of(args)).expect("starts beside a tokens file");
    assert!(
        parts
            .coordinator
            .accounts()
            .snapshot()
            .user("admin")
            .is_some()
    );
    match parts.coordinator.caller(Some(ALICE)).await {
        Caller::Operator(name) => assert_eq!(name.as_str(), "alice"),
        other => panic!("not alice: {other:?}"),
    }

    let files = Files::new("ia-beside-single");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() + 600),
        0o400,
    );
    let mut args = files.args();
    args.extend([
        "--initial-admin-file".into(),
        files.path("initial-admin.toml").into(),
        "--password-memory-kib".into(),
        "8".into(),
        "--password-iterations".into(),
        "1".into(),
    ]);
    let parts = build_given(&flags_of(args)).expect("starts beside the single token");
    assert!(
        parts
            .coordinator
            .accounts()
            .snapshot()
            .user("admin")
            .is_some()
    );
    assert!(matches!(
        parts.coordinator.caller(Some(support::OPERATOR)).await,
        Caller::Operator(_)
    ));
}

/// Proves: the start rule keeps refusing what it refused (both token flags, with or without the
/// initial-admin file; none of the three flags) and a start without the flag makes no user.
///
/// Catches: the new flag making the two-token-flags mistake acceptable, or a start that makes
/// users without being asked.
#[test]
fn the_start_rule_still_refuses_two_token_flags_and_no_flag_at_all() {
    let files = Files::new("ia-rule");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() + 600),
        0o400,
    );
    let mut both = initial_only(&files);
    both.extend([
        "--operator-token-file".into(),
        files.path("operator.token").into(),
        "--tokens-file".into(),
        files.tokens(&[("alice", ALICE)]).into(),
    ]);
    assert!(matches!(refusal(both), StartError::TokenFlags));

    let none = without(&files.args(), "--operator-token-file");
    assert!(matches!(refusal(none), StartError::NoTokenFlag));

    let parts = build_given(&flags_of(files.args())).expect("the old flags start");
    assert!(parts.coordinator.accounts().snapshot().is_empty());
}

/// Proves: a restart over the same state adopts the same file nothing more, so a password the
/// user changed is not reset by the file that is still there, and an expired file is deleted at
/// start, its record voided and the expiry audited.
///
/// Catches: a restart that resets a password twice, or an expired file that stays on the disk.
#[test]
fn a_restart_adopts_nothing_twice_and_an_expired_file_is_deleted_at_start() {
    let files = Files::new("ia-restart");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() + 600),
        0o400,
    );
    let first = build_given(&flags_of(initial_only(&files))).expect("starts");
    let hash = first
        .coordinator
        .accounts()
        .snapshot()
        .user("admin")
        .unwrap()
        .hash
        .clone();
    drop(first);
    let second = build_given(&flags_of(initial_only(&files))).expect("starts again");
    let again = second.coordinator.accounts().snapshot();
    assert_eq!(again.user("admin").unwrap().hash, hash);
    drop(second);

    // The same user, a file that expired a minute ago: gone at the next start.
    std::fs::remove_file(files.path("initial-admin.toml")).expect("replaced");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() - 60),
        0o400,
    );
    let third = build_given(&flags_of(initial_only(&files))).expect("starts over an old file");
    assert!(!files.path("initial-admin.toml").exists());
    let held = third.coordinator.accounts().snapshot();
    assert!(held.user("admin").unwrap().initial_void);
}

/// Proves: a start refuses a file of the wrong mode, one that asks for more than an hour, and one
/// that is not TOML, with text that names the flag and holds neither the path nor the password,
/// and makes no user.
///
/// Catches: a file anyone can read or edit that is believed, a deadline the file can stretch, or
/// a password in an error text.
#[test]
fn a_start_refuses_a_bad_file_naming_the_flag_and_never_the_password() {
    let files = Files::new("ia-refused");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() + 600),
        0o644,
    );
    let why = refusal(initial_only(&files));
    assert!(
        matches!(why, StartError::InitialAdmin(InitialAdminError::Read(_))),
        "{why}"
    );
    assert!(
        why.to_string().starts_with("--initial-admin-file: "),
        "{why}"
    );

    let files = Files::new("ia-refused-far");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() + 7200),
        0o400,
    );
    let why = refusal(initial_only(&files));
    assert!(
        matches!(
            why,
            StartError::InitialAdmin(InitialAdminError::TooFar { secs: 3600 })
        ),
        "{why}"
    );
    assert!(!files.path("desired.json.accounts").exists());

    let files = Files::new("ia-refused-shape");
    files.put("initial-admin.toml", &format!("{PASSWORD} = ["), 0o400);
    let why = refusal(initial_only(&files)).to_string();
    assert_eq!(
        why,
        "--initial-admin-file: it is not TOML holding id, user, password and valid_until"
    );
}

/// Proves: password settings the algorithm refuses stop the start naming the flag's file, and
/// `--initial-password-secs` takes 1 to 3600 only.
///
/// Catches: a longer lifetime than the owner's hour by a flag, or hashes made under parameters
/// that do not run.
#[test]
fn the_password_settings_and_the_lifetime_are_checked() {
    let files = Files::new("ia-settings");
    files.put(
        "initial-admin.toml",
        &file("id-1", "admin", now() + 600),
        0o400,
    );
    let weak = with(&initial_only(&files), "--password-memory-kib", "1");
    let why = refusal(weak);
    assert!(
        matches!(why, StartError::InitialAdmin(InitialAdminError::Hash(_))),
        "{why}"
    );

    for secs in ["0", "3601"] {
        let mut args = initial_only(&files);
        args.extend(["--initial-password-secs".into(), secs.into()]);
        let refused = Cli::try_parse_from(args).expect_err("out of range");
        assert_eq!(refused.kind(), ErrorKind::ValueValidation, "{secs}");
    }
    let mut args = initial_only(&files);
    args.extend(["--initial-password-secs".into(), "3600".into()]);
    build_given(&flags_of(args)).expect("an hour is taken");
}
