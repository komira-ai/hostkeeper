//! The Mac of one self-update test of `tests/device_mac_selfupdate.rs`: the program
//! `hostkeeper-selfupdate-daemon` (its module says what it replaces and why) run as a launchd job
//! of the system domain from `current` of a binary directory the test owns, with the releases of
//! hostkeeper in its package store and a coordinator the test writes (`link/order.json`, read at
//! each heartbeat; `link/beats.jsonl`, each heartbeat sent).
//!
//! **Where.** The release a self-update stages from must be a tree whose every directory, up to
//! `/`, is root's and closed to group and others (`hostkeeper_steps::Staged::check`), and every
//! file in it root's: the target directory is the runner's user's, so the host lives under
//! `/var/hk-su-<tag>-<pid>` (a link to `/private/var`, as the daemon's default directories are
//! reached), and the programs are written into it byte for byte (`fs::copy` on macOS clones a
//! file and keeps the owner of its source).
//!
//! **The job.** Its plist is the daemon's own (`hostkeeper_launchd::plist::daemon_plist`:
//! `RunAtLoad` and plain `KeepAlive` true, so that the clean exit the follow-on asks for starts
//! `current` again), its program `current` of the test's binary directory, with the fixture's
//! flags before the verb and the test's directories and `--heartbeat-secs 1` after it, its
//! output in two files of the test's tree.
//!
//! **What this rig does not have.** The daemon writes its fallback job's plist into the unit
//! directory of its placement (the fixture's `units/`, not `/Library/LaunchDaemons`) and loads it
//! from `/Library/LaunchDaemons`, where it is not: that load fails and is an alert of the daemon,
//! which goes on (`hostkeeper_daemon::selfunits`). So the fallback of a next binary that cannot
//! start is not run here; [`MacSelfHost::new`] refuses a Mac where that plist is present, so a
//! fallback job of another host's tree is never loaded.
//!
//! Dropping the host boots out the daemon's job, the probe's job and the fallback label, prints
//! the daemon's output when the test is failing, and removes the tree.
//!
//! Every wait reads a state a bounded number of times ([`wait`]) and fails at once when the state
//! can no longer come (an order refused, [`MacSelfHost::check_not_stuck`]); nothing asserts how
//! long anything took.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_config::platform::Platform;
use hostkeeper_daemon::selfunits::fallback_label;
use hostkeeper_daemon::selfupdate::PROBE_UNIT;
use hostkeeper_launchd::launchctl::{JobStatus, bootout_args, bootstrap_args, print};
use hostkeeper_launchd::procid::darwin::DarwinPidInfo;
use hostkeeper_launchd::{DaemonJob, Label, Launchctl, LaunchctlProcess, PidInfo, daemon_plist};
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::{Layout, RecordStore, Seal, tree_digest};
use serde_json::Value;

/// The daemon program of these tests.
pub const DAEMON: &str = env!("CARGO_BIN_EXE_hostkeeper-selfupdate-daemon");
/// The workload fixture, the hook of every release.
pub const FIXTURE: &str = env!("CARGO_BIN_EXE_hostkeeper-fixture-hook");
/// The service of hostkeeper itself.
pub const HK: &str = "hostkeeper";
/// The version the host runs first.
pub const N: &str = "1.0.0";
/// The version it is ordered to.
pub const NEXT: &str = "1.0.1";
/// The package digest the orders name.
const DIGEST: &str = "abababababababababababababababababababababababababababababababab";
/// The order id of the self-update.
pub const ORDER: &str = "self-update-1";
/// The reason of the verdict of an update whose new binary has not run yet.
const AWAITING_REASON: &str = "awaiting_restart";
/// Where launchd reads the plists of the system domain.
const LAUNCHD_DIR: &str = "/Library/LaunchDaemons";
/// How many times a wait reads its state, and the pause between two reads.
const READS: u32 = 1500;
const PAUSE: Duration = Duration::from_millis(200);

/// `launchctl`, as the daemon runs it.
fn launchctl() -> LaunchctlProcess {
    LaunchctlProcess::system(Duration::from_secs(60))
}

/// Reads `done` until it holds, [`READS`] times at most; fails naming `what` when it never did.
pub fn wait(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..READS {
        if done() {
            return;
        }
        std::thread::sleep(PAUSE);
    }
    panic!("{what}: not reached after {READS} reads");
}

