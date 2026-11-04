//! The allow-list: an entry lets the reads of its path pass and no other, and an entry that is
//! stale, unused, without its reason or not a plain relative path is refused.

use crate::{lint, refusal, st_now};

/// Catches: an allow-list matched globally instead of by path, a directory entry matching a
/// sibling whose name merely starts with it, and a file entry matching more than its file.
#[test]
fn an_allow_listed_path_passes() {
    let read: &str = &format!("fn valid() {{ {}; }}\n", st_now());
    let allowed = "# certificate validity\ncrates/tls/src/cert.rs\n\
                   # the OIDC verifier\ncrates/oidc/src/verify/\n";
    let listed = [
        ("crates/tls/src/cert.rs", read),
        ("crates/oidc/src/verify/token.rs", read),
        ("crates/oidc/src/verify/keys/fetch.rs", read),
    ];
    assert_eq!(lint(&listed, allowed), Ok(()));
    let mut siblings = listed.to_vec();
    siblings.extend([
        ("crates/tls/src/decide.rs", read),
        ("crates/tls/src/cert.rs.bak/x.rs", read),
        ("crates/oidc/src/verify_more.rs", read),
    ]);
    assert_eq!(
        lint(&siblings, allowed),
        Err(vec![
            refusal("crates/oidc/src/verify_more.rs", 1, "SystemTime::now"),
            refusal("crates/tls/src/cert.rs.bak/x.rs", 1, "SystemTime::now"),
            refusal("crates/tls/src/decide.rs", 1, "SystemTime::now"),
        ])
    );
}

/// Catches: an entry kept after its file was removed or renamed, or that names a file the scan
/// does not read (a test file), whether a file or a directory entry.
#[test]
fn an_allow_list_entry_for_a_missing_path_fails() {
    let allowed = "# certificate validity\ncrates/gone/src/cert.rs\n# nonce\ncrates/gone/\n\
                   # a test file is not scanned\ncrates/demo/tests/it.rs\n";
    assert_eq!(
        lint(&[("crates/demo/tests/it.rs", "")], allowed),
        Err(vec![
            "stale allow-list entry: crates/gone/src/cert.rs matches no file".to_owned(),
            "stale allow-list entry: crates/gone/ matches no file".to_owned(),
            "stale allow-list entry: crates/demo/tests/it.rs matches no file".to_owned(),
        ])
    );
}

/// Catches: an entry kept after its read moved away, which would let a later read in that path
/// pass unreviewed; for a file entry and for a directory entry.
#[test]
fn an_allow_list_entry_with_no_read_fails() {
    let allowed = "# certificate validity\ncrates/demo/src/lib.rs\n# nonce\ncrates/demo/src/n/\n";
    let quiet = "fn f() { let a = std::time::Instant::now(); }\n";
    assert_eq!(
        lint(
            &[
                ("crates/demo/src/lib.rs", quiet),
                ("crates/demo/src/n/a.rs", quiet)
            ],
            allowed
        ),
        Err(vec![
            "unused allow-list entry: crates/demo/src/lib.rs has no wall-clock read".to_owned(),
            "unused allow-list entry: crates/demo/src/n/ has no wall-clock read".to_owned(),
        ])
    );
}

