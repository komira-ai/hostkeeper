//! The Mac the end-to-end test of `tests/device_mac_e2e.rs` runs against: a directory tree under
//! `/var` (a link to `/private/var`, as the daemon's default packages directory is reached), the
//! fixed host paths the real daemon uses on a Mac, the daemon as a launchd job of the system
//! domain, and the reads of launchd's state the test asserts on.
//!
//! The daemon reads some paths from no flag: the CA bundle its package fetch trusts
//! (`/etc/ssl/cert.pem`), `gh` and the Sigstore trusted root (`/usr/local/lib/hostkeeper`), the
//! pki directory (`/etc/updater`), the placed-path state (`/var/lib/hostkeeper`) and launchd's
//! directory (`/Library/LaunchDaemons`), where it links a package's plists. A test that drives it
//! for real puts files at those places, which is only safe on a throwaway machine, which the
//! hosted runner of `privileged.yml` is. [`MacHost::new`] moves aside whatever is at the places it takes over (another test of the crate may have left it),
//! and the drop boots out every job the test made or the daemon loaded, removes what the test
//! and the daemon wrote there, puts back what was moved aside and writes the CA bundle back as
//! it was, whether the test passed or failed. When the test is failing, the drop first prints
//! the daemon's output, the jobs' output and the hook's record to the log.
//!
//! Every read of launchd goes through `LaunchctlProcess::system`, the `launchctl` adapter the
//! daemon uses. Nothing here asserts how long anything takes; a wait polls a state
//! ([`super::wait_for`]).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use std::num::NonZeroU64;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_package::gh::{FileTrust, GhVerifier, OwnedBy, Runner, Spawn, SystemRunner};
use hostkeeper_package::ports::{Attest, CommitId, SourceRef, Verifier};
use hostkeeper_steps::{Clock, Stamp};

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::adapters::Adapters;
use hostkeeper_daemon::boot::BOOT_ID_FILE;
use hostkeeper_daemon::cli::{DEFAULT_FACTS_FILE, HostFiles, host_ports};
use hostkeeper_daemon::start::Settings;
use hostkeeper_daemon::verb::PackageDigest;
use hostkeeper_launchd::launchctl::{JobStatus, bootout_args, bootstrap_args, print, print_args};
use hostkeeper_launchd::{Label, Launchctl, LaunchctlProcess, Platform};
use hostkeeper_steps::record::RECORD_FILE;

use super::hello::{SERVICE, SLOTS, label};

/// The daemon program: `hostkeeper`, built with these tests (it holds `daemon`).
pub const DAEMON: &str = env!("CARGO_BIN_EXE_hostkeeper-device-daemon");
/// The CA bundle the daemon's package fetch trusts on a Mac (a fixed path of the daemon).
const CA_BUNDLE: &str = "/etc/ssl/cert.pem";
/// The directory of the programs the daemon trusts on a Mac: `gh` and the Sigstore trusted root.
const LIBRARY_DIR: &str = "/usr/local/lib/hostkeeper";
/// The `gh` the daemon checks attestations with on a Mac.
pub const GH_PATH: &str = "/usr/local/lib/hostkeeper/gh";
/// The Sigstore trusted root the daemon hands `gh` on a Mac.
pub const TRUSTED_ROOT_PATH: &str = "/usr/local/lib/hostkeeper/trusted_root.json";
/// The directory the enrollment loop keeps its key and certificate in.
const PKI_DIR: &str = "/etc/updater";
/// The daemon's placed-path state directory.
pub const PLACED_DIR: &str = "/var/lib/hostkeeper";
/// Where launchd reads the plists of the system domain, and the daemon links a package's.
pub const LAUNCHD_DIR: &str = "/Library/LaunchDaemons";
/// The deadline of one `launchctl` command: `bootout` waits for the job to end.
const LAUNCHCTL_DEADLINE: Duration = Duration::from_secs(60);
/// How many looks [`MacHost::start_daemon`] takes at the job and the state directory before it
/// gives up.
const START_TRIES: u32 = 240;

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

