//! The Mac of one self-update test of `tests/device_mac_selfupdate.rs`: the program
//! `hostkeeper-selfupdate-daemon` (its module says what it replaces and why) run as a launchd job
//! of the system domain from `current` of a binary directory the test owns, with the `.pkg`
//! files of hostkeeper's releases served to it by their pins and a coordinator the test writes
//! (`link/order.json`, read at each heartbeat; `link/beats.jsonl`, each heartbeat sent), or the
//! real one ([`Plan::Pkg`] with `--fixture-link coordinator`, see [`MacSelfHost::new_coordinated`]).
//!
//! **hostkeeper's package.** A release of hostkeeper on a Mac is a native package, its signed
//! `.pkg` (docs/design/packages-native.md, section 10.4): the binary of the program under test at
//! `/usr/local/bin/hostkeeper` (the program's own path on a Mac, where the pkg's launchd plist
//! starts it), nothing else. The test builds one `.pkg` of it for each of [`N`] and [`NEXT`] with
//! the Mac's `pkgbuild` and `productbuild` (`shared/pkg_build.rs`), serves it to the daemon by
//! its pin (`shared/macos_pkg.rs` says how the daemon fetches it and how its signature is judged),
//! and orders it by the one `macos-pkg` entry `release targets` writes ([`MacSelfHost::entry`]).
//! The daemon stages it, hands over to the binary of its tree (`tree/usr/local/bin/hostkeeper`),
//! and the new binary, once it confirmed itself, has the installer install the package: the Mac's
//! `installer` puts the file at `/usr/local/bin/hostkeeper`, the receipt is `pkgutil`'s, and the
//! daemon's record is its own `placed.json` (the fixture's, not the host's).
//!
//! **Where.** The release a self-update stages from must be a tree whose every directory, up to
//! `/`, is root's and closed to group and others (`hostkeeper_steps::Staged::check`), and every
//! file in it root's: the target directory is the runner's user's, so the host lives under
//! `/var/hk-su-<tag>-<pid>` (a link to `/private/var`, as the daemon's default directories are
//! reached), and the programs are written into it byte for byte (`fs::copy` on macOS clones a
//! file and keeps the owner of its source). The package installs at `/usr/local/bin/`, a host
//! path: the test moves nothing aside (the host has no hostkeeper installed, which it checks),
//! makes the directories above it ones only root can write when the runner's are not (the
//! daemon refuses to install through any other), and the drop removes the file, forgets the
//! receipt and removes the directories the install made.
//!
//! **The job.** Its plist is the daemon's own (`hostkeeper_launchd::plist::daemon_plist`:
//! `RunAtLoad` and plain `KeepAlive` true, so that the clean exit the follow-on asks for starts
//! the program again), with the fixture's flags before the verb and the test's directories and
//! `--heartbeat-secs 1` after it, its output in two files of the test's tree. Its program is
//! `current` of the test's binary directory ([`Plan::Installed`]) or, as the pkg's plist names the
//! binary the pkg installed (`DaemonJob::standard`'s program, `/usr/local/bin/hostkeeper`), a copy
//! of the fixture daemon at a packaged path of the test's tree, named by the version it stands
//! for ([`Plan::Pkg`]; the fixture flag `--fixture-packaged` makes that path the packaged binary
//! of the daemon's start). The binary directory of a [`Plan::Pkg`] Mac is empty: the packaged
//! binary seeds it and execs `current`, in the process launchd started, so the job's process
//! runs the binary directory's file of [`N`] after the start.
//!
//! **An installed Mac.** [`Plan::Installed`] starts the daemon of [`N`] with its binary
//! directory made and no package of hostkeeper installed, as the pkg leaves a Mac, and has the
//! package installed the way a Mac gets it: the coordinator (the file) orders the install of the
//! version that runs, which is `Succeeded` at once and installs the package without a restart
//! (the order [`SETUP_ORDER`]); the update the test offers next finds hostkeeper installed.
//!
//! **The fallback.** The hand-over has no fallback job (docs/design/two-programs.md, section
//! 4.3): a next binary that cannot start is execed three times by the old binary, which then
//! settles the update failed ([`Plan::InstalledFailingNext`], [`MacSelfHost::refused_starts`]),
//! under the same launchd the job runs in.
//!
//! Dropping the host boots out the daemon's job, prints the daemon's output when the test is
//! failing, and removes the tree and what the packages put on the host.
//!
//! **The coordinator.** By default the test writes it ([`MacSelfHost::offer`]); a test may
//! answer instead with the real coordinator in its process (`e2e::coordinator`), which the
//! daemon enrolls with and beats to over mutual TLS ([`MacSelfHost::new_coordinated`]). The
//! daemon's observer watches the test's job as the service `hostkeeper` (the fixture's
//! `--fixture-own-unit`), as a Mac's daemon watches its own job `dev.hostkeeper.hostkeeper`.
//!
//! Every wait reads a state a bounded number of times ([`wait`]) and fails at once when the state
//! can no longer come (an order refused, [`MacSelfHost::check_not_stuck`]); nothing asserts how
//! long anything took.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_config::platform::Platform;
use hostkeeper_daemon::target;
use hostkeeper_hook::Verdict;
use hostkeeper_install::macos_pkg::receipt;
use hostkeeper_launchd::launchctl::{JobStatus, bootout_args, bootstrap_args, print};
use hostkeeper_launchd::procid::darwin::DarwinPidInfo;
use hostkeeper_launchd::{DaemonJob, Label, Launchctl, LaunchctlProcess, PidInfo, daemon_plist};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Location, Manifest, StepBounds, Target, TargetList,
};
use hostkeeper_steps::record::{Loaded, Record, State};
use hostkeeper_steps::{BadVersions, Layout, RecordStore};
use serde_json::Value;

