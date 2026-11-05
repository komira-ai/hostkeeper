//! The operator's named tokens: the names, the tokens file, the reload on the tick, and the name
//! that reaches the audit.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_oidc::testkit::static_verifier;
use hostkeeper_testkit::Scratch;
use hostkeeper_wire::secret::SecretError;
use serde_json::json;

use crate::desired::DesiredStore;
use crate::identity::Identity;
use crate::ops::OperateError;
use crate::ops_file::digest;
use crate::service::{Coordinator, Unauthorized};
use crate::testutil::{
    LEGACY, Log, beat, ci_token, clock_of, id, kbf_prefixes, kbf_services, ops_of, role, scratch,
};
use crate::tokens::{OperatorTokens, Reload, TokenName, TokenNameError, TokensError};

const ALICE: &str = "alice-token-0123456789abcdef0123456789";
const BOB: &str = "bob-token-0123456789abcdef0123456789abc";
const ALICE2: &str = "alice-token-v2-0123456789abcdef012345";
const GRAMMAR: &str = "a token name is 1 to 32 bytes of a-z, 0-9 and -";

/// One `[[token]]` table.
fn entry(name: &str, sha256: &str) -> String {
    format!("[[token]]\nname = \"{name}\"\nsha256 = \"{sha256}\"\n")
}