/// One package [`MacHost::check_package_store`] takes through the daemon's store: its version,
/// SHA-256, file name, bytes and Sigstore bundle.
pub struct CheckedPackage {
    pub version: String,
    pub sha256: String,
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub bundle: Vec<u8>,
}

/// The labels of hello's two slots.
fn slot_labels() -> [Label; 2] {
    SLOTS.map(label)
}

/// The places of the host this rig takes over: the daemon's programs, its pki and placed
/// state, its default facts file (a daemon started without `--facts-file`, as the across-release
/// rig's must be, writes it), and the plists of hello's two slots.
fn taken_places() -> Vec<PathBuf> {
    let mut places = vec![
        PathBuf::from(LIBRARY_DIR),
        PathBuf::from(PKI_DIR),
        PathBuf::from(PLACED_DIR),
        PathBuf::from(DEFAULT_FACTS_FILE),
    ];
    places.extend(slot_labels().map(|label| plist_link(&label)));
    places
}

/// Where the daemon links the plist of `label`.
pub fn plist_link(label: &Label) -> PathBuf {
    Path::new(LAUNCHD_DIR).join(format!("{label}.plist"))
}

/// The test's view of the Mac: its directory tree and what it changed outside it.
///
/// Dropping it undoes every change (see the module documentation).
pub struct MacHost {
    tag: String,
    base: PathBuf,
    /// The label of the daemon's job, once [`MacHost::start_daemon`] loaded it.
    daemon: Mutex<Option<Label>>,
    /// The bytes of the CA bundle before [`MacHost::trust_ca`] first appended to it.
    ca_bundle: Mutex<Option<Vec<u8>>>,
    /// The places taken over that were there already, each with where it was moved aside.
    saved: Vec<(PathBuf, PathBuf)>,
}

impl MacHost {
    /// Checks that the test runs as root, moves aside what is at the places the rig takes over,
    /// and makes the test's tree and the host directories the daemon reads programs from and
    /// keeps its placed state in.
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
        // Under /var, a link to /private/var, as the daemon's default `/var/lib/hostkeeper` is:
        // the seal names each version by its real path, which the check before each step judges.
        let base = PathBuf::from(format!("/var/hk-e2e-{tag}-{pid}"));
        // `create_dir_all` would take over a directory that is already there.
        fs::create_dir(&base).unwrap_or_else(|e| panic!("create {}: {e}", base.display()));
        let host = MacHost {
            tag: tag.to_owned(),
            base,
            daemon: Mutex::new(None),
            ca_bundle: Mutex::new(None),
            saved,
        };
        make_dir(&host.base, 0o755);
        make_dir(&host.packages(), 0o755);
        make_dir(&host.state(), 0o755);
        make_dir(&host.records(), 0o755);
        make_dir(&host.base.join("logs"), 0o755);
        make_dir(Path::new(PLACED_DIR), 0o755);
        make_dir(Path::new(LIBRARY_DIR), 0o755);
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

