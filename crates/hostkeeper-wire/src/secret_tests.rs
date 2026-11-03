//! The secret file rule: a regular file of this user with mode `0600` or `0400`, never followed
//! through a symlink, and an error that names the flag and the rule, never the path or the
//! content.

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_testkit::Scratch;

use crate::secret::{SecretError, TOKEN_FLAG, euid, file_rule, read_secret, read_token};

const TOKEN: &str = "tok-0123456789abcdef0123456789abcdef";

fn scratch(name: &str) -> Scratch {
    let base = std::env::current_exe()
        .expect("the test binary")
        .parent()
        .expect("its directory")
        .join("hostkeeper-wire-unit");
    Scratch::under(&base, name).expect("a scratch directory")
}

fn file(dir: &Path, name: &str, text: &str, mode: u32) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
}

/// Catches a token file accepted that the file rule refuses: wrong mode, a symlink, a
/// directory, too short, a byte that could split a header, empty, or too long; and an error
/// text that holds the secret or the file's path (it names the flag).
#[test]
fn the_token_file_is_held_to_the_file_rule() {
    let scratch = scratch("tokrule");
    let dir = scratch.path();
    let good = file(dir, "good", &format!("  {TOKEN}\n\n"), 0o400);
    assert_eq!(read_token(&good).as_deref(), Ok(TOKEN));
    let readable = file(dir, "r", TOKEN, 0o644);
    let short = file(dir, "short", "tok-short", 0o600);
    let split = file(dir, "split", &format!("{TOKEN}\r\nX-Evil: 1"), 0o600);
    let inner = file(dir, "inner", &format!("{TOKEN} {TOKEN}"), 0o600);
    let empty = file(dir, "empty", "  \n", 0o600);
    let long = file(dir, "long", &"t".repeat(4097), 0o600);
    let exact = file(dir, "exact", &"t".repeat(4096), 0o600);
    // Bytes that are not text are an error, not a token.
    let binary = dir.join("binary");
    std::fs::write(&binary, [0xff_u8; 40]).expect("write");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let link = dir.join("link");
    symlink(&good, &link).expect("symlink");
    for (path, why) in [
        (&readable, "mode must be 0600 or 0400"),
        (&short, "32 to 4096"),
        (&split, "32 to 4096"),
        (&inner, "32 to 4096"),
        (&empty, "32 to 4096"),
        (&long, "32 to 4096"),
        (&dir.join("missing"), "not found"),
        (&link, "symbolic"),
        (&dir.to_path_buf(), "not a regular file"),
        (&binary, "32 to 4096"),
    ] {
        let e = read_token(path).expect_err(why).to_string();
        assert!(e.starts_with("--token-file: "), "{e}");
        assert!(!e.contains(&path.display().to_string()), "path in {e}");
        assert!(!e.contains("X-Evil") && !e.contains(TOKEN), "{e}");
        let ok = e.to_lowercase().contains(&why.to_lowercase()) || why == "symbolic";
        assert!(ok, "{why} not in {e}");
    }
    assert_eq!(read_token(&exact).map(|t| t.len()), Ok(4096));
    assert_eq!(TOKEN_FLAG, "--token-file");
}

/// Catches a token cached for ever: the file is read on every call, so a rotated token needs no
/// restart.
#[test]
fn the_token_is_read_on_every_call() {
    let scratch = scratch("tokread");
    let path = file(scratch.path(), "token", TOKEN, 0o600);
    assert_eq!(read_token(&path).as_deref(), Ok(TOKEN));
    let rotated = "rotated-0123456789abcdef0123456789abcdef";
    std::fs::write(&path, rotated).expect("rewrite");
    assert_eq!(read_token(&path).as_deref(), Ok(rotated));
}

/// Catches a secret read past its limit, or cut at it: `read_secret` returns the file's bytes
/// and one more than `limit` at most, so that a caller sees a file that is too long; and an
/// error names the flag it is given.
#[test]
fn a_secret_is_read_up_to_one_byte_past_the_limit() {
    let scratch = scratch("secret");
    let path = file(scratch.path(), "s", "0123456789", 0o600);
    assert_eq!(read_secret(&path, "--x", 100), Ok(b"0123456789".to_vec()));
    assert_eq!(read_secret(&path, "--x", 10), Ok(b"0123456789".to_vec()));
    assert_eq!(read_secret(&path, "--x", 4), Ok(b"01234".to_vec()));
    let open = file(scratch.path(), "open", "x", 0o666);
    let e = read_secret(&open, "--services-file", 10)
        .expect_err("mode")
        .to_string();
    assert!(e.starts_with("--services-file: "), "{e}");
}

/// Catches an overflow at the extremes of the limit: `0` still reads one byte, so a non-empty
/// file is seen to be too long, and `u64::MAX` reads the whole file instead of overflowing.
#[test]
fn a_secret_limit_of_zero_or_u64_max_does_not_overflow() {
    let scratch = scratch("secretedge");
    let path = file(scratch.path(), "s", "0123456789", 0o600);
    assert_eq!(read_secret(&path, "--x", 0), Ok(b"0".to_vec()));
    assert_eq!(
        read_secret(&path, "--x", u64::MAX),
        Ok(b"0123456789".to_vec())
    );
}

/// Catches `euid` reading another user than the one that owns the files this process makes.
#[test]
fn the_effective_user_owns_the_files_it_makes() {
    let scratch = scratch("euid");
    let path = file(scratch.path(), "mine", "x", 0o600);
    let meta = std::fs::metadata(&path).expect("metadata");
    assert_eq!(meta.uid(), euid());
}

/// Catches the file rule letting through a file of another user, a device or a directory, or a
/// mode wider than 0600/0400 or with a special bit; a second uid needs root, so the rule is
/// checked with fakes.
#[test]
fn the_file_rule_wants_a_regular_file_of_mine_with_mode_0600_or_0400() {
    assert_eq!(file_rule(true, 7, 0o100_600, 7), Ok(()));
    assert_eq!(file_rule(true, 7, 0o100_400, 7), Ok(()));
    assert!(
        file_rule(false, 7, 0o600, 7)
            .expect_err("dir")
            .contains("regular")
    );
    assert!(
        file_rule(true, 8, 0o600, 7)
            .expect_err("owner")
            .contains("owned")
    );
    assert!(
        file_rule(true, 0, 0o600, 7)
            .expect_err("root's")
            .contains("owned")
    );
    for mode in [
        0o644, 0o660, 0o640, 0o700, 0o4600, 0o1600, 0o200, 0o000, 0o604,
    ] {
        assert!(
            file_rule(true, 7, mode, 7)
                .expect_err("mode")
                .contains("mode"),
            "{mode:o}"
        );
    }
}

/// Catches an error text that drops the flag or names more than the rule: each kind of refusal
/// reads, in full, as the flag and the rule it breaks.
#[test]
fn each_refusal_reads_as_the_flag_and_the_rule() {
    let io = SecretError::Io {
        flag: "--x".to_owned(),
        kind: std::io::ErrorKind::NotFound,
    };
    assert_eq!(io.to_string(), "--x: entity not found");
    let rule = SecretError::Rule {
        flag: "--x".to_owned(),
        why: "its mode must be 0600 or 0400",
    };
    assert_eq!(rule.to_string(), "--x: its mode must be 0600 or 0400");
    let token = SecretError::Token {
        flag: TOKEN_FLAG.to_owned(),
    };
    assert_eq!(
        token.to_string(),
        "--token-file: it must hold one token of 32 to 4096 visible ASCII characters"
    );
}
