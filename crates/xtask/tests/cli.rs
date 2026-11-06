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

// The release verbs run here read no port: a verb that reads git or GitHub is tested in the
// library over fakes, never through the binary, which would reach the real programs.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn release_stamp_prints_the_stamp_with_status_0() {
    // Catches: `--version` or `--commit` not reaching the stamp, a stamp that is not the text a
    // built binary prints, or a non-zero status for a verb that reads no port.
    let d = dir("stamp");
    let o = run(
        &d,
        &["release", "stamp", "--version", "0.1.2", "--commit", COMMIT],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "hostkeeper 0.1.2+0123456789ab\n");
}

#[test]
fn release_stamp_with_a_two_number_version_is_a_usage_error_with_status_2() {
    // Catches: a version that is not X.Y.Z accepted, or reported as a failure (status 1).
    let d = dir("stamp-usage");
    let o = run(
        &d,
        &["release", "stamp", "--version", "1.2", "--commit", COMMIT],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
}

#[test]
fn release_compare_images_of_missing_files_is_a_failure_with_status_1() {
    // Catches: unreadable images compared as equal, or reported with a status other than 1.
    let d = dir("compare-missing");
    let o = run(&d, &["release", "compare-images", "a.tar", "b.tar"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).starts_with("xtask: read "), "{}", stderr(&o));
}

#[test]
fn release_check_image_ui_of_a_missing_image_is_a_failure_with_status_1() {
    // Catches: an unreadable image reported as one that holds the web app, or with a status
    // other than 1.
    let d = dir("check-image-ui-missing");
    let o = run(&d, &["release", "check-image-ui", "image.tar", "ui"]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).starts_with("xtask: read image.tar: "),
        "{}",
        stderr(&o)
    );
    assert_eq!(stdout(&o), "");
}