/// The line the daemon's log holds once its probe ran the program's `self-update-protocol`: the
/// program's line (its `probed_line`) under the probe's unit, as the launcher logs a run.
pub fn probed_log_line() -> String {
    format!(
        "hostkeeper daemon: {PROBE_UNIT}: hostkeeper-selfupdate-daemon: asked self-update-protocol"
    )
}

/// `path` made a directory of mode `mode`.
fn dir(path: &Path, mode: u32) {
    fs::create_dir_all(path).unwrap_or_else(|e| panic!("make {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// `bytes` written at `path` (a new file, root's), mode `mode`.
fn file(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// The program `from` written at `to`, byte for byte, mode 0755.
fn program(from: &str, to: &Path) {
    let bytes = fs::read(from).unwrap_or_else(|e| panic!("read {from}: {e}"));
    file(to, &bytes, 0o755);
}

/// The manifest of hostkeeper at `version`: its hook the workload fixture in mode `ok`, which
/// ends every step `0` with a valid last line.
fn manifest(version: &str) -> String {
    let platform = Platform::current();
    format!(
        r#"schema = 1
name = "{HK}"
version = "{version}"
platform = "{platform}"
min_hostkeeper = "0.1.0"

[hook]
command = ["bin/hook"]
verify_deadline_secs = 1600

[hook.timeout_secs]
prepare = 600
cutover = 300
verify = 1000
rollback = 600
cleanup = 1300

[params]
"fixture.mode" = "ok"
"#
    )
}

/// The daemon config: every key the loader requires. Nothing reads its addresses: the fixture's
/// link, package store and enroller replace what would.
const CONFIG: &str = r#"coordinator = "https://coordinator.example"
ca_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// How the host is when the daemon starts.
#[derive(Clone, Copy, Debug)]
pub enum Plan {
    /// hostkeeper's package at [`N`] installed (`current` of its package directory), the
    /// releases of [`N`] and [`NEXT`] in the store: the coordinator orders an `update`.
    Installed,
    /// As the pkg leaves a Mac: the binary directory alone, no package of hostkeeper installed,
    /// the release of the version given in the store: the coordinator orders an `install`.
    Pkg(&'static str),
}

/// One test's Mac (the module says what it is).
pub struct MacSelfHost {
    base: PathBuf,
    label: Label,
}

impl Drop for MacSelfHost {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", self.logs());
        }
        for label in [
            Some(self.label.clone()),
            Label::probe(PROBE_UNIT).ok(),
            Label::parse(&fallback_label()).ok(),
        ]
        .into_iter()
        .flatten()
        {
            let _ = launchctl().run(&bootout_args(&label));
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

impl MacSelfHost {
    /// The Mac of the test `tag` (1 to 8 of `[a-z0-9]`) under `plan`, its daemon's job loaded,
    /// running [`N`].
    pub fn new(tag: &str, plan: Plan) -> MacSelfHost {
        assert!(
            rustix::process::geteuid().is_root(),
            "the device tests run as root (privileged.yml's cargo runner `sudo`)"
        );
        let fallback = Path::new(LAUNCHD_DIR).join(format!("{}.plist", fallback_label()));
        assert!(
            fs::symlink_metadata(&fallback).is_err(),
            "{} is on this Mac: the daemon under test would load that fallback job",
            fallback.display()
        );
        let pid = std::process::id();
        let base = PathBuf::from(format!("/var/hk-su-{tag}-{pid}"));
        // `create_dir_all` would take over a directory that is already there.
        fs::create_dir(&base).unwrap_or_else(|e| panic!("create {}: {e}", base.display()));
        let label = Label::parse(&format!("dev.hostkeeper.ci-su{tag}-{pid}")).expect("a job label");
        let host = MacSelfHost { base, label };
        host.make(plan);
        host.load();
        host.runs(N);
        host
    }

    fn path(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }

    fn state(&self) -> PathBuf {
        self.path("state")
    }

    fn fixture(&self) -> PathBuf {
        self.path("fixture")
    }

    /// The package directory of hostkeeper.
    pub fn service(&self) -> PathBuf {
        self.path("packages").join(HK)
    }

    /// The binary directory of hostkeeper's own update.
    pub fn layout(&self) -> Layout {
        Layout::new(&self.state().join(HK).join("bin"))
    }

    fn make(&self, plan: Plan) {
        dir(&self.base, 0o755);
        dir(&self.state(), 0o700);
        dir(&self.state().join(HK), 0o700);
        dir(&self.path("packages"), 0o755);
        dir(&self.path("run"), 0o700);
        for sub in ["link", "units", "placed"] {
            dir(&self.fixture().join(sub), 0o755);
        }
        file(&self.path("hostkeeper.toml"), CONFIG.as_bytes(), 0o644);
        dir(&self.service(), 0o755);
        match plan {
            Plan::Installed => {
                for version in [N, NEXT] {
                    self.release(version);
                }
                std::os::unix::fs::symlink(N, self.service().join("current"))
                    .expect("link current");
            }
            Plan::Pkg(version) => self.release(version),
        }
        let daemon = self.path("hostkeeper-selfupdate-daemon");
        program(DAEMON, &daemon);
        let layout = self.layout();
        layout.stage(N, &daemon).expect("stage the first binary");
        layout.switch(N).expect("switch to it");
    }

    /// The release of hostkeeper at `version`, as the real store leaves it: the tree (manifest,
    /// the hook, the binary), then its seal beside it.
    fn release(&self, version: &str) {
        let tree = self.service().join(version);
        dir(&tree, 0o755);
        dir(&tree.join("bin"), 0o755);
        file(
            &tree.join("manifest.toml"),
            manifest(version).as_bytes(),
            0o644,
        );
        program(FIXTURE, &tree.join("bin/hook"));
        program(DAEMON, &tree.join("bin/hostkeeper"));
        let digest = tree_digest(&tree).expect("the tree digest");
        let seal = Seal::new(&digest, DIGEST).expect("a seal");
        file(
            &self.service().join(format!("{version}.seal")),
            seal.to_json().as_bytes(),
            0o644,
        );
    }

    /// The daemon's plist, as `daemon_plist` renders it, with the test's arguments around the
    /// verb; its job bootstrapped.
    fn load(&self) {
        let current = self.layout().current_path();
        let job = DaemonJob {
            label: self.label.clone(),
            program: current,
            stdout: self.path("daemon.out.log"),
            stderr: self.path("daemon.err.log"),
        };
        let rendered = daemon_plist(&job).expect("the daemon's plist");
        let arg = |text: String| format!("<string>{text}</string>");
        let text = |name: &str| self.path(name).display().to_string();
        let mut argv = vec![
            arg("--fixture-dir".to_owned()),
            arg(self.fixture().display().to_string()),
            arg("daemon".to_owned()),
        ];
        for (flag, value) in [
            ("--config", text("hostkeeper.toml")),
            ("--state-dir", text("state")),
            ("--packages-dir", text("packages")),
            ("--admin-socket", text("run/admin.sock")),
            ("--heartbeat-secs", "1".to_owned()),
        ] {
            argv.push(arg(flag.to_owned()));
            argv.push(arg(value));
        }
        let verb = "<string>daemon</string>";
        assert_eq!(
            rendered.matches(verb).count(),
            1,
            "the daemon's plist runs `current daemon`:\n{rendered}"
        );
        assert!(
            rendered.contains("<key>KeepAlive</key>\n\t<true/>"),
            "the daemon's plist keeps its job alive:\n{rendered}"
        );
        let plist = self.path(&format!("{}.plist", self.label));
        file(
            &plist,
            rendered.replacen(verb, &argv.join("\n\t\t"), 1).as_bytes(),
            0o644,
        );
        let _ = launchctl().run(&bootout_args(&self.label));
        let out = launchctl()
            .run(&bootstrap_args(&plist))
            .expect("launchctl bootstrap ran");
        assert_eq!(
            out.code,
            Some(0),
            "bootstrap {}: {}",
            self.label,
            out.stderr_text()
        );
    }

    /// The daemon's standard output and error, and the heartbeats' tail, for a failure.
    pub fn logs(&self) -> String {
        let read = |name: &str| fs::read_to_string(self.path(name)).unwrap_or_default();
        let beats = self.beats();
        let tail = beats.len().saturating_sub(3);
        format!(
            "--- daemon.out.log:\n{}\n--- daemon.err.log:\n{}\n--- the last heartbeats:\n{:#?}",
            read("daemon.out.log"),
            read("daemon.err.log"),
            &beats[tail..]
        )
    }

    /// The daemon's standard error so far.
    pub fn daemon_log(&self) -> String {
        fs::read_to_string(self.path("daemon.err.log")).unwrap_or_default()
    }

    /// The coordinator orders `verb` of hostkeeper at `version`, order [`ORDER`] try 1, in every
    /// reply from now on.
    pub fn offer(&self, verb: &str, version: &str) {
        let link = self.fixture().join("link");
        let order = serde_json::json!({
            "id": ORDER,
            "try": 1,
            "verb": verb,
            "service": HK,
            "version": version,
            "digest": DIGEST,
        });
        let tmp = link.join("order.json.tmp");
        file(&tmp, order.to_string().as_bytes(), 0o644);
        fs::rename(&tmp, link.join("order.json")).expect("place the order");
    }

    /// The version `current` of the binary directory names.
    pub fn current(&self) -> Option<String> {
        self.layout().current().expect("read current")
    }

    /// The step record of hostkeeper, when there is one this daemon reads.
    pub fn record(&self) -> Option<Record> {
        match RecordStore::new(&self.state().join(HK)).load() {
            Ok(Loaded::Current(rec)) => Some(*rec),
            Ok(Loaded::Absent | Loaded::Newer { .. }) => None,
            Err(e) => panic!("the step record cannot be read: {e}"),
        }
    }

    /// Every heartbeat the daemon sent so far, oldest first; a last line still being written is
    /// left out.
    pub fn beats(&self) -> Vec<Value> {
        let text = fs::read_to_string(self.fixture().join("link/beats.jsonl")).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The newest `last_update` the heartbeats reported for hostkeeper's order [`ORDER`].
    pub fn verdict(&self) -> Option<Value> {
        self.beats().into_iter().rev().find_map(|beat| {
            beat["services"]
                .as_array()?
                .iter()
                .filter(|service| service["name"] == HK)
                .map(|service| service["last_update"].clone())
                .find(|result| result["order_id"] == ORDER && result["order_try"] == 1)
        })
    }

    /// Fails at once, with the daemon's output, when the order was answered with a refusal (any
    /// failure but the wait for the restart): what a wait waits for can no longer come.
    pub fn check_not_stuck(&self) {
        if let Some(result) = self.verdict() {
            let refused = (result["outcome"] == "failed") && (result["reason"] != AWAITING_REASON);
            assert!(!refused, "the order was refused: {result}\n{}", self.logs());
        }
    }

    /// Waits until the step record of hostkeeper is finished, and gives it.
    pub fn settled(&self) -> Record {
        let mut found = None;
        wait("the step record of hostkeeper is finished", || {
            self.check_not_stuck();
            found = self
                .record()
                .filter(|rec| matches!(rec.state, State::Finished { .. }));
            found.is_some()
        });
        found.expect("a finished record")
    }

    /// The pid of the daemon's job, while it has a process.
    pub fn pid(&self) -> Option<u32> {
        match print(&launchctl(), &self.label) {
            Ok(JobStatus::Loaded(info)) => info.pid,
            Ok(JobStatus::NotLoaded) => panic!("the daemon's job is gone\n{}", self.logs()),
            Err(e) => panic!("launchctl print system/{}: {e}", self.label),
        }
    }

    /// The real path of the binary directory's file of `version`.
    pub fn binary_of(&self, version: &str) -> PathBuf {
        fs::canonicalize(self.layout().version_path(version)).expect("the staged binary")
    }

    /// Waits until the daemon's job has a process that runs the binary of `version`, checks that
    /// `current` names it, and gives its pid.
    pub fn runs(&self, version: &str) -> u32 {
        let want = self.binary_of(version);
        let mut running = None;
        wait(&format!("the daemon's job runs {}", want.display()), || {
            self.check_not_stuck();
            running = self
                .pid()
                .filter(|pid| DarwinPidInfo.path(*pid).is_ok_and(|path| path == want));
            running.is_some()
        });
        assert_eq!(self.current().as_deref(), Some(version), "{}", self.logs());
        running.expect("a pid")
    }

    /// Waits until a heartbeat of the daemon at `agent` reports hostkeeper at `version` with the
    /// order's `last_update` succeeded.
    pub fn reports_succeeded(&self, agent: &str, version: &str) {
        wait("a heartbeat reports the order succeeded", || {
            self.check_not_stuck();
            self.beats().iter().any(|beat| {
                beat["agent"]["version"] == agent
                    && beat["services"].as_array().is_some_and(|services| {
                        services.iter().any(|service| {
                            service["name"] == HK
                                && service["version"] == version
                                && service["last_update"]["order_id"] == ORDER
                                && service["last_update"]["outcome"] == "succeeded"
                        })
                    })
            })
        });
    }
}
