//! The host the end-to-end tests run against: a directory tree under `/run`, the fixed host
//! paths the real daemon uses, and the systemd units it and the fixture service run in.
//!
//! The daemon binary reads some paths from no flag (the repository entries it installs from, the
//! placed-unit state and the systemd unit directory), and the package it installs is the host's
//! own in dpkg's database, so a test that drives it for real has to put files at those places and
//! leaves a package there. That is only safe on a throwaway machine, which is what the hosted
//! runner of `privileged.yml` is. [`HostRoot`] therefore moves aside what is already at those
//! places, puts what the test needs there, and takes all of it away again when it is dropped (the
//! fixture package purged), whether the test passed or failed. When the test is panicking it
//! first copies the hook records, the journal of every unit it saw and the step record to a
//! directory the workflow uploads.
//!
//! The cross-release rig (`tests/across/`) also has the host put the pinned `gh` and a Sigstore
//! trusted root at their fixed paths ([`HostRoot::install_gh`],
//! [`HostRoot::install_trusted_root`]), where the published release it runs as the host's daemon
//! looks for them.
//!
//! Nothing here asserts how long anything takes; a wait polls for a state ([`super::wait_for`]).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, PoisonError};

use hostkeeper_privileged::{SYSTEMD_RUNTIME_DIR, check_host};
use hostkeeper_steps::record::RECORD_FILE;
use hostkeeper_steps::systemd::SYSTEMCTL;

use super::SERVICE;
use super::aptrepo::AptRepo;
use super::aptrepo::REGISTRY_NAME;

/// The daemon program: `hostkeeper`, built with these tests (it holds `daemon`).
pub const DAEMON: &str = env!("CARGO_BIN_EXE_hostkeeper-device-daemon");
/// Where `journalctl` is.
const JOURNALCTL: &str = "/usr/bin/journalctl";
/// The directory of the programs a released daemon trusts: `gh` and the Sigstore trusted root.
const LIBRARY_DIR: &str = "/usr/lib/hostkeeper";
/// The `gh` a released daemon checks attestations with (a fixed path of the daemon).
pub const GH_PATH: &str = "/usr/lib/hostkeeper/gh";
/// The Sigstore trusted root a released daemon hands `gh` (a fixed path of the daemon).
pub const TRUSTED_ROOT_PATH: &str = "/usr/lib/hostkeeper/trusted_root.json";
/// The directory of the daemon's configuration, which holds the repository entries.
const CONFIG_DIR: &str = "/etc/hostkeeper";
/// The directory the enrollment loop keeps its key and certificate in.
const PKI_DIR: &str = "/etc/updater";
/// The daemon's placed-unit state directory.
const PLACED_DIR: &str = "/var/lib/hostkeeper";
/// Where systemd looks for the units an administrator (and the daemon) places.
const UNIT_DIR: &str = "/etc/systemd/system";
/// The directory a placed unit's `WantedBy=multi-user.target` link goes in.
const WANTS_DIR: &str = "/etc/systemd/system/multi-user.target.wants";
/// The glob of every run instance of the fixture service.
const INSTANCES: &str = "hostkeeper-ci-tierstore@*.service";
/// The glob of the transient units the daemon runs the hook steps in.
const STEP_UNITS: &str = "updater-hostkeeper-ci-tierstore-*.service";

/// `/usr/bin/systemctl --system <args>`, its input closed.
///
/// # Panics
/// `systemctl` cannot be run.
pub fn systemctl(args: &[&str]) -> Output {
    Command::new(SYSTEMCTL)
        .arg("--system")
        .arg("--no-pager")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run {SYSTEMCTL}: {e}"))
}

/// Whether `unit` is active now (`systemctl is-active --quiet`).
pub fn is_active(unit: &str) -> bool {
    systemctl(&["is-active", "--quiet", unit]).status.success()
}

/// `systemctl` run for a result nobody needs: a unit with nothing to undo fails some of these.
fn systemctl_quiet(args: &[&str]) {
    let _ = systemctl(args);
}

