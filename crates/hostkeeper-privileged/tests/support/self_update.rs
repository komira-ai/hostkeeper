//! The host of one self-update test: hostkeeper's real unit file, drop-in and fallback unit under
//! the host's systemd, running the binary directory's `current` of a state directory the test
//! owns, with the release of the next version in its package store and a coordinator the test
//! writes (the program `hostkeeper-selfupdate-daemon` says what it replaces and why).
//!
//! **Where.** The release a self-update stages from must be a tree whose every directory, up to
//! `/`, is root's and closed to group and others (`hostkeeper_steps::Staged::check`), and the
//! fallback unit runs `previous` of the binary directory as root. The target directory is the
//! runner's user's, so the host lives in a scratch directory under [`BASE`] (`/run`, root's
//! tmpfs), removed when the test ends, with the units under unit names of the test's rig.
//!
//! **The units.**
//! - The main unit is hostkeeper's package unit, `packaging/linux/units/hostkeeper.service`
//!   ([`REAL_UNIT`]), or, for [`Plan::deb`], the bootstrap unit the deb installs,
//!   `packaging/linux/hostkeeper.service` ([`BOOTSTRAP_UNIT`]), every line kept but these
//!   ([`test_unit`]): `ExecStart=` runs the same program and flags with the test's directories,
//!   the fixture's own flags before the verb, `--admin-socket` at the scratch directory (the real
//!   units pass none, so the daemon would bind the host's default socket) and `--heartbeat-secs 1`
//!   after the verb; the program is `current` of the test's binary directory for the package
//!   unit, and for the bootstrap unit a copy of the fixture daemon at a packaged path of the
//!   scratch directory, named by the version [`N`] it stands for (`--fixture-packaged`: the
//!   packaged binary seeds the empty binary directory and execs `current`); the seed
//!   (`ExecStartPre=`: the test stages the first binary itself) of the package unit is left out.
//!   A change to the unit's `ExecStart=` that this does not know fails the test by name.
//! - The fallback unit is `hostkeeper_steps::selfupdate::fallback_unit` of the test's binary
//!   directory, naming the main unit.
//! - The drop-in is `hostkeeper_steps::selfupdate::main_dropin` naming the fallback unit, with
//!   `START_LIMIT_BURST`, in the main unit's drop-in directory under `/run/systemd/system`
//!   (`hostkeeper_daemon::selfunits::DROPIN_FILE`), so systemd reads it as it reads the one the
//!   daemon writes for `hostkeeper.service`. Its `RestartSec=1` is the production value.
//!
//! The two unit files are linked from the scratch directory (`systemctl link`); dropping the
//! [`Host`] stops, unlinks and resets both, removes the drop-in, reloads systemd and removes the
//! scratch directory, failed or not.
//!
//! Every wait polls systemd or the files for a state ([`support::wait_until`]) and fails at once
//! when the state can no longer come: an order refused, the main unit failed with no fallback
//! to run, or the fallback failed ([`Host::check_not_stuck`]).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_config::platform::Platform;
use hostkeeper_daemon::selfunits::DROPIN_FILE;
use hostkeeper_privileged::Rig;
use hostkeeper_privileged::rig::service_unit;
use hostkeeper_steps::record::{Loaded, RECORD_FILE, Record, State};
use hostkeeper_steps::selfupdate::{START_LIMIT_BURST, fallback_unit, main_dropin};
use hostkeeper_steps::systemd::SYSTEMCTL;
use hostkeeper_steps::{BadVersions, Layout, RecordStore, Seal, tree_digest};
use hostkeeper_testkit::Scratch;
use serde_json::Value;

use super::{FIXTURE, journal, property, run, run_ok, wait_until};