use crate::e2e::macos_pkg::{self, PKGS_DIR, SIGNERS_FILE, signers_file_text};
use crate::e2e::pkg_build::{self, PkgFile, PkgSource};

/// The daemon program of these tests.
pub const DAEMON: &str = env!("CARGO_BIN_EXE_hostkeeper-selfupdate-daemon");
/// The service of hostkeeper itself.
pub const HK: &str = "hostkeeper";
/// The package identifier of hostkeeper's `.pkg`, the name `pkgutil` knows its receipt by.
pub const IDENTIFIER: &str = "dev.hostkeeper.ci-selfupdate";
/// The version the host runs first.
pub const N: &str = "1.0.0";
/// The version it is ordered to.
pub const NEXT: &str = "1.0.1";
/// Where hostkeeper's `.pkg` installs the program: the path the pkg's launchd plist starts.
pub const INSTALLED: &str = "/usr/local/bin/hostkeeper";
/// The order id of the self-update.
pub const ORDER: &str = "self-update-1";
/// The order id of the install of the version that runs which makes a Mac an installed one.
pub const SETUP_ORDER: &str = "setup-install-1";
/// The reason of the verdict of an update whose new binary has not run yet.
const AWAITING_REASON: &str = "awaiting_restart";
/// The reason of the verdict of a version whose new binary failed its tries.
pub const FALLBACK_REASON: &str = "self_update_fallback";
/// How many times a wait reads its state, and the pause between two reads.
const READS: u32 = 1500;
const PAUSE: Duration = Duration::from_millis(200);
/// The deadline of `pkgutil` and `chown` on the host.
const TOOL_DEADLINE: Duration = Duration::from_secs(120);

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

