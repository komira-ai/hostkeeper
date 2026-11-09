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

#[test]
fn mdm_signing_test_report_prints_an_empty_run_with_status_0() {
    // Catches: the signing test's verbs not reached from the command line, or a report of a
    // run with nothing recorded failing instead of saying it is incomplete.
    let d = dir("signing-report");
    let o = run(&d, &["mdm-signing-test", "report", "--kit", "."]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "| Step | Command | Answer | Installed | Note |\n|---|---|---|---|---|\n\n\
         Verdict: incomplete: pkg-1 has no recorded outcome\n"
    );
}

#[test]
fn mdm_signing_test_build_into_a_directory_that_exists_fails_with_status_1() {
    // Catches: a refusal of the signing test exiting 0 or 2, or its words not printed.
    let d = dir("signing-build");
    let o = run(
        &d,
        &[
            "mdm-signing-test",
            "build",
            "--out",
            ".",
            "--profile-identifier",
            "com.example.hostkeeper.mdm",
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        "xtask: . exists: the kit is built into a new directory\n"
    );
}

#[test]
fn mdm_signing_test_send_refuses_an_api_url_off_loopback_with_status_2() {
    // Catches: NanoHUB's API key sent to an address beyond the host: the flag parser refuses it.
    let d = dir("signing-send");
    let o = run(
        &d,
        &[
            "mdm-signing-test",
            "send",
            "--kit",
            ".",
            "--step",
            "pkg-1",
            "--enrollment-id",
            "A",
            "--api-url",
            "http://192.0.2.1:9004",
            "--api-key-file",
            "key",
            "--mdm-host",
            "mdm.example.net",
        ],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(
        stderr(&o).contains("is not NanoHUB's loopback API URL"),
        "{}",
        stderr(&o)
    );
}

/// The bytes of the signing test's root profile in the CLI tests.
const SIGNING_PROFILE: &[u8] = b"<plist>a root profile</plist>";

/// A signing-test kit in `d`, as `mdm-signing-test build` leaves one: pkgs whose bytes are
/// `pkg <n>` under their digests, the profile and `kit.toml`. The digest of pkg `n`, by index.
fn signing_kit(d: &Path) -> Vec<String> {
    std::fs::create_dir(d.join("pkg")).expect("pkg dir");
    std::fs::write(d.join("signing-test-root.mobileconfig"), SIGNING_PROFILE).expect("profile");
    let mut text = format!(
        "profile_identifier = \"com.example.hostkeeper.mdm\"\n\n[root_profile]\n\
         identifier = \"com.example.hostkeeper.mdm.signing-test.root\"\nsha256 = \"{}\"\n\
         root_sha256 = \"{}\"\n",
        sha256(SIGNING_PROFILE),
        "0".repeat(64)
    );
    let mut digests = Vec::new();
    for n in 1..=4 {
        let bytes = format!("pkg {n}");
        let digest = sha256(bytes.as_bytes());
        std::fs::write(d.join("pkg").join(format!("{digest}.pkg")), &bytes).expect("pkg");
        text.push_str(&format!(
            "\n[[pkg]]\nstep = \"pkg-{n}\"\nidentifier = \"dev.hostkeeper.signing-test.pkg{n}\"\n\
             version = \"1\"\nsha256 = \"{digest}\"\nbuild_check_signature = \"\"\n"
        ));
        digests.push(digest);
    }
    std::fs::write(d.join("kit.toml"), text).expect("kit.toml");
    digests
}

/// A NanoHUB on `127.0.0.1`, port 0, that answers one call `200 {}`; the join gives the
/// request it read.
fn one_call_nanohub() -> (String, std::thread::JoinHandle<String>) {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = Vec::new();
        let mut buf = [0_u8; 4096];
        loop {
            let n = stream.read(&mut buf).expect("read");
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request).into_owned();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .expect("answer");
        String::from_utf8(request).expect("a UTF-8 request")
    });
    (url, server)
}