/// The daemon program of these tests.
pub const DAEMON: &str = env!("CARGO_BIN_EXE_hostkeeper-selfupdate-daemon");
/// The unit file hostkeeper's package carries, which starts `current` of the binary directory.
pub const REAL_UNIT: &str = include_str!("../../../../packaging/linux/units/hostkeeper.service");
/// The bootstrap unit file the deb, the rpm and the boot image install, which starts the packaged
/// binary.
pub const BOOTSTRAP_UNIT: &str = include_str!("../../../../packaging/linux/hostkeeper.service");
/// Where the bootstrap unit's `ExecStart=` has the packaged binary.
const PACKAGED_BINARY: &str = "/usr/bin/hostkeeper";
/// The root-owned directory the test hosts are made in.
pub const BASE: &str = "/run/hostkeeper-ci";
/// Where systemd reads the drop-ins of a unit from at run time.
const RUNTIME_UNIT_DIR: &str = "/run/systemd/system";
/// The service of hostkeeper itself.
pub const HK: &str = "hostkeeper";
/// The version the host runs first.
pub const N: &str = "1.0.0";
/// The version it is ordered to.
pub const NEXT: &str = "1.0.1";
/// The package digest the orders name.
pub const DIGEST: &str = "abababababababababababababababababababababababababababababababab";
/// The order id of the self-update.
pub const ORDER: &str = "self-update-1";
/// The reason of the verdict of a version whose new binary fell back.
pub const FALLBACK_REASON: &str = "self_update_fallback";
/// The reason of the verdict of an update whose new binary has not run yet.
const AWAITING_REASON: &str = "awaiting_restart";

/// The real `ExecStart=` paths the test replaces, by flag, and the binary it starts.
const REAL_STATE_DIR: &str = "/var/lib/updater/state";

/// What the main unit's binary is told besides the daemon's flags.
#[derive(Clone, Copy, Debug, Default)]
pub struct Plan {
    /// A daemon of [`NEXT`] exits 1 at its start (`--fixture-fail-start`).
    pub next_fails: bool,
    /// The daemon holds its exit after the follow-on switched (`--fixture-hold-exit`).
    pub hold_exit: bool,
    /// The host is as the deb leaves it: no package of hostkeeper installed (only the binary
    /// directory, as `self-update-seed` makes it), the unit enabled by a `WantedBy=` link to the
    /// unit the deb installs ([`DEB_UNIT`]) that `placed.json` does not list, and the release of
    /// [`NEXT`] carrying the real unit, run ([`REAL_UNIT`], as CI packages it). The main unit is
    /// the bootstrap unit ([`BOOTSTRAP_UNIT`]) over a packaged binary named [`N`], and the binary
    /// directory is empty: the start seeds it. The coordinator orders it an `install`.
    pub deb: bool,
}

/// Where the deb installs the unit its `systemctl enable` links.
pub const DEB_UNIT: &str = "/usr/lib/systemd/system/hostkeeper.service";
/// The unit hostkeeper's release carries and runs.
pub const UNIT_FILE: &str = "hostkeeper.service";

/// One test's host (the module says what it is).
pub struct Host {
    pub rig: Rig,
    /// The main unit's name, without `.service`.
    pub main: String,
    /// The fallback unit's name, without `.service`.
    pub fallback: String,
    dropin_dir: PathBuf,
    scratch: Scratch,
}

impl Drop for Host {
    fn drop(&mut self) {
        // The fallback first: its `ExecStartPost=` starts the main unit.
        for args in self.rig.teardown() {
            let _ = run(SYSTEMCTL, &args);
        }
        let _ = fs::remove_dir_all(&self.dropin_dir);
        let _ = run(SYSTEMCTL, &["daemon-reload"]);
    }
}