/// Panics with what `output` printed unless it ended with exit status 0.
fn assert_ok(what: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{what} failed: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Creates `dir` (and its parents) with mode `mode`, owned by whoever runs the test (root).
fn make_dir(dir: &Path, mode: u32) {
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    fs::set_permissions(dir, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", dir.display()));
}

/// Writes `bytes` to `path` and sets its mode to `mode`.
fn put(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
}

/// The test's view of the host: its directory tree and what it changed outside it.
///
/// Dropping it undoes every change (see the module documentation).
pub struct HostRoot {
    tag: String,
    base: PathBuf,
    /// Every daemon unit started through [`HostRoot::start_daemon`], for the journal copy and
    /// the teardown.
    units: Mutex<Vec<String>>,
    /// The fixed places of the host this rig takes over that were there already (another test
    /// of this crate may leave them), each with where it was moved aside; put back at the drop.
    saved: Vec<(PathBuf, PathBuf)>,
}

/// The fixed places of the host the rig takes over: the daemon's configuration (its repository
/// entries), the programs a released daemon trusts, and the daemon's pki and placed state.
fn taken_places() -> [PathBuf; 4] {
    [
        PathBuf::from(CONFIG_DIR),
        PathBuf::from(LIBRARY_DIR),
        PathBuf::from(PKI_DIR),
        PathBuf::from(PLACED_DIR),
    ]
}

impl HostRoot {
    /// Checks that the test runs as root under systemd on a host this rig may use, then makes
    /// its directory tree and the two host directories the daemon reads programs and keeps its
    /// state in.
    ///
    /// `tag` is 1 to 8 characters of `[a-z0-9]`; it names the tree (`/run/hk-e2e-<tag>-<pid>`)
    /// and the directory of the artifacts a failed test leaves.
    ///
    /// # Panics
    /// The test is not root or systemd is not the service manager, a place the rig takes over
    /// cannot be moved aside, or a directory cannot be made.
    pub fn new(tag: &str) -> HostRoot {
        assert!(
            (1..=8).contains(&tag.len())
                && tag
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "the tag {tag:?} is not 1 to 8 characters of [a-z0-9]"
        );
        let root = rustix::process::geteuid().is_root();
        if let Err(e) = check_host(root, Path::new(SYSTEMD_RUNTIME_DIR)) {
            panic!("{e}");
        }
        // This rig writes to fixed places of the host. What is there already is moved aside,
        // beside itself, and put back when the test ends: another test of this crate may have
        // left it, and nothing of it may leak into this test or be lost.
        let pid = std::process::id();
        let saved = taken_places()
            .into_iter()
            .filter(|place| fs::symlink_metadata(place).is_ok())
            .map(|place| {
                let aside = PathBuf::from(format!("{}.hk-e2e-saved-{tag}-{pid}", place.display()));
                fs::rename(&place, &aside).unwrap_or_else(|e| {
                    panic!("move {} aside to {}: {e}", place.display(), aside.display())
                });
                (place, aside)
            })
            .collect();
        let base = PathBuf::from(format!("/run/hk-e2e-{tag}-{}", std::process::id()));
        // `create_dir_all` would take over a directory that is already there.
        fs::create_dir(&base).unwrap_or_else(|e| panic!("create {}: {e}", base.display()));
        let host = HostRoot {
            tag: tag.to_owned(),
            base,
            units: Mutex::new(Vec::new()),
            saved,
        };
        // The package check wants the packages directory and every ancestor root-owned and
        // writable by nobody else.
        make_dir(&host.base, 0o755);
        make_dir(&host.packages(), 0o755);
        make_dir(&host.state(), 0o755);
        make_dir(&host.records(), 0o755);
        make_dir(Path::new(PLACED_DIR), 0o755);
        make_dir(Path::new(LIBRARY_DIR), 0o755);
        // The fixture package may be left by a test that failed before: the host starts without it.
        purge_fixture_package();
        host
    }

    /// The tree of this test: `/run/hk-e2e-<tag>-<pid>`.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// The daemon's `--packages-dir`.
    pub fn packages(&self) -> PathBuf {
        self.base.join("packages")
    }

    /// The daemon's `--state-dir`.
    pub fn state(&self) -> PathBuf {
        self.base.join("state")
    }

    /// Where the fixture hook writes its records and the test writes the drain gate.
    pub fn records(&self) -> PathBuf {
        self.base.join("hooks")
    }

    /// The daemon's `--config`.
    pub fn config_path(&self) -> PathBuf {
        self.base.join("hostkeeper.toml")
    }

    /// The fixture service's directory under the packages directory.
    pub fn service_dir(&self) -> PathBuf {
        self.packages().join(SERVICE)
    }

    /// The step record of the fixture service, as the daemon last wrote it; `None` when there
    /// is none.
    pub fn step_record(&self) -> Option<serde_json::Value> {
        let bytes = fs::read(self.state().join(SERVICE).join(RECORD_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Puts the `gh` program at the path the daemon runs it from: `/usr/lib/hostkeeper/gh`,
    /// mode 0755, owned by root.
    pub fn install_gh(&self, gh: &Path) {
        let to = Path::new(GH_PATH);
        fs::copy(gh, to)
            .unwrap_or_else(|e| panic!("copy {} to {}: {e}", gh.display(), to.display()));
        fs::set_permissions(to, fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("chmod {}: {e}", to.display()));
    }

    /// Puts the Sigstore trusted root (`jsonl`, one line) where the daemon reads it:
    /// `/usr/lib/hostkeeper/trusted_root.json`, mode 0644.
    pub fn install_trusted_root(&self, jsonl: &str) {
        put(Path::new(TRUSTED_ROOT_PATH), jsonl.as_bytes(), 0o644);
    }

    /// Puts the entry of `repo`, the repository of the packages the test orders, where the
    /// daemon reads its repository entries (`/etc/hostkeeper/repositories`), as the
    /// coordinator's registry renders it under the name [`REGISTRY_NAME`] (so that the daemon
    /// adopts the registry's set), and makes the host's apt trust the repository's https server.
    /// Call it before the daemon starts: it reads the directory at its start.
    pub fn install_repository(&self, repo: &AptRepo) {
        repo.install_registered(REGISTRY_NAME);
    }

    /// Writes the daemon config at [`HostRoot::config_path`], mode 0600, owned by root.
    pub fn write_config(&self, text: &str) {
        put(&self.config_path(), text.as_bytes(), 0o600);
    }

    /// Sets the drain gate the fixture's `prepare` waits on: `busy` holds the update in its
    /// drain, `idle` lets it go on.
    pub fn set_gate(&self, busy: bool) {
        let text = if busy { "busy" } else { "idle" };
        put(&self.records().join("drain-gate"), text.as_bytes(), 0o644);
    }

    /// The record file `name` of the fixture hook (see the scenario for the names), without
    /// trailing whitespace; `None` when it is not there.
    pub fn record(&self, name: &str) -> Option<String> {
        let text = fs::read_to_string(self.records().join(name)).ok()?;
        Some(text.trim_end().to_owned())
    }

    /// Starts the daemon as the systemd unit `<unit>.service` and waits for it to be ready.
    ///
    /// The unit is `Type=notify`, so `systemctl start` returns once the daemon sent `READY=1`:
    /// after its start pass (which finishes or rolls back an update a crash left half done).
    /// It heartbeats every second, which keeps the scenarios short.
    ///
    /// # Panics
    /// The unit cannot be linked or does not become ready.
    pub fn start_daemon(&self, unit: &str) -> DaemonUnit {
        let name = format!("{unit}.service");
        let file = self.base.join(&name);
        let text = format!(
            "[Unit]\n\
             Description=hostkeeper daemon of the end-to-end test {tag}\n\
             \n\
             [Service]\n\
             Type=notify\n\
             NotifyAccess=main\n\
             ExecStart={DAEMON} daemon --config {config} --state-dir {state} \
             --packages-dir {packages} --notify-systemd \
             --heartbeat-secs 1\n",
            tag = self.tag,
            config = self.config_path().display(),
            state = self.state().display(),
            packages = self.packages().display(),
        );
        put(&file, text.as_bytes(), 0o644);
        self.units
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(name.clone());
        let file_arg = file.to_string_lossy().into_owned();
        assert_ok("systemctl link", &systemctl(&["link", &file_arg]));
        let started = systemctl(&["start", &name]);
        if !started.status.success() {
            // Say why the daemon did not come up before the failure hides it.
            let journal = journal_of(&name);
            panic!(
                "systemctl start {name} failed: {}\nstderr: {}\njournal:\n{journal}",
                started.status,
                String::from_utf8_lossy(&started.stderr),
            );
        }
        DaemonUnit { unit: name, file }
    }

    /// Where a failed test leaves what there is to look at:
    /// `<target tmpdir>/../privileged-artifacts/<tag>/`, which the workflow uploads.
    pub fn artifact_dir(&self) -> PathBuf {
        Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("..")
            .join("privileged-artifacts")
            .join(&self.tag)
    }

    /// Copies what a failed test leaves to look at into
    /// `<target tmpdir>/../privileged-artifacts/<tag>/`, which the workflow uploads. Every
    /// step ignores its errors: this runs while the test is already failing.
    fn save_artifacts(&self) {
        let out = self.artifact_dir();
        if fs::create_dir_all(out.join("records")).is_err() {
            return;
        }
        if let Ok(entries) = fs::read_dir(self.records()) {
            for entry in entries.flatten() {
                let _ = fs::copy(entry.path(), out.join("records").join(entry.file_name()));
            }
        }
        let _ = fs::copy(
            self.state().join(SERVICE).join(RECORD_FILE),
            out.join(RECORD_FILE),
        );
        let mut units = self
            .units
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        units.push(INSTANCES.to_owned());
        units.push(STEP_UNITS.to_owned());
        for unit in units {
            // The artifact upload refuses a file name with `*` in it.
            let file = format!("journal-{}.txt", unit.replace('*', "all"));
            let _ = fs::write(out.join(file), journal_of(&unit));
        }
    }
}

/// The journal of `unit` (a name or a glob), or why it could not be read.
fn journal_of(unit: &str) -> String {
    match Command::new(JOURNALCTL)
        .args(["--no-pager", "--unit", unit])
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(e) => format!("cannot run {JOURNALCTL}: {e}"),
    }
}

/// Puts the host as it was before the fixture package: its units stopped, its package unheld and
/// purged, the enablement links of its run units removed (the package's purge removes the links
/// the installer made, and the instances of a failed test are left to this).
fn purge_fixture_package() {
    systemctl_quiet(&["stop", INSTANCES]);
    let _ = Command::new("/usr/bin/apt-mark")
        .args(["unhold", SERVICE])
        .stdin(Stdio::null())
        .output();
    let _ = Command::new("/usr/bin/dpkg")
        .args(["--purge", SERVICE])
        .stdin(Stdio::null())
        .output();
    remove_placed_links();
}

/// Removes the links of the fixture service in the unit directory: the instances
/// `multi-user.target` wants.
fn remove_placed_links() {
    let prefix = format!("{SERVICE}@");
    if let Ok(entries) = fs::read_dir(WANTS_DIR) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

impl Drop for HostRoot {
    /// Puts the host back as the test found it. Every step goes on whatever the one before it
    /// did, because a failed test is the likeliest reason for a half-built host.
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.save_artifacts();
        }
        let units = self
            .units
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        // The daemon first, so that it starts nothing while the rest is stopped.
        for unit in &units {
            systemctl_quiet(&["stop", unit]);
        }
        systemctl_quiet(&["stop", STEP_UNITS]);
        for unit in &units {
            let _ = fs::remove_file(Path::new(UNIT_DIR).join(unit));
        }
        purge_fixture_package();
        for dir in [CONFIG_DIR, LIBRARY_DIR, PKI_DIR, PLACED_DIR] {
            let _ = fs::remove_dir_all(dir);
        }
        for (place, aside) in &self.saved {
            let _ = fs::rename(aside, place);
        }
        systemctl_quiet(&["daemon-reload"]);
        systemctl_quiet(&["reset-failed"]);
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// The daemon as a systemd unit started by [`HostRoot::start_daemon`].
pub struct DaemonUnit {
    /// The unit's full name, `<unit>.service`.
    unit: String,
    /// The unit file in the test's tree, which `systemctl link` linked.
    file: PathBuf,
}

impl DaemonUnit {
    /// The unit's full name, `<unit>.service`.
    pub fn name(&self) -> &str {
        &self.unit
    }

    /// Stops the unit (SIGTERM to the daemon) and waits for it to end.
    ///
    /// # Panics
    /// `systemctl stop` fails.
    pub fn stop(&self) {
        assert_ok("systemctl stop", &systemctl(&["stop", &self.unit]));
    }

    /// Starts the unit and waits for the daemon's `READY=1`.
    ///
    /// # Panics
    /// `systemctl start` fails.
    pub fn start(&self) {
        assert_ok("systemctl start", &systemctl(&["start", &self.unit]));
    }

    /// Whether the unit is active.
    pub fn is_active(&self) -> bool {
        is_active(&self.unit)
    }

    /// The properties `props` of the unit as `systemctl show` prints them (`ActiveState`,
    /// `Result`, `ExecMainStatus`, ...). A property systemd does not know is left out.
    pub fn show(&self, props: &[&str]) -> BTreeMap<String, String> {
        let property = format!("--property={}", props.join(","));
        let out = systemctl(&["show", &property, &self.unit]);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }
}

impl Drop for DaemonUnit {
    /// Stops the unit and unlinks it. Errors are ignored: [`HostRoot`]'s drop may have done so
    /// already, and a test that panics must not panic again here.
    fn drop(&mut self) {
        systemctl_quiet(&["stop", &self.unit]);
        let _ = fs::remove_file(Path::new(UNIT_DIR).join(&self.unit));
        let _ = fs::remove_file(&self.file);
        systemctl_quiet(&["reset-failed", &self.unit]);
    }
}
