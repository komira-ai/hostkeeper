//! The host of one self-update test: hostkeeper's real unit file under the host's systemd,
//! running the binary directory's `current` of a state directory the test owns, with the
//! `.deb`s of hostkeeper in a signed apt repository the test builds and a coordinator the test
//! writes (the program `hostkeeper-selfupdate-daemon` says what it replaces and why).
//!
//! **hostkeeper's package.** The next version is a native package, as hostkeeper's own release
//! is (docs/design/packages-native.md, section 10.4): its `.deb` (the binary of the program under
//! test at `usr/bin/hostkeeper`, and the package unit) is in the test's repository, and the order
//! carries its `apt` entry ([`Host::entry`]). The daemon stages it from the repository, hands over
//! to its binary, and the new binary, once it confirmed itself, installs the package with apt
//! (the host's own dpkg database, purged when the test ends; the enablement links of its `run`
//! unit go under the fixture's root, not the host's).
//!
//! **Where.** The release a self-update stages from must be a tree whose every directory, up to
//! `/`, is root's and closed to group and others (`hostkeeper_steps::Staged::check`). The target
//! directory is the runner's user's, so the host lives in a scratch directory under [`BASE`]
//! (`/run`, root's tmpfs), removed when the test ends, with the unit under a unit name of the
//! test's rig.
//!
//! **The units.**
//! - The main unit is hostkeeper's package unit, `packaging/linux/units/hostkeeper.service`
//!   ([`REAL_UNIT`]), or, for [`Plan::deb`], the bootstrap unit the deb installs,
//!   `packaging/linux/hostkeeper.service` ([`BOOTSTRAP_UNIT`]), every line kept but these
//!   ([`test_unit`]): `ExecStart=` runs the same program and flags with the test's directories,
//!   the fixture's own flags before the verb and `--heartbeat-secs 1` after it; the program is
//!   `current` of the test's binary directory for the package unit, and for the bootstrap unit a
//!   copy of the fixture daemon at a packaged path of the scratch directory, named by the version
//!   [`N`] it stands for (`--fixture-packaged`: the packaged binary seeds the empty binary
//!   directory and execs `current`).
//!   A change to the unit's `ExecStart=` that this does not know fails the test by name.
//! - There is no fallback unit and no drop-in: the hand-over has neither
//!   (docs/design/two-programs.md, section 4.3), and the unit has no start limit, so a daemon that
//!   cannot start is retried every `RestartSec=1`, never left failed.
//!
//! The unit file is linked from the scratch directory (`systemctl link`); dropping the [`Host`]
//! stops, unlinks and resets it, purges the package, reloads systemd and removes the scratch
//! directory, failed or not.
//!
//! Every wait polls systemd or the files for a state ([`support::wait_until`]) and fails at once
//! when the state can no longer come: an order refused, or the main unit failed
//! ([`Host::check_not_stuck`]).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use hostkeeper_daemon::target;
use hostkeeper_privileged::Rig;
use hostkeeper_privileged::rig::service_unit;
use hostkeeper_proto::hostkeeper::release::v1::{StepBounds, Target};
use hostkeeper_steps::record::{Loaded, RECORD_FILE, Record, State};
use hostkeeper_steps::systemd::SYSTEMCTL;
use hostkeeper_steps::{BadVersions, Layout, RecordStore, Seal, tree_digest};
use hostkeeper_testkit::Scratch;
use serde_json::Value;

use super::aptrepo::{self, AptRepo, Deb, File, Published, Service};
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
/// The service of hostkeeper itself.
pub const HK: &str = "hostkeeper";
/// The version the host runs first.
pub const N: &str = "1.0.0";
/// The version it is ordered to.
pub const NEXT: &str = "1.0.1";
/// The name of the host's repository entry of hostkeeper's `.deb`s.
const REPOSITORY: &str = "hostkeeper-su";
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
    /// The daemon holds its exit after the follow-on handed over (`--fixture-hold-exit`).
    pub hold_exit: bool,
    /// The daemon of [`NEXT`] holds its start after the hand-over exec'd it, before it settles
    /// the update (`--fixture-hold-start`).
    pub hold_start: bool,
    /// The host is as the deb leaves it: no package of hostkeeper installed and no version of it
    /// in the packages directory (only the binary directory, as the packaged start seeds it). The
    /// main unit is the bootstrap unit ([`BOOTSTRAP_UNIT`]) over a packaged binary named [`N`], and
    /// the binary directory is empty: the start seeds it. The coordinator orders it an `install`.
    pub deb: bool,
}

/// Where the deb installs the unit of hostkeeper's package.
pub const DEB_UNIT: &str = "/usr/lib/systemd/system/hostkeeper.service";
/// The unit hostkeeper's release carries and runs.
pub const UNIT_FILE: &str = "hostkeeper.service";
/// The package of hostkeeper, as dpkg names it.
pub const PACKAGE: &str = "hostkeeper";