#[test]
fn mdm_signing_test_send_dry_runs_then_applies_through_nanohub() {
    // Catches: `send`'s flags not reaching the send from the command line: a dry run that
    // calls NanoHUB or writes results.toml, `--apply` not sending the step's command to the
    // enrollment id with the API key, or the send it made not recorded under its UUID.
    let d = dir("signing-send-apply");
    let digests = signing_kit(&d);
    std::fs::write(d.join("key"), "the-key\n").expect("key");
    std::fs::set_permissions(d.join("key"), std::fs::Permissions::from_mode(0o600)).expect("mode");
    let (url, server) = one_call_nanohub();
    let flags = [
        "mdm-signing-test",
        "send",
        "--kit",
        ".",
        "--step",
        "pkg-2",
        "--enrollment-id",
        "0000FE00-ABC",
        "--api-url",
        &url,
        "--api-key-file",
        "key",
        "--mdm-host",
        "mdm.example.net",
    ];
    let dry = run(&d, &flags);
    assert_eq!(dry.status.code(), Some(0), "{}", stderr(&dry));
    let printed = stdout(&dry);
    let first = printed.lines().next().expect("a line");
    let uuid = first
        .strip_prefix("pkg-2 to 0000FE00-ABC: command ")
        .expect("the step, the Mac and the command's UUID")
        .to_owned();
    assert!(
        printed.contains("<string>InstallEnterpriseApplication</string>"),
        "{printed}"
    );
    assert!(
        printed.ends_with("dry run: nothing sent; add --apply to send it\n"),
        "{printed}"
    );
    assert!(!d.join("results.toml").exists());

    let mut apply = flags.to_vec();
    apply.push("--apply");
    let sent = run(&d, &apply);
    assert_eq!(sent.status.code(), Some(0), "{}", stderr(&sent));
    assert_eq!(
        stdout(&sent),
        format!(
            "pkg-2 to 0000FE00-ABC: command {uuid}\nqueued and pushed\nfind the answer: the \
             NanoHUB unit's log line with command_uuid={uuid}, then `mdm-signing-test record \
             --step pkg-2`\n"
        )
    );
    let request = server.join().expect("the server");
    let (head, body) = request.split_once("\r\n\r\n").expect("a head");
    assert!(
        head.starts_with("PUT /api/v1/nanomdm/enqueue/0000FE00-ABC HTTP/1.1\r\n"),
        "{head}"
    );
    // base64 of `nanohub:the-key`.
    assert!(
        head.contains("\r\nAuthorization: Basic bmFub2h1Yjp0aGUta2V5\r\n"),
        "{head}"
    );
    assert!(
        body.contains("<string>InstallEnterpriseApplication</string>"),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            "<string>https://mdm.example.net/pkg/{}.pkg</string>",
            digests[1]
        )),
        "{body}"
    );
    assert!(body.contains(&format!("<string>{uuid}</string>")), "{body}");
    let results: toml::Table = std::fs::read_to_string(d.join("results.toml"))
        .expect("results.toml")
        .parse()
        .expect("TOML");
    let sends = results["send"].as_array().expect("sends");
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["step"].as_str(), Some("pkg-2"));
    assert_eq!(sends[0]["command_uuid"].as_str(), Some(uuid.as_str()));
    assert!(sends[0].get("push_error").is_none());
}

#[test]
fn mdm_signing_test_record_writes_every_flag_to_the_send() {
    // Catches: `record`'s flags not reaching results.toml from the command line: the answer,
    // the install, pkg 3's trust or the note dropped or written to another send.
    let d = dir("signing-record");
    signing_kit(&d);
    std::fs::write(
        d.join("results.toml"),
        "[[send]]\nstep = \"pkg-3\"\ncommand_uuid = \"OLD\"\n\n\
         [[send]]\nstep = \"pkg-3\"\ncommand_uuid = \"NEW\"\n",
    )
    .expect("results.toml");
    let o = run(
        &d,
        &[
            "mdm-signing-test",
            "record",
            "--kit",
            ".",
            "--step",
            "pkg-3",
            "--answer",
            "Acknowledged",
            "--installed",
            "true",
            "--trusted",
            "true",
            "--note",
            "Installed dev.hostkeeper.signing-test.pkg3",
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "recorded pkg-3 (command NEW)\n");
    let results: toml::Table = std::fs::read_to_string(d.join("results.toml"))
        .expect("results.toml")
        .parse()
        .expect("TOML");
    let sends = results["send"].as_array().expect("sends");
    assert!(sends[0].get("outcome").is_none(), "{sends:?}");
    let outcome = sends[1]["outcome"]
        .as_table()
        .expect("the last send's outcome");
    assert_eq!(outcome["answer"].as_str(), Some("Acknowledged"));
    assert_eq!(outcome["installed"].as_bool(), Some(true));
    assert_eq!(outcome["trusted"].as_bool(), Some(true));
    assert_eq!(
        outcome["note"].as_str(),
        Some("Installed dev.hostkeeper.signing-test.pkg3")
    );
    let refused = run(
        &d,
        &[
            "mdm-signing-test",
            "record",
            "--kit",
            ".",
            "--step",
            "pkg-1",
            "--answer",
            "none",
            "--installed",
            "false",
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        stderr(&refused),
        "xtask: pkg-1 has not been sent: `send --apply` it first\n"
    );
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
            "| power-on | 1000 | hostkeeper-pxe: the first DHCPREQUEST from the MAC |",
            "| install started | not reached | hostkeeper-pxe: the seed fetch |",
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
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
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