/// The line the daemon program writes to standard error when `--fixture-fail-start` refuses its
/// start at `version` (the program's `refused_line`).
pub fn refused_line(version: &str) -> String {
    format!(
        "hostkeeper-selfupdate-daemon: version {version} refuses to start (--fixture-fail-start)"
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

/// The version of the receipt of hostkeeper's package; `None` when `pkgutil` knows none.
pub fn receipt_version() -> Option<String> {
    let args = ["--pkg-info-plist".to_owned(), IDENTIFIER.to_owned()];
    let out = LaunchctlProcess::new(Path::new("/usr/sbin/pkgutil"), TOOL_DEADLINE)
        .run(&args)
        .unwrap_or_else(|e| panic!("pkgutil did not run to its end: {e}"));
    if out.code != Some(0) {
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            said.contains(receipt::NO_RECEIPT),
            "an unexpected pkgutil failure: {said}"
        );
        return None;
    }
    Some(
        receipt::parse_pkg_info(&out.stdout)
            .expect("pkgutil's receipt")
            .version,
    )
}

/// The daemon config: every key the loader requires. Nothing reads its addresses on the file
/// link: the fixture's link and package store replace what would.
const CONFIG: &str = r#"coordinator = "https://coordinator.example"
ca_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "komira-ai"
workflow = ".github/workflows/release.yml"
refs = ["refs/tags/*"]
hosted_runner = true
"#;

/// The directories above [`INSTALLED`] that the host may lack, outermost first.
const PARENTS: [&str; 2] = ["/usr/local", "/usr/local/bin"];

/// How the host is when the daemon starts.
#[derive(Clone, Copy, Debug)]
pub enum Plan {
    /// hostkeeper's package at [`N`] installed (the install of the version that runs, ordered as
    /// the module says), the releases of [`N`] and [`NEXT`] served: the coordinator orders an
    /// `update`.
    Installed,
    /// [`Plan::Installed`], with the daemon of [`NEXT`] refusing to start
    /// (`--fixture-fail-start`): a next binary that cannot start.
    InstalledFailingNext,
    /// As the pkg leaves a Mac: the job's program the packaged binary (a copy of the fixture
    /// daemon named [`N`]) and the binary directory empty, no package of hostkeeper installed,
    /// the releases of [`N`] and [`NEXT`] served: the coordinator orders an `install`.
    Pkg,
}

/// One release of hostkeeper's package, served.
struct Release {
    /// The entry of an order for it.
    entry: Target,
}

/// One test's Mac (the module says what it is).
pub struct MacSelfHost {
    base: PathBuf,
    label: Label,
    /// The releases served, by version.
    releases: BTreeMap<&'static str, Release>,
    /// The directories of [`PARENTS`] that were not on the host before the test, which the drop
    /// removes when the installs left them empty.
    made: Vec<&'static str>,
    /// Whether the job's program is the real coordinator's link (`--fixture-link coordinator`).
    coordinated: bool,
}

impl Drop for MacSelfHost {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("{}", self.logs());
        }
        let _ = launchctl().run(&bootout_args(&self.label));
        let forget = ["--forget".to_owned(), IDENTIFIER.to_owned()];
        let _ = LaunchctlProcess::new(Path::new("/usr/sbin/pkgutil"), TOOL_DEADLINE).run(&forget);
        let _ = fs::remove_file(INSTALLED);
        for parent in self.made.iter().rev() {
            let _ = fs::remove_dir(parent);
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The directories above [`INSTALLED`] made fit for the daemon's install: a directory only root
/// writes. The hosted runner may own `/usr/local/bin` itself, which the daemon refuses to install
/// through (`unsafe_directory`); this makes it root's. Gives the directories that are not there.
fn prepare_parents() -> Vec<&'static str> {
    let mut missing = Vec::new();
    for parent in PARENTS {
        match fs::metadata(parent) {
            Ok(meta) => {
                if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                    std::os::unix::fs::chown(parent, Some(0), Some(0))
                        .unwrap_or_else(|e| panic!("chown {parent}: {e}"));
                    fs::set_permissions(
                        parent,
                        fs::Permissions::from_mode(meta.mode() & 0o7777 & !0o022),
                    )
                    .unwrap_or_else(|e| panic!("chmod {parent}: {e}"));
                }
            }
            Err(_) => missing.push(parent),
        }
    }
    missing
}