/// One test's host (the module says what it is).
pub struct Host {
    pub rig: Rig,
    /// The main unit's name, without `.service`.
    pub main: String,
    scratch: Scratch,
    /// The apt repository of hostkeeper's `.deb`s, which the daemon's repository entry names.
    repo: AptRepo,
    /// The entry of the release of [`NEXT`] that the orders carry.
    entry: Target,
}

impl Drop for Host {
    fn drop(&mut self) {
        for args in self.rig.teardown() {
            let _ = run(SYSTEMCTL, &args);
        }
        // A package a stopped step left half configured is finished first, so that the purge
        // can run.
        let _ = run("/usr/bin/dpkg", &["--configure", "-a"]);
        let _ = run("/usr/bin/apt-mark", &["unhold", PACKAGE]);
        let _ = run("/usr/bin/dpkg", &["--purge", PACKAGE]);
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

/// The `.deb` of hostkeeper at `version`: the binary of the program under test and the package
/// unit, the files of hostkeeper's own package the test needs (the daemon's own list is
/// `hostkeeper_install::repository::tree::OWN_FILES`).
fn deb(version: &str) -> Deb {
    Deb {
        package: PACKAGE.to_owned(),
        version: version.to_owned(),
        files: vec![
            File::new(
                "usr/bin/hostkeeper",
                fs::read(DAEMON).unwrap_or_else(|e| panic!("read {DAEMON}: {e}")),
                0o755,
            ),
            File::new(
                "usr/lib/systemd/system/hostkeeper.service",
                REAL_UNIT,
                0o644,
            ),
        ],
    }
}

/// The entry of hostkeeper's `.deb` `published`, as `release targets` writes it: the repository,
/// the package version, the pin, and a manifest with the package unit as its `run` unit and the
/// bounds of the steps (hostkeeper's package has no hook: the steps are the built-in ones).
fn entry_of(published: &Published) -> Target {
    let service = Service {
        repository: REPOSITORY.to_owned(),
        run: vec![UNIT_FILE.to_owned()],
        health_url: None,
        params: std::collections::BTreeMap::new(),
        steps: StepBounds {
            prepare: 600,
            cutover: 300,
            verify: 1000,
            rollback: 600,
            cleanup: 1300,
            ..StepBounds::default()
        },
        verify_deadline_secs: 1600,
    };
    aptrepo::entry(&service, published)
}

/// The daemon config: every key the loader requires. Nothing reads its addresses: the
/// fixture's link and enroller replace what would.
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
    /// The directory of the packaged binary of a [`Plan::deb`] host.
    pub packaged_dir: PathBuf,
    /// The fixture's directory: `link/`, `repositories/` (the daemon's repository entries),
    /// `root/` (where the installer enables the package's `run` units), `units/` (the unit
    /// directory of the placement roots), `placed/`.
    pub fixture: PathBuf,
    /// The file the daemon holds its exit at.
    pub held: PathBuf,
    /// The file the daemon of [`NEXT`] holds its start at.
    pub held_start: PathBuf,
}

impl Paths {
    pub fn under(root: &Path) -> Paths {
        Paths {
            state: root.join("state"),
            packages: root.join("packages"),
            config: root.join("hostkeeper.toml"),
            packaged_dir: root.join("packaged"),
            fixture: root.join("fixture"),
            held: root.join("held"),
            held_start: root.join("held-start"),
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
        let dropped = ["RuntimeDirectory=", "RuntimeDirectoryMode="]
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
                other => panic!(
                    "the real unit's ExecStart= has a flag this test does not place: {other}"
                ),
            };
            assert!(rest.next().is_some(), "{flag} has a value in the real unit");
            daemon.push((*flag).to_owned());
            daemon.push(plain(ours));
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
    /// [`Plan::deb`], its binary alone), the `.deb` of [`NEXT`] in the repository the daemon's
    /// entry names, the unit linked, the main unit started under `plan` and ready (its start
    /// returns once the daemon notified systemd).
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
        let main = rig.unit();
        let repo = AptRepo::new(scratch.path(), "hostkeeper-su");
        let published_next = repo.publish(&deb(NEXT));
        let entry = entry_of(&published_next);
        let host = Host {
            rig,
            main,
            scratch,
            repo,
            entry,
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
        for sub in ["link", "units", "placed", "root"] {
            dir(&paths.fixture.join(sub), 0o755);
        }
        self.repo
            .install_entry_in(&paths.fixture.join("repositories"), REPOSITORY);
        file(&paths.config, CONFIG.as_bytes(), 0o644);
        let service = paths.packages.join(HK);
        dir(&service, 0o755);
        if !plan.deb {
            // The release the host runs: a version of the package as the daemon leaves one, its
            // entry and its tree, `current` naming it. The package itself is not installed.
            self.release(&service, N, &self.repo.publish(&deb(N)));
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
        if plan.hold_start {
            flags.extend([
                "--fixture-hold-start".to_owned(),
                format!("{NEXT}={}", plain(&paths.held_start)),
            ]);
        }
        if plan.deb {
            flags.extend(["--fixture-packaged".to_owned(), plain(&paths.packaged())]);
        }
        let real = if plan.deb { BOOTSTRAP_UNIT } else { REAL_UNIT };
        let path = self.scratch.path().join(&main);
        file(&path, test_unit(real, &paths, &flags).as_bytes(), 0o644);
        run_ok(SYSTEMCTL, &["link", "--system", &plain(&path)]);
        run_ok(SYSTEMCTL, &["daemon-reload"]);
    }

    /// The release of hostkeeper at `version` in the package directory `service`, as the daemon
    /// leaves a staged version: the entry it was staged for (`target.json`) and the tree of its
    /// package (the binary), then its seal beside it, naming the package's pin.
    fn release(&self, service: &Path, version: &str, published: &Published) {
        let tree = service.join(version);
        dir(&tree, 0o755);
        target::write(&tree, &entry_of(published)).expect("the entry of the version");
        for sub in ["tree", "tree/usr", "tree/usr/bin"] {
            dir(&tree.join(sub), 0o755);
        }
        program(DAEMON, &tree.join("tree/usr/bin/hostkeeper"));
        let digest = tree_digest(&tree).expect("the tree digest");
        let seal = Seal::new(&digest, &published.sha256).expect("a seal");
        file(
            &service.join(format!("{version}.seal")),
            seal.to_json().as_bytes(),
            0o644,
        );
    }

    /// The coordinator orders the update of hostkeeper to [`NEXT`] by its entry, order [`ORDER`]
    /// try `order_try`, in every reply from now on.
    pub fn offer(&self, order_try: u64) {
        self.order("update", order_try);
    }

    /// The coordinator orders the install of hostkeeper at [`NEXT`] by its entry, as it orders a
    /// host whose heartbeat lists no hostkeeper, order [`ORDER`] try `order_try`, in every reply
    /// from now on.
    pub fn offer_install(&self, order_try: u64) {
        self.order("install", order_try);
    }

    /// The entry of the release of hostkeeper at [`NEXT`] that an order carries: the `apt` entry
    /// of its `.deb` in the host's repository.
    pub fn entry(&self) -> &Target {
        &self.entry
    }

    /// The entry the daemon staged the version [`NEXT`] of hostkeeper for (its sealed
    /// `target.json`), `None` while it has not staged it.
    pub fn staged_entry(&self) -> Option<Target> {
        target::read(&self.paths().packages.join(HK).join(NEXT)).expect("target.json")
    }

    fn order(&self, verb: &str, order_try: u64) {
        let link = self.paths().fixture.join("link");
        let entry = target::to_json(&self.entry).expect("the entry as JSON");
        let order = serde_json::json!({
            "id": ORDER,
            "try": order_try,
            "verb": verb,
            "service": HK,
            "version": NEXT,
            "digest": self.entry.digest,
            "entry": serde_json::from_slice::<Value>(&entry).expect("the entry's JSON"),
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

    /// The version of the package [`PACKAGE`] when dpkg has it installed whole, `None` otherwise.
    pub fn package_version(&self) -> Option<String> {
        let out = run(
            "/usr/bin/dpkg-query",
            &["-W", "-f=${Version}\\t${db:Status-Abbrev}\\n", PACKAGE],
        );
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let (version, status) = text
            .trim_end()
            .split_once('\t')
            .filter(|_| out.status.success())?;
        // The abbreviation is the wanted state, the state and the error: `hi ` is installed and
        // held.
        (status.as_bytes().get(1) == Some(&b'i')).then(|| version.to_owned())
    }

    /// Whether the package [`PACKAGE`] is on apt's list of held packages.
    pub fn package_held(&self) -> bool {
        let out = run("/usr/bin/apt-mark", &["showhold"]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line == PACKAGE)
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

    /// Fails at once, with the journal of the unit, when what a wait waits for can no longer
    /// come: an order of hostkeeper answered with a refusal (any failure but the wait for the
    /// restart and the fallback's), or the main unit failed (it has no start limit, so it is
    /// retried for good: a failed unit is a daemon that systemd gave up on).
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
        assert_ne!(
            property(&self.main, "ActiveState"),
            "failed",
            "the main unit failed\n{}",
            self.journals()
        );
    }

    /// The journal of the main unit, for a failure's message.
    pub fn journals(&self) -> String {
        format!("--- {}:\n{}", self.main, journal(&self.main))
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
    /// the old binary has settled the update failed after [`NEXT`] failed its tries, and the main
    /// unit runs [`N`] again.
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
