//! The boot server: the real `hostkeeper-pxe` (the program `hostkeeper-privileged-pxe`, which
//! only forwards to `hostkeeper_pxe::cli::main`), its boot set fetched and sealed by its own
//! `fetch`, and `serve` run in the boot server's namespace in one mode at a time.
//!
//! The job downloads the boot set's ISO by its pinned URL, builds the deb of this commit (x86-64)
//! and renders `netboot.json` from the committed pins, under `target/net-install/`, before the
//! test. The test serves them as a release from its channel on the LAN (`netboot.json` with the
//! ISO's URL moved to the channel, which is the one change to the pins), each attested by the
//! end-to-end rig's signer, and `fetch` checks every file against the pins and the attestations
//! with the job's pinned `gh`, as on a farm host. The arm64 job builds no arm64 deb: its boot set
//! gets a stand-in deb, which nothing fetches before the seed, where that test ends.

use std::fs;
use std::net::Ipv4Addr;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::events::{Bus, Source};
use super::lan::{LAN_LINK, Lan};
use super::program::Program;
use crate::e2e::signer::FixtureSigner;
use crate::support::channel::Channel;
use crate::support::pair::{Coordinator, put, sha256_hex};
use crate::support::run;

/// The boot server program, built by cargo with these tests.
pub const PXE: &str = env!("CARGO_BIN_EXE_hostkeeper-privileged-pxe");

/// What the job left under `target/net-install/` for the test.
pub fn job_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("../net-install")
        .canonicalize()
        .expect("target/net-install, which the job fills before the test")
}

/// The pinned `gh` the job installed (`cargo xtask install-tool`).
fn job_gh() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("../privileged-tools/gh")
}

/// The user `serve` drops to: the owner of the build directory, the runner's own user, who can
/// read the sealed directory the test made under it. It is never root.
pub fn drop_user() -> (u32, u32) {
    let meta = fs::metadata(env!("CARGO_TARGET_TMPDIR")).expect("the build directory");
    assert_ne!(
        meta.uid(),
        0,
        "the build directory is root's: the tests are built as the runner's user and run as root"
    );
    (meta.uid(), meta.gid())
}

/// The sealed boot set of `platform` (`linux-x86_64` or `linux-arm64`), made by `fetch` under
/// `dir`, from a release the test serves on `channel`.
pub fn fetch(
    dir: &Path,
    platform: &str,
    channel: &Channel,
    channel_ca: &str,
    signer: &FixtureSigner,
    daemon_config: &Path,
) -> PathBuf {
    let job = job_dir();
    let text = fs::read_to_string(job.join("netboot.json")).expect("the job's netboot.json");
    let mut netboot: Value = serde_json::from_str(&text).expect("netboot.json is JSON");
    let version = netboot["version"].as_str().expect("a version").to_owned();
    let iso = netboot["platforms"][platform]["iso"]["name"]
        .as_str()
        .expect("the ISO's name")
        .to_owned();
    netboot["platforms"][platform]["iso"]["url"] =
        Value::String(format!("{}/iso/{iso}", channel.url()));
    let netboot = serde_json::to_vec(&netboot).expect("JSON");
    channel.serve_file(&format!("/iso/{iso}"), &job.join(&iso));
    channel.serve_bytes(
        "/netboot.json.sigstore.json",
        signer.bundle("netboot.json", &sha256_hex(&netboot)),
    );
    channel.serve_bytes("/netboot.json", netboot);

    let arch = if platform == "linux-arm64" {
        "arm64"
    } else {
        "amd64"
    };
    let deb_name = format!("hostkeeper_{version}_{arch}.deb");
    let built = job.join(&deb_name);
    let deb = if built.is_file() {
        fs::read(&built).expect("the job's deb")
    } else {
        assert_eq!(
            arch,
            "arm64",
            "the job builds the deb of {platform}: {} is missing",
            built.display()
        );
        b"a stand-in for the arm64 deb, which this test never installs\n".to_vec()
    };
    channel.serve_bytes(
        &format!("/{deb_name}.sigstore.json"),
        signer.bundle(&deb_name, &sha256_hex(&deb)),
    );
    channel.serve_bytes(&format!("/{deb_name}"), deb);

    // gh and the trusted root are root's, not writable by others, as fetch requires.
    let tools = dir.join("tools");
    let gh = tools.join("gh");
    put(&gh, &fs::read(job_gh()).expect("the job's gh"), 0o755);
    let root = tools.join("trusted_root.json");
    put(&root, signer.trusted_root_jsonl().as_bytes(), 0o644);
    let ca = tools.join("channel-ca.pem");
    put(&ca, channel_ca.as_bytes(), 0o644);
    let sealed = dir.join("boot");
    let args: Vec<String> = [
        "fetch",
        "--release",
        &channel.url(),
        "--dir",
        &sealed.display().to_string(),
        "--daemon-config",
        &daemon_config.display().to_string(),
        "--platform",
        platform,
        "--ca-bundle",
        &ca.display().to_string(),
        "--gh",
        &gh.display().to_string(),
        "--trusted-root",
        &root.display().to_string(),
    ]
    .map(String::from)
    .to_vec();
    let out = run(PXE, &args);
    assert!(
        out.status.success(),
        "hostkeeper-pxe fetch: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    println!("{}", String::from_utf8_lossy(&out.stdout).trim());
    sealed
}

/// How a run of `serve` answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Proxy,
    Router,
}