/// The manifest's bounds of hostkeeper's entry: its package has no hook (the steps are the
/// built-in ones, which succeed at once for hostkeeper itself), so these only bound a wait.
const STEPS: StepBounds = StepBounds {
    prepare: 600,
    cutover: 300,
    verify: 1000,
    rollback: 600,
    cleanup: 1300,
    uninstall: None,
};

impl MacSelfHost {
    /// The Mac of the test `tag` (1 to 8 of `[a-z0-9]`) under `plan`, its daemon's job loaded,
    /// running [`N`], on the files link; under [`Plan::Installed`] with the package installed.
    pub fn new(tag: &str, plan: Plan) -> MacSelfHost {
        let host = MacSelfHost::make(tag, plan, false);
        host.load(plan);
        host.runs(N);
        if matches!(plan, Plan::Installed | Plan::InstalledFailingNext) {
            host.place_order(SETUP_ORDER, "install", N);
            host.settled(N);
            fs::remove_file(host.link().join("order.json")).expect("take the order away");
            host.assert_installed(N);
        }
        host
    }

    /// The Mac of the test `tag` as the pkg leaves it ([`Plan::Pkg`]) with the daemon on the real
    /// coordinator's link: `config` is the daemon's config (the coordinator's address and CA),
    /// which the daemon reads at its start, and the caller approves its enrollment once this
    /// returns. Nothing is ordered here.
    pub fn new_coordinated(tag: &str, config: &str) -> MacSelfHost {
        let plan = Plan::Pkg;
        let host = MacSelfHost::make(tag, plan, true);
        file(&host.path("hostkeeper.toml"), config.as_bytes(), 0o600);
        host.load(plan);
        host.runs(N);
        host
    }

    /// The tree, the fixture, the served releases and the programs, the job not loaded.
    fn make(tag: &str, plan: Plan, coordinated: bool) -> MacSelfHost {
        assert!(
            rustix::process::geteuid().is_root(),
            "the device tests run as root (privileged.yml's cargo runner `sudo`)"
        );
        assert!(
            fs::symlink_metadata(INSTALLED).is_err(),
            "{INSTALLED} is on the host before the test: the test installs hostkeeper's package there"
        );
        assert_eq!(
            receipt_version(),
            None,
            "no receipt of the package to begin with"
        );
        let pid = std::process::id();
        let base = PathBuf::from(format!("/var/hk-su-{tag}-{pid}"));
        // `create_dir_all` would take over a directory that is already there.
        fs::create_dir(&base).unwrap_or_else(|e| panic!("create {}: {e}", base.display()));
        let label = Label::parse(&format!("dev.hostkeeper.ci-su{tag}-{pid}")).expect("a job label");
        let made = prepare_parents();
        let mut host = MacSelfHost {
            base,
            label,
            releases: BTreeMap::new(),
            made,
            coordinated,
        };
        dir(&host.base, 0o755);
        dir(&host.state(), 0o700);
        dir(&host.state().join(HK), 0o700);
        dir(&host.path("packages"), 0o755);
        for sub in ["link", "units", "placed", PKGS_DIR] {
            dir(&host.fixture().join(sub), 0o755);
        }
        file(
            &host.fixture().join(SIGNERS_FILE),
            signers_file_text().as_bytes(),
            0o644,
        );
        file(&host.path("hostkeeper.toml"), CONFIG.as_bytes(), 0o644);
        dir(&host.service(), 0o755);
        for version in [N, NEXT] {
            host.publish(version);
        }
        match plan {
            Plan::Installed | Plan::InstalledFailingNext => {
                let daemon = host.path("hostkeeper-selfupdate-daemon");
                program(DAEMON, &daemon);
                let layout = host.layout();
                layout.stage(N, &daemon).expect("stage the first binary");
                layout.switch(N).expect("switch to it");
            }
            Plan::Pkg => {
                // The binary directory is left empty: the packaged binary seeds it.
                dir(&host.path("packaged"), 0o755);
                program(DAEMON, &host.packaged());
            }
        }
        host
    }

