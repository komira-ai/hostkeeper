//! The Mac the end-to-end test of `tests/device_mac_e2e.rs` runs against: a directory tree under
//! `/var` (a link to `/private/var`, as the daemon's default packages directory is reached), the
//! program `hostkeeper-selfupdate-daemon` as a launchd job of the system domain on the real
//! coordinator's link, the places hello's `.pkg` installs at, and the reads of launchd's state
//! the test asserts on.
//!
//! The daemon is the shipped daemon on the Mac's own adapters, with the three ports the program
//! replaces (`tests/bin/selfupdate_daemon.rs` says which and why): the `macos-pkg` signers file
//! and the `.pkg` files it fetches by their pins are the fixture's (`fixture/`), the installer's
//! signature check is answered as a signed package for a package the fixture serves, and its pki
//! directory is the fixture's (`fixture/pki`), so the host's own are not touched. Its enrollment
//! is the real one: the coordinator in process approves it, and it beats over mutual TLS.
//!
//! A package installs at fixed paths of the host: hello's files under
//! `/Library/Application Support/hello` and its plists in `/Library/LaunchDaemons`, which a
//! test that installs it for real puts there, which is only safe on a throwaway machine, which
//! the hosted runner of `privileged.yml` is. [`MacHost::new`] moves aside whatever is at the
//! places it takes over (another test of the crate may have left it), and the drop boots out
//! every job the test made or the package loaded, removes what the test and the installer wrote
//! there, forgets the package's receipt, puts back what was moved aside, whether the test passed
//! or failed. When the test is failing, the drop first prints the daemon's output, the jobs'
//! output and the hook's record to the log.
//!
//! Every read of launchd goes through `LaunchctlProcess::system`, the `launchctl` adapter the
//! daemon uses. Nothing here asserts how long anything takes; a wait polls a state
//! ([`super::wait_for`]).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_daemon::boot::BOOT_ID_FILE;
use hostkeeper_daemon::cli::DEFAULT_FACTS_FILE;
use hostkeeper_install::macos_pkg::receipt;
use hostkeeper_launchd::launchctl::{JobStatus, bootout_args, bootstrap_args, print, print_args};
use hostkeeper_launchd::{Label, Launchctl, LaunchctlProcess};
use hostkeeper_steps::record::RECORD_FILE;

use super::hello::{IDENTIFIER, SERVICE, SLOTS, SUPPORT, label, plist_path};
use super::macos_pkg::{PKGS_DIR, SIGNERS_FILE, serve, signers_file_text};

/// The program `hostkeeper-selfupdate-daemon`, which the test copies to the name of its version.
pub const DAEMON: &str = env!("CARGO_BIN_EXE_hostkeeper-selfupdate-daemon");
/// The version the daemon runs as: the name of the file it runs from.
pub const DAEMON_VERSION: &str = "1.0.0";
/// The deadline of one `launchctl` command: `bootout` waits for the job to end.
const LAUNCHCTL_DEADLINE: Duration = Duration::from_secs(60);
/// How many looks [`MacHost::start_daemon`] takes at the job and the state directory before it
/// gives up.
const START_TRIES: u32 = 240;
/// The deadline of `pkgutil --forget`.
const PKGUTIL_DEADLINE: Duration = Duration::from_secs(120);

/// `launchctl`, as the daemon runs it.
pub fn launchctl() -> LaunchctlProcess {
    LaunchctlProcess::system(LAUNCHCTL_DEADLINE)
}

/// Boots `label` out of the system domain; a job that is not loaded is done. Errors are
/// ignored: this runs while putting the host back.
fn bootout_quiet(label: &Label) {
    let _ = launchctl().run(&bootout_args(label));
}

/// Whether launchd holds `label` in the system domain.
///
/// # Panics
/// `launchctl print` cannot be run or read.
pub fn is_loaded(label: &Label) -> bool {
    match print(&launchctl(), label) {
        Ok(JobStatus::Loaded(_)) => true,
        Ok(JobStatus::NotLoaded) => false,
        Err(e) => panic!("launchctl print system/{label}: {e}"),
    }
}

