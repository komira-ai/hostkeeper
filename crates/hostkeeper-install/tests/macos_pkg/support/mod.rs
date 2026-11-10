//! Support for `macos_pkg.rs`: a fake Mac (`pkgutil`, `installer`, `lsbom` and `launchctl` over a
//! scratch host root), an in-memory record store, and a rig that holds a host, a packages
//! directory and a lock file, and stages versions as the core does.
//!
//! A fake package is identified by the content of the artifact file, `fake-pkg:<name>`, so the
//! copy `stage` keeps in a version directory maps to the same package as the file it was given.
//! Tools receive logical paths (`/Library/...`) for the host and real paths for the files of a
//! version directory; the fake maps the first to its scratch root.

mod fake;

pub use fake::*;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::Failure;
use hostkeeper_install::host::{Host, Ids};
use hostkeeper_install::lock::InstallerLock;
use hostkeeper_install::macos_pkg::record::{PkgRecord, RecordError, Records};
use hostkeeper_install::macos_pkg::signers::{Fingerprint, Signers};
use hostkeeper_install::macos_pkg::{Activated, Bounds, MacosPkg, Order};
use hostkeeper_launchd::Label;
use hostkeeper_testkit::Scratch;

/// The service every test installs.
pub const SERVICE: &str = "svc";
/// Its component package's identifier.
pub const IDENTIFIER: &str = "dev.svc.server";

/// A fingerprint made of `byte` (two hex digits) repeated.
pub fn fp(byte: &str) -> String {
    byte.repeat(32)
}

/// The leaf certificate every fixture package is signed by, and the host's signer.
pub fn leaf() -> String {
    fp("3d")
}

/// What `pkgutil --check-signature` prints for a package signed by `leaf` under a trusted chain,
/// the leaf first, then an intermediate and a root with fingerprints of their own.
pub fn signed_output(leaf: &str, status: &str) -> String {
    let spaced = |hex: &str| {
        let pairs: Vec<String> = (0..32)
            .map(|n| hex[n * 2..n * 2 + 2].to_ascii_uppercase())
            .collect();
        format!(
            "           {}\n           {}\n",
            pairs[..16].join(" "),
            pairs[16..].join(" ")
        )
    };
    format!(
        "Package \"svc.pkg\":\n   Status: {status}\n   Certificate Chain:\n    1. Svc Installer\n       Expires: 2030-01-01 00:00:00 +0000\n       SHA256 Fingerprint:\n{}       ------------------------------------------------------------------------\n    2. Svc Intermediate\n       SHA256 Fingerprint:\n{}       ------------------------------------------------------------------------\n    3. Svc Root\n       SHA256 Fingerprint:\n{}       ------------------------------------------------------------------------\n",
        spaced(leaf),
        spaced(&fp("11")),
        spaced(&fp("22")),
    )
}

/// The status line of a package signed by a chain the Mac trusts.
pub const TRUSTED: &str = "signed by a certificate trusted by macOS";

/// A job plist: `Label`, `ProgramArguments` of `program`, and `Disabled` when given.
pub fn job_plist(label: &str, program: &str, disabled: Option<bool>) -> String {
    let disabled = match disabled {
        Some(true) => "<key>Disabled</key><true/>",
        Some(false) => "<key>Disabled</key><false/>",
        None => "",
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>ProgramArguments</key><array><string>{program}</string></array>{disabled}</dict></plist>\n"
    )
}

// ---------------------------------------------------------------------------------------------
// The record store
// ---------------------------------------------------------------------------------------------

/// The records in memory, with the saves in order and failures to ask for.
#[derive(Default)]
pub struct MemRecords {
    map: Mutex<BTreeMap<String, PkgRecord>>,
    saves: Mutex<Vec<(String, PkgRecord)>>,
    pub fail_load: AtomicBool,
    pub fail_holder: AtomicBool,
    fail_save_at: Mutex<Option<usize>>,
}

impl MemRecords {
    pub fn get(&self, service: &str) -> Option<PkgRecord> {
        self.map.lock().unwrap().get(service).cloned()
    }

    pub fn set(&self, service: &str, record: PkgRecord) {
        self.map.lock().unwrap().insert(service.into(), record);
    }

    pub fn all(&self) -> BTreeMap<String, PkgRecord> {
        self.map.lock().unwrap().clone()
    }

    /// Every record saved since the start, oldest first.
    pub fn saves(&self) -> Vec<PkgRecord> {
        self.saves
            .lock()
            .unwrap()
            .iter()
            .map(|(_, r)| r.clone())
            .collect()
    }

    pub fn clear_saves(&self) {
        self.saves.lock().unwrap().clear();
    }

    /// Fails the `n`th save from now on (0 is the next).
    pub fn fail_save_nth(&self, n: usize) {
        let saved = self.saves.lock().unwrap().len();
        *self.fail_save_at.lock().unwrap() = Some(saved + n);
    }
}

