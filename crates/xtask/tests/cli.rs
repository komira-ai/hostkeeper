//! The `xtask` program end to end: each verb is run as a process and its exit status, words and
//! effects are read. These tests hold the dispatch of the verbs (which flag reaches which
//! function), the exit statuses (0, 1 for a failure, 2 for a usage error) and the words of an
//! error; the logic of each verb is tested beside it, in the library.

#[path = "../src/testdir.rs"]
mod testdir;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use sha2::Digest;
use testdir::TestDir;

const TOOL: &[u8] = b"\x7fELF a pinned tool's bytes";

fn dir(name: &str) -> TestDir {
    TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("cli-{name}"),
    )
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run xtask")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

/// A gzip tar holding `TOOL` at `tool-1.0/tool`.
fn tar_gz() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_mode(0o644);
    h.set_size(TOOL.len() as u64);
    b.append_data(&mut h, "tool-1.0/tool", TOOL).expect("file");
    let tar = b.into_inner().expect("tar");
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(&tar).expect("gzip");
    e.finish().expect("gzip")
}

fn assert_installed(out: &Path) {
    assert_eq!(std::fs::read(out).expect("installed"), TOOL);
    let mode = std::fs::metadata(out).expect("stat").permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
}

#[test]
fn install_tool_installs_a_raw_download_with_status_0() {
    // Catches: `--download`, `--sha256` or `--out` not reaching the install, a non-zero status
    // for a good install, or no line saying what was installed.
    let d = dir("raw");
    std::fs::write(d.join("tool.download"), TOOL).expect("download");
    let digest = sha256(TOOL);
    let o = run(
        &d,
        &[
            "install-tool",
            "--download",
            "tool.download",
            "--sha256",
            &digest,
            "--out",
            "bin/tool",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        format!("ok: tool.download has SHA-256 {digest}; installed bin/tool\n")
    );
    assert_installed(&d.join("bin/tool"));
}

#[test]
fn install_tool_takes_the_member_out_of_a_tar_gz_with_status_0() {
    // Catches: `--extract` or `--member` not reaching the install (the archive itself would be
    // installed, or nothing).
    let d = dir("tar-gz");
    let archive = tar_gz();
    std::fs::write(d.join("tool.tar.gz"), &archive).expect("download");
    let o = run(
        &d,
        &[
            "install-tool",
            "--download",
            "tool.tar.gz",
            "--sha256",
            &sha256(&archive),
            "--out",
            "tool",
            "--extract",
            "tar.gz",
            "--member",
            "tool-1.0/tool",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_installed(&d.join("tool"));
}

#[test]
fn install_tool_with_extract_and_no_member_is_a_usage_error_with_status_2() {
    // Catches: flags that do not fit together reported as a failure (status 1), or accepted.
    let d = dir("usage");
    let o = run(
        &d,
        &[
            "install-tool",
            "--download",
            "tool.tar.gz",
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
fn install_tool_with_a_malformed_sha256_is_refused_by_the_flag_parser() {
    // Catches: a short or upper-case pin accepted, which pins nothing.
    let d = dir("bad-sha");
    let o = run(
        &d,
        &[
            "install-tool",
            "--download",
            "tool",
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
fn install_tool_with_a_wrong_sha256_is_a_failure_with_status_1_and_installs_nothing() {
    // Catches: the check not reached through the verb, or its failure exiting 0.
    let d = dir("mismatch");
    std::fs::write(d.join("tool.download"), TOOL).expect("download");
    let pinned = "0".repeat(64);
    let o = run(
        &d,
        &[
            "install-tool",
            "--download",
            "tool.download",
            "--sha256",
            &pinned,
            "--out",
            "tool",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(
        stderr(&o),
        format!(
            "xtask: the download's SHA-256 is {}, not the pinned {pinned}; nothing was installed\n",
            sha256(TOOL)
        )
    );
    assert!(!d.join("tool").exists());
}
