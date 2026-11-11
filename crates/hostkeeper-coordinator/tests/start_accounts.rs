//! `serve`'s start over the users and API tokens file (`<state-file>.accounts`,
//! `docs/design/operator-login.md` section 9): a start with no file holds no user and no token and
//! writes none, a start with one holds what it names, and a file that does not read refuses the
//! start naming it.

mod support;

use hostkeeper_coordinator::config::StartError;
use support::serve::{Files, build_given, flags_of, refusal};

const HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaA";

fn accounts_text() -> String {
    format!(
        r#"{{"users": {{"alice": {{"hash": "{HASH}", "enabled": true, "created_by": "coordinator",
                                   "created_at": 1800000000}}}},
            "tokens": {{"ci-ops": {{"sha256": "{}", "created_by": "alice", "created_at": 1800000100}}}}}}"#,
        "a".repeat(64)
    )
}

/// Catches: a start that needs the accounts file, which the release before never wrote, or that
/// writes one when it finds none.
#[test]
fn serve_starts_with_no_accounts_when_there_is_no_file() {
    let files = Files::new("start-accounts-none");
    let parts = build_given(&flags_of(files.args())).expect("starts");
    assert!(parts.coordinator.accounts().snapshot().is_empty());
    assert!(!files.path("desired.json.accounts").exists());
}

/// Catches: users and tokens in a file beside the state file that a restart does not load, or an
/// accounts store that commits somewhere other than that file.
#[test]
fn serve_starts_with_the_users_and_tokens_of_the_accounts_file_and_writes_changes_to_it() {
    let files = Files::new("start-accounts-file");
    files.put("desired.json.accounts", &accounts_text(), 0o600);
    let parts = build_given(&flags_of(files.args())).expect("starts");
    let held = parts.coordinator.accounts().snapshot();
    assert_eq!(
        held.users().map(|u| u.name.as_str()).collect::<Vec<_>>(),
        ["alice"]
    );
    assert_eq!(
        held.tokens().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["ci-ops"]
    );

    parts
        .coordinator
        .accounts()
        .change(|held| {
            held.with_enabled("alice", true)
                .and_then(|h| h.without_token("ci-ops"))
        })
        .expect("a token revoked");
    let written = std::fs::read_to_string(files.path("desired.json.accounts")).expect("the file");
    assert!(!written.contains("ci-ops"), "{written}");
    assert!(written.contains("alice"), "{written}");
}

/// Catches: a file that does not read taken as no users, which the next change would overwrite.
#[test]
fn serve_refuses_to_start_on_an_accounts_file_it_cannot_read() {
    let files = Files::new("start-accounts-bad");
    files.put("desired.json.accounts", "{ not json", 0o600);
    let why = refusal(files.args());
    assert!(matches!(why, StartError::Accounts(_)), "{why}");
    assert!(
        why.to_string().starts_with(&format!(
            "{}: ",
            files.path("desired.json.accounts").display()
        )),
        "{why}"
    );
}
