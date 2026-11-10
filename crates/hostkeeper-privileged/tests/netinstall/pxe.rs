//! The boot server: the real `pxe` (the program `hostkeeper-privileged-pxe`, which only forwards
//! to `hostkeeper_pxe::cli::main`), its boot set fetched and sealed by its own `prepare` step,
//! and `serve` run in the boot server's namespace in one mode at a time.
//!
//! The job downloads the boot set's ISO by its pinned URL, builds the deb of this commit (x86-64)
//! and renders `netboot.json` from the committed pins, under `target/net-install/`, before the
//! test. The test serves them as a release from its channel on the LAN (`netboot.json` with the
//! ISO's URL moved to the channel, which is the one change to the pins), each attested by the
//! end-to-end rig's signer ([`release`]), and `prepare` checks every file against the pins and
//! the attestations with the job's pinned `gh`, as on a farm host ([`prepare`]): it is run as the
//! daemon runs it, `pxe prepare <staged directory> --params <file>`, with a params file and a
//! settings file ([`ServeSettings`], [`Tools`]) of the test's. The arm64 job builds no arm64 deb:
//! its boot set gets a stand-in deb, which nothing fetches before the seed, where that test ends.
//! `serve` is run with flags, not with the settings file.

use std::fs;
use std::net::Ipv4Addr;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::events::{Bus, Source};
use super::lan::{LAN_LINK, Lan};
use super::program::Program;
use crate::e2e::signer::FixtureSigner;
use crate::support::channel::{Channel, platform};
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

/// Serves the release `prepare` fetches on `channel` and returns its version: `netboot.json`
/// of the job with the ISO's URL of `platform` (`linux-x86_64` or `linux-arm64`) moved to the
/// channel, that ISO, and the deb of `platform`, each attested by `signer`.
pub fn release(channel: &Channel, signer: &FixtureSigner, platform: &str) -> String {
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
    version
}

/// What `prepare` checks the release with, as files root owns and nobody else can write: the
/// job's `gh`, the signer's trusted root and the channel's CA.
pub struct Tools {
    pub gh: PathBuf,
    pub trusted_root: PathBuf,
    pub ca_bundle: PathBuf,
}

impl Tools {
    /// Puts the tools under `dir/tools`.
    pub fn put(dir: &Path, channel_ca: &str, signer: &FixtureSigner) -> Tools {
        let tools = dir.join("tools");
        let gh = tools.join("gh");
        put(&gh, &fs::read(job_gh()).expect("the job's gh"), 0o755);
        let trusted_root = tools.join("trusted_root.json");
        put(&trusted_root, signer.trusted_root_jsonl().as_bytes(), 0o644);
        let ca_bundle = tools.join("channel-ca.pem");
        put(&ca_bundle, channel_ca.as_bytes(), 0o644);
        Tools {
            gh,
            trusted_root,
            ca_bundle,
        }
    }

    /// The `[prepare]` table of a settings file: these tools and the boot set of `platform`.
    pub fn table(&self, platform: &str) -> String {
        format!(
            "[prepare]\nplatform = [{}]\nca-bundle = {}\ngh = {}\ntrusted-root = {}\n",
            quoted(platform),
            quoted(&self.ca_bundle.display().to_string()),
            quoted(&self.gh.display().to_string()),
            quoted(&self.trusted_root.display().to_string()),
        )
    }
}

/// A TOML string (a basic string; the paths and names of the tests need no more than its
/// quoting).
pub fn quoted(text: &str) -> String {
    format!("{text:?}")
}

/// The `[serve]` table of a settings file: serve's flags of the boot server in `mode`, the keys
/// being the flags' names. It holds no `dir` (the command line gives it) and no `interface`
/// (the LAN's interface is in the boot server's namespace, which `prepare` does not run in).
pub struct ServeSettings<'a> {
    pub mode: Mode,
    pub address: Ipv4Addr,
    pub coordinator: &'a Coordinator,
    pub daemon_config: &'a Path,
    pub ssh_keys: &'a Path,
    /// The guest's serial console, when the settings name one.
    pub serial_console: Option<&'a str>,
    /// Every listener on `127.0.0.1` with a port the system picks, not the default ports.
    pub loopback: bool,
}

