#![cfg(all(target_os = "macos", feature = "privileged"))]
//! The `macos-pkg` installer (`hostkeeper_install::macos_pkg::MacosPkg`) against the Mac's real
//! `pkgutil`, `installer`, `lsbom` and `launchctl`, as root, in privileged.yml's job
//! `macos-device` (and pr-device.yml's copy of it). The daemon's fetch and proof are not part of
//! it; the test drives the installer's steps directly, with the version directories a core would
//! keep (a staging directory per version, renamed to the version once `stage` has filled it).
//!
//! The test makes its own signing identity with `/usr/bin/openssl`: a root and an installer leaf
//! (`digitalSignature`, `codeSigning`), imported into a keychain of its own, with the root trusted
//! for the run in the System keychain (`security add-trusted-cert -d -r trustRoot`) and taken out
//! after; a second root that is never trusted signs the untrusted fixture. Fixture packages are
//! built at test time with `pkgbuild` and `productbuild` and signed with `productsign`. A guard
//! takes the trust, the keychain, the receipt and the installed files away when the test ends,
//! failed or not.
//!
//! - `the_forms_of_pkgutils_answers_are_the_ones_the_parsers_read`: `pkgutil --check-signature`,
//!   `--pkg-info-plist` of an unknown identifier and `--file-info-plist` of a file no receipt
//!   lists, in the forms `hostkeeper_install` parses.
//! - `a_signed_package_is_installed_updated_rolled_back_and_removed`: `stage` refuses an unsigned
//!   package, one signed by a root the Mac does not trust and one whose leaf no signer names, then
//!   install, update (the job enabled by launchd's own override), a hand install of the older
//!   version found as drift and put back by the next install, rollback (after the test's root is
//!   taken out of the trust settings) and removal of a two-version fixture.
//!
//! It runs as root and says so when it does not: it never skips. Like the other device tests it is
//! not counted by the coverage gate.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::Duration;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::host::Host;
use hostkeeper_install::lock::InstallerLock;
use hostkeeper_install::macos_pkg::record::{PkgRecord, RecordError, Records};
use hostkeeper_install::macos_pkg::signers::{Fingerprint, Signers};
use hostkeeper_install::macos_pkg::{Bounds, MacosPkg, Order, Seen, drift, receipt, signature};
use hostkeeper_install::{Failure, HostTools, Reason};
use hostkeeper_launchd::Label;
use hostkeeper_testkit::Scratch;

const SERVICE: &str = "hk-a9-fixture";
const IDENTIFIER: &str = "dev.hostkeeper.a9-fixture";
const SUPPORT: &str = "/Library/Application Support/hk-a9-fixture";
const PLIST: &str = "/Library/LaunchDaemons/hk-a9-fixture.plist";
const SYSTEM_KEYCHAIN: &str = "/Library/Keychains/System.keychain";
const PRODUCTSIGN: &str = "/usr/bin/productsign";

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch directory")
}

/// Runs `program` with `args`; the output, whatever its status.
fn run_any(program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {program}: {e}"))
}

