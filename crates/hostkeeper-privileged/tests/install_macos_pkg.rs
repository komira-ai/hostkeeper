#![cfg(all(target_os = "macos", feature = "privileged"))]
//! The `macos-pkg` installer (`hostkeeper_install::macos_pkg::MacosPkg`) against the Mac's real
//! `pkgutil`, `installer`, `lsbom` and `launchctl`, as root, in privileged.yml's job
//! `macos-device` (and pr-device.yml's copy of it). The daemon's fetch is not part of it; the test
//! drives the installer's steps directly, with the version directories a core would keep (a
//! staging directory per version, renamed to the version once `stage` has filled it).
//!
//! Nothing is signed on the runner, and no trust setting, keychain or keychain search list is
//! changed: on a GitHub-hosted macos-15 runner `productsign` refused the installer identity the
//! test made, whose root the Mac did not trust, and the undo of a trust setting there made no
//! progress until the job's timeout (docs/operations/pkg-signing-test.md). The fixtures are M11's two
//! unsigned forms, built at test time with M11's commands (`crates/xtask/src/mdm_signing_test`):
//! a component package (`pkgbuild`, M11's pkg-1) and a distribution package around it
//! (`productbuild --package`, M11's pkg-2). Every program the test runs is bounded by
//! [`TOOL_BOUND`], so one that hangs fails the test naming it.
//!
//! The real `pkgutil --check-signature` refuses both unsigned forms at `stage`. To drive the
//! rest of the lifecycle with real tools, the installer's [`Tools`] port is the Mac's own but for
//! one call: `pkgutil --check-signature` of a package the test names is answered as a package
//! signed by the leaf [`LEAF`] with a chain the Mac trusts ([`Signed`]). Every other call
//! (`--expand-full`, `lsbom`, `installer`, the receipts, `launchctl`) is the Mac's. What this
//! does not show on real tools is the form `pkgutil` prints of a trusted signature and of its
//! leaf's fingerprint; that waits for M11's signed run on an enrolled Mac.
//!
//! - `the_forms_of_pkgutils_answers_are_the_ones_the_parsers_read`: `pkgutil --check-signature`
//!   of each unsigned form, `--pkg-info-plist` of an unknown identifier and `--file-info-plist` of
//!   a file no receipt lists, in the forms `hostkeeper_install` parses.
//! - `a_package_is_refused_unsigned_then_installed_updated_repaired_rolled_back_and_removed`:
//!   `stage` refuses both unsigned forms (real) and a leaf no signer names (the answered call);
//!   the real expansion of each form passes `stage`'s checks; then install, update (with a hook at
//!   its fixed path, the job enabled by launchd's own override), a hand install of the older
//!   version found as drift and put back by the next install, rollback with no signature check,
//!   and removal of a two-version fixture.
//!
//! - `a_mac_daemon_announces_macos_pkg_with_its_signers_file_and_installs_the_ordered_pkg`: the
//!   daemon's own order path in this process (`install_macos_pkg/daemon.rs`) on the Mac's
//!   adapters and tools: with no signers file it announces `hostkeeper-archive` alone and defers
//!   an order of the `.pkg` `installer_not_listed`, fetching nothing; with the signers file
//!   (loaded by the daemon's loader) and the Mac's `installer` and `pkgutil` it announces
//!   `macos-pkg`, refuses the unsigned `.pkg` at `stage` (`signature`, real `pkgutil`, nothing
//!   staged or installed), installs the answered one, updates it, and uninstalls it.
//!
//! The tests run one after the other (`--test-threads=1`, which the workflow lint holds). It
//! runs as root and says so when it does not: it never skips. Like the other device tests it is
//! not counted by the coverage gate.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::host::Host;
use hostkeeper_install::lock::InstallerLock;
use hostkeeper_install::macos_pkg::record::{PkgRecord, RecordError, Records};
use hostkeeper_install::macos_pkg::signers::{Fingerprint, Signers};
use hostkeeper_install::macos_pkg::{Bounds, MacosPkg, Order, Seen, drift, receipt, signature};
use hostkeeper_install::tools::{ToolError, ToolOutput};
use hostkeeper_install::{Failure, HostTools, Reason, Tool, Tools};
use hostkeeper_launchd::{Label, Launchctl, LaunchctlProcess};
use hostkeeper_testkit::Scratch;

