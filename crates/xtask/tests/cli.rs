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
fn install_tool_decodes_a_zst_download_with_status_0() {
    // Catches: `--extract zst` not reaching the install (the compressed bytes would be installed),
    // or a status other than 0 for a good zstd download, the way buck2's releases ship.
    let d = dir("zst");
    let archive =
        ruzstd::encoding::compress_to_vec(TOOL, ruzstd::encoding::CompressionLevel::Fastest);
    std::fs::write(d.join("buck2.zst"), &archive).expect("download");
    let digest = sha256(&archive);
    let o = run(
        &d,
        &[
            "install-tool",
            "--download",
            "buck2.zst",
            "--sha256",
            &digest,
            "--out",
            "bin/buck2",
            "--extract",
            "zst",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        format!("ok: buck2.zst has SHA-256 {digest}; installed bin/buck2\n")
    );
    assert_installed(&d.join("bin/buck2"));
}

#[test]
fn install_tool_with_zst_and_a_member_is_a_usage_error_with_status_2() {
    // Catches: a `--member` silently ignored for a zstd download, which holds one file and no
    // members, or the misfit reported as a failure (status 1).
    let d = dir("zst-member");
    let o = run(
        &d,
        &[
            "install-tool",
            "--download",
            "buck2.zst",
            "--sha256",
            &"0".repeat(64),
            "--out",
            "buck2",
            "--extract",
            "zst",
            "--member",
            "buck2",
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(
        stderr(&o),
        "xtask: --extract zst is one compressed file: it takes no --member\n"
    );
    assert!(!d.join("buck2").exists());
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

#[test]
fn the_removed_release_verbs_are_usage_errors_with_status_2() {
    // Catches: a verb of the archive package, the release indexes or the release channel still
    // taken by the binary (a workflow step calling it would run, or fail for another reason than
    // that the verb is gone).
    let d = dir("removed-verbs");
    for verb in [
        "package",
        "index",
        "channel-stage",
        "channel-sync",
        "channel-host",
    ] {
        let o = run(&d, &["release", verb]);
        assert_eq!(o.status.code(), Some(2), "{verb}: {}", stderr(&o));
        assert_eq!(stdout(&o), "", "{verb}");
        assert!(
            stderr(&o).contains("unrecognized subcommand"),
            "{}",
            stderr(&o)
        );
    }
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

/// The flags of `netinstall-report` over the files of the directory it runs in, with `mac` as
/// its `--mac`.
fn netinstall_report(mac: &str) -> Vec<&str> {
    vec![
        "netinstall-report",
        "--mac",
        mac,
        "--host-id",
        "net-1",
        "--mode",
        "router",
        "--pxe-log",
        "serve.log",
        "--pending",
        "pending.json",
        "--host",
        "host.json",
    ]
}

#[test]
fn netinstall_report_prints_the_report_with_status_0() {
    // Catches: the verb not reached from the command line, or its flags reaching other fields
    // (router mode reads the REQUEST, not the DISCOVER).
    let d = dir("netinstall-report");
    std::fs::write(
        d.join("serve.log"),
        "hostkeeper-pxe serve: 990 dhcp: DHCPDISCOVER from 52:54:00:4e:10:01, architecture 7, \
         PXEClient, on the DHCP port: its architecture is kept for its REQUEST\n\
         hostkeeper-pxe serve: 1000 dhcp: DHCPREQUEST from 52:54:00:4e:10:01, architecture 7, \
         PXEClient, on the DHCP port: it takes 192.0.2.50\n",
    )
    .expect("the log");
    std::fs::write(d.join("pending.json"), "{}").expect("the list");
    std::fs::write(
        d.join("host.json"),
        r#"{"host": {"nodeId": "net-1", "enrolledUnix": "1960"}}"#,
    )
    .expect("the host");
    let o = run(&d, &netinstall_report("52:54:00:4e:10:01"));
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[..6],
        [
            "Network install of 52:54:00:4e:10:01 as host net-1, router mode.",
            "",
            "| Event | Unix seconds | Recorded by |",
            "|---|---|---|",
            "| power-on | 1000 | pxe: the first DHCPREQUEST from the MAC |",
            "| install started | not reached | pxe: the seed fetch |",
        ],
        "{out}"
    );
    assert_eq!(
        lines[7],
        "| approved | 1960 | the coordinator: the host's `enrolled_unix` |"
    );
}

#[test]
fn netinstall_report_of_a_missing_file_fails_with_status_1_and_a_bad_mac_with_2() {
    // Catches: a report made without one of its files exiting 0, its words not printed, and a
    // MAC that is not one reaching the verb.
    let d = dir("netinstall-report-missing");
    let o = run(&d, &netinstall_report("52:54:00:4e:10:01"));
    assert_eq!(o.status.code(), Some(1));
    let err = stderr(&o);
    assert!(err.starts_with("xtask: --pxe-log serve.log: "), "{err}");
    assert_eq!(stdout(&o), "");
    let o = run(&d, &netinstall_report("52:54:00:4e:10"));
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains(
            "`52:54:00:4e:10` is not a MAC address: six pairs of hex digits, separated by colons"
        ),
        "{}",
        stderr(&o)
    );
}

#[test]
fn channels_gate_with_no_setting_set_is_a_green_notice_and_writes_enabled_false_with_status_0() {
    let d = dir("channels-gate");
    let out = d.join("out");
    let o = run(
        &d,
        &[
            "channels",
            "gate",
            "--label",
            "testing-apt-read",
            "--setting",
            "APT_BUCKET=",
            "--output",
            out.to_str().expect("utf8"),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(
        stdout(&o).starts_with("::notice title=The testing-apt-read job is off::"),
        "{}",
        stdout(&o)
    );
    assert_eq!(
        std::fs::read_to_string(out).expect("out"),
        "enabled=false\n"
    );
}

#[test]
fn channels_with_flags_that_do_not_fit_is_a_usage_error_with_status_2_and_a_failed_step_status_1() {
    // A session name of another product, and a gate with some but not all of its settings.
    let d = dir("channels-exit");
    let o = run(
        &d,
        &[
            "channels",
            "assume",
            "--role-arn",
            "arn:aws:iam::123456789012:role/release",
            "--session-name",
            "other-product-1",
            "--region",
            "us-east-1",
            "--token-url",
            "https://token.example/request",
            "--env-file",
            d.join("env").to_str().expect("utf8"),
        ],
    );
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("--session-name does not start with \"hostkeeper-\""),
        "{}",
        stderr(&o)
    );
    let o = run(
        &d,
        &[
            "channels",
            "gate",
            "--label",
            "testing-apt-read",
            "--setting",
            "APT_BUCKET=set",
            "--setting",
            "APT_DISTRIBUTION_ID=",
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("set all of them or none"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn channels_assume_outside_a_job_with_a_token_is_a_failure_with_status_1() {
    // Catches: the credentials step going on without the runner's token request, which only a job
    // with `id-token: write` is offered. The variables are taken out of the environment, so the
    // test is the same on a runner that has them.
    let d = dir("channels-assume");
    let o = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(&*d)
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
        .args([
            "channels",
            "assume",
            "--role-arn",
            "arn:aws:iam::123456789012:role/release",
            "--session-name",
            "hostkeeper-apt-1",
            "--region",
            "us-east-1",
            "--token-url",
            "https://token.example/request",
            "--env-file",
            d.join("env").to_str().expect("utf8"),
        ])
        .output()
        .expect("run xtask");
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("the runner offers no OIDC token"),
        "{}",
        stderr(&o)
    );
    assert!(!d.join("env").exists());
}

/// A CA bundle of one self-signed certificate, in `d`: what `--ca-bundle` reads before any
/// download.
fn ca_bundle(d: &Path) -> String {
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("a certificate");
    let path = d.join("ca.pem");
    std::fs::write(&path, certified.cert.pem()).expect("the bundle");
    path.to_str().expect("utf8").to_owned()
}

const DEB: &[u8] = b"a pinned package";

/// A pins file of `.github/debs` pinning `DEB` as `a 1.0` for both architectures, in `d`.
fn debs_pins(d: &Path) -> String {
    let table = |arch: &str| {
        format!(
            "[{arch}]\nmirrors = [\"https://mirror.example/ubuntu\"]\ndebs = [\n  {{ package = \"a\", version = \"1.0\", path = \"pool/main/a/a_1.0_all.deb\", sha256 = \"{}\" }},\n]\n",
            sha256(DEB)
        )
    };
    let path = d.join("pins.toml");
    std::fs::write(&path, format!("{}{}", table("amd64"), table("arm64"))).expect("pins");
    path.to_str().expect("utf8").to_owned()
}

#[test]
fn debs_keeps_a_restored_set_with_status_0_and_refuses_a_stray_file_with_status_1() {
    // Proves: `debs` reaches its verb with `--pins`, `--dir` and `--ca-bundle`; a set the cache
    // restored is kept with one line and no download (so no network); a file no pin names fails
    // with status 1 naming it, a bundle that cannot be read fails with status 1 before anything
    // else, and a missing `--dir` is a usage error (2).
    let d = dir("debs");
    let pins = debs_pins(&d);
    let bundle = ca_bundle(&d);
    let set = d.join("set");
    std::fs::create_dir(&set).expect("set");
    std::fs::write(set.join("a_1.0_all.deb"), DEB).expect("the restored deb");
    let set_arg = set.to_str().expect("utf8");
    let args = [
        "debs",
        "--pins",
        &pins,
        "--dir",
        set_arg,
        "--ca-bundle",
        &bundle,
    ];
    let o = run(&d, &args);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        format!("ok: the cached a_1.0_all.deb has SHA-256 {}\n", sha256(DEB))
    );

    std::fs::write(set.join("evil.deb"), b"x").expect("a stray file");
    let o = run(&d, &args);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!("xtask: {set:?} holds \"evil.deb\", which no pin names; nothing was installed\n")
    );

    let missing = d.join("missing.pem");
    let missing_arg = missing.to_str().expect("utf8");
    let o = run(
        &d,
        &[
            "debs",
            "--pins",
            &pins,
            "--dir",
            set_arg,
            "--ca-bundle",
            missing_arg,
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).starts_with(&format!("xtask: --ca-bundle {}: ", missing.display())),
        "{}",
        stderr(&o)
    );

    let o = run(&d, &["debs", "--pins", &pins]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
}

/// Pins of both boot sets whose linux-arm64 ISO is `ISO` (the other files placeholders), in `d`.
fn netboot_pins(d: &Path, iso: &[u8]) -> String {
    let other = "a".repeat(64);
    let set = |platform: &str, arch: &str, url: &str, bytes: usize, sha: &str| {
        format!(
            "[{platform}.iso]\nurl = \"{url}\"\nsums = \"https://cd.example/SHA256SUMS\"\nbytes = {bytes}\nsha256 = \"{sha}\"\n\
             [{platform}.shim]\niso_path = \"pool/s/shim_1_{arch}.deb\"\ndeb_path = \"usr/s\"\nbytes = 1\nsha256 = \"{other}\"\n\
             [{platform}.grub]\niso_path = \"pool/g/grub_1_{arch}.deb\"\ndeb_path = \"usr/g\"\nbytes = 1\nsha256 = \"{other}\"\n\
             [{platform}.kernel]\niso_path = \"casper/vmlinuz\"\nbytes = 1\nsha256 = \"{other}\"\n\
             [{platform}.initrd]\niso_path = \"casper/initrd\"\nbytes = 1\nsha256 = \"{other}\"\n"
        )
    };
    let text = format!(
        "ubuntu = \"24.04.5\"\n{}{}",
        set(
            "linux-arm64",
            "arm64",
            "https://cd.example/r/u-arm64.iso",
            iso.len(),
            &sha256(iso)
        ),
        set(
            "linux-x86_64",
            "amd64",
            "https://cd.example/r/u-amd64.iso",
            1,
            &other
        ),
    );
    let path = d.join("pins.toml");
    std::fs::write(&path, text).expect("pins");
    path.to_str().expect("utf8").to_owned()
}

#[test]
fn netboot_iso_keeps_a_fetched_iso_with_status_0_and_refuses_a_bad_mirror_with_status_1() {
    // Proves: `netboot-iso` reaches its verb with `--pins`, `--platform`, `--dir`, each `--mirror`
    // and `--ca-bundle`; an ISO already there with the pinned size and digest is kept with one
    // line and no download; a mirror that is not an https:// base fails with status 1 naming it,
    // before any download; a missing `--platform` is a usage error (2); a bundle that cannot be
    // read fails with status 1 naming it.
    let d = dir("netboot-iso");
    let iso = b"an ISO's bytes";
    let pins = netboot_pins(&d, iso);
    let bundle = ca_bundle(&d);
    let into = d.join("iso");
    std::fs::create_dir(&into).expect("iso dir");
    std::fs::write(into.join("u-arm64.iso"), iso).expect("the ISO");
    let into_arg = into.to_str().expect("utf8");
    let base = [
        "netboot-iso",
        "--pins",
        &pins,
        "--platform",
        "linux-arm64",
        "--dir",
        into_arg,
        "--ca-bundle",
        &bundle,
        "--mirror",
        "https://mirror.example/cdimage",
    ];
    let o = run(&d, &base);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        format!(
            "ok: the kept u-arm64.iso has {} bytes and SHA-256 {}\n",
            iso.len(),
            sha256(iso)
        )
    );

    let o = run(
        &d,
        &[&base[..], &["--mirror", "http://mirror.example/x"]].concat(),
    );
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        "xtask: the mirror \"http://mirror.example/x\" is not an https:// URL with no trailing '/', query, fragment or user\n"
    );

    let o = run(&d, &["netboot-iso", "--pins", &pins, "--dir", into_arg]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));

    let missing = d.join("missing.pem");
    let mut no_bundle = base.to_vec();
    no_bundle[8] = missing.to_str().expect("utf8");
    let o = run(&d, &no_bundle);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).starts_with(&format!("xtask: --ca-bundle {}: ", missing.display())),
        "{}",
        stderr(&o)
    );
}