/// Catches: an allow-list entry let in without its reason (a reason serves one entry only),
/// twice, outside the repository or not as a plain relative path (a space or a tab inside it
/// too); each refused with its line. CRLF line ends and a missing final newline are accepted.
#[test]
fn an_allow_list_entry_without_a_reason_fails() {
    let read: &str = &format!("fn valid() {{ {}; }}\n", st_now());
    let files = [("crates/tls/src/cert.rs", read)];
    let cases = [
        (
            "crates/tls/src/cert.rs\n",
            "allowed.txt:1: crates/tls/src/cert.rs has no `#` line above it saying why it may read the wall clock",
        ),
        (
            "# certificate validity\n\ncrates/tls/src/cert.rs\n",
            "allowed.txt:3: crates/tls/src/cert.rs has no `#` line above it saying why it may read the wall clock",
        ),
        (
            "#\ncrates/tls/src/cert.rs\n",
            "allowed.txt:2: crates/tls/src/cert.rs has no `#` line above it saying why it may read the wall clock",
        ),
        (
            "# a\ncrates/tls/src/cert.rs\n# b\ncrates/tls/src/cert.rs\n",
            "allowed.txt:4: crates/tls/src/cert.rs is listed twice (first on line 2)",
        ),
        (
            "# a\n/crates/tls/src/cert.rs\n",
            "allowed.txt:2: /crates/tls/src/cert.rs is absolute; give it from the repository root",
        ),
        (
            "# a\ncrates/tls/../tls/src/cert.rs\n",
            "allowed.txt:2: crates/tls/../tls/src/cert.rs has a `..` segment",
        ),
        (
            "# a\ncrates/tls/./src/\n",
            "allowed.txt:2: crates/tls/./src/ is not a plain relative path (forward slashes, no `.`, empty segment or space)",
        ),
        (
            "# a\ncrates//tls/src/\n",
            "allowed.txt:2: crates//tls/src/ is not a plain relative path (forward slashes, no `.`, empty segment or space)",
        ),
        (
            "# a\ncrates\\tls\\src\\cert.rs\n",
            "allowed.txt:2: crates\\tls\\src\\cert.rs is not a plain relative path (forward slashes, no `.`, empty segment or space)",
        ),
        (
            "# a\ncrates/tls/src/cert.rs \n",
            "allowed.txt:2: trailing whitespace",
        ),
        (
            "# a \ncrates/tls/src/cert.rs\n",
            "allowed.txt:1: trailing whitespace",
        ),
        (
            "# a\ncrates/tls/src/ce rt.rs\n",
            "allowed.txt:2: crates/tls/src/ce rt.rs is not a plain relative path (forward slashes, no `.`, empty segment or space)",
        ),
        (
            "# a\ncrates/tls/src/ce\trt.rs\n",
            "allowed.txt:2: crates/tls/src/ce\trt.rs is not a plain relative path (forward slashes, no `.`, empty segment or space)",
        ),
        (
            "# one reason\ncrates/tls/src/cert.rs\ncrates/tls/src/other.rs\n",
            "allowed.txt:3: crates/tls/src/other.rs has no `#` line above it saying why it may read the wall clock",
        ),
    ];
    for (allowed, want) in cases {
        assert_eq!(
            lint(&files, allowed),
            Err(vec![want.to_owned()]),
            "in {allowed:?}"
        );
    }
    assert_eq!(
        lint(&files, "# no reason line below\n/x\n"),
        Err(vec![
            "allowed.txt:2: /x is absolute; give it from the repository root".to_owned()
        ])
    );
    assert_eq!(
        lint(&files, "/x\n"),
        Err(vec![
            "allowed.txt:1: /x has no `#` line above it saying why it may read the wall clock"
                .to_owned(),
            "allowed.txt:1: /x is absolute; give it from the repository root".to_owned(),
        ])
    );
    for allowed in [
        "# certificate validity\r\ncrates/tls/src/cert.rs\r\n",
        "# certificate validity\ncrates/tls/src/cert.rs",
    ] {
        assert_eq!(lint(&files, allowed), Ok(()), "in {allowed:?}");
    }
}

/// Catches: a scan that stops at the first read of a file, reads on one line reported once, an
/// allow-listed read reported, or the entry checks dropped when reads are refused; the whole
/// list comes in order: the reads by file and line, then the entries by line.
#[test]
fn every_read_and_entry_error_is_reported_in_order() {
    let now = st_now();
    let decide = format!("fn a() {{ {now}; {now}; }}\nfn b() {{ chrono::Utc::now(); }}\n");
    let cert = format!("fn valid() {{ {now}; }}\n");
    let files = [
        ("crates/demo/src/decide.rs", decide.as_str()),
        ("crates/demo/src/quiet.rs", "fn q() {}\n"),
        ("crates/tls/src/cert.rs", cert.as_str()),
    ];
    let allowed = "# certificate validity\ncrates/tls/src/cert.rs\n# gone\ncrates/gone/x.rs\n\
                   # quiet\ncrates/demo/src/quiet.rs\n";
    let decide_path = "crates/demo/src/decide.rs";
    assert_eq!(
        lint(&files, allowed),
        Err(vec![
            refusal(decide_path, 1, "SystemTime::now"),
            refusal(decide_path, 1, "SystemTime::now"),
            refusal(decide_path, 2, "Utc::now"),
            "stale allow-list entry: crates/gone/x.rs matches no file".to_owned(),
            "unused allow-list entry: crates/demo/src/quiet.rs has no wall-clock read".to_owned(),
        ])
    );
}