/// The text of a tokens file naming each `(name, token)`: the file holds the hash.
fn text(tokens: &[(&str, &str)]) -> String {
    tokens
        .iter()
        .map(|(name, token)| entry(name, &digest(token.as_bytes())))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tokens file of `dir`, written whole with `mode`.
fn put(dir: &Scratch, bytes: &[u8], mode: u32) -> PathBuf {
    let path = dir.path().join("tokens.toml");
    fs::write(&path, bytes).expect("written");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
    path
}

/// The tokens file of `dir`, naming each `(name, token)`, with mode 0600.
fn write(dir: &Scratch, tokens: &[(&str, &str)]) -> PathBuf {
    put(dir, text(tokens).as_bytes(), 0o600)
}

/// Why the tokens file `bytes` (mode 0600) is refused.
fn refused(name: &str, bytes: &[u8]) -> TokensError {
    let dir = scratch(name);
    let path = put(&dir, bytes, 0o600);
    OperatorTokens::from_file(&path).err().expect("refused")
}

/// A coordinator over the recorded kbf service, its files in `dir`, its operators `tokens`.
fn coordinator(dir: &Scratch, tokens: OperatorTokens) -> Coordinator {
    let clock = Arc::new(AtomicU64::new(1_000));
    let store =
        DesiredStore::open(&dir.path().join("desired.json"), Some(LEGACY)).expect("a store");
    let (verifier, _) = static_verifier();
    Coordinator::new(store, verifier, tokens, clock_of(&clock))
        .with_services(kbf_services())
        .with_legacy_service(Some(LEGACY.to_owned()))
        .with_prefixes(kbf_prefixes())
        .with_ops(ops_of(dir))
}

/// The name of the token `presented`, as text.
fn name_of(tokens: &OperatorTokens, presented: &str) -> Option<String> {
    tokens
        .name_of(presented)
        .map(|name| name.as_str().to_owned())
}

/// Proves: a token name is 1 to 32 bytes of `a-z`, `0-9` and `-`, `as_str` gives it back and the
/// refusal says the rule.
///
/// Catches: a grammar that lets in capitals, spaces, underscores or non-ASCII, or a length bound
/// off by one.
#[test]
fn a_token_name_is_1_to_32_bytes_of_a_z_0_9_and_hyphen() {
    let longest = "a".repeat(32);
    for good in ["a", "0", "ci-ops", "-", longest.as_str()] {
        let name: TokenName = good.parse().expect("a name");
        assert_eq!(name.as_str(), good);
    }
    let too_long = "a".repeat(33);
    for bad in ["", too_long.as_str(), "Alice", "a b", "a_b", "é"] {
        assert_eq!(bad.parse::<TokenName>(), Err(TokenNameError::Grammar));
    }
    assert_eq!(TokenNameError::Grammar.to_string(), GRAMMAR);
}

/// Proves: `coordinator`, `ci` and `mdm-nonce` are not token names, since the audit uses them
/// for changes that are not an operator's; a name that only starts with one is fine.
///
/// Catches: an operator token named `ci` whose changes read as CI's in the audit, or one named
/// `mdm-nonce` whose approvals read as a Mac's automatic one.
#[test]
fn no_token_may_take_a_name_the_audit_uses_for_other_actors() {
    assert_eq!(
        "coordinator".parse::<TokenName>(),
        Err(TokenNameError::Reserved("coordinator"))
    );
    assert_eq!(
        "mdm-nonce".parse::<TokenName>(),
        Err(TokenNameError::Reserved("mdm-nonce"))
    );
    assert_eq!(
        "ci".parse::<TokenName>(),
        Err(TokenNameError::Reserved("ci"))
    );
    for fine in ["ci-ops", "coordinator-2"] {
        assert_eq!(fine.parse::<TokenName>().expect("a name").as_str(), fine);
    }
    assert_eq!(
        TokenNameError::Reserved("ci").to_string(),
        "a token may not be named ci: the audit uses that name for changes that are not an \
         operator's"
    );
}

/// Proves: each token of the file is known by its name, a near miss, the name, or the hash is
/// no token, and the file holds neither token.
///
/// Catches: a comparison on a prefix or on the hash itself, or a file that stores the token.
#[test]
fn the_tokens_file_names_each_token_and_holds_only_hashes() {
    let dir = scratch("tokens-hashes");
    let path = write(&dir, &[("alice", ALICE), ("bob", BOB)]);
    let tokens = OperatorTokens::from_file(&path).expect("tokens");
    assert_eq!(name_of(&tokens, ALICE), Some("alice".to_owned()));
    assert_eq!(name_of(&tokens, BOB), Some("bob".to_owned()));
    let misses = [
        String::new(),
        "alice".to_owned(),
        format!("{ALICE}x"),
        ALICE[1..].to_owned(),
        ALICE.to_uppercase(),
        digest(ALICE.as_bytes()),
    ];
    for miss in &misses {
        assert_eq!(name_of(&tokens, miss), None);
    }
    let content = fs::read_to_string(&path).expect("read");
    assert_eq!(content.matches(ALICE).count(), 0);
    assert_eq!(content.matches(BOB).count(), 0);
}

/// Proves: each rule of the file's grammar refuses it with a text that names the flag and the
/// token, and a text the parser makes starts with the flag.
///
/// Catches: a file with no token locking everyone out, a bad digest or a repeated name accepted,
/// or a refusal that does not say which token.
#[test]
fn a_tokens_file_that_breaks_a_rule_is_refused_naming_the_flag() {
    let good = digest(b"x");
    let other = digest(b"y");
    let exact: Vec<(String, &str)> = vec![
        (String::new(), "--tokens-file: it names no token"),
        ("# nothing\n".to_owned(), "--tokens-file: it names no token"),
        (
            format!("{}{}", entry("a", &good), entry("Alice", &other)),
            "--tokens-file: token 2: a token name is 1 to 32 bytes of a-z, 0-9 and -",
        ),
        (
            entry("ci", &good),
            "--tokens-file: token 1: a token may not be named ci: the audit uses that name for \
             changes that are not an operator's",
        ),
        (
            entry("a", &"0".repeat(63)),
            "--tokens-file: token a: sha256 is not 64 lowercase hex digits",
        ),
        (
            entry("a", &format!("g{}", "0".repeat(63))),
            "--tokens-file: token a: sha256 is not 64 lowercase hex digits",
        ),
        (
            entry("a", &"A".repeat(64)),
            "--tokens-file: token a: sha256 is not 64 lowercase hex digits",
        ),
        (
            format!("{}{}", entry("a", &good), entry("a", &other)),
            "--tokens-file: the name a is used twice",
        ),
    ];
    for (n, (file, expected)) in exact.iter().enumerate() {
        let why = refused(&format!("tokens-rule-{n}"), file.as_bytes());
        assert_eq!(why.to_string(), *expected);
    }
    let parsed: Vec<(String, &str)> = vec![
        ("[[token]]\nname = \"a\"\n".to_owned(), "sha256"),
        (
            format!("{}colour = 1\n", entry("a", &good)),
            "unknown field `colour`",
        ),
        ("not toml [".to_owned(), "--tokens-file: "),
    ];
    for (n, (file, needle)) in parsed.iter().enumerate() {
        let why = refused(&format!("tokens-parse-{n}"), file.as_bytes());
        assert!(matches!(why, TokensError::Parse(_)));
        assert!(why.to_string().starts_with("--tokens-file: "));
        assert!(why.to_string().contains(needle));
    }
}

/// Proves: the file is read under the rule of a secret file (mode, existence, no symlink), is
/// at most 64 KiB and is UTF-8, each refusal naming the flag.
///
/// Catches: a tokens file that anyone can write, one read through a symlink, or an unbounded or
/// non-text file read whole.
#[test]
fn a_tokens_file_is_read_under_the_rule_of_a_secret_file() {
    let dir = scratch("tokens-secret");
    let loose = put(&dir, text(&[("alice", ALICE)]).as_bytes(), 0o644);
    let why = OperatorTokens::from_file(&loose).err().expect("refused");
    assert_eq!(
        why.to_string(),
        "--tokens-file: its mode must be 0600 or 0400"
    );
    assert!(matches!(why, TokensError::Read(SecretError::Rule { .. })));

    let missing = dir.path().join("absent.toml");
    let why = OperatorTokens::from_file(&missing).err().expect("refused");
    assert!(matches!(why, TokensError::Read(SecretError::Io { .. })));
    assert!(why.to_string().starts_with("--tokens-file: "));

    let long = put(&dir, "#".repeat(65_537).as_bytes(), 0o600);
    let why = OperatorTokens::from_file(&long).err().expect("refused");
    assert_eq!(why.to_string(), "--tokens-file: it is over 65536 bytes");
    assert!(matches!(why, TokensError::TooLong));

    let binary = put(&dir, &[0xff, 0xfe], 0o600);
    let why = OperatorTokens::from_file(&binary).err().expect("refused");
    assert_eq!(why.to_string(), "--tokens-file: it is not UTF-8");
    assert!(matches!(why, TokensError::NotText));

    let real = write(&dir, &[("alice", ALICE)]);
    let link = dir.path().join("link.toml");
    symlink(&real, &link).expect("a symlink");
    let why = OperatorTokens::from_file(&link).err().expect("refused");
    assert!(matches!(why, TokensError::Read(_)));
}

/// Proves: a token removed from the file or replaced in it stops working at the next look, a
/// new one works, and an operator action by the revoked token is refused, with no restart.
///
/// Catches: tokens read once at start, so a leaked token works until the coordinator restarts.
#[test]
fn a_revoked_token_is_refused_without_restart() {
    let dir = scratch("tokens-revoke");
    let path = write(&dir, &[("alice", ALICE), ("bob", BOB)]);
    let tokens = OperatorTokens::from_file(&path).expect("tokens");
    let c = coordinator(&dir, tokens);
    assert!(c.status(Some(ALICE)).is_ok());
    assert!(c.status(Some(BOB)).is_ok());

    write(&dir, &[("alice", ALICE2)]);
    assert_eq!(c.tokens().reload(), Reload::Applied);

    assert_eq!(c.status(Some(BOB)).err(), Some(Unauthorized));
    assert_eq!(c.status(Some(ALICE)).err(), Some(Unauthorized));
    assert!(c.status(Some(ALICE2)).is_ok());
    let denied = c.operate(
        Some(BOB),
        br#"{"action":"include","service":"kbf","node_id":"d1"}"#,
    );
    assert!(matches!(denied, Err(OperateError::Unauthorized(_))));
}

/// Proves: a tokens file that cannot be read again, or is refused, leaves the tokens in use and
/// is logged; the same file is `Unchanged`; a file with other tokens is `Applied`.
///
/// Catches: a truncated or deleted tokens file locking every operator out, or a reload that says
/// nothing.
#[test]
fn a_tokens_file_that_cannot_be_read_again_leaves_the_tokens_in_use() {
    let dir = scratch("tokens-keep");
    let log = Log::default();
    let _guard = log.install();
    let path = write(&dir, &[("alice", ALICE)]);
    let tokens = OperatorTokens::from_file(&path).expect("tokens");
    assert_eq!(tokens.reload(), Reload::Unchanged);

    put(&dir, b"garbage [", 0o600);
    assert_eq!(tokens.reload(), Reload::Rejected);
    assert_eq!(name_of(&tokens, ALICE), Some("alice".to_owned()));
    assert_eq!(
        log.text()
            .matches("the tokens file was not read again")
            .count(),
        1
    );

    fs::remove_file(&path).expect("removed");
    assert_eq!(tokens.reload(), Reload::Rejected);
    assert_eq!(name_of(&tokens, ALICE), Some("alice".to_owned()));
    assert_eq!(
        log.text()
            .matches("the tokens file was not read again")
            .count(),
        2
    );

    write(&dir, &[("bob", BOB)]);
    assert_eq!(tokens.reload(), Reload::Applied);
    assert_eq!(log.text().matches("the tokens file changed").count(), 1);
    assert_eq!(name_of(&tokens, ALICE), None);
    assert_eq!(name_of(&tokens, BOB), Some("bob".to_owned()));
}

/// Proves: two tokens with the same `sha256` are refused naming both, when the file is read at
/// start and when it is read again; a refused reload keeps the tokens in use, so each secret
/// still answers to the one name it had.
///
/// Catches: one secret answering to two names, so that the audit's `by` hangs on the order of
/// the file and removing one name leaves its secret working under the other.
#[test]
fn two_tokens_with_one_sha256_are_refused_at_start_and_on_reload() {
    let shared = format!(
        "{}{}",
        entry("alice", &digest(ALICE.as_bytes())),
        entry("bob", &digest(ALICE.as_bytes()))
    );
    let why = refused("tokens-shared-start", shared.as_bytes());
    assert_eq!(
        why.to_string(),
        "--tokens-file: tokens alice and bob have the same sha256"
    );
    assert!(matches!(why, TokensError::SharedDigest { .. }));

    let dir = scratch("tokens-shared-reload");
    let path = write(&dir, &[("alice", ALICE), ("bob", BOB)]);
    let tokens = OperatorTokens::from_file(&path).expect("tokens");
    put(&dir, shared.as_bytes(), 0o600);
    assert_eq!(tokens.reload(), Reload::Rejected);
    assert_eq!(name_of(&tokens, ALICE), Some("alice".to_owned()));
    assert_eq!(name_of(&tokens, BOB), Some("bob".to_owned()));
}

/// Proves: one token from `--operator-token-file` is named `operator` and is never read again.
///
/// Catches: the legacy flag giving a token another name than the one the audit has always
/// recorded, or a reload of a token that has no file.
#[test]
fn a_single_token_is_named_operator_and_never_read_again() {
    let single = OperatorTokens::single(ALICE);
    assert_eq!(name_of(&single, ALICE), Some("operator".to_owned()));
    assert_eq!(single.reload(), Reload::Unchanged);
    let from = OperatorTokens::from(ALICE.to_owned());
    assert_eq!(name_of(&from, ALICE), Some("operator".to_owned()));
    assert_eq!(from.reload(), Reload::Unchanged);
}

/// Proves: the halt the coordinator raises, then four operator actions by two tokens, are
/// recorded under `coordinator`, `alice`, `bob`, `alice`, `bob`: in the answers, in the ring the
/// status shows and in the audit file, numbered 1 to 5.
///
/// Catches: a hard-coded `operator` as the actor, or a name that reaches the answer but not the
/// ring or the file.
#[test]
fn every_action_is_audited_under_the_token_name() {
    let dir = scratch("tokens-audit");
    let path = write(&dir, &[("alice", ALICE), ("bob", BOB)]);
    let c = coordinator(&dir, OperatorTokens::from_file(&path).expect("tokens"));
    let mut halting = beat("1.0");
    halting.last_update = Some(
        serde_json::from_value(json!({
            "target": "2.0", "outcome": "failed", "phase": "halt", "reason": "disk_tainted"
        }))
        .expect("a report"),
    );
    let who = Identity {
        host_id: id("s1"),
        role: role("storage"),
    };
    c.heartbeat(who, halting).expect("a heartbeat");

    let actions = [
        (
            ALICE,
            r#"{"action":"exclude","service":"kbf","node_id":"d1","reason":"disk"}"#,
        ),
        (
            BOB,
            r#"{"action":"include","service":"kbf","node_id":"d1"}"#,
        ),
        (
            ALICE,
            r#"{"action":"set_labels","node_id":"d1","labels":{"zone":"a"}}"#,
        ),
        (
            BOB,
            r#"{"action":"clear_halt","service":"kbf","reason":"replaced"}"#,
        ),
    ];
    let answered: Vec<String> = actions
        .iter()
        .map(|&(token, body)| c.operate(Some(token), body.as_bytes()).expect("done").by)
        .collect();
    assert_eq!(answered, ["alice", "bob", "alice", "bob"]);

    let want = ["coordinator", "alice", "bob", "alice", "bob"];
    let ring = c.status(Some(ALICE)).expect("status").ops.audit;
    let ringed: Vec<&str> = ring.iter().map(|e| e.by.as_str()).collect();
    assert_eq!(ringed, want);

    let file = crate::audit_file::path_of(&dir.path().join("desired.json.ops"));
    let found = crate::audit_file::read(&file).expect("read");
    let filed: Vec<&str> = found.lines.iter().map(|l| l.by.as_str()).collect();
    assert_eq!(filed, want);
    let numbers: Vec<u64> = found.lines.iter().map(|l| l.seq).collect();
    assert_eq!(numbers, [1, 2, 3, 4, 5]);
}

/// Proves: only the exact token is an operator: no bearer, an empty one, the token plus a space,
/// without its first byte, in capitals, the name of the token, or CI's token each give
/// `Unauthorized`.
///
/// Catches: a prefix or case-insensitive match, or CI's token accepted as an operator's.
#[test]
fn a_near_miss_of_a_token_is_not_a_token() {
    let dir = scratch("tokens-near");
    let path = write(&dir, &[("alice", ALICE)]);
    let c = coordinator(&dir, OperatorTokens::from_file(&path).expect("tokens"));
    let name = c.operator(Some(ALICE)).expect("a token");
    assert_eq!(name.as_str(), "alice");
    let spaced = format!("{ALICE} ");
    let capitals = ALICE.to_uppercase();
    let ci = ci_token();
    let misses = [
        None,
        Some(""),
        Some(spaced.as_str()),
        Some(&ALICE[1..]),
        Some(capitals.as_str()),
        Some("alice"),
        Some(ci.as_str()),
    ];
    for miss in misses {
        assert_eq!(c.operator(miss), Err(Unauthorized));
    }
}