#[test]
fn release_set_version_writes_the_version_into_the_workspace_with_status_0() {
    // Catches: `--version` or `--root` not reaching set-version, or the workspace manifest left
    // at its old version.
    let d = dir("set-version");
    std::fs::create_dir_all(d.join("crates/a")).expect("member directory");
    std::fs::write(
        d.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = \"0.1.0\"\n",
    )
    .expect("root manifest");
    std::fs::write(
        d.join("crates/a/Cargo.toml"),
        "[package]\nname = \"a\"\nversion.workspace = true\n",
    )
    .expect("member manifest");
    std::fs::write(
        d.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\n",
    )
    .expect("lock file");
    let root = d.to_str().expect("a UTF-8 path");
    let o = run(
        &d,
        &[
            "release",
            "set-version",
            "--version",
            "0.1.9",
            "--root",
            root,
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let manifest = std::fs::read_to_string(d.join("Cargo.toml")).expect("manifest");
    assert!(manifest.contains("version = \"0.1.9\""), "{manifest}");
    let lock = std::fs::read_to_string(d.join("Cargo.lock")).expect("lock");
    assert!(lock.contains("version = \"0.1.9\""), "{lock}");
}

/// The package of a one-platform release of tierstore 1.5.0.
const STAGED_PACKAGE: &str = "tierstore-1.5.0-linux-x86_64.tar.zst";

/// The argv of `release channel-stage` of tierstore 1.5.0 from `from/` into `out/`, with
/// `extra` after it.
fn channel_stage<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    [
        "release",
        "channel-stage",
        "--service",
        "tierstore",
        "--version",
        "1.5.0",
        "--from",
        "from",
        "--out",
        "out",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .collect()
}

/// Writes into `d/from` the release of tierstore 1.5.0 for linux-x86_64 alone: `package`, an
/// unsigned bundle whose statement lists its SHA-256, and the canonical index.
fn channel_release(d: &Path, package: &[u8]) {
    use base64::Engine as _;
    let from = d.join("from");
    std::fs::create_dir_all(&from).expect("from/");
    std::fs::write(from.join(STAGED_PACKAGE), package).expect("the package");
    let digest = sha256(package);
    let statement = format!(r#"{{"subject":[{{"digest":{{"sha256":"{digest}"}}}}]}}"#);
    let payload = base64::engine::general_purpose::STANDARD.encode(statement);
    std::fs::write(
        from.join(format!("{STAGED_PACKAGE}.sigstore.json")),
        format!(r#"{{"dsseEnvelope":{{"payload":"{payload}"}}}}"#),
    )
    .expect("the bundle");
    let index = format!(
        r#"{{"packages":[{{"bytes":{},"file":"{STAGED_PACKAGE}","platform":"linux-x86_64","sha256":"{digest}"}}],"schema":1,"service":"tierstore","version":"1.5.0"}}"#,
        package.len()
    );
    std::fs::write(from.join("index.json"), index).expect("the index");
}

#[test]
fn release_channel_stage_stages_a_release_once_and_then_changes_nothing_with_status_0() {
    // Catches: a flag not reaching the verb, the tree not where the channel serves it, a rerun
    // that is not a no-op, or a non-zero status for a staged release.
    let d = dir("channel-stage");
    channel_release(&d, b"a package");
    let argv = channel_stage(&["--require-platforms", "linux-x86_64"]);
    let o = run(&d, &argv);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let at = format!("tierstore/1.5.0/{STAGED_PACKAGE}");
    assert_eq!(
        stdout(&o),
        format!(
            "staged {at}\nstaged {at}.sigstore.json\nstaged tierstore/1.5.0/index.json\n\
             out/tierstore/1.5.0: the index and the packages of linux-x86_64 with their \
             bundles; 3 of 3 files written\n"
        )
    );
    assert_eq!(
        std::fs::read(d.join("out").join(&at)).expect("the staged package"),
        b"a package"
    );
    let again = run(&d, &argv);
    assert_eq!(again.status.code(), Some(0), "{}", stderr(&again));
    assert!(
        stdout(&again).ends_with("; 0 of 3 files written\n"),
        "{}",
        stdout(&again)
    );
}

#[test]
fn release_channel_sync_without_its_channel_or_trust_list_fails_with_status_1_and_bad_flags_status_2()
 {
    // Catches: a missing channel directory made, synced into or reported as success, or a
    // missing trust list read as an empty one (the request is refused before GitHub is asked
    // anything), or a bad flag read as a failure.
    let d = dir("channel-sync");
    std::fs::write(
        d.join("trust.toml"),
        "[[service]]\nname = \"tierstore\"\nrepository = \"komira-ai/tierstore\"\n\
         workflow = \".github/workflows/artifacts.yml\"\nrefs = [\"refs/heads/main\"]\n\
         hosted_runner = true\n",
    )
    .expect("write the trust list");
    let o = run(
        &d,
        &[
            "release",
            "channel-sync",
            "--trust-list",
            "trust.toml",
            "--channel-dir",
            "missing",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).starts_with("xtask: --channel-dir missing: "),
        "{}",
        stderr(&o)
    );
    assert!(!d.join("missing").exists());
    let o = run(
        &d,
        &[
            "release",
            "channel-sync",
            "--trust-list",
            "no-such.toml",
            "--channel-dir",
            ".",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).starts_with("xtask: --trust-list no-such.toml: "),
        "{}",
        stderr(&o)
    );
    for bad in [
        ["--repo", "komira-ai/tierstore", "--channel-dir", "."],
        ["--since", "0.1.0", "--channel-dir", "."],
        ["--every-secs", "0", "--channel-dir", "."],
    ] {
        let mut argv = vec!["release", "channel-sync", "--trust-list", "trust.toml"];
        argv.extend(bad);
        let o = run(&d, &argv);
        assert_eq!(o.status.code(), Some(2), "{bad:?}: {}", stderr(&o));
        assert_eq!(stdout(&o), "");
    }
}

#[test]
fn release_channel_stage_refusals_are_failures_with_status_1_and_bad_names_status_2() {
    // Catches: a rebuilt package written over a staged one, a release lacking a required
    // platform staged, or a name outside the daemon's grammar reported as a failure (status 1)
    // rather than a usage error.
    let d = dir("channel-stage-refused");
    channel_release(&d, b"a package");
    let o = run(&d, &channel_stage(&[]));
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(
        stderr(&o),
        "xtask: no_package_for_platform: the release index has no package for linux-arm64, \
         which --require-platforms names\nno_package_for_platform: the release index has no \
         package for macos-arm64, which --require-platforms names\n"
    );
    assert!(!d.join("out").exists());

    let argv = channel_stage(&["--require-platforms", "linux-x86_64"]);
    assert_eq!(run(&d, &argv).status.code(), Some(0));
    channel_release(&d, b"a rebuilt package");
    let o = run(&d, &argv);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).starts_with(&format!(
            "xtask: out/tierstore/1.5.0/{STAGED_PACKAGE}: already staged with other bytes"
        )),
        "{}",
        stderr(&o)
    );
    let staged = d.join("out/tierstore/1.5.0").join(STAGED_PACKAGE);
    assert_eq!(
        std::fs::read(staged).expect("the staged package"),
        b"a package"
    );

    let mut bad = channel_stage(&[]);
    bad[3] = "tier+store";
    let o = run(&d, &bad);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
}

#[test]
fn advisories_reaches_cargo_deny_first_and_fails_with_status_1_when_it_cannot_start() {
    // Catches: `advisories` not dispatched to the verb, a GitHub call made before the check
    // ran, a cargo that cannot start reported as "no advisories" (status 0, which would close
    // the issue), and a bad `--repo` accepted. The PATH holds no program, so nothing is run.
    let d = dir("advisories");
    let empty = d.join("empty-path");
    std::fs::create_dir(&empty).expect("an empty PATH directory");
    let o = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(&d)
        .env("PATH", &empty)
        .args(["advisories", "--repo", "komira-ai/hostkeeper"])
        .output()
        .expect("run xtask");
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(
        stderr(&o),
        "xtask: cargo deny --log-level warn --format json --manifest-path ./Cargo.toml \
         --all-features check advisories: No such file or directory (os error 2)\n"
    );

    let o = run(&d, &["advisories", "--repo", "not a repository"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
}