impl ServeSettings<'_> {
    /// The table, with the user and group `serve` drops to ([`drop_user`]).
    pub fn table(&self) -> String {
        let (uid, gid) = drop_user();
        let path = |p: &Path| quoted(&p.display().to_string());
        let mut text = format!(
            "[serve]\nmode = {}\naddress = {}\ncoordinator = {}\nca-file = {}\n\
             token-file = {}\ndaemon-config = {}\nssh-authorized-keys = {}\n\
             user-id = {uid}\ngroup-id = {gid}\n",
            quoted(self.mode.flag()),
            quoted(&self.address.to_string()),
            quoted(&self.coordinator.control_url()),
            path(&self.coordinator.private_ca_path()),
            path(&self.coordinator.token_path()),
            path(self.daemon_config),
            path(self.ssh_keys),
        );
        if let Some(console) = self.serial_console {
            text.push_str(&format!("serial-console = {}\n", quoted(console)));
        }
        if self.loopback {
            for key in ["dhcp-listen", "tftp-listen", "http-listen", "proxy-listen"] {
                text.push_str(&format!("{key} = \"127.0.0.1:0\"\n"));
            }
        }
        text
    }
}

/// The sealed boot set of `platform` (`linux-x86_64` or `linux-arm64`), made by `prepare` under
/// `dir/boot`, from a release the test serves on `channel`: the directory `dir/boot/<version>`.
/// `serve` is the settings `serve` checks the set against (the check `prepare` makes of them
/// before it fetches).
pub fn prepare(
    dir: &Path,
    platform: &str,
    channel: &Channel,
    channel_ca: &str,
    signer: &FixtureSigner,
    serve: &ServeSettings<'_>,
) -> PathBuf {
    let version = release(channel, signer, platform);
    let tools = Tools::put(dir, channel_ca, signer);
    let settings = dir.join("pxe.toml");
    put(
        &settings,
        format!("{}\n{}", serve.table(), tools.table(platform)).as_bytes(),
        0o600,
    );
    let boot_dir = dir.join("boot");
    let staged = dir.join("staged");
    fs::create_dir_all(&staged).expect("the staged directory");
    let params = dir.join("params.toml");
    put(
        &params,
        format!(
            "release = {}\nsettings = {}\nboot_dir = {}\nunit = \"pxe.service\"\n\
             \"updater.target_version\" = {}\n\"updater.staged_release\" = {}\n",
            quoted(&channel.url()),
            quoted(&settings.display().to_string()),
            quoted(&boot_dir.display().to_string()),
            quoted(&version),
            quoted(&staged.display().to_string()),
        )
        .as_bytes(),
        0o600,
    );
    let args: Vec<String> = [
        "prepare",
        &staged.display().to_string(),
        "--params",
        &params.display().to_string(),
    ]
    .map(String::from)
    .to_vec();
    let out = run(PXE, &args);
    assert!(
        out.status.success(),
        "pxe prepare: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    println!("{}", String::from_utf8_lossy(&out.stderr).trim());
    boot_dir.join(version)
}

/// The committed golden manifest of the `pxe` package (version 1.2.3, linux-x86_64).
const GOLDEN_MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packaging/pxe/linux/manifest.toml"
));
/// The committed golden unit of the `pxe` package, as the package carries it.
pub const UNIT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packaging/pxe/linux/units/pxe.service"
));

/// The manifest of the `pxe` package for this host's platform: the golden, with its `version`,
/// its `platform` and the `release` of its `[params]` the only lines changed.
pub fn manifest(version: &str, release: &str) -> String {
    let mut changed = [false; 3];
    let lines: Vec<String> = GOLDEN_MANIFEST
        .lines()
        .map(|line| {
            if line == "version = \"1.2.3\"" {
                changed[0] = true;
                format!("version = {}", quoted(version))
            } else if line == "platform = \"linux-x86_64\"" {
                changed[1] = true;
                format!("platform = {}", quoted(platform()))
            } else if line.starts_with("release = ") {
                changed[2] = true;
                format!("release = {}", quoted(release))
            } else {
                line.to_owned()
            }
        })
        .collect();
    assert_eq!(
        changed, [true; 3],
        "the golden manifest's version, platform and release"
    );
    lines.join("\n") + "\n"
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
        &format!("pxe serve --mode {}", mode.flag()),
        &lan.pxe_ns,
        PXE,
        &args,
        // The file keeps its name from before the program was `pxe`: the job's report and its
        // artifacts name it.
        &inputs.log_dir.join("hostkeeper-pxe.log"),
    );
    bus.wait(from, Source::Pxe, "pxe's start", |line| {
        line.contains(&format!(
            "started in {} mode as uid {uid} gid {gid}",
            mode.flag()
        ))
    });
    program
}