#[path = "install_macos_pkg/daemon.rs"]
mod daemon;

const SERVICE: &str = "hk-a9-fixture";
const IDENTIFIER: &str = "dev.hostkeeper.a9-fixture";
const SUPPORT: &str = "/Library/Application Support/hk-a9-fixture";
const PLIST: &str = "/Library/LaunchDaemons/hk-a9-fixture.plist";
/// The hook of 1.1.0, at its fixed path.
const HOOK: &str = "/Library/Application Support/hk-a9-fixture/hostkeeper-hook";
/// The most any program the test runs is waited for before it is killed and the test fails
/// naming it: a bound on a hang, not a measure of time.
const TOOL_BOUND: Duration = Duration::from_secs(300);
/// The leaf certificate's SHA-256 the answered `--check-signature` names, and the host's signer.
const LEAF: &str = "a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9";

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

/// Runs `program` with `args` through the bounded runner (an empty environment, no standard
/// input, at most [`TOOL_BOUND`]); how it ended, whatever its status.
fn run_any(program: &str, args: &[&str]) -> ToolOutput {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    LaunchctlProcess::new(Path::new(program), TOOL_BOUND)
        .run(&args)
        .unwrap_or_else(|e| panic!("{program} did not run to its end: {e}"))
}

/// Runs `program` with `args`, which must exit 0; its standard output.
fn run(program: &str, args: &[&str]) -> String {
    let out = run_any(program, args);
    assert_eq!(
        out.code,
        Some(0),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What `out` printed, standard error then standard output.
fn said(out: &ToolOutput) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    )
}

