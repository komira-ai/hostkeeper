//! `xtask repository check` end to end: the verb is run as a process on a tree in a directory and
//! its exit status and words are read. The rules of the check are tested beside it, in the
//! library; these tests hold the dispatch of the flags and the exit statuses (1 for a repository
//! that is not valid, 2 for a usage error).

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::Path;
use std::process::{Command, Output};

use testdir::TestDir;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(args)
        .output()
        .expect("run xtask")
}

/// A tree of a repository with one apt channel, `stable`, whose keyring is a single version 4 key
/// packet with the fingerprint `fingerprint`.
fn tree(d: &Path, fingerprint: &str) {
    std::fs::create_dir_all(d.join("apt/dists/stable")).expect("dirs");
    std::fs::write(d.join("apt/dists/stable/InRelease"), b"signed").expect("InRelease");
    let mut body = vec![4, 0, 0, 0, 1, 22];
    body.extend([7_u8; 8]);
    let mut keyring = vec![0xc0 | 6, u8::try_from(body.len()).expect("short")];
    keyring.extend(body);
    std::fs::write(d.join("apt/key.gpg"), keyring).expect("keyring");
    std::fs::write(
        d.join("hostkeeper-repository.json"),
        format!(
            r#"{{"version":1,"channels":["stable"],"key":{{"keyring":"apt/key.gpg","fingerprints":["{fingerprint}"]}},"formats":{{"apt":{{"path":"apt","components":["main"]}}}}}}"#
        ),
    )
    .expect("descriptor");
}

/// Proves: the verb exits 1 and names the unmet claim for a descriptor whose fingerprint the
/// keyring lacks, and exits 2 for a path that is no directory and no https URL, and for a URL
/// given without a directory for the downloads. (The line a valid tree prints and its exit 0 are
/// tested in the library, `repository/tests.rs`.)
///
/// Catches: a verb whose exit status does not say whether a publisher may publish.
#[test]
fn repository_check_exits_1_for_an_invalid_tree_and_2_for_a_misuse() {
    let beside = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "repository-cli");
    let bad = beside.join("bad");
    tree(&bad, &"0".repeat(40));
    let wrong = run(&["repository", "check", bad.to_str().expect("utf-8")]);
    assert_eq!(wrong.status.code(), Some(1));
    let said = String::from_utf8_lossy(&wrong.stderr).into_owned();
    assert!(
        said.contains("the descriptor claims the fingerprint 000"),
        "{said}"
    );

    let missing = beside.join("absent");
    let misuse = run(&["repository", "check", missing.to_str().expect("utf-8")]);
    assert_eq!(misuse.status.code(), Some(2));
    let url = run(&["repository", "check", "https://repository.invalid"]);
    assert_eq!(url.status.code(), Some(2));
}