/// `path` made a directory of mode `mode`.
fn dir(path: &Path, mode: u32) {
    fs::create_dir_all(path).unwrap_or_else(|e| panic!("make {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// `text` written at `path`, mode `mode`.
fn file(path: &Path, text: &[u8], mode: u32) {
    fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// A copy of the program `from` at `to`, mode 0755.
fn program(from: &str, to: &Path) {
    fs::copy(from, to).unwrap_or_else(|e| panic!("copy {from} to {}: {e}", to.display()));
    fs::set_permissions(to, fs::Permissions::from_mode(0o755)).expect("its mode");
}

/// The manifest of hostkeeper at `version`: its hook the workload fixture in mode `ok`, which
/// ends every step `0` with a valid last line; with `unit`, it runs [`UNIT_FILE`].
fn manifest(version: &str, unit: bool) -> String {
    let platform = Platform::current();
    let units = match unit {
        true => format!("\n[units]\nrun = [\"{UNIT_FILE}\"]\n"),
        false => String::new(),
    };
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
{units}"#
    )
}

/// The daemon config: every key the loader requires. Nothing reads its addresses: the
/// fixture's link, package store and enroller replace what would.
const CONFIG: &str = r#"coordinator = "https://coordinator.example"
ca_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// The directories of a host, as its main unit names them.
pub struct Paths {
    pub state: PathBuf,
    pub packages: PathBuf,
    pub config: PathBuf,
    pub socket: PathBuf,
    /// The directory of the packaged binary of a [`Plan::deb`] host.
    pub packaged_dir: PathBuf,
    /// The fixture's directory: `link/`, `units/` (the unit directory the daemon places
    /// units in), `placed/`.
    pub fixture: PathBuf,
    /// The file the daemon holds its exit at.
    pub held: PathBuf,
}

impl Paths {
    pub fn under(root: &Path) -> Paths {
        Paths {
            state: root.join("state"),
            packages: root.join("packages"),
            config: root.join("hostkeeper.toml"),
            socket: root.join("admin.sock"),
            packaged_dir: root.join("packaged"),
            fixture: root.join("fixture"),
            held: root.join("held"),
        }
    }

    /// The packaged binary of a [`Plan::deb`] host: named by the version it stands for.
    pub fn packaged(&self) -> PathBuf {
        self.packaged_dir.join(N)
    }

    /// The binary directory of hostkeeper's own update.
    pub fn bin(&self) -> PathBuf {
        self.state.join(HK).join("bin")
    }

    /// The `WantedBy=` link of [`UNIT_FILE`] in the fixture's unit directory.
    pub fn wants(&self) -> PathBuf {
        self.fixture
            .join("units/multi-user.target.wants")
            .join(UNIT_FILE)
    }
}

/// `text` as a path for a unit file: what `ExecStart=` reads back as itself.
fn plain(path: &Path) -> String {
    let text = path.to_str().expect("a UTF-8 path").to_owned();
    assert!(
        text.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(&b)),
        "{text:?} is not a plain path"
    );
    text
}

/// The real unit `real` (the package unit or the bootstrap unit) as the test's main unit (the
/// module's list), with `fixture_flags` before the verb.
pub fn test_unit(real: &str, paths: &Paths, fixture_flags: &[String]) -> String {
    let mut out = String::new();
    let mut exec_starts = 0;
    for line in real.lines() {
        let dropped = [
            "ExecStartPre=",
            "RuntimeDirectory=",
            "RuntimeDirectoryMode=",
        ]
        .iter()
        .any(|key| line.starts_with(key));
        if dropped {
            continue;
        }
        if let Some(description) = line.strip_prefix("Description=") {
            out.push_str(&format!(
                "Description=hostkeeper privileged test: {description}\n"
            ));
            continue;
        }
        let Some(command) = line.strip_prefix("ExecStart=") else {
            out.push_str(line);
            out.push('\n');
            continue;
        };
        exec_starts += 1;
        let words: Vec<&str> = command.split_whitespace().collect();
        let current = format!("{REAL_STATE_DIR}/{HK}/bin/current");
        let program = match words.first() {
            Some(first) if *first == current => plain(&paths.bin().join("current")),
            Some(&PACKAGED_BINARY) => plain(&paths.packaged()),
            _ => {
                panic!("the real unit starts neither `current` nor the packaged binary: {command}")
            }
        };
        assert_eq!(
            words.get(1),
            Some(&"daemon"),
            "the real unit starts `<program> daemon`: {command}"
        );
        let mut daemon = vec![program];
        daemon.extend(fixture_flags.iter().cloned());
        daemon.push("daemon".to_owned());
        let mut socket_placed = false;
        let mut rest = words[2..].iter();
        while let Some(flag) = rest.next() {
            let ours = match *flag {
                "--notify-systemd" => {
                    daemon.push((*flag).to_owned());
                    continue;
                }
                "--config" => &paths.config,
                "--state-dir" => &paths.state,
                "--packages-dir" => &paths.packages,
                "--admin-socket" => {
                    socket_placed = true;
                    &paths.socket
                }
                other => panic!(
                    "the real unit's ExecStart= has a flag this test does not place: {other}"
                ),
            };
            assert!(rest.next().is_some(), "{flag} has a value in the real unit");
            daemon.push((*flag).to_owned());
            daemon.push(plain(ours));
        }
        if !socket_placed {
            daemon.extend(["--admin-socket".to_owned(), plain(&paths.socket)]);
        }
        daemon.extend(["--heartbeat-secs", "1"].map(str::to_owned));
        out.push_str(&format!("ExecStart={}\n", daemon.join(" ")));
    }
    assert_eq!(exec_starts, 1, "the real unit has one ExecStart= line");
    out
}

impl Host {
    /// The host of the test `test` (lowercase letters, digits, `_`): hostkeeper [`N`] installed
    /// (its package `current`, its binary staged and `current` of the binary directory; with
    /// [`Plan::deb`], its binary alone), the release of [`NEXT`] in the package store, the units linked and the drop-in in place, the
    /// main unit started under `plan` and ready (its start returns once the daemon notified
    /// systemd).
    pub fn start(test: &str, plan: Plan) -> Host {
        let root = rustix::process::geteuid().is_root();
        if let Err(e) = hostkeeper_privileged::check_host(
            root,
            Path::new(hostkeeper_privileged::SYSTEMD_RUNTIME_DIR),
        ) {
            panic!("{e}");
        }
        dir(Path::new(BASE), 0o755);
        let scratch = Scratch::under(Path::new(BASE), test).expect("create the scratch directory");
        dir(scratch.path(), 0o755);
        let rig = Rig::new(scratch.path(), Path::new(FIXTURE), test, std::process::id())
            .expect("make the rig");
        // Named in this order, torn down in this order.
        let fallback = rig.unit();
        let main = rig.unit();
        let dropin_dir = Path::new(RUNTIME_UNIT_DIR).join(format!("{}.d", service_unit(&main)));
        let host = Host {
            rig,
            main,
            fallback,
            dropin_dir,
            scratch,
        };
        host.make(plan);
        run_ok(SYSTEMCTL, &["start", "--system", &service_unit(&host.main)]);
        host
    }

    pub fn paths(&self) -> Paths {
        Paths::under(self.scratch.path())
    }

    /// Everything [`Host::start`] puts in place before the start.
    fn make(&self, plan: Plan) {
        let paths = self.paths();
        dir(&paths.state, 0o700);
        dir(&paths.state.join(HK), 0o700);
        dir(&paths.packages, 0o755);
        for sub in ["link", "units", "placed"] {
            dir(&paths.fixture.join(sub), 0o755);
        }
        file(&paths.config, CONFIG.as_bytes(), 0o644);
        let service = paths.packages.join(HK);
        dir(&service, 0o755);
        if plan.deb {
            self.release(&service, NEXT, true);
            let wants = paths.wants();
            dir(wants.parent().expect("the wants directory"), 0o755);
            std::os::unix::fs::symlink(DEB_UNIT, &wants).expect("the deb's enable link");
        } else {
            for version in [N, NEXT] {
                self.release(&service, version, false);
            }
            std::os::unix::fs::symlink(N, service.join("current")).expect("link current");
        }
        let layout = Layout::new(&paths.bin());
        if plan.deb {
            // The binary directory is left as it is: the packaged binary seeds it.
            dir(&paths.packaged_dir, 0o755);
            program(DAEMON, &paths.packaged());
        } else {
            layout
                .stage(N, Path::new(DAEMON))
                .expect("stage the first binary");
            layout.switch(N).expect("switch to it");
        }

        let main = service_unit(&self.main);
        // The daemon's observer watches its own unit, the test's, as the service `hostkeeper`.
        let mut flags = vec![
            "--fixture-dir".to_owned(),
            plain(&paths.fixture),
            "--fixture-own-unit".to_owned(),
            main.clone(),
        ];
        if plan.next_fails {
            flags.extend(["--fixture-fail-start".to_owned(), NEXT.to_owned()]);
        }
        if plan.hold_exit {
            flags.extend(["--fixture-hold-exit".to_owned(), plain(&paths.held)]);
        }
        if plan.deb {
            flags.extend(["--fixture-packaged".to_owned(), plain(&paths.packaged())]);
        }
        let real = if plan.deb { BOOTSTRAP_UNIT } else { REAL_UNIT };
        let fallback = service_unit(&self.fallback);
        let units = [
            (&fallback, fallback_unit(&layout, &main)),
            (&main, test_unit(real, &paths, &flags)),
        ];
        for (name, text) in units {
            let path = self.scratch.path().join(name);
            file(&path, text.as_bytes(), 0o644);
            run_ok(SYSTEMCTL, &["link", "--system", &plain(&path)]);
        }
        dir(&self.dropin_dir, 0o755);
        file(
            &self.dropin_dir.join(DROPIN_FILE),
            main_dropin(&fallback, START_LIMIT_BURST).as_bytes(),
            0o644,
        );
        run_ok(SYSTEMCTL, &["daemon-reload"]);
    }

    /// The release of hostkeeper at `version` in the package directory `service`, as the real
    /// store leaves it: the tree (manifest, the hook, the binary and, with `unit`, the real unit
    /// as `units/hostkeeper.service`), then its seal beside it.
    fn release(&self, service: &Path, version: &str, unit: bool) {
        let tree = service.join(version);
        dir(&tree, 0o755);
        dir(&tree.join("bin"), 0o755);
        file(
            &tree.join("manifest.toml"),
            manifest(version, unit).as_bytes(),
            0o644,
        );
        if unit {
            dir(&tree.join("units"), 0o755);
            file(
                &tree.join("units").join(UNIT_FILE),
                REAL_UNIT.as_bytes(),
                0o444,
            );
        }
        program(FIXTURE, &tree.join("bin/hook"));
        program(DAEMON, &tree.join("bin/hostkeeper"));
        let digest = tree_digest(&tree).expect("the tree digest");
        let seal = Seal::new(&digest, DIGEST).expect("a seal");
        file(
            &service.join(format!("{version}.seal")),
            seal.to_json().as_bytes(),
            0o644,
        );
    }

    /// The coordinator orders the update of hostkeeper to [`NEXT`], order [`ORDER`] try
    /// `order_try`, in every reply from now on.
    pub fn offer(&self, order_try: u64) {
        self.order("update", order_try);
    }

    /// The coordinator orders the install of hostkeeper at [`NEXT`], as it orders a host whose
    /// heartbeat lists no hostkeeper, order [`ORDER`] try `order_try`, in every reply from now
    /// on.
    pub fn offer_install(&self, order_try: u64) {
        self.order("install", order_try);
    }

    fn order(&self, verb: &str, order_try: u64) {
        let link = self.paths().fixture.join("link");
        let order = serde_json::json!({
            "id": ORDER,
            "try": order_try,
            "verb": verb,
            "service": HK,
            "version": NEXT,
            "digest": DIGEST,
        });
        let tmp = link.join("order.json.tmp");
        file(&tmp, order.to_string().as_bytes(), 0o644);
        fs::rename(&tmp, link.join("order.json")).expect("place the order");
    }

    /// The binary directory.
    pub fn layout(&self) -> Layout {
        Layout::new(&self.paths().bin())
    }

    /// The version `current` of the binary directory names.
    pub fn current(&self) -> Option<String> {
        self.layout().current().expect("read current")
    }

    /// The step record of hostkeeper, when there is one this daemon reads.
    pub fn record(&self) -> Option<Record> {
        match RecordStore::new(&self.paths().state.join(HK)).load() {
            Ok(Loaded::Current(rec)) => Some(*rec),
            Ok(Loaded::Absent | Loaded::Newer { .. }) => None,
            Err(e) => panic!("the step record cannot be read: {e}"),
        }
    }

    /// The bytes of the step record file.
    pub fn record_bytes(&self) -> Vec<u8> {
        fs::read(self.paths().state.join(HK).join(RECORD_FILE)).expect("the step record")
    }

    /// Whether [`NEXT`] is on the local list of bad versions.
    pub fn next_is_bad(&self) -> bool {
        BadVersions::load(&self.paths().state.join(HK))
            .expect("the list of bad versions")
            .contains(NEXT)
    }

    /// Every heartbeat the daemon sent so far, oldest first. A last line still being written
    /// is left out.
    pub fn beats(&self) -> Vec<Value> {
        let path = self.paths().fixture.join("link/beats.jsonl");
        let text = fs::read_to_string(path).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The newest verdict the heartbeats reported for try `order_try` of [`ORDER`]: hostkeeper's
    /// `last_update`.
    pub fn verdict_of(&self, order_try: u64) -> Option<Value> {
        self.beats().into_iter().rev().find_map(|beat| {
            beat["services"]
                .as_array()?
                .iter()
                .filter(|service| service["name"] == HK)
                .map(|service| service["last_update"].clone())
                .find(|result| result["order_id"] == ORDER && result["order_try"] == order_try)
        })
    }

    /// Fails at once, with the journal of both units, when what a wait waits for can no longer
    /// come: an order of hostkeeper answered with a refusal (any failure but the wait for the
    /// restart and the fallback's); the main unit failed and the fallback neither queued nor ever
    /// started (no `OnFailure=` reached it); or the fallback failed.
    pub fn check_not_stuck(&self) {
        for order_try in [1, 2] {
            if let Some(result) = self.verdict_of(order_try) {
                let reason = result["reason"].as_str().unwrap_or_default();
                let refused = (result["outcome"] == "failed")
                    && ![AWAITING_REASON, FALLBACK_REASON].contains(&reason);
                assert!(
                    !refused,
                    "the order was refused: {result}\n{}",
                    self.journals()
                );
            }
        }
        // Read in this order. Once the main unit is failed, its `OnFailure=` job is already
        // queued; a job no longer queued has started the fallback, which then has an invocation
        // id. So a failed main unit, then no job, then no invocation id: no job was ever queued.
        let main_failed = property(&self.main, "ActiveState") == "failed";
        let queued = !run_ok(
            SYSTEMCTL,
            &[
                "list-jobs",
                "--no-legend",
                "--plain",
                &service_unit(&self.fallback),
            ],
        )
        .trim()
        .is_empty();
        let fallback = super::show(&self.fallback, &["ActiveState", "InvocationID"]);
        let never_ran = fallback.get("InvocationID").is_none_or(String::is_empty);
        assert!(
            !(main_failed && never_ran && !queued),
            "the main unit failed and no OnFailure= fallback was started\n{}",
            self.journals()
        );
        assert_ne!(
            fallback.get("ActiveState").map(String::as_str),
            Some("failed"),
            "the fallback unit failed\n{}",
            self.journals()
        );
    }

    /// The journal of the main unit and of the fallback, for a failure's message.
    pub fn journals(&self) -> String {
        format!(
            "--- {}:\n{}\n--- {}:\n{}",
            self.main,
            journal(&self.main),
            self.fallback,
            journal(&self.fallback)
        )
    }

    /// Waits until the step record of hostkeeper is finished, and gives it.
    pub fn settled(&self) -> Record {
        let mut found = None;
        wait_until(|| {
            found = self
                .record()
                .filter(|rec| matches!(rec.state, State::Finished { .. }));
            if found.is_none() {
                self.check_not_stuck();
            }
            found.is_some()
        });
        found.expect("a finished record")
    }

    /// Waits until the main unit is active: its daemon told systemd it is ready.
    pub fn wait_active(&self) {
        wait_until(|| {
            self.check_not_stuck();
            property(&self.main, "ActiveState") == "active"
        });
    }

    /// The pid of the main unit's main process.
    pub fn main_pid(&self) -> u32 {
        let pid = property(&self.main, "ExecMainPID");
        pid.parse()
            .unwrap_or_else(|_| panic!("ExecMainPID is {pid:?}"))
    }

    /// What the main unit's main process runs: its `/proc/<pid>/exe` link.
    pub fn main_exe(&self) -> PathBuf {
        let pid = self.main_pid();
        fs::read_link(format!("/proc/{pid}/exe")).unwrap_or_else(|e| panic!("/proc/{pid}/exe: {e}"))
    }

    /// Where the binary directory keeps `version`.
    pub fn binary_of(&self, version: &str) -> PathBuf {
        self.layout().version_path(version)
    }

    /// Waits until the main unit is active, then checks that it runs the binary of `version`
    /// and that `current` names it.
    pub fn runs(&self, version: &str) {
        self.wait_active();
        assert_eq!(
            self.main_exe(),
            self.binary_of(version),
            "{}",
            self.journals()
        );
        assert_eq!(self.current().as_deref(), Some(version));
    }

    /// Runs the self-update to the failing [`NEXT`] (a host started with `next_fails`) until
    /// the fallback has put [`N`] back and the main unit runs it again.
    pub fn fall_back(&self) -> Record {
        self.offer(1);
        let rec = self.settled();
        self.runs(N);
        rec
    }
}

/// The line the daemon program writes when `--fixture-fail-start` refuses its start at
/// `version` (the program's `refused_line`).
pub fn refused_line(version: &str) -> String {
    format!(
        "hostkeeper-selfupdate-daemon: version {version} refuses to start (--fixture-fail-start)"
    )
}

/// How many times `needle` is in `haystack`.
pub fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}