    /// The directory the jobs of `version`'s plists send their output to (made here).
    pub fn log_dir(&self, version: &str) -> PathBuf {
        let dir = self.base.join("logs").join(version);
        make_dir(&dir, 0o755);
        dir
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

    /// Puts the `gh` program at the path the daemon runs it from on a Mac, mode 0755, owned by
    /// root. Its bytes are written to a new file: `fs::copy` clones the file on macOS and keeps
    /// the owner of the source (the runner's user, who unpacked it), and the daemon trusts a `gh`
    /// owned by root only.
    ///
    /// # Panics
    /// The program cannot be read or written, or the new file is not root's.
    pub fn install_gh(&self, gh: &Path) {
        let to = Path::new(GH_PATH);
        let bytes = fs::read(gh).unwrap_or_else(|e| panic!("read {}: {e}", gh.display()));
        put(to, &bytes, 0o755);
        let owner = fs::metadata(to).map(|meta| meta.uid());
        assert_eq!(owner.ok(), Some(0), "{} is root's", to.display());
    }

    /// Puts the Sigstore trusted root (`jsonl`) where the daemon reads it on a Mac, mode 0644.
    pub fn install_trusted_root(&self, jsonl: &str) {
        put(Path::new(TRUSTED_ROOT_PATH), jsonl.as_bytes(), 0o644);
    }

    /// Makes the daemon's package fetch trust the CA `pem` by appending it to the host's CA
    /// bundle. The first call keeps the bundle's original bytes, which the drop puts back.
    ///
    /// # Panics
    /// The bundle cannot be read or written.
    pub fn trust_ca(&self, pem: &str) {
        let mut original = self
            .ca_bundle
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if original.is_none() {
            let bytes = fs::read(CA_BUNDLE).unwrap_or_else(|e| panic!("read {CA_BUNDLE}: {e}"));
            *original = Some(bytes);
        }
        let mut bundle = fs::OpenOptions::new()
            .append(true)
            .open(CA_BUNDLE)
            .unwrap_or_else(|e| panic!("open {CA_BUNDLE}: {e}"));
        // A bundle that does not end in a newline would swallow the first line of the PEM.
        let ends_in_newline = original.as_deref().is_none_or(|b| b.last() == Some(&b'\n'));
        let separator = if ends_in_newline { "" } else { "\n" };
        bundle
            .write_all(format!("{separator}{pem}\n").as_bytes())
            .unwrap_or_else(|e| panic!("append to {CA_BUNDLE}: {e}"));
    }

    /// Writes the daemon config at [`MacHost::config_path`], mode 0600, owned by root.
    pub fn write_config(&self, text: &str) {
        put(&self.config_path(), text.as_bytes(), 0o600);
    }

    /// Starts the daemon as the launchd job `dev.hostkeeper.ci-<tag>-<pid>` of the system
    /// domain, heartbeating every second, and waits until launchd runs it and it has recorded the
    /// boot id in its state directory (it has taken the state directory's lock before that). The
    /// coordinator learns of the daemon once its enrollment is approved, which the caller does
    /// next.
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
        let text = |path: PathBuf| path.display().to_string();
        let argv = vec![
            DAEMON.to_owned(),
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
    /// the boot id in [`MacHost::state`] (it took the state directory's lock before that), the
    /// signals that a daemon of this checkout and of the older release give alike.
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

    /// Takes each `(version, digest)` of `service` through the daemon's own package store on
    /// this Mac, as an order does before its first step: `host_ports` over the daemon config
    /// `config` and the host's files and adapters, then `version_dir` (fetch from the release
    /// channel, the attestation checked by the installed `gh`, the digest, the unpack with the
    /// Mac's quarantine and code-signature checks, the seal), into a packages directory of its
    /// own, so that the daemon's stays untouched. A refusal there is one the daemon would give
    /// the order and report only once the service is listed, so it fails the test here with
    /// the daemon's own reason and detail.
    ///
    /// # Panics
    /// The store cannot be built, or it refuses a version.
    pub fn check_package_store(&self, config: &str, service: &str, versions: &[CheckedPackage]) {
        let parsed = DaemonConfig::parse(config).expect("the daemon config parses");
        let settings = Settings {
            config: self.config_path(),
            state_dir: self.base.join("check-state"),
            packages_dir: self.base.join("check-packages"),
            facts_file: self.base.join("facts.json"),
            run_dir: self.base.join("check-run"),
            trusted_uid: 0,
            trusted_root_max_age_days: NonZeroU64::new(3650).expect("not zero"),
        };
        make_dir(&settings.state_dir, 0o755);
        make_dir(&settings.packages_dir, 0o755);
        let own = HostkeeperVersion::parse(env!("CARGO_PKG_VERSION")).expect("a version");
        let adapters = Adapters::for_platform(&Platform::host())
            .unwrap_or_else(|e| panic!("the Mac's adapters: {e}"));
        let ports = host_ports(&parsed, &settings, &HostFiles::host(), own, adapters)
            .unwrap_or_else(|e| panic!("the daemon's ports: {e}"));
        let name = ServiceName::new(service).expect("a service name");
        for package in versions {
            let digest = PackageDigest::new(&package.sha256).expect("a digest");
            let version = Version::new(&package.version).expect("a version");
            if let Err(refused) = ports
                .packages
                .version_dir(&name, &version, &digest, None, None)
            {
                let why = self.explain_attestation(&parsed, package);
                panic!("the daemon's package store refuses {service} {version}: {refused}\n{why}");
            }
        }
    }

    /// What the attestation check meets on this Mac, for the message of a refused package:
    /// the trust check of `gh` and of the trusted root, `gh --version`, and the production
    /// verifier run on `package` and its bundle, each through the daemon's own `SystemRunner`
    /// and `OwnedBy`, with the time each took (printed, never judged).
    fn explain_attestation(&self, config: &DaemonConfig, package: &CheckedPackage) -> String {
        let files = HostFiles::host();
        let clock: Arc<dyn Clock> = Arc::new(hostkeeper_daemon::adapters::host_clock());
        let home = self.base.join("explain-gh");
        make_dir(&home, 0o700);
        let mut out = String::new();
        for path in [&files.gh, &files.trusted_root] {
            let meta = fs::symlink_metadata(path).map(|m| (m.uid(), format!("{:o}", m.mode())));
            let parent = path
                .parent()
                .map(|dir| fs::metadata(dir).map(|m| (m.uid(), format!("{:o}", m.mode()))));
            out.push_str(&format!(
                "trust of {}: {:?} (file {meta:?}, directory {parent:?})\n",
                path.display(),
                OwnedBy { uid: 0 }.check(path)
            ));
        }
        let deadline = |secs: u64| {
            let now = Stamp::now(clock.as_ref());
            Stamp {
                mono_secs: now.mono_secs + secs,
                ..now
            }
        };
        let runner = SystemRunner::new(Arc::clone(&clock));
        let started = Stamp::now(clock.as_ref());
        let version = runner.run(&Spawn {
            program: files.gh.clone(),
            args: vec!["--version".into()],
            env: Vec::new(),
            cwd: home.clone(),
            deadline: deadline(120),
        });
        out.push_str(&format!(
            "gh --version: {version:?}, after {:?} s\n",
            started.elapsed_secs(clock.as_ref())
        ));
        let archive = home.join(&package.file_name);
        let bundle = home.join(format!("{}.sigstore.json", package.file_name));
        put(&archive, &package.bytes, 0o644);
        put(&bundle, &package.bundle, 0o644);
        let verifier = GhVerifier::new(
            files.gh.clone(),
            files.trusted_root.clone(),
            home.clone(),
            Box::new(SystemRunner::new(Arc::clone(&clock))),
            Box::new(OwnedBy { uid: 0 }),
        );
        let sha256 = Sha256Hex::parse(&package.sha256).expect("a digest");
        let commit = CommitId::new(super::COMMIT).expect("a commit");
        let source_ref = SourceRef::new(super::SOURCE_REF).expect("a ref");
        let until = deadline(600);
        let started = Stamp::now(clock.as_ref());
        let verdict = verifier.verify(&Attest {
            archive: &archive,
            bundle: &bundle,
            sha256: &sha256,
            commit: &commit,
            source_ref: &source_ref,
            signers: &config.trusted_signers,
            deadline: &until,
        });
        out.push_str(&format!(
            "gh attestation verify of {}: {verdict:?}, after {:?} s (the daemon allows {} s)\n",
            package.file_name,
            started.elapsed_secs(clock.as_ref()),
            config.bounds.attestation_timeout_secs
        ));
        out
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
        if let Ok(versions) = fs::read_dir(self.base.join("logs")) {
            for dir in versions.flatten() {
                if let Ok(logs) = fs::read_dir(dir.path()) {
                    for log in logs.flatten() {
                        show(
                            format!("job output {}", log.file_name().to_string_lossy()),
                            &log.path(),
                        );
                    }
                }
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
        // The daemon first, so that it loads nothing while the rest is unloaded.
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
        let original = self
            .ca_bundle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(bytes) = original {
            let _ = fs::write(CA_BUNDLE, bytes);
        }
        for (place, aside) in &self.saved {
            let _ = fs::rename(aside, place);
        }
        open_up(&self.base);
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Makes every directory under `dir` writable by its owner (unpacked trees are mode 0555), so
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