impl Records for MemRecords {
    fn load(&self, service: &ServiceName) -> Result<Option<PkgRecord>, RecordError> {
        if self.fail_load.load(Ordering::SeqCst) {
            return Err(RecordError("cannot read placed.json".into()));
        }
        Ok(self.get(service.as_str()))
    }

    fn save(&self, service: &ServiceName, record: &PkgRecord) -> Result<(), RecordError> {
        let mut saves = self.saves.lock().unwrap();
        if *self.fail_save_at.lock().unwrap() == Some(saves.len()) {
            return Err(RecordError("cannot write placed.json".into()));
        }
        saves.push((service.as_str().into(), record.clone()));
        self.map
            .lock()
            .unwrap()
            .insert(service.as_str().into(), record.clone());
        Ok(())
    }

    fn holder(
        &self,
        identifier: &str,
        service: &ServiceName,
    ) -> Result<Option<ServiceName>, RecordError> {
        if self.fail_holder.load(Ordering::SeqCst) {
            return Err(RecordError("cannot read placed.json".into()));
        }
        let map = self.map.lock().unwrap();
        let other = map
            .iter()
            .find(|(name, r)| r.identifier == identifier && name.as_str() != service.as_str());
        Ok(other.map(|(name, _)| ServiceName::new(name).unwrap()))
    }
}

// ---------------------------------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------------------------------

/// What the host holds at a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Dir(u32),
    File(u32, Vec<u8>),
    Link(String),
    Other,
}

/// A host root, a packages directory, a lock file, a fake Mac and a record store.
pub struct Rig {
    pub scratch: Scratch,
    pub root: PathBuf,
    pub packages: PathBuf,
    pub downloads: PathBuf,
    pub euid: u32,
    pub egid: u32,
    pub ids: Ids,
    pub host: Host,
    pub mac: FakeMac,
    pub records: MemRecords,
    pub lock: InstallerLock,
    pub signers: Signers,
    staged: AtomicU32,
}

impl Rig {
    /// A rig whose accounts are the test user's: root, `wheel` and a group `admin` it is not in.
    pub fn new(name: &str) -> Rig {
        Rig::with_ids(name, |uid, gid| Ids {
            root: uid,
            wheel: gid,
            admin: gid.wrapping_add(1),
        })
    }

