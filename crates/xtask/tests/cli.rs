//! The `xtask` program end to end: each verb is run as a process and its exit status and words
//! are read. These tests hold the dispatch of the verbs (which flag reaches which function), the
//! exit statuses (0, 1 for a failure, 2 for a usage error) and the words of an error; the logic
//! of each verb is tested beside it, in the library.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run xtask")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn fetch_with_extract_and_no_member_is_a_usage_error_with_status_2() {
    // Catches: flags that do not fit together reported as a failure (status 1), or accepted.
    let d = dir("usage");
    let o = run(
        &d,
        &[
            "fetch",
            "--url",
            "https://example.invalid/tool.tar.gz",
            "--sha256",
            &"0".repeat(64),
            "--out",
            "tool",
            "--extract",
            "tar.gz",
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(
        stderr(&o),
        "xtask: --extract tar.gz needs --member, the file to take\n"
    );
    assert!(!d.join("tool").exists());
}

#[test]
fn fetch_with_a_malformed_sha256_is_refused_by_the_flag_parser() {
    // Catches: a short or upper-case pin accepted, which pins nothing.
    let d = dir("bad-sha");
    let o = run(
        &d,
        &[
            "fetch",
            "--url",
            "https://example.invalid/tool",
            "--sha256",
            &"A".repeat(64),
            "--out",
            "tool",
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("not a SHA-256"), "{}", stderr(&o));
}

#[test]
fn fetch_over_plain_http_is_a_failure_with_status_1_and_installs_nothing() {
    // Catches: the verb not reaching `fetch`'s HTTPS rule, or the failure exiting 0. The rule
    // refuses before any name is looked up or any connection made, so the URL needs no server.
    let d = dir("http");
    let o = run(
        &d,
        &[
            "fetch",
            "--url",
            "http://example.invalid/tool",
            "--sha256",
            &"0".repeat(64),
            "--out",
            "tool",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(
        stderr(&o),
        format!(
            "xtask: download http://example.invalid/tool: {}\n",
            xtask::fetch::PLAIN_HTTP
        )
    );
    assert!(!d.join("tool").exists());
}