/// The top-level `key = value` lines of `launchctl print system/<label>` (`state`, `pid`,
/// `program`, `path`, `stdout path`, ...), the nested blocks left out but for the first line of
/// `arguments = {`, kept as `arguments[0]`; `None` when the job is not loaded.
///
/// # Panics
/// `launchctl print` cannot be run.
pub fn job_fields(label: &Label) -> Option<BTreeMap<String, String>> {
    if !is_loaded(label) {
        return None;
    }
    let out = launchctl()
        .run(&print_args(label))
        .unwrap_or_else(|e| panic!("launchctl print system/{label}: {e}"));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut fields = BTreeMap::new();
    let mut depth = 0usize;
    let mut in_arguments = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with('{') {
            in_arguments = depth == 1 && trimmed.starts_with("arguments = ");
            depth += 1;
            continue;
        }
        if in_arguments {
            in_arguments = false;
            if trimmed != "}" {
                fields.insert("arguments[0]".to_owned(), trimmed.to_owned());
            }
        }
        if trimmed == "}" {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth == 1
            && let Some((key, value)) = trimmed.split_once(" = ")
        {
            fields
                .entry(key.trim().to_owned())
                .or_insert_with(|| value.trim().to_owned());
        }
    }
    Some(fields)
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

/// `text` for the inside of an XML element.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The plist of the daemon's job: `argv`, started at load, never kept alive, its output in
/// `out` and `err`.
fn daemon_plist(label: &Label, argv: &[String], out: &Path, err: &Path) -> String {
    let args: String = argv
        .iter()
        .map(|arg| format!("    <string>{}</string>\n", xml(arg)))
        .collect();
    let out = xml(&out.display().to_string());
    let err = xml(&err.display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n  \
         <key>KeepAlive</key>\n  <false/>\n  <key>Label</key>\n  <string>{label}</string>\n  \
         <key>ProgramArguments</key>\n  <array>\n{args}  </array>\n  <key>RunAtLoad</key>\n  \
         <true/>\n  <key>StandardErrorPath</key>\n  <string>{err}</string>\n  \
         <key>StandardOutPath</key>\n  <string>{out}</string>\n</dict>\n</plist>\n"
    )
}

/// The labels of hello's two slots.
fn slot_labels() -> [Label; 2] {
    SLOTS.map(label)
}

/// The places of the host this rig takes over: the directory hello's package installs in, the
/// plists of hello's two slots, and the daemon's default facts file (a daemon started without
/// `--facts-file` writes it).
fn taken_places() -> Vec<PathBuf> {
    let mut places = vec![PathBuf::from(SUPPORT), PathBuf::from(DEFAULT_FACTS_FILE)];
    places.extend(SLOTS.map(plist_path));
    places
}

/// The test's view of the Mac: its directory tree and what it changed outside it.
///
/// Dropping it undoes every change (see the module documentation).
pub struct MacHost {
    tag: String,
    base: PathBuf,
    /// The label of the daemon's job, once [`MacHost::start_daemon`] loaded it.
    daemon: Mutex<Option<Label>>,
    /// The places taken over that were there already, each with where it was moved aside.
    saved: Vec<(PathBuf, PathBuf)>,
}

impl MacHost {
    /// Checks that the test runs as root, moves aside what is at the places the rig takes over,
    /// and makes the test's tree: the daemon's directories and the fixture's (the signers file,
    /// the directory the `.pkg` files are served from; the pki directory is the enrollment's to
    /// make).
    ///
    /// `tag` is 1 to 8 characters of `[a-z0-9]`; it names the tree
    /// (`/var/hk-e2e-<tag>-<pid>`) and the daemon's job.
    ///
    /// # Panics
    /// The test is not root, a place cannot be moved aside, or a directory cannot be made.
    pub fn new(tag: &str) -> MacHost {
        assert!(
            (1..=8).contains(&tag.len())
                && tag
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "the tag {tag:?} is not 1 to 8 characters of [a-z0-9]"
        );
        assert!(
            rustix::process::geteuid().is_root(),
            "the device tests run as root (privileged.yml's cargo runner `sudo`)"
        );
        // A job left loaded by another test would run beside this one's.
        for label in slot_labels() {
            bootout_quiet(&label);
        }
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
        // Under /var, a link to /private/var, as the daemon's default directories are: the seal
        // names each version by its real path, which the check before each step judges.
        let base = PathBuf::from(format!("/var/hk-e2e-{tag}-{pid}"));
        // `create_dir_all` would take over a directory that is already there.
        fs::create_dir(&base).unwrap_or_else(|e| panic!("create {}: {e}", base.display()));
        let host = MacHost {
            tag: tag.to_owned(),
            base,
            daemon: Mutex::new(None),
            saved,
        };
        make_dir(&host.base, 0o755);
        make_dir(&host.packages(), 0o755);
        make_dir(&host.state(), 0o755);
        make_dir(&host.records(), 0o755);
        make_dir(&host.log_dir(), 0o755);
        make_dir(&host.base.join("bin"), 0o755);
        make_dir(&host.fixture(), 0o755);
        make_dir(&host.fixture().join(PKGS_DIR), 0o755);
        make_dir(&host.fixture().join("placed"), 0o755);
        make_dir(&host.fixture().join("units"), 0o755);
        put(
            &host.fixture().join(SIGNERS_FILE),
            signers_file_text().as_bytes(),
            0o644,
        );
        host
    }

    /// The tree of this test.
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

    /// The parent of hello's hook record directory.
    pub fn records(&self) -> PathBuf {
        self.base.join("hooks")
    }

    /// The directory the jobs' output goes to.
    pub fn log_dir(&self) -> PathBuf {
        self.base.join("logs")
    }

    /// The fixture's directory (the module says what is in it).
    pub fn fixture(&self) -> PathBuf {
        self.base.join("fixture")
    }

    /// The daemon's `--config`.
    pub fn config_path(&self) -> PathBuf {
        self.base.join("hostkeeper.toml")
    }

    /// hello's directory under the packages directory.
    pub fn service_dir(&self) -> PathBuf {
        self.packages().join(SERVICE)
    }

    /// hello's step record, as the daemon last wrote it; `None` when there is none.
    pub fn step_record(&self) -> Option<serde_json::Value> {
        let bytes = fs::read(self.state().join(SERVICE).join(RECORD_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Serves the `.pkg` at `pkg` to the daemon by its pin; the pin and the size in bytes.
    pub fn serve_pkg(&self, pkg: &Path) -> (String, u64) {
        serve(&self.fixture(), pkg)
    }

    /// Writes the daemon config at [`MacHost::config_path`], mode 0600, owned by root.
    pub fn write_config(&self, text: &str) {
        put(&self.config_path(), text.as_bytes(), 0o600);
    }

    /// Starts the daemon as the launchd job `dev.hostkeeper.ci-<tag>-<pid>` of the system
    /// domain, heartbeating every second over the coordinator's link, and waits until launchd
    /// runs it and it has recorded the boot id in its state directory (it has taken the state
    /// directory's lock before that). The coordinator learns of the daemon once its enrollment
    /// is approved, which the caller does next.
    ///
    /// # Panics
    /// The job cannot be loaded, or never runs with its state directory taken.
    pub fn start_daemon(&self) {
        let label = Label::parse(&format!(
            "dev.hostkeeper.ci-{}-{}",
            self.tag,
            std::process::id()
        ))
        .expect("a label");
        // The program runs from a file named by its version, as the program's version is.
        let program = self.base.join("bin").join(DAEMON_VERSION);
        let bytes = fs::read(DAEMON).unwrap_or_else(|e| panic!("read {DAEMON}: {e}"));
        put(&program, &bytes, 0o755);
        let text = |path: PathBuf| path.display().to_string();
        let argv = vec![
            text(program),
            "--fixture-dir".to_owned(),
            text(self.fixture()),
            "--fixture-link".to_owned(),
            "coordinator".to_owned(),
            "daemon".to_owned(),
            "--config".to_owned(),
            text(self.config_path()),
            "--state-dir".to_owned(),
            text(self.state()),
            "--packages-dir".to_owned(),
            text(self.packages()),
            "--facts-file".to_owned(),
            text(self.base.join("facts.json")),
            "--heartbeat-secs".to_owned(),
            "1".to_owned(),
        ];
        let plist = self.base.join(format!("{label}.plist"));
        let out = self.base.join("daemon.out.log");
        let err = self.base.join("daemon.err.log");
        put(
            &plist,
            daemon_plist(&label, &argv, &out, &err).as_bytes(),
            0o644,
        );
        bootout_quiet(&label);
        *self.daemon.lock().unwrap_or_else(PoisonError::into_inner) = Some(label.clone());
        let loaded = launchctl()
            .run(&bootstrap_args(&plist))
            .expect("launchctl bootstrap ran");
        assert_eq!(
            loaded.code,
            Some(0),
            "bootstrap {label}: {}",
            loaded.stderr_text()
        );
        self.wait_started(&label);
    }

    /// Waits until launchd runs the job `label` (`state = running`) and the daemon has recorded
    /// the boot id in [`MacHost::state`] (it took the state directory's lock before that).
    ///
    /// # Panics
    /// The job never runs with the boot id recorded.
    pub fn wait_started(&self, label: &Label) {
        let boot_id = self.state().join(BOOT_ID_FILE);
        super::wait_for(
            "launchd runs the daemon and it has recorded the boot id",
            START_TRIES,
            || {
                boot_id.exists()
                    && job_fields(label)
                        .is_some_and(|job| job.get("state").map(String::as_str) == Some("running"))
            },
        );
    }

    /// What a failing test leaves to look at, printed: the daemon's output, the slots' output,
    /// the hook's record files and the step record. Every read ignores its errors.
    fn print_artifacts(&self) {
        let mut report = String::new();
        let mut show = |name: String, path: &Path| {
            let text = fs::read_to_string(path).unwrap_or_else(|e| format!("({e})"));
            report.push_str(&format!("--- {name} ({}):\n{text}\n", path.display()));
        };
        show(
            "daemon stdout".to_owned(),
            &self.base.join("daemon.out.log"),
        );
        show(
            "daemon stderr".to_owned(),
            &self.base.join("daemon.err.log"),
        );
        if let Ok(logs) = fs::read_dir(self.log_dir()) {
            for log in logs.flatten() {
                show(
                    format!("job output {}", log.file_name().to_string_lossy()),
                    &log.path(),
                );
            }
        }
        let hook = self.records().join(SERVICE);
        if let Ok(entries) = fs::read_dir(&hook) {
            for entry in entries.flatten() {
                show(
                    format!("hook record {}", entry.file_name().to_string_lossy()),
                    &entry.path(),
                );
            }
        }
        show(
            "step record".to_owned(),
            &self.state().join(SERVICE).join(RECORD_FILE),
        );
        for label in slot_labels() {
            let printed = printed_quiet(&label);
            report.push_str(&format!("--- launchctl print system/{label}:\n{printed}\n"));
        }
        eprintln!("what the Mac held when the test failed:\n{report}");
    }
}

/// The version of the receipt of hello's package; `None` when `pkgutil` knows none.
///
/// # Panics
/// `pkgutil` cannot be run, or fails in a way that is no missing receipt.
pub fn receipt_version() -> Option<String> {
    let args = ["--pkg-info-plist".to_owned(), IDENTIFIER.to_owned()];
    let out = LaunchctlProcess::new(Path::new("/usr/sbin/pkgutil"), PKGUTIL_DEADLINE)
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

/// What `launchctl print system/<label>` prints, or why it printed nothing; never panics: this
/// runs while the test is already failing.
fn printed_quiet(label: &Label) -> String {
    match launchctl().run(&print_args(label)) {
        Ok(out) => format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            out.stderr_text()
        ),
        Err(e) => format!("({e})"),
    }
}

impl Drop for MacHost {
    /// Puts the Mac back as the test found it. Every step goes on whatever the one before it
    /// did, because a failed test is the likeliest reason for a half-built host.
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.print_artifacts();
        }
        // The daemon first, so that it orders nothing while the rest is unloaded.
        let daemon = self
            .daemon
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(label) = daemon {
            bootout_quiet(&label);
        }
        for label in slot_labels() {
            bootout_quiet(&label);
        }
        for place in taken_places() {
            match fs::symlink_metadata(&place) {
                Ok(meta) if meta.is_dir() => drop(fs::remove_dir_all(&place)),
                Ok(_) => drop(fs::remove_file(&place)),
                Err(_) => {}
            }
        }
        // The receipt of hello's package, which a test that failed before its uninstall leaves.
        let forget = ["--forget".to_owned(), IDENTIFIER.to_owned()];
        let _ =
            LaunchctlProcess::new(Path::new("/usr/sbin/pkgutil"), PKGUTIL_DEADLINE).run(&forget);
        for (place, aside) in &self.saved {
            let _ = fs::rename(aside, place);
        }
        open_up(&self.base);
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Makes every directory under `dir` writable by its owner (a sealed tree is mode 0555), so
/// that the tree can be removed.
fn open_up(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    drop(fs::set_permissions(dir, fs::Permissions::from_mode(0o755)));
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            open_up(&entry.path());
        }
    }
}