    /// Builds the `.pkg` of hostkeeper at `version` (the program under test at
    /// [`INSTALLED`]), serves it by its pin, and keeps its entry.
    fn publish(&mut self, version: &'static str) {
        let source = PkgSource {
            identifier: IDENTIFIER.to_owned(),
            version: version.to_owned(),
            files: vec![PkgFile::new(
                INSTALLED.trim_start_matches('/'),
                fs::read(DAEMON).unwrap_or_else(|e| panic!("read {DAEMON}: {e}")),
                0o755,
            )],
            postinstall: None,
        };
        let pkg = pkg_build::build(&self.path(&format!("build-{version}")), &source);
        let (digest, bytes) = macos_pkg::serve(&self.fixture(), &pkg);
        let entry = Target {
            installer: "macos-pkg".to_owned(),
            locations: vec![Location {
                store: "channel".to_owned(),
                locator: format!("{HK}/{version}/{HK}-{version}-macos-arm64.pkg"),
            }],
            digest,
            bytes,
            manifest: Some(Manifest {
                min_hostkeeper: "0.1.0".to_owned(),
                package: Some(IDENTIFIER.to_owned()),
                hook: Some(Hook {
                    timeout_secs: Some(STEPS),
                    verify_deadline_secs: 1600,
                    ..Hook::default()
                }),
                installer_bounds: Some(InstallerBounds {
                    stage_secs: 300,
                    activate_secs: 300,
                    remove_secs: 300,
                }),
                ..Manifest::default()
            }),
            ..Target::default()
        };
        self.releases.insert(version, Release { entry });
    }

    fn path(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }

    fn state(&self) -> PathBuf {
        self.path("state")
    }