/// Runs `program` with `args`, which must succeed; its standard output. A failure names the
/// program and what it said, never its arguments (some of them are passwords).
fn run(program: &str, args: &[&str]) -> String {
    let out = run_any(program, args);
    assert!(
        out.status.success(),
        "{program} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
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

/// The config `openssl req -x509` makes a root with (as the mdm signing kit's).
fn root_config(cn: &str) -> String {
    format!(
        "[ req ]\ndistinguished_name = dn\nprompt = no\n\n[ dn ]\nCN = {cn}\n\n[ root_ext ]\n\
         basicConstraints = critical, CA:TRUE\nkeyUsage = critical, keyCertSign, cRLSign\n\
         subjectKeyIdentifier = hash\n"
    )
}

/// The config an installer identity's request and certificate extensions are made with.
fn leaf_config(cn: &str) -> String {
    format!(
        "[ req ]\ndistinguished_name = dn\nprompt = no\n\n[ dn ]\nCN = {cn}\n\n[ leaf_ext ]\n\
         basicConstraints = critical, CA:FALSE\nkeyUsage = critical, digitalSignature\n\
         extendedKeyUsage = codeSigning\nsubjectKeyIdentifier = hash\n\
         authorityKeyIdentifier = keyid\n"
    )
}

/// A root and the installer leaf it signed.
struct Ca {
    root_pem: PathBuf,
    leaf_pem: PathBuf,
    p12: PathBuf,
    leaf_cn: String,
}

impl Ca {
    /// Makes the root and the leaf in `dir`, with `openssl`, and the leaf's PKCS#12 file.
    fn make(dir: &Path, tag: &str, password: &str) -> Ca {
        let at = |name: &str| dir.join(format!("{tag}-{name}"));
        let (root_cn, leaf_cn) = (
            format!("hostkeeper a9 fixture root {tag}"),
            format!("hostkeeper a9 fixture installer {tag}"),
        );
        let (root_cnf, leaf_cnf) = (at("root.cnf"), at("leaf.cnf"));
        fs::write(&root_cnf, root_config(&root_cn)).expect("write the root config");
        fs::write(&leaf_cnf, leaf_config(&leaf_cn)).expect("write the leaf config");
        let (root_key, root_pem, leaf_key) = (at("root.key"), at("root.pem"), at("leaf.key"));
        let (leaf_csr, leaf_pem, p12) = (at("leaf.csr"), at("leaf.pem"), at("leaf.p12"));
        let openssl = "/usr/bin/openssl";
        run(
            openssl,
            &[
                "req",
                "-x509",
                "-new",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-sha256",
                "-days",
                "2",
                "-config",
                text(&root_cnf),
                "-extensions",
                "root_ext",
                "-keyout",
                text(&root_key),
                "-out",
                text(&root_pem),
            ],
        );
        run(
            openssl,
            &[
                "req",
                "-new",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-sha256",
                "-config",
                text(&leaf_cnf),
                "-keyout",
                text(&leaf_key),
                "-out",
                text(&leaf_csr),
            ],
        );
        run(
            openssl,
            &[
                "x509",
                "-req",
                "-sha256",
                "-days",
                "2",
                "-in",
                text(&leaf_csr),
                "-CA",
                text(&root_pem),
                "-CAkey",
                text(&root_key),
                "-set_serial",
                "2",
                "-extfile",
                text(&leaf_cnf),
                "-extensions",
                "leaf_ext",
                "-out",
                text(&leaf_pem),
            ],
        );
        run(
            openssl,
            &[
                "pkcs12",
                "-export",
                "-inkey",
                text(&leaf_key),
                "-in",
                text(&leaf_pem),
                "-certfile",
                text(&root_pem),
                "-name",
                &leaf_cn,
                "-passout",
                &format!("pass:{password}"),
                "-out",
                text(&p12),
            ],
        );
        Ca {
            root_pem,
            leaf_pem,
            p12,
            leaf_cn,
        }
    }

    /// The leaf's SHA-256 fingerprint as `openssl x509 -fingerprint -sha256` computes it,
    /// normalised.
    fn fingerprint(&self) -> Fingerprint {
        let said = run(
            "/usr/bin/openssl",
            &[
                "x509",
                "-in",
                text(&self.leaf_pem),
                "-noout",
                "-fingerprint",
                "-sha256",
            ],
        );
        let hex: String = said
            .trim()
            .split_once('=')
            .expect("openssl prints Name=hex")
            .1
            .chars()
            .filter(|c| *c != ':')
            .collect();
        Fingerprint::parse(&hex).expect("a SHA-256 fingerprint")
    }
}

/// Everything the test changes on the host outside its scratch directory, undone on drop (a
/// failed test too): the trust of the test's root, the keychain and the search list, the
/// fixture's receipt and installed files.
struct Undo {
    keychain: Option<PathBuf>,
    search_list: Vec<String>,
    trusted_root: Option<PathBuf>,
}

impl Undo {
    fn new() -> Undo {
        Undo {
            keychain: None,
            search_list: Vec::new(),
            trusted_root: None,
        }
    }

    /// Trusts `root` for package signing in the System keychain, for the run.
    fn trust(&mut self, root: &Path) {
        run(
            "/usr/bin/security",
            &[
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                SYSTEM_KEYCHAIN,
                text(root),
            ],
        );
        self.trusted_root = Some(root.to_owned());
    }

    /// Takes the test's root out of the trust settings.
    fn untrust(&mut self) {
        if let Some(root) = self.trusted_root.take() {
            run(
                "/usr/bin/security",
                &["remove-trusted-cert", "-d", text(&root)],
            );
        }
    }

    /// A keychain of its own holding the identities of `cas`, first in the search list.
    fn keychain(&mut self, dir: &Path, password: &str, cas: &[&Ca]) -> PathBuf {
        let keychain = dir.join("fixture.keychain-db");
        let kc = text(&keychain);
        let listed = run("/usr/bin/security", &["list-keychains", "-d", "user"]);
        self.search_list = listed
            .lines()
            .map(|line| line.trim().trim_matches('"').to_owned())
            .filter(|line| !line.is_empty())
            .collect();
        run(
            "/usr/bin/security",
            &["create-keychain", "-p", password, kc],
        );
        self.keychain = Some(keychain.clone());
        let mut list = vec!["list-keychains", "-d", "user", "-s", kc];
        list.extend(self.search_list.iter().map(String::as_str));
        run("/usr/bin/security", &list);
        run(
            "/usr/bin/security",
            &["unlock-keychain", "-p", password, kc],
        );
        run("/usr/bin/security", &["set-keychain-settings", kc]);
        for ca in cas {
            run(
                "/usr/bin/security",
                &[
                    "import",
                    text(&ca.p12),
                    "-k",
                    kc,
                    "-P",
                    password,
                    "-T",
                    PRODUCTSIGN,
                ],
            );
        }
        run(
            "/usr/bin/security",
            &[
                "set-key-partition-list",
                "-S",
                "apple-tool:,apple:",
                "-s",
                "-k",
                password,
                kc,
            ],
        );
        keychain
    }
}

impl Drop for Undo {
    fn drop(&mut self) {
        // Each undo is tried whatever the others did: a panicking test must still leave the host
        // as it found it.
        if let Some(root) = self.trusted_root.take() {
            let _ = run_any(
                "/usr/bin/security",
                &["remove-trusted-cert", "-d", text(&root)],
            );
        }
        if self.keychain.is_some() {
            let mut restore = vec!["list-keychains", "-d", "user", "-s"];
            restore.extend(self.search_list.iter().map(String::as_str));
            let _ = run_any("/usr/bin/security", &restore);
        }
        if let Some(keychain) = self.keychain.take() {
            let _ = run_any("/usr/bin/security", &["delete-keychain", text(&keychain)]);
        }
        let _ = run_any("/usr/sbin/pkgutil", &["--forget", IDENTIFIER]);
        let _ = fs::remove_dir_all(SUPPORT);
        let _ = fs::remove_file(PLIST);
    }
}

fn random_password() -> String {
    run("/usr/bin/openssl", &["rand", "-hex", "16"])
        .trim()
        .to_owned()
}

/// The fixture's content at `version`: 1.0.0 has `legacy.txt` and 1.1.0 drops it; 1.1.0 adds
/// `lib/new.txt` in a directory 1.0.0 lacks.
fn payload(root: &Path, version: &str) {
    let service = root.join("Library/Application Support/hk-a9-fixture");
    let mut dirs = vec![
        root.join("Library"),
        root.join("Library/Application Support"),
        service.clone(),
        service.join("bin"),
        root.join("Library/LaunchDaemons"),
    ];
    let mut files: Vec<(PathBuf, String, u32)> = vec![
        (
            service.join("bin/fixture"),
            format!("#!/bin/sh\necho {version}\n"),
            0o755,
        ),
        (
            root.join("Library/LaunchDaemons/hk-a9-fixture.plist"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
                 \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
                 <plist version=\"1.0\"><dict>\n<key>Label</key><string>{SERVICE}</string>\n\
                 <key>Disabled</key><true/>\n<key>ProgramArguments</key><array>\
                 <string>{SUPPORT}/bin/fixture</string></array>\n</dict></plist>\n"
            ),
            0o644,
        ),
    ];
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

/// Builds the fixture at `version` into `dir`: the product archive as `productbuild` makes it,
/// unsigned.
fn build_unsigned(dir: &Path, version: &str) -> PathBuf {
    let root = dir.join(format!("root-{version}"));
    fs::create_dir(&root).expect("create the payload root");
    payload(&root, version);
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
    let product = dir.join(format!("unsigned-{version}.pkg"));
    run(
        "/usr/bin/productbuild",
        &["--package", text(&component), text(&product)],
    );
    product
}

/// `unsigned` signed by the identity `cn` of `keychain`, as `<name>.pkg` beside it.
fn sign(unsigned: &Path, keychain: &Path, cn: &str, name: &str) -> PathBuf {
    let signed = unsigned.with_file_name(format!("{name}.pkg"));
    run(
        PRODUCTSIGN,
        &[
            "--sign",
            cn,
            "--keychain",
            text(keychain),
            "--timestamp=none",
            text(unsigned),
            text(&signed),
        ],
    );
    signed
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
    tools: HostTools,
    records: Memory,
    lock: InstallerLock,
    packages: PathBuf,
}

impl Rig {
    fn new(dir: &Path) -> Rig {
        let packages = dir.join("packages");
        fs::create_dir(&packages).expect("create the packages directory");
        Rig {
            host: Host::mac(),
            tools: HostTools,
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
        let wait = Duration::from_secs(300);
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
        version: &str,
        artifact: &Path,
    ) -> Result<PathBuf, (Failure, PathBuf)> {
        let staging = self
            .packages
            .join(format!("{version}.staging-{}", self.count()));
        fs::create_dir(&staging).expect("create the staging directory");
        match self
            .installer(signers)
            .stage(&self.order(version), artifact, &staging)
        {
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
    if !out.status.success() {
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

/// Stages `artifact` and expects `reason`, with nothing expanded.
fn assert_refused(rig: &Rig, signers: &Signers, artifact: &Path, reason: Reason) {
    let (failure, staging) = rig
        .stage(signers, "1.0.0", artifact)
        .expect_err("stage refuses it");
    assert_eq!(failure.reason(), reason, "{failure}");
    assert!(!staging.join("expanded").exists(), "nothing was expanded");
    assert!(!staging.join("tree").exists(), "no payload was kept");
}

#[test]
fn the_forms_of_pkgutils_answers_are_the_ones_the_parsers_read() {
    require_root();
    let dir = scratch("install-macos-pkg-forms");
    let mut guard = Undo::new();
    let password = random_password();
    let ca = Ca::make(dir.path(), "forms", &password);
    let keychain = guard.keychain(dir.path(), &password, &[&ca]);
    guard.trust(&ca.root_pem);
    let signed = sign(
        &build_unsigned(dir.path(), "1.0.0"),
        &keychain,
        &ca.leaf_cn,
        "signed",
    );

    let checked = run_any("/usr/sbin/pkgutil", &["--check-signature", text(&signed)]);
    let printed = String::from_utf8_lossy(&checked.stdout).into_owned();
    assert!(
        checked.status.success(),
        "pkgutil --check-signature: {printed}"
    );
    assert_eq!(
        signature::leaf_fingerprint(&printed),
        Some(ca.fingerprint()),
        "{printed}"
    );
    let status = signature::status(&printed).expect("a status line");
    assert!(
        signature::TRUSTED_STATUS
            .iter()
            .any(|ok| status.starts_with(ok)),
        "the status {status:?} is one TRUSTED_STATUS accepts"
    );

    let unknown = run_any(
        "/usr/sbin/pkgutil",
        &["--pkg-info-plist", "dev.hostkeeper.a9-unknown"],
    );
    assert!(!unknown.status.success(), "no receipt is a non-zero exit");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&unknown.stderr),
        String::from_utf8_lossy(&unknown.stdout)
    );
    assert!(said.contains(receipt::NO_RECEIPT), "{said}");

    let file = dir.path().join("listed-by-no-receipt");
    fs::write(&file, "x").expect("write the file");
    let info = run_any("/usr/sbin/pkgutil", &["--file-info-plist", text(&file)]);
    assert!(
        info.status.success(),
        "{}",
        String::from_utf8_lossy(&info.stderr)
    );
    assert_eq!(
        receipt::parse_file_info(&info.stdout).expect("pkgutil's file info"),
        Vec::<String>::new(),
        "no receipt lists the file"
    );
}

#[test]
fn a_signed_package_is_installed_updated_rolled_back_and_removed() {
    require_root();
    let dir = scratch("install-macos-pkg-lifecycle");
    let mut guard = Undo::new();
    let password = random_password();
    let trusted = Ca::make(dir.path(), "trusted", &password);
    let other = Ca::make(dir.path(), "other", &password);
    let keychain = guard.keychain(dir.path(), &password, &[&trusted, &other]);
    guard.trust(&trusted.root_pem);
    assert!(
        fs::symlink_metadata(SUPPORT).is_err(),
        "the fixture is not on the host to begin with"
    );
    let launch_daemons_before = fs::symlink_metadata("/Library/LaunchDaemons").is_ok();

    let rig = Rig::new(dir.path());
    let signers = Signers::new(vec![trusted.fingerprint()]).expect("one signer");
    let v100 = build_unsigned(dir.path(), "1.0.0");
    let v110 = build_unsigned(dir.path(), "1.1.0");

    // (b) The signature is checked before anything is expanded.
    assert_refused(&rig, &signers, &v100, Reason::Signature);
    let untrusted = sign(&v100, &keychain, &other.leaf_cn, "untrusted-root");
    assert_refused(&rig, &signers, &untrusted, Reason::Signature);
    let signed100 = sign(&v100, &keychain, &trusted.leaf_cn, "signed-1.0.0");
    let elsewhere =
        Signers::new(vec![Fingerprint::parse(&"ab".repeat(32)).expect("64 hex")]).expect("one");
    assert_refused(&rig, &elsewhere, &signed100, Reason::Signature);
    assert_eq!(receipt_version(), None, "nothing was installed");

    // (c) Install 1.0.0.
    let dir100 = rig
        .stage(&signers, "1.0.0", &signed100)
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

    // (d) Update to 1.1.0: the dropped file goes, the new file and directory come.
    let signed110 = sign(&v110, &keychain, &trusted.leaf_cn, "signed-1.1.0");
    let dir110 = rig
        .stage(&signers, "1.1.0", &signed110)
        .unwrap_or_else(|(f, _)| panic!("{f}"));
    rig.installer(&signers)
        .activate(&rig.order("1.1.0"), &dir110, Some(&dir100))
        .unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(receipt_version().as_deref(), Some("1.1.0"));
    assert_absent(&format!("{SUPPORT}/legacy.txt"));
    assert_installed(&format!("{SUPPORT}/lib"), 0o755);
    assert_installed(&format!("{SUPPORT}/lib/new.txt"), 0o644);
    assert_installed(&format!("{SUPPORT}/bin/fixture"), 0o755);
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
            text(&dir100.join("artifact")),
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
    let repaired = rig
        .installer(&signers)
        .activate(&rig.order("1.1.0"), &dir110, None)
        .unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(repaired.drift, Some(Some("1.0.0".to_owned())));
    assert_eq!(receipt_version().as_deref(), Some("1.1.0"));
    assert_absent(&format!("{SUPPORT}/legacy.txt"));
    assert_installed(&format!("{SUPPORT}/lib/new.txt"), 0o644);

    // (e) Roll back to 1.0.0 with the test's root out of the trust settings: the kept package is
    // installed with `-allowUntrusted`, its signature not checked again.
    guard.untrust();
    rig.installer(&signers)
        .activate(&rig.order("1.0.0"), &dir100, Some(&dir110))
        .unwrap_or_else(|f| panic!("{f}"));
    assert_eq!(receipt_version().as_deref(), Some("1.0.0"));
    assert_installed(&format!("{SUPPORT}/legacy.txt"), 0o644);
    assert_absent(&format!("{SUPPORT}/lib/new.txt"));
    assert_absent(&format!("{SUPPORT}/lib"));
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
        PLIST.to_owned(),
        format!("{SUPPORT}/bin"),
        format!("{SUPPORT}/lib"),
        SUPPORT.to_owned(),
    ] {
        assert_absent(&path);
    }
    assert_eq!(receipt_version(), None, "pkgutil forgot the receipt");
    let pkgs = run("/usr/sbin/pkgutil", &["--pkgs"]);
    assert!(
        !pkgs.lines().any(|line| line.trim() == IDENTIFIER),
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