impl Mode {
    pub fn flag(self) -> &'static str {
        match self {
            Mode::Proxy => "proxy",
            Mode::Router => "router",
        }
    }
}

/// The inputs every run of `serve` takes.
pub struct ServeInputs {
    pub sealed: PathBuf,
    pub daemon_config: PathBuf,
    pub ssh_keys: PathBuf,
    pub serial_console: &'static str,
    pub log_dir: PathBuf,
}

/// Starts `serve` in `mode` in the boot server's namespace, reading `coordinator`, with
/// `--reinstall` for each of `reinstall`, and waits for its start line.
pub fn serve(
    bus: &mut Bus,
    lan: &Lan,
    coordinator: &Coordinator,
    inputs: &ServeInputs,
    mode: Mode,
    reinstall: &[&str],
) -> Program {
    let (uid, gid) = drop_user();
    let address: Ipv4Addr = lan.pxe();
    let mut args: Vec<String> = [
        "serve",
        "--mode",
        mode.flag(),
        "--address",
        &address.to_string(),
        "--interface",
        LAN_LINK,
        "--coordinator",
        &coordinator.control_url(),
        "--ca-file",
        &coordinator.private_ca_path().display().to_string(),
        "--token-file",
        &coordinator.token_path().display().to_string(),
        "--dir",
        &inputs.sealed.display().to_string(),
        "--daemon-config",
        &inputs.daemon_config.display().to_string(),
        "--ssh-authorized-keys",
        &inputs.ssh_keys.display().to_string(),
        "--serial-console",
        inputs.serial_console,
        "--user-id",
        &uid.to_string(),
        "--group-id",
        &gid.to_string(),
    ]
    .map(String::from)
    .to_vec();
    for host in reinstall {
        args.extend(["--reinstall".to_owned(), (*host).to_owned()]);
    }
    let from = bus.mark();
    let program = Program::start(
        bus,
        Source::Pxe,
        &format!("hostkeeper-pxe serve --mode {}", mode.flag()),
        &lan.pxe_ns,
        PXE,
        &args,
        &inputs.log_dir.join("hostkeeper-pxe.log"),
    );
    bus.wait(from, Source::Pxe, "hostkeeper-pxe's start", |line| {
        line.contains(&format!(
            "started in {} mode as uid {uid} gid {gid}",
            mode.flag()
        ))
    });
    program
}