    /// The fixture's directory: `link/`, `units/`, `placed/`, `pkgs/` and the signers file.
    pub fn fixture(&self) -> PathBuf {
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

    /// The packaged binary of a [`Plan::Pkg`] Mac: a copy of the fixture daemon whose file name
    /// is the version [`N`] it stands for.
    fn packaged(&self) -> PathBuf {
        self.path("packaged").join(N)
    }

    /// The daemon's plist, as `daemon_plist` renders it, with the test's arguments around the
    /// verb; its job bootstrapped. Its program is the packaged binary under [`Plan::Pkg`] (with
    /// `--fixture-packaged` among the fixture's flags) and `current` otherwise.
    fn load(&self, plan: Plan) {
        let pkg = matches!(plan, Plan::Pkg);
        let program = match pkg {
            true => self.packaged(),
            false => self.layout().current_path(),
        };
        let job = DaemonJob {
            label: self.label.clone(),
            program,
            stdout: self.path("daemon.out.log"),
            stderr: self.path("daemon.err.log"),
        };
        let rendered = daemon_plist(&job).expect("the daemon's plist");
        let arg = |text: String| format!("<string>{text}</string>");
        let text = |name: &str| self.path(name).display().to_string();
        let mut argv = vec![
            arg("--fixture-dir".to_owned()),
            arg(self.fixture().display().to_string()),
            arg("--fixture-own-unit".to_owned()),
            arg(self.label.to_string()),
        ];
        if self.coordinated {
            argv.push(arg("--fixture-link".to_owned()));
            argv.push(arg("coordinator".to_owned()));
        }
        if pkg {
            argv.push(arg("--fixture-packaged".to_owned()));
            argv.push(arg(self.packaged().display().to_string()));
        }
        if matches!(plan, Plan::InstalledFailingNext) {
            argv.push(arg("--fixture-fail-start".to_owned()));
            argv.push(arg(NEXT.to_owned()));
        }
        argv.push(arg("daemon".to_owned()));
        for (flag, value) in [
            ("--config", text("hostkeeper.toml")),
            ("--state-dir", text("state")),
            ("--packages-dir", text("packages")),
            ("--heartbeat-secs", "1".to_owned()),
        ] {
            argv.push(arg(flag.to_owned()));
            argv.push(arg(value));
        }
        let verb = "<string>daemon</string>";
        assert_eq!(
            rendered.matches(verb).count(),
            1,
            "the daemon's plist runs its program with the verb `daemon`:\n{rendered}"
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

    /// How many times the daemon of [`NEXT`] refused to start so far (`--fixture-fail-start`):
    /// the tries of the hand-over.
    pub fn refused_starts(&self) -> usize {
        self.daemon_log().matches(&refused_line(NEXT)).count()
    }

    /// Whether [`NEXT`] is on the daemon's local list of bad versions of hostkeeper.
    pub fn next_is_bad(&self) -> bool {
        BadVersions::load(&self.state().join(HK))
            .expect("the list of bad versions")
            .contains(NEXT)
    }

    /// The coordinator orders `verb` of hostkeeper at `version`, order [`ORDER`] try 1, in every
    /// reply from now on, by the entry of the release ([`MacSelfHost::entry`]).
    pub fn offer(&self, verb: &str, version: &str) {
        self.place_order(ORDER, verb, version);
    }

    /// The entry of the release of hostkeeper at `version` that an order carries: the `macos-pkg`
    /// entry of its `.pkg`.
    pub fn entry(&self, version: &str) -> &Target {
        &self.releases[version].entry
    }

    /// The `targets` of a set of hostkeeper at `version`: its one `macos-pkg` entry, for this
    /// Mac's platform.
    pub fn targets(&self, version: &str) -> BTreeMap<String, TargetList> {
        BTreeMap::from([(
            Platform::current().to_string(),
            TargetList {
                entries: vec![self.entry(version).clone()],
            },
        )])
    }

    /// The entry the daemon staged the version `version` of hostkeeper for (its sealed
    /// `target.json`), `None` while it has not staged it.
    pub fn staged_entry(&self, version: &str) -> Option<Target> {
        target::read(&self.service().join(version)).expect("target.json")
    }

    fn place_order(&self, id: &str, verb: &str, version: &str) {
        let link = self.fixture().join("link");
        let entry = self.entry(version);
        let json = target::to_json(entry).expect("the entry as JSON");
        let order = serde_json::json!({
            "id": id,
            "try": 1,
            "verb": verb,
            "service": HK,
            "version": version,
            "digest": entry.digest,
            "entry": serde_json::from_slice::<Value>(&json).expect("the entry's JSON"),
        });
        let tmp = link.join("order.json.tmp");
        file(&tmp, order.to_string().as_bytes(), 0o644);
        fs::rename(&tmp, link.join("order.json")).expect("place the order");
    }

    /// The fixture's link directory: the heartbeats the daemon wrote, and the coordinator's
    /// answer it reads.
    pub fn link(&self) -> PathBuf {
        self.fixture().join("link")
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
    /// left out. None on the real coordinator's link, which writes no file.
    pub fn beats(&self) -> Vec<Value> {
        let text = fs::read_to_string(self.fixture().join("link/beats.jsonl")).unwrap_or_default();
        text.lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The newest `last_update` the heartbeats reported for hostkeeper's order `id`, try 1.
    pub fn verdict(&self, id: &str) -> Option<Value> {
        self.beats().into_iter().rev().find_map(|beat| {
            beat["services"]
                .as_array()?
                .iter()
                .filter(|service| service["name"] == HK)
                .map(|service| service["last_update"].clone())
                .find(|result| result["order_id"] == id && result["order_try"] == 1)
        })
    }

    /// Fails at once, with the daemon's output, when an order of the test was answered with a
    /// refusal (any failure but the wait for the restart and the fallback's): what a wait waits
    /// for can no longer come. On the real coordinator's link, which writes no heartbeat file, a
    /// record of hostkeeper that ended failed is the refusal.
    pub fn check_not_stuck(&self) {
        if self.coordinated
            && let Some(Record {
                state:
                    State::Finished {
                        verdict: Verdict::Failed { .. },
                        reason,
                    },
                ..
            }) = self.record()
        {
            panic!("an order ended failed ({reason:?})\n{}", self.logs());
        }
        for id in [ORDER, SETUP_ORDER] {
            if let Some(result) = self.verdict(id) {
                let refused = (result["outcome"] == "failed")
                    && ![AWAITING_REASON, FALLBACK_REASON]
                        .contains(&result["reason"].as_str().unwrap_or_default());
                assert!(!refused, "the order was refused: {result}\n{}", self.logs());
            }
        }
    }

    /// Waits until the step record of hostkeeper is finished for an update or install to
    /// `version`, and gives it.
    pub fn settled(&self, version: &str) -> Record {
        let mut found = None;
        wait(
            &format!("the step record of hostkeeper is finished for {version}"),
            || {
                self.check_not_stuck();
                found = self.record().filter(|rec| {
                    rec.target_version == version && matches!(rec.state, State::Finished { .. })
                });
                found.is_some()
            },
        );
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

    /// The real path of the binary directory's file of `version`, once it is there.
    pub fn binary_of(&self, version: &str) -> Option<PathBuf> {
        fs::canonicalize(self.layout().version_path(version)).ok()
    }

    /// Waits until the daemon's job has a process that runs the binary of `version`, checks that
    /// `current` names it, and gives its pid. The file of `version` may not be there yet when the
    /// wait begins: on a [`Plan::Pkg`] Mac the job's packaged binary puts it in at its start.
    pub fn runs(&self, version: &str) -> u32 {
        let shown = self.layout().version_path(version);
        let mut running = None;
        wait(
            &format!("the daemon's job runs {}", shown.display()),
            || {
                self.check_not_stuck();
                let Some(want) = self.binary_of(version) else {
                    return false;
                };
                running = self
                    .pid()
                    .filter(|pid| DarwinPidInfo.path(*pid).is_ok_and(|path| path == want));
                running.is_some()
            },
        );
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

    /// The link `name` (`current`, `previous`) of hostkeeper's package directory.
    pub fn package_link(&self, name: &str) -> Option<String> {
        fs::read_link(self.service().join(name))
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }

    /// hostkeeper's package is installed at `version`: `pkgutil` has its receipt at `version`, the
    /// program is at [`INSTALLED`] with the bytes of the program under test, root's, mode 0755,
    /// `current` of the packages directory names `version`, and the daemon's record names the
    /// `macos-pkg` installer, the package and the version.
    pub fn assert_installed(&self, version: &str) {
        assert_eq!(
            receipt_version().as_deref(),
            Some(version),
            "{}",
            self.logs()
        );
        let meta = fs::symlink_metadata(INSTALLED)
            .unwrap_or_else(|e| panic!("{INSTALLED} is not installed: {e}"));
        assert_eq!(
            (meta.uid(), meta.mode() & 0o7777),
            (0, 0o755),
            "{INSTALLED} is root's, mode 0755"
        );
        assert_eq!(
            fs::read(INSTALLED).expect("the installed program"),
            fs::read(DAEMON).expect("the program under test"),
            "{INSTALLED} is the program the package carries"
        );
        assert_eq!(
            self.package_link("current").as_deref(),
            Some(version),
            "{}",
            self.logs()
        );
        let placed = fs::read(self.fixture().join("placed/placed.json")).unwrap_or_default();
        let placed: Value = serde_json::from_slice(&placed).unwrap_or(Value::Null);
        let installed = &placed["installed"][HK];
        assert_eq!(
            (
                installed["installer"].as_str(),
                installed["package"].as_str(),
                installed["version"].as_str()
            ),
            (Some("macos-pkg"), Some(IDENTIFIER), Some(version)),
            "the daemon's record of hostkeeper: {placed}"
        );
    }
}