fn text(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

fn require_root() {
    assert!(
        rustix::process::geteuid().is_root(),
        "install_macos_pkg runs as root, in the job macos-device (the cargo runner sudo); it does not skip"
    );
}

/// What the test changes on the host outside its scratch directory, undone on drop (a failed
/// test too): the fixture's receipt and installed files. Nothing here may panic.
struct Undo;

impl Drop for Undo {
    fn drop(&mut self) {
        let forget = ["--forget".to_owned(), IDENTIFIER.to_owned()];
        let _ = LaunchctlProcess::new(Path::new(Tool::Pkgutil.path()), TOOL_BOUND).run(&forget);
        let _ = fs::remove_dir_all(SUPPORT);
        let _ = fs::remove_file(PLIST);
    }
}

/// `pkgutil --check-signature`'s answer for a package signed by [`LEAF`] whose chain the Mac
/// trusts, in the form `hostkeeper_install::macos_pkg::signature` documents.
fn signed_answer(package: &str) -> String {
    let hex_line = |hex: &str| {
        hex.as_bytes()
            .chunks(2)
            .map(|pair| String::from_utf8_lossy(pair).to_uppercase())
            .collect::<Vec<_>>()
            .join(" ")
    };
    [
        format!("Package \"{package}\":"),
        "   Status: signed by a certificate trusted by macOS".to_owned(),
        "   Certificate Chain:".to_owned(),
        "    1. hostkeeper a9 fixture installer".to_owned(),
        "       SHA256 Fingerprint:".to_owned(),
        format!("           {}", hex_line(&LEAF[..32])),
        format!("           {}", hex_line(&LEAF[32..])),
        format!("       {}", "-".repeat(72)),
        String::new(),
    ]
    .join("\n")
}

/// The Mac's tools, but for `pkgutil --check-signature` of a package in `packages`, answered as
/// signed by [`LEAF`] with a chain the Mac trusts. Every `--check-signature` asked is recorded.
#[derive(Default)]
struct Signed {
    packages: Mutex<Vec<PathBuf>>,
    asked: Mutex<Vec<String>>,
}

impl Signed {
    fn answer_for(&self, package: &Path) {
        self.packages
            .lock()
            .expect("packages")
            .push(package.to_owned());
    }

    fn asked(&self) -> usize {
        self.asked.lock().expect("asked").len()
    }
}

impl Tools for Signed {
    fn run(
        &self,
        tool: Tool,
        args: &[String],
        deadline: Duration,
    ) -> Result<ToolOutput, ToolError> {
        if tool == Tool::Pkgutil && args.first().map(String::as_str) == Some("--check-signature") {
            let package = args.get(1).cloned().unwrap_or_default();
            self.asked.lock().expect("asked").push(package.clone());
            let answered = self.packages.lock().expect("packages");
            if answered.iter().any(|path| text(path) == package) {
                return Ok(ToolOutput {
                    code: Some(0),
                    stdout: signed_answer(&package).into_bytes(),
                    stderr: Vec::new(),
                });
            }
        }
        HostTools.run(tool, args, deadline)
    }
}

/// The fixture's content at `version`: 1.0.0 has `legacy.txt` and a later version drops it and
/// adds `lib/new.txt` in a directory 1.0.0 lacks; 1.1.0 also has its hook at the fixed path.
/// With `job`, it carries the plist of the job [`SERVICE`]; without, `/Library/LaunchDaemons`
/// is only a directory above the service's files.
fn payload(root: &Path, version: &str, job: bool) {
    let service = root.join("Library/Application Support/hk-a9-fixture");
    let mut dirs = vec![
        root.join("Library"),
        root.join("Library/Application Support"),
        service.clone(),
        service.join("bin"),
        root.join("Library/LaunchDaemons"),
    ];
    let mut files: Vec<(PathBuf, String, u32)> = vec![(
        service.join("bin/fixture"),
        format!("#!/bin/sh\necho {version}\n"),
        0o755,
    )];
    let plist = (
        root.join("Library/LaunchDaemons/hk-a9-fixture.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
                 \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\"><dict>\n<key>Label</key><string>{SERVICE}</string>\n\
                 <key>Disabled</key><true/>\n<key>ProgramArguments</key><array>\
                 <string>{SUPPORT}/bin/fixture</string></array>\n</dict></plist>\n"
        ),
        0o644,
    );
    if job {
        files.push(plist);
    }
    if version == "1.0.0" {
        files.push((
            service.join("legacy.txt"),
            "dropped in 1.1.0\n".into(),
            0o644,
        ));
    } else {
        dirs.push(service.join("lib"));
        files.push((
            service.join("lib/new.txt"),
            "added in 1.1.0\n".into(),
            0o644,
        ));
    }
    if version == "1.1.0" {
        files.push((
            service.join("hostkeeper-hook"),
            "#!/bin/sh\nexit 0\n".into(),
            0o755,
        ));
    }
    for dir in &dirs {
        fs::create_dir_all(dir).expect("create a payload directory");
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    for (path, content, mode) in files {
        fs::write(&path, content).expect("write a payload file");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
    }
    run("/usr/sbin/chown", &["-R", "0:0", text(root)]);
}

/// The fixture at `version`, unsigned, in M11's two forms, built into `dir` with M11's
/// commands: the component package (`pkgbuild`, pkg-1's form) and the distribution package
/// around it (`productbuild --package`, pkg-2's form).
struct Forms {
    component: PathBuf,
    product: PathBuf,
}

fn build_unsigned(dir: &Path, version: &str) -> Forms {
    build_unsigned_with(dir, version, true)
}

/// [`build_unsigned`], the payload carrying the job's plist only when `job`.
fn build_unsigned_with(dir: &Path, version: &str, job: bool) -> Forms {
    let root = dir.join(format!("root-{version}"));
    fs::create_dir(&root).expect("create the payload root");
    payload(&root, version, job);
    let component = dir.join(format!("component-{version}.pkg"));
    run(
        "/usr/bin/pkgbuild",
        &[
            "--root",
            text(&root),
            "--identifier",
            IDENTIFIER,
            "--version",
            version,
            "--install-location",
            "/",
            "--ownership",
            "recommended",
            text(&component),
        ],
    );
    let product = dir.join(format!("product-{version}.pkg"));
    run(
        "/usr/bin/productbuild",
        &[
            "--package",
            text(&component),
            "--identifier",
            IDENTIFIER,
            "--version",
            version,
            text(&product),
        ],
    );
    Forms { component, product }
}

/// The records of the installer, in memory.
#[derive(Default)]
struct Memory(Mutex<BTreeMap<String, PkgRecord>>);

impl Memory {
    fn get(&self) -> Option<PkgRecord> {
        self.0.lock().expect("records").get(SERVICE).cloned()
    }
}

impl Records for Memory {
    fn load(&self, service: &ServiceName) -> Result<Option<PkgRecord>, RecordError> {
        Ok(self
            .0
            .lock()
            .expect("records")
            .get(service.as_str())
            .cloned())
    }

    fn save(&self, service: &ServiceName, record: &PkgRecord) -> Result<(), RecordError> {
        self.0
            .lock()
            .expect("records")
            .insert(service.as_str().to_owned(), record.clone());
        Ok(())
    }

    fn holder(
        &self,
        identifier: &str,
        service: &ServiceName,
    ) -> Result<Option<ServiceName>, RecordError> {
        let held = self.0.lock().expect("records");
        let other = held
            .iter()
            .find(|(name, record)| {
                name.as_str() != service.as_str() && record.identifier == identifier
            })
            .map(|(name, _)| ServiceName::new(name).expect("a service name"));
        Ok(other)
    }
}

/// The installer's ports on the real host, and the packages directory the version directories
/// are in.
struct Rig {
    host: Host,
    tools: Signed,
    records: Memory,
    lock: InstallerLock,
    packages: PathBuf,
}

impl Rig {
    fn new(dir: &Path) -> Rig {
        let packages = dir.join("packages");
        fs::create_dir_all(&packages).expect("create the packages directory");
        Rig {
            host: Host::mac(),
            tools: Signed::default(),
            records: Memory::default(),
            lock: InstallerLock::new(&dir.join("installer.lock")),
            packages,
        }
    }

    fn installer<'a>(&'a self, signers: &'a Signers) -> MacosPkg<'a> {
        MacosPkg {
            tools: &self.tools,
            host: &self.host,
            records: &self.records,
            lock: &self.lock,
            signers,
        }
    }

    fn order(&self, version: &str) -> Order {
        let wait = TOOL_BOUND;
        Order {
            service: ServiceName::new(SERVICE).expect("service name"),
            version: Version::new(version).expect("version"),
            identifier: IDENTIFIER.to_owned(),
            run: vec![Label::parse(SERVICE).expect("label")],
            bounds: Bounds {
                stage: wait,
                activate: wait,
                remove: wait,
            },
            boot: false,
        }
    }

    /// Stages `artifact` as `version` into a staging directory, which becomes the version's
    /// directory when `stage` succeeds, as the core does; on a failure the staging directory.
    fn stage(
        &self,
        signers: &Signers,
        order: &Order,
        artifact: &Path,
    ) -> Result<PathBuf, (Failure, PathBuf)> {
        let version = order.version.as_str();
        let staging = self
            .packages
            .join(format!("{version}.staging-{}", self.count()));
        fs::create_dir(&staging).expect("create the staging directory");
        match self.installer(signers).stage(order, artifact, &staging) {
            Ok(()) => {
                let dir = self.packages.join(version);
                fs::rename(&staging, &dir).expect("name the version directory");
                Ok(dir)
            }
            Err(failure) => Err((failure, staging)),
        }
    }

    fn count(&self) -> usize {
        fs::read_dir(&self.packages)
            .expect("read the packages directory")
            .count()
    }
}

/// Whether launchd's override enables `label`: `launchctl print-disabled system` prints
/// `"<label>" => enabled` (or `=> false` on older releases) once `launchctl enable` ran.
fn enabled_by_launchd(label: &str) -> bool {
    let printed = run("/bin/launchctl", &["print-disabled", "system"]);
    let key = format!("\"{label}\" =>");
    printed.lines().any(|line| {
        line.trim()
            .strip_prefix(&key)
            .is_some_and(|rest| matches!(rest.trim(), "enabled" | "false"))
    })
}

/// The version of the receipt of the fixture's identifier; `None` when there is none.
fn receipt_version() -> Option<String> {
    let out = run_any("/usr/sbin/pkgutil", &["--pkg-info-plist", IDENTIFIER]);
    if out.code != Some(0) {
        let said = said(&out);
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

/// Asserts that `path` is a file or directory of root:wheel with `mode`.
fn assert_installed(path: &str, mode: u32) {
    let meta =
        fs::symlink_metadata(path).unwrap_or_else(|e| panic!("{path} is not installed: {e}"));
    assert_eq!(
        (meta.uid(), meta.gid(), meta.mode() & 0o7777),
        (0, 0, mode),
        "{path} is root:wheel with mode {mode:o}"
    );
}

fn assert_absent(path: &str) {
    assert!(fs::symlink_metadata(path).is_err(), "{path} is gone");
}

/// Stages `artifact` as 1.0.0 and expects `reason`, with nothing expanded; the failure.
fn assert_refused(rig: &Rig, signers: &Signers, artifact: &Path, reason: Reason) -> Failure {
    let (failure, staging) = rig
        .stage(signers, &rig.order("1.0.0"), artifact)
        .expect_err("stage refuses it");
    assert_eq!(failure.reason(), reason, "{failure}");
    assert!(!staging.join("expanded").exists(), "nothing was expanded");
    assert!(!staging.join("tree").exists(), "no payload was kept");
    failure
}

#[test]
fn the_forms_of_pkgutils_answers_are_the_ones_the_parsers_read() {
    require_root();
    let dir = scratch("install-macos-pkg-forms");
    let forms = build_unsigned(dir.path(), "1.0.0");
    let signers = Signers::new(vec![Fingerprint::parse(LEAF).expect("64 hex")]).expect("one");
    for package in [&forms.component, &forms.product] {
        let checked = run_any("/usr/sbin/pkgutil", &["--check-signature", text(package)]);
        let printed = String::from_utf8_lossy(&checked.stdout).into_owned();
        eprintln!(
            "pkgutil --check-signature of {} exited {:?}:\n{}",
            text(package),
            checked.code,
            said(&checked)
        );
        let status = signature::status(&printed).expect("a status line");
        assert!(
            !signature::TRUSTED_STATUS
                .iter()
                .any(|ok| status.starts_with(ok)),
            "the unsigned package's status {status:?} is not one TRUSTED_STATUS accepts"
        );
        assert_eq!(signature::leaf_fingerprint(&printed), None, "{printed}");
        assert!(
            signature::check(checked.code, &printed, &signers).is_err(),
            "{printed}"
        );
    }

    let unknown = run_any(
        "/usr/sbin/pkgutil",
        &["--pkg-info-plist", "dev.hostkeeper.a9-unknown"],
    );
    assert_ne!(unknown.code, Some(0), "no receipt is a non-zero exit");
    assert!(
        said(&unknown).contains(receipt::NO_RECEIPT),
        "{}",
        said(&unknown)
    );

    let file = dir.path().join("listed-by-no-receipt");
    fs::write(&file, "x").expect("write the file");
    let info = run_any("/usr/sbin/pkgutil", &["--file-info-plist", text(&file)]);
    assert_eq!(info.code, Some(0), "{}", said(&info));
    assert_eq!(
        receipt::parse_file_info(&info.stdout).expect("pkgutil's file info"),
        Vec::<String>::new(),
        "no receipt lists the file"
    );
}

#[test]
fn a_package_is_refused_unsigned_then_installed_updated_repaired_rolled_back_and_removed() {
    require_root();
    let _undo = Undo;
    let dir = scratch("install-macos-pkg-lifecycle");
    assert!(
        fs::symlink_metadata(SUPPORT).is_err(),
        "the fixture is not on the host to begin with"
    );
    let launch_daemons_before = fs::symlink_metadata("/Library/LaunchDaemons").is_ok();

    let rig = Rig::new(dir.path());
    let signers = Signers::new(vec![Fingerprint::parse(LEAF).expect("64 hex")]).expect("one");
    let v100 = build_unsigned(dir.path(), "1.0.0");
    let v110 = build_unsigned(dir.path(), "1.1.0");

    // (b) The real `pkgutil` refuses both unsigned forms before anything is expanded; a leaf no
    // signer names is refused too (the answered call).
    for unsigned in [&v100.component, &v100.product] {
        let failure = assert_refused(&rig, &signers, unsigned, Reason::Signature);
        eprintln!("{}: {failure}", text(unsigned));
    }
    rig.tools.answer_for(&v100.product);
    let elsewhere =
        Signers::new(vec![Fingerprint::parse(&"ab".repeat(32)).expect("64 hex")]).expect("one");
    assert_refused(&rig, &elsewhere, &v100.product, Reason::Signature);
    assert_eq!(receipt_version(), None, "nothing was installed");

    // The real expansion of the component form passes the shape, Bom, payload and hook checks.
    let component_rig = Rig::new(&dir.path().join("component"));
    component_rig.tools.answer_for(&v100.component);
    component_rig
        .stage(&signers, &component_rig.order("1.0.0"), &v100.component)
        .unwrap_or_else(|(f, _)| panic!("the component form: {f}"));

    // (c) Install 1.0.0.
    let dir100 = rig
        .stage(&signers, &rig.order("1.0.0"), &v100.product)
        .unwrap_or_else(|(f, _)| panic!("{f}"));
    let activated = rig
        .installer(&signers)
        .activate(&rig.order("1.0.0"), &dir100, None)
        .unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(activated.drift, None, "a first install found no drift");
    assert_eq!(receipt_version().as_deref(), Some("1.0.0"));
    assert_installed(SUPPORT, 0o755);
    assert_installed(&format!("{SUPPORT}/bin/fixture"), 0o755);
    assert_installed(&format!("{SUPPORT}/legacy.txt"), 0o644);
    assert_installed(PLIST, 0o644);
    assert_absent(&format!("{SUPPORT}/lib"));
    assert_absent(HOOK);
    let record = rig
        .records
        .get()
        .expect("the installer recorded the install");
    assert_eq!((record.version.as_str(), record.pending), ("1.0.0", false));
    assert!(
        record.created_dirs.contains(SUPPORT),
        "{:?}",
        record.created_dirs
    );

    // (d) Update to 1.1.0, whose hook (at its fixed path) lets the order set `hook.boot`: the
    // dropped file goes, the new file, directory and hook come.
    rig.tools.answer_for(&v110.product);
    let mut order110 = rig.order("1.1.0");
    order110.boot = true;
    let dir110 = rig
        .stage(&signers, &order110, &v110.product)
        .unwrap_or_else(|(f, _)| panic!("{f}"));
    rig.installer(&signers)
        .activate(&order110, &dir110, Some(&dir100))
        .unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(receipt_version().as_deref(), Some("1.1.0"));
    assert_absent(&format!("{SUPPORT}/legacy.txt"));
    assert_installed(&format!("{SUPPORT}/lib"), 0o755);
    assert_installed(&format!("{SUPPORT}/lib/new.txt"), 0o644);
    assert_installed(&format!("{SUPPORT}/bin/fixture"), 0o755);
    assert_installed(HOOK, 0o755);
    let record = rig
        .records
        .get()
        .expect("the installer recorded the update");
    assert_eq!(record.version, "1.1.0");
    assert!(
        record.created_dirs.contains(&format!("{SUPPORT}/lib")),
        "{:?}",
        record.created_dirs
    );
    assert!(
        enabled_by_launchd(SERVICE),
        "activate enabled the job of the run label"
    );

    // (d') A hand install of 1.0.0 over 1.1.0 is drift, which the observer sees with no tool run
    // and the next install of 1.1.0 (`current`, so no `from`) puts back as `drift_repaired`.
    run(
        "/usr/sbin/installer",
        &[
            "-allowUntrusted",
            "-pkg",
            text(&dir100.join("macos-pkg/artifact.pkg")),
            "-target",
            "/",
        ],
    );
    let mut seen = Seen::default();
    let found = drift::observe(
        &rig.host,
        IDENTIFIER,
        rig.records.get().as_ref(),
        Some(&dir110),
        &mut seen,
    )
    .expect("the observer reads the receipt")
    .expect("the hand install is drift");
    assert_eq!(
        (found.found.as_deref(), found.recorded.as_deref()),
        (Some("1.0.0"), Some("1.1.0"))
    );
    let asked = rig.tools.asked();
    let repaired = rig
        .installer(&signers)
        .activate(&order110, &dir110, None)
        .unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(repaired.drift, Some(Some("1.0.0".to_owned())));
    assert_eq!(receipt_version().as_deref(), Some("1.1.0"));
    assert_absent(&format!("{SUPPORT}/legacy.txt"));
    assert_installed(&format!("{SUPPORT}/lib/new.txt"), 0o644);

    // (e) Roll back to 1.0.0: the kept package is installed with `-allowUntrusted`, and no
    // signature is checked (no `--check-signature` is asked by any `activate`).
    rig.installer(&signers)
        .activate(&rig.order("1.0.0"), &dir100, Some(&dir110))
        .unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(rig.tools.asked(), asked, "activate checks no signature");
    assert_eq!(receipt_version().as_deref(), Some("1.0.0"));
    assert_installed(&format!("{SUPPORT}/legacy.txt"), 0o644);
    assert_absent(&format!("{SUPPORT}/lib/new.txt"));
    assert_absent(&format!("{SUPPORT}/lib"));
    assert_absent(HOOK);
    let record = rig
        .records
        .get()
        .expect("the installer recorded the rollback");
    assert_eq!(record.version, "1.0.0");
    assert!(
        !record.created_dirs.contains(&format!("{SUPPORT}/lib")),
        "{:?}",
        record.created_dirs
    );

    // (f) Remove: every payload file and the directories the installs created are gone, the
    // receipt is forgotten, and a directory that was there before stays.
    rig.installer(&signers)
        .remove(&rig.order("1.0.0"), &[dir100.as_path(), dir110.as_path()])
        .unwrap_or_else(|f| panic!("{f}"));
    for path in [
        format!("{SUPPORT}/bin/fixture"),
        format!("{SUPPORT}/legacy.txt"),
        format!("{SUPPORT}/lib/new.txt"),
        HOOK.to_owned(),
        PLIST.to_owned(),
        format!("{SUPPORT}/bin"),
        format!("{SUPPORT}/lib"),
        SUPPORT.to_owned(),
    ] {
        assert_absent(&path);
    }
    assert_eq!(receipt_version(), None, "pkgutil forgot the receipt");
    let pkgs = run_any("/usr/sbin/pkgutil", &["--pkgs"]);
    assert_eq!(pkgs.code, Some(0), "{}", said(&pkgs));
    assert!(
        !hostkeeper_install::tools::may_be_cut(&pkgs.stdout),
        "pkgutil --pkgs printed all it had"
    );
    assert!(
        !String::from_utf8_lossy(&pkgs.stdout)
            .lines()
            .any(|line| line.trim() == IDENTIFIER),
        "pkgutil --pkgs lists no {IDENTIFIER}"
    );
    assert!(
        fs::metadata("/Library/LaunchDaemons").is_ok(),
        "the directory that was there stays"
    );
    assert!(
        launch_daemons_before,
        "/Library/LaunchDaemons is on a Mac to begin with"
    );
}

/// The daemon's verdict and reason of `reply`, which must be a finished order.
fn finished(reply: hostkeeper_daemon::Reply) -> (hostkeeper_hook::Verdict, Option<String>) {
    match reply {
        hostkeeper_daemon::Reply::Finished {
            verdict, reason, ..
        } => (verdict, reason),
        other => panic!("the order did not finish: {other:?}"),
    }
}

/// Proves, with the daemon's order path on this Mac: a Mac without the signers file announces no
/// `macos-pkg` and is ordered no `.pkg` (`installer_not_listed`, nothing fetched); with the file
/// and the Mac's `installer` and `pkgutil`, it announces `macos-pkg`, refuses an unsigned `.pkg`
/// at `stage` with the real `pkgutil` (`Failed`, `environment`, `signature`, nothing staged,
/// installed or received), installs the `.pkg` it is ordered (the receipt, the files root's,
/// the version sealed with its `target.json`), updates it (a file the new version drops is
/// deleted), and uninstalls it (no receipt, no file, no version directory).
///
/// Catches: a Mac daemon that announces `macos-pkg` without its signers file, does not announce
/// it with one, stages an unsigned package, or does not run the `.pkg` it is ordered through the
/// installer's steps.
#[test]
fn a_mac_daemon_announces_macos_pkg_with_its_signers_file_and_installs_the_ordered_pkg() {
    require_root();
    let _undo = Undo;
    let dir = scratch("pkg-daemon");
    // The entries run no job, so the packages carry no job's plist.
    let v100 = build_unsigned_with(dir.path(), "1.0.0", false);
    let v120 = build_unsigned_with(dir.path(), "1.2.0", false);
    let signers_file = dir.path().join("macos-pkg.toml");
    let deferred = (
        hostkeeper_hook::Verdict::Deferred {
            retry_after_secs: Some(hostkeeper_daemon::entry::NOT_LISTED_RETRY_SECS),
        },
        Some("installer_not_listed".to_owned()),
    );
    let succeeded = (
        hostkeeper_hook::Verdict::Succeeded {
            promotion_pending: false,
        },
        None,
    );

    let without = daemon::daemon(&signers_file);
    assert_eq!(without.names, ["hostkeeper-archive"], "no signers file");
    let one = without.publish("1.0.0", &v100.component, true);
    assert_eq!(
        finished(without.daemon.order(&daemon::install("1.0.0", &one, 1))),
        deferred
    );
    assert!(without.store.asked().is_empty(), "nothing fetched");
    assert_eq!(receipt_version(), None);
    drop(without);

    fs::write(&signers_file, format!("signers = [\"{LEAF}\"]\n")).expect("the signers file");
    fs::set_permissions(&signers_file, fs::Permissions::from_mode(0o644)).expect("its mode");
    let mac = daemon::daemon(&signers_file);
    assert_eq!(mac.names, ["hostkeeper-archive", "macos-pkg"]);

    let unsigned = mac.publish("1.0.0", &v100.product, false);
    let refused = finished(mac.daemon.order(&daemon::install("1.0.0", &unsigned, 1)));
    assert_eq!(
        refused,
        (
            hostkeeper_hook::Verdict::Failed {
                phase: hostkeeper_hook::Phase::BeforeCommit,
                class: hostkeeper_hook::Class::Environment,
            },
            Some("signature".to_owned())
        )
    );
    assert_eq!(receipt_version(), None, "nothing installed");
    let service = mac.packages.join(SERVICE);
    assert!(!service.join("1.0.0").exists(), "nothing staged");

    let one = mac.publish("1.0.0", &v100.component, true);
    assert_eq!(
        finished(mac.daemon.order(&daemon::install("1.0.0", &one, 2))),
        succeeded
    );
    assert_eq!(receipt_version().as_deref(), Some("1.0.0"));
    assert_installed(&format!("{SUPPORT}/bin/fixture"), 0o755);
    assert_installed(&format!("{SUPPORT}/legacy.txt"), 0o644);
    assert!(service.join("1.0.0/target.json").is_file());
    assert!(service.join("1.0.0.seal").is_file());

    let two = mac.publish("1.2.0", &v120.component, true);
    assert_eq!(
        finished(mac.daemon.order(&daemon::update("1.2.0", &two, 3))),
        succeeded
    );
    assert_eq!(receipt_version().as_deref(), Some("1.2.0"));
    assert_installed(&format!("{SUPPORT}/lib/new.txt"), 0o644);
    assert_absent(&format!("{SUPPORT}/legacy.txt"));

    assert_eq!(finished(mac.daemon.order(&daemon::uninstall(4))), succeeded);
    assert_eq!(receipt_version(), None);
    assert_absent(&format!("{SUPPORT}/bin/fixture"));
    assert_absent(SUPPORT);
    assert!(!service.exists(), "the version directories remain");
}