    /// A rig whose accounts `ids` makes from the test user's uid and gid.
    pub fn with_ids(name: &str, ids: impl FnOnce(u32, u32) -> Ids) -> Rig {
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).unwrap();
        let meta = fs::metadata(scratch.path()).unwrap();
        let (euid, egid) = (meta.uid(), meta.gid());
        let ids = ids(euid, egid);
        let base = scratch.path().to_owned();
        let (root, packages, downloads) = (
            base.join("host"),
            base.join("packages"),
            base.join("downloads"),
        );
        for dir in [&root, &packages, &downloads] {
            fs::create_dir(dir).unwrap();
        }
        let lock_path = base.join("installer.lock");
        let signers = Signers::new(vec![Fingerprint::parse(&leaf()).unwrap()]).unwrap();
        Rig {
            host: Host::new(&root, ids),
            mac: FakeMac::new(&root, &lock_path, ids, euid, egid),
            records: MemRecords::default(),
            lock: InstallerLock::new(&lock_path),
            signers,
            staged: AtomicU32::new(0),
            scratch,
            root,
            packages,
            downloads,
            euid,
            egid,
            ids,
        }
    }

    /// The installer over this rig's host.
    pub fn pkg(&self) -> MacosPkg<'_> {
        self.pkg_on(&self.host)
    }

    /// The installer over `host`, another view of the same root.
    pub fn pkg_on<'a>(&'a self, host: &'a Host) -> MacosPkg<'a> {
        MacosPkg {
            tools: &self.mac,
            host,
            records: &self.records,
            lock: &self.lock,
            signers: &self.signers,
        }
    }

    /// A host over this rig's root whose accounts are `ids`.
    pub fn host_with(&self, ids: Ids) -> Host {
        Host::new(&self.root, ids)
    }

    /// The order for version `version` running the job named `svc`.
    pub fn order(&self, version: &str) -> Order {
        self.order_run(version, &[SERVICE])
    }

    pub fn order_run(&self, version: &str, labels: &[&str]) -> Order {
        Order {
            service: ServiceName::new(SERVICE).unwrap(),
            version: Version::new(version).unwrap(),
            identifier: IDENTIFIER.into(),
            run: labels.iter().map(|l| Label::parse(l).unwrap()).collect(),
            bounds: Bounds {
                stage: Duration::from_secs(60),
                activate: Duration::from_secs(60),
                remove: Duration::from_secs(60),
            },
            hook: None,
        }
    }

    /// Writes the verified file of package `name` as the core's fetch leaves it.
    pub fn artifact(&self, name: &str) -> PathBuf {
        let path = self.downloads.join(format!("{name}.pkg"));
        fs::write(&path, format!("fake-pkg:{name}")).unwrap();
        path
    }

    /// `stage` of package `name` into a fresh staging directory: its result and the directory.
    pub fn try_stage(&self, order: &Order, name: &str) -> (Result<(), Failure>, PathBuf) {
        let n = self.staged.fetch_add(1, Ordering::SeqCst);
        let staging = self.packages.join(SERVICE).join(format!(".stage-{n}"));
        fs::create_dir_all(&staging).unwrap();
        let result = self.pkg().stage(order, &self.artifact(name), &staging);
        (result, staging)
    }

    /// Stages package `name` as the core does: `stage`, then the staging directory renamed to
    /// `<packages>/<service>/<version>`. The version directory.
    pub fn stage(&self, order: &Order, name: &str) -> PathBuf {
        let (result, staging) = self.try_stage(order, name);
        result.unwrap();
        let dir = self.packages.join(SERVICE).join(order.version.as_str());
        if dir.exists() {
            hostkeeper_install::host::remove_tree(&dir).unwrap();
        }
        fs::rename(staging, &dir).unwrap();
        dir
    }

    /// A version directory made from package `name` without `stage`: the artifact and the
    /// payload as the fake expands it (for contents `stage` refuses on this platform).
    pub fn manual_dir(&self, version: &str, name: &str) -> PathBuf {
        let dir = self.packages.join(SERVICE).join(version);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("artifact"), format!("fake-pkg:{name}")).unwrap();
        fs::create_dir_all(dir.join("macos-pkg")).unwrap();
        fs::hard_link(dir.join("artifact"), dir.join("macos-pkg/artifact.pkg")).unwrap();
        let pkg = self.mac.pkg_named(name);
        let scratch = self.packages.join(format!(".expand-{name}"));
        self.mac.materialize(&pkg, &scratch);
        fs::rename(scratch.join("Payload"), dir.join("tree")).unwrap();
        hostkeeper_install::host::remove_tree(&scratch).unwrap();
        dir
    }

    pub fn activate(
        &self,
        order: &Order,
        dir: &Path,
        from: Option<&Path>,
    ) -> Result<Activated, Failure> {
        self.pkg().activate(order, dir, from)
    }

    // --- the host ---

    pub fn real(&self, logical: &str) -> PathBuf {
        self.host.real(logical)
    }

    pub fn mkdir(&self, logical: &str, mode: u32) {
        mkdir_p(&self.real(logical));
        fs::set_permissions(self.real(logical), fs::Permissions::from_mode(mode)).unwrap();
    }

    pub fn write(&self, logical: &str, bytes: &[u8], mode: u32) {
        let real = self.real(logical);
        mkdir_p(real.parent().unwrap());
        let _ = fs::remove_file(&real);
        fs::write(&real, bytes).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(mode)).unwrap();
    }

    pub fn read(&self, logical: &str) -> Option<Vec<u8>> {
        fs::read(self.real(logical)).ok()
    }

    pub fn read_text(&self, logical: &str) -> String {
        String::from_utf8(
            self.read(logical)
                .unwrap_or_else(|| panic!("{logical} is missing")),
        )
        .unwrap()
    }

    /// Whether anything, a link included, is at `logical`.
    pub fn exists(&self, logical: &str) -> bool {
        fs::symlink_metadata(self.real(logical)).is_ok()
    }

    pub fn remove_file(&self, logical: &str) {
        fs::remove_file(self.real(logical)).unwrap();
    }

    /// Every entry under the host root but the receipts' area (`/var`), with its content.
    pub fn snapshot(&self) -> BTreeMap<String, Node> {
        let mut found = BTreeMap::new();
        let mut todo = vec![self.root.clone()];
        while let Some(dir) = todo.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let logical = format!("/{}", path.strip_prefix(&self.root).unwrap().display());
                if logical == "/var" {
                    continue;
                }
                let meta = fs::symlink_metadata(&path).unwrap();
                let mode = meta.mode() & 0o7777;
                let node = if meta.is_dir() {
                    todo.push(path.clone());
                    Node::Dir(mode)
                } else if meta.file_type().is_symlink() {
                    Node::Link(fs::read_link(&path).unwrap().display().to_string())
                } else if meta.is_file() {
                    Node::File(mode, fs::read(&path).unwrap())
                } else {
                    Node::Other
                };
                found.insert(logical, node);
            }
        }
        found
    }

    /// The paths of `snapshot`.
    pub fn paths(&self) -> Vec<String> {
        self.snapshot().into_keys().collect()
    }
}

/// The entries of `dir` by name.
pub fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}
