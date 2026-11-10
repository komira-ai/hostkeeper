//! hostkeeper's native package in the cross-release rig, on Linux: the `.deb` of each version, the
//! signed apt repository on loopback that lists them, the one `apt` entry a set orders, and what
//! the host says once a daemon installed one.
//!
//! - **The `.deb`** of a version is built here around the `hostkeeper` binary of its [`Build`], as
//!   `packaging/linux/nfpm.yaml` lays the package out: `/usr/bin/hostkeeper`, the unit
//!   ([`unit_text`]: the rig's, which starts `current` of the test's binary directory), the
//!   tmpfiles rules and the config template of `packaging/linux/`, and its one issue link
//!   ([`ISSUE_LINK`]), with the shipped `postinst` and `prerm` (`packaging/linux/scripts/`) as its
//!   maintainer scripts. Three files of nfpm.yaml's list are left out because the host is a
//!   test's: the pinned `gh` and the Sigstore
//!   trusted root, which the end-to-end rig installed at their fixed paths with the test signer's
//!   root (a package that replaced it with the production root would refuse every later
//!   package), and `hostkeeper-pxe`. It is built with `dpkg-deb`, gzip at level 1 (the binaries
//!   are debug builds).
//! - **The repository** is a signed apt repository (`../install_apt/repo.rs`: the suite `hk`, its
//!   one component `main`, `InRelease` clear-signed by a key made for the test) in the test's
//!   tree, which lists every version published so far. It is served over https on `127.0.0.1`
//!   (`super::https`: a static file server of the tree, on a port the system picks, with a
//!   certificate of a CA made for the test), because the coordinator's registry takes `https://`
//!   repositories only. The host's apt is told to trust that CA by a drop-in of its
//!   configuration, [`APT_CONF`], which names the CA's file for this address; the drop removes
//!   the drop-in. The repository is the registry entry [`Repository::entry`]: the name
//!   `hostkeeper`, the suite `hk`, so that its one channel is [`REPOSITORY`]; the rig puts it in
//!   the coordinator's registry before it orders anything (`Repositories.Put`), as a set of `apt`
//!   entries is refused for a repository the registry does not hold. The host's entry of it is the
//!   files the registry renders that entry to (hostkeeper-release's `registry::render`), in
//!   `/etc/hostkeeper/repositories/` (`<REPOSITORY>.sources` and the keyring `hostkeeper.gpg`),
//!   the fixed place the daemon reads without a flag; the released daemon reads the directory at
//!   its start and takes no pushed set, and this build, pushed the same entry, finds the
//!   directory as it would write it and adopts it.
//! - **The entry** a set orders ([`Built::entry`]) is the one `apt` entry `release targets` writes
//!   for hostkeeper's `.deb` (`crates/xtask/src/release/targets.rs`): the repository's channel
//!   ([`REPOSITORY`]), the package version (the release's), the `.deb`'s SHA-256 as the pin, no location, no
//!   size and no attestation, a manifest that names the package, its `run` unit, the floor and
//!   the bounds of the installer and of the built-in steps, and a hook with no command. The set
//!   carries it alone: no release index, no index digest ([`Rig::order`]).
//! - **The host's evidence** that a version went the native path is read from the host, not from
//!   a count of requests: dpkg's version of the package and its hold, `/usr/bin/hostkeeper`, the
//!   version directory of the packages directory (its sealed `target.json`, its `tree/`), the link
//!   `current` there and the installer `placed.json` records ([`Rig::assert_native`]).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};

use hostkeeper_config::platform::Platform;
use hostkeeper_daemon::selfupdate::APT_RELEASE_BINARY;
use hostkeeper_placed::roots::STATE_DIR;
use hostkeeper_placed::{RealFs, Store};
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, Repository as RegistryEntry, StepBounds, Target, TargetList,
};
use hostkeeper_steps::Layout;
use hostkeeper_testkit::Scratch;
use sha2::{Digest, Sha256};

use super::super::e2e::scenario::LOCAL;
use super::super::e2e::wait_for;
use super::https::Server;
use super::linux::{DEB_UNIT, PACKAGE, REPOSITORIES_DIR, unit_text};

/// The one link of hostkeeper's `.deb` (`packaging/linux/nfpm.yaml`): the console's issue file
/// the daemon writes while the host waits for its enrollment.
const ISSUE_LINK: (&str, &str) = (
    "etc/issue.d/hostkeeper.issue",
    "/run/hostkeeper/enrollment.issue",
);
use super::repo::{Key, SUITE, Signing, architecture, run, write_index};
use super::{Build, HK, Rig, floor};

/// The name the rig's repository has in the registry.
const REGISTRY_NAME: &str = "hostkeeper";
/// The channel of the rig's repository, `<registry name>-<suite>`: the name of the host's
/// repository entry (the stem of its `.sources` file) and the repository of the entries a set
/// orders.
pub const REPOSITORY: &str = "hostkeeper-hk";
/// The drop-in of apt's configuration that makes apt trust the CA of the rig's https server. Its
/// name starts with `99` so that it is read last and cannot be a file of the system's.
const APT_CONF: &str = "/etc/apt/apt.conf.d/99hostkeeper-privileged-test";
/// The unit the package's `run` names.
const RUN_UNIT: &str = "hostkeeper.service";

/// The unit text of the `.deb`'s `hostkeeper.service`, the tmpfiles rules and the config
/// template: the files of `packaging/linux/` as the package installs them.
const TMPFILES: &str = include_str!("../../../../packaging/linux/hostkeeper.tmpfiles.conf");
const TEMPLATE: &str = include_str!("../../../../packaging/linux/hostkeeper.toml.template");
/// The shipped maintainer scripts.
const POSTINST: &str = include_str!("../../../../packaging/linux/scripts/postinstall.sh");
const PRERM: &str = include_str!("../../../../packaging/linux/scripts/preremove.sh");

/// A package of hostkeeper the rig built and published.
#[derive(Clone, Debug)]
pub struct Built {
    /// The release's version, which is also the package's.
    pub version: String,
    /// The `.deb`'s SHA-256: the pin of its entry.
    pub sha256: String,
    /// The `min_hostkeeper` of its entry's manifest.
    pub floor: String,
    /// The `.deb` in the repository's pool.
    pub file: PathBuf,
}

impl Built {
    /// The one `apt` entry of this package, as `release targets` writes it for hostkeeper's
    /// `.deb` (the module says what is in it).
    pub fn entry(&self) -> Target {
        let bounds = StepBounds {
            prepare: 300,
            cutover: 300,
            verify: 300,
            rollback: 300,
            cleanup: 300,
            ..StepBounds::default()
        };
        Target {
            installer: "apt".to_owned(),
            digest: self.sha256.clone(),
            manifest: Some(Manifest {
                min_hostkeeper: self.floor.clone(),
                package: Some(PACKAGE.to_owned()),
                run: vec![RUN_UNIT.to_owned()],
                hook: Some(Hook {
                    timeout_secs: Some(bounds),
                    verify_deadline_secs: 600,
                    ..Hook::default()
                }),
                installer_bounds: Some(InstallerBounds {
                    stage_secs: 600,
                    activate_secs: 300,
                    remove_secs: 300,
                }),
                ..Manifest::default()
            }),
            repository: Some(REPOSITORY.to_owned()),
            package_version: Some(self.version.clone()),
            ..Target::default()
        }
    }

    /// The `targets` of a set of this package: the one entry, for the host's platform.
    pub fn targets(&self) -> BTreeMap<String, TargetList> {
        BTreeMap::from([(
            Platform::current().to_string(),
            TargetList {
                entries: vec![self.entry()],
            },
        )])
    }
}

/// One package of the repository's index.
struct Stanza {
    version: String,
    text: String,
}

/// The rig's apt repository and the host's entry of it (the module says what they are).
pub struct Repository {
    dir: PathBuf,
    key: Key,
    /// The https server of `dir`, held for as long as the repository is.
    _server: Server,
    /// The registry entry of the repository.
    entry: RegistryEntry,
    work: Scratch,
    stanzas: Mutex<Vec<Stanza>>,
}

/// Writes `bytes` at `path` below `root`, mode `mode`, making its directories.
fn put(root: &Path, path: &str, bytes: &[u8], mode: u32) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().expect("a parent")).expect("make the directory");
    fs::write(&file, bytes).expect("write the file");
    fs::set_permissions(&file, fs::Permissions::from_mode(mode)).expect("set the mode");
}

/// Sets every directory below and at `dir` to mode 0755.
fn open_directories(dir: &Path) {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("a directory's mode");
    for entry in fs::read_dir(dir).expect("read a directory") {
        let entry = entry.expect("an entry");
        if entry.file_type().expect("its type").is_dir() {
            open_directories(&entry.path());
        }
    }
}

impl Repository {
    /// The repository in the tree of `base`, served over https, and the host's entry of it: the
    /// key, the files of the registry entry ([`Repository::entry`]) in [`REPOSITORIES_DIR`]
    /// (root's, mode 0755/0644), and the apt drop-in that trusts the server's CA ([`APT_CONF`]).
    ///
    /// # Panics
    /// A file or a directory cannot be made, or `gpg` fails.
    pub fn new(base: &Path) -> Repository {
        let dir = base.join("apt-repo");
        fs::create_dir(&dir).expect("make the repository");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("its mode");
        let key = Key::new(&base.join("apt-gpg"), "hostkeeper-x1");
        let config = Path::new(REPOSITORIES_DIR);
        fs::create_dir_all(config).expect("the repository configuration directory");
        for dir in [config.parent().expect("its parent"), config] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
        }
        let server = Server::start(&dir);
        let exported = base.join("apt-keyring.gpg");
        key.export(&exported);
        let keyring = fs::read(&exported).expect("read the exported keyring");
        // The primary key's fingerprint, as gpg lists it: the registry needs one listed
        // fingerprint per primary key of the keyring.
        let listing = run(Command::new("gpg")
            .args(["--batch", "--no-tty", "--homedir"])
            .arg(base.join("apt-gpg"))
            .args(["--with-colons", "--list-keys"]));
        let fingerprint = String::from_utf8_lossy(&listing)
            .lines()
            .find_map(|line| line.strip_prefix("fpr:"))
            .and_then(|rest| rest.split(':').nth(8))
            .expect("the key's fingerprint")
            .to_owned();
        let uri = format!("https://127.0.0.1:{}", server.port());
        let entry = RegistryEntry {
            name: REGISTRY_NAME.to_owned(),
            kind: "apt".to_owned(),
            uri: uri.clone(),
            suites: vec![SUITE.to_owned()],
            components: vec!["main".to_owned()],
            fingerprints: vec![fingerprint],
            keyring: keyring.clone(),
        };
        // The host's entry as the registry renders the entry (`<name>-<suite>.sources` and
        // `<name>.gpg`, hostkeeper-release's `registry::render`), so that this build, pushed the
        // entry, finds the directory as it would write it and adopts it.
        let keyring_file = format!("{REGISTRY_NAME}.gpg");
        put(config, &keyring_file, &keyring, 0o644);
        put(
            config,
            &format!("{REPOSITORY}.sources"),
            format!(
                "Types: deb\nURIs: {uri}\nSuites: {SUITE}\nComponents: main\nSigned-By: \
                 {REPOSITORIES_DIR}/{keyring_file}\n"
            )
            .as_bytes(),
            0o644,
        );
        let ca = base.join("apt-ca.pem");
        put(base, "apt-ca.pem", server.ca_pem().as_bytes(), 0o644);
        put(
            Path::new("/"),
            APT_CONF.trim_start_matches('/'),
            format!("Acquire::https::127.0.0.1::CaInfo \"{}\";\n", ca.display()).as_bytes(),
            0o644,
        );
        let work = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "x1deb")
            .expect("the package trees' directory");
        Repository {
            dir,
            key,
            _server: server,
            entry,
            work,
            stanzas: Mutex::new(Vec::new()),
        }
    }

    /// The repository as the coordinator's registry holds it: what the rig puts there.
    pub fn entry(&self) -> RegistryEntry {
        self.entry.clone()
    }

    /// Builds the `.deb` of hostkeeper at `version` around `binary` with the unit text `unit`,
    /// adds it to the repository's index (replacing a package of that version) and signs the
    /// suite again.
    ///
    /// # Panics
    /// The package cannot be built.
    pub fn publish(&self, version: &str, binary: &[u8], unit: &str, floor: &str) -> Built {
        let architecture = architecture();
        let tree = self.work.path().join(format!("{PACKAGE}-{version}"));
        let _ = fs::remove_dir_all(&tree);
        fs::create_dir(&tree).expect("make the package tree");
        let files: [(&str, &[u8], u32); 4] = [
            ("usr/bin/hostkeeper", binary, 0o755),
            (
                "usr/lib/systemd/system/hostkeeper.service",
                unit.as_bytes(),
                0o644,
            ),
            (
                "usr/lib/tmpfiles.d/hostkeeper.conf",
                TMPFILES.as_bytes(),
                0o644,
            ),
            (
                "usr/share/hostkeeper/hostkeeper.toml.template",
                TEMPLATE.as_bytes(),
                0o644,
            ),
        ];
        for (path, bytes, mode) in files {
            put(&tree, path, bytes, mode);
        }
        {
            let (path, target) = ISSUE_LINK;
            let link = tree.join(path);
            fs::create_dir_all(link.parent().expect("a parent")).expect("make the directory");
            symlink(target, &link).expect("the package's link");
        }
        let control = format!(
            "Package: {PACKAGE}\nVersion: {version}\nArchitecture: {architecture}\n\
             Section: admin\nPriority: optional\n\
             Maintainer: hostkeeper tests <tests@example.invalid>\n\
             Description: hostkeeper, the update daemon of a host\n \
             The package of the cross-release rig, laid out as packaging/linux/nfpm.yaml does.\n"
        );
        put(&tree, "DEBIAN/control", control.as_bytes(), 0o644);
        put(&tree, "DEBIAN/postinst", POSTINST.as_bytes(), 0o755);
        put(&tree, "DEBIAN/prerm", PRERM.as_bytes(), 0o755);
        open_directories(&tree);
        let pool = self.dir.join("pool/main");
        fs::create_dir_all(&pool).expect("make the pool");
        let file = pool.join(format!("{PACKAGE}_{version}_{architecture}.deb"));
        run(Command::new("dpkg-deb")
            .args(["--root-owner-group", "-Zgzip", "-z1", "--build"])
            .arg(&tree)
            .arg(&file));
        fs::remove_dir_all(&tree).expect("remove the package tree");
        let bytes = fs::read(&file).expect("read the package");
        let sha256 = hex::encode(Sha256::digest(&bytes));
        let text = format!(
            "{control}Filename: pool/main/{}\nSize: {}\nSHA256: {sha256}\n\n",
            file.file_name().expect("a name").to_string_lossy(),
            bytes.len()
        );
        let index = {
            let mut stanzas = self.stanzas.lock().unwrap_or_else(PoisonError::into_inner);
            stanzas.retain(|stanza| stanza.version != version);
            stanzas.push(Stanza {
                version: version.to_owned(),
                text,
            });
            stanzas.iter().map(|s| s.text.as_str()).collect::<String>()
        };
        write_index(&self.dir, &architecture, &index, Signing::Signed(&self.key));
        Built {
            version: version.to_owned(),
            sha256,
            floor: floor.to_owned(),
            file,
        }
    }
}

impl Drop for Repository {
    /// Removes the apt drop-in; the CA file it names is in the test's tree, the https server stops
    /// with the field, and the host's entry is at a place the host's drop takes away.
    fn drop(&mut self) {
        let _ = fs::remove_file(APT_CONF);
    }
}

impl Rig {
    /// Builds hostkeeper's `.deb` at `version` carrying the `hostkeeper` of `build`, and
    /// publishes it in the rig's repository.
    pub fn publish(&self, version: &str, build: Build) -> Built {
        self.repository.publish(
            version,
            &self.binary(build),
            &unit_text(self.host.root()),
            &floor(build, &self.released),
        )
    }

    /// Sets the desired version of hostkeeper to the published package `built` as CI's set of a
    /// release with native entries does (design stage 2 and 3): by targets alone, the one `apt`
    /// entry for the host's platform ([`Built::entry`]), through the operator API, with no
    /// release index and no index digest. Every daemon that announces `apt` is ordered the entry
    /// and installs the package through it.
    pub fn order(&self, built: &Built) {
        self.coordinator
            .set_desired_targets_alone(HK, &built.version, built.targets());
    }

    /// The released daemon's start as the host's daemon: the released version's `.deb` published
    /// and installed with `dpkg -i` (the bootstrap install of a host set up from the `.deb`), and
    /// the daemon started on it. Its start adopts the package: it holds it and records it as
    /// installed by `apt`.
    pub(super) fn start_released(&self, first: &Path) {
        let built = self.publish(&self.released, Build::Released);
        self.host.install_package(&built.file);
        self.host.start(first, &self.released);
    }

    /// Waits until the released daemon holds its package and has recorded it as installed by
    /// `apt` at the released version: what the start's adoption does.
    pub(super) fn converge_on_released(&self) {
        let released = self.released.clone();
        wait_for(
            &format!("the released daemon adopted its package at {released}"),
            LOCAL,
            || {
                self.placed_install() == Some(("apt".to_owned(), Some(released.clone())))
                    && self.host.package_held()
            },
        );
    }

    /// The installer and the version `placed.json` records for hostkeeper, when it records one.
    pub fn placed_install(&self) -> Option<(String, Option<String>)> {
        let fs = RealFs::new(Path::new("/"));
        let record = Store::new(&fs, Path::new(STATE_DIR))
            .load()
            .unwrap_or_else(|e| panic!("placed.json: {e}"));
        record
            .installed
            .get(HK)
            .map(|entry| (entry.installer.clone(), entry.version.clone()))
    }

    /// The version directory of hostkeeper in the packages directory.
    pub fn staged_dir(&self, version: &str) -> PathBuf {
        self.host.root().packages().join(HK).join(version)
    }

    /// Checks that `built` is on the host as its `.deb`, installed through its entry: dpkg has
    /// the package at the version and holds it, `/usr/bin/hostkeeper` is the binary of `build`,
    /// the version directory holds the sealed `target.json` of the entry (installer, pin, repository
    /// and package version) and a
    /// `tree/` whose `usr/bin/hostkeeper` is that binary, `current` of the packages directory
    /// names the version, and `placed.json` records `apt` at the version.
    pub fn assert_native(&self, built: &Built, build: Build) {
        let version = &built.version;
        let binary = self.binary(build);
        assert_eq!(
            self.host.package_version().as_deref(),
            Some(version.as_str()),
            "dpkg has hostkeeper at {version}\n{}",
            self.host.logs()
        );
        assert!(self.host.package_held(), "hostkeeper is held by apt");
        assert!(
            fs::read("/usr/bin/hostkeeper").expect("/usr/bin/hostkeeper") == binary,
            "/usr/bin/hostkeeper is the binary of {version}"
        );
        let dir = self.staged_dir(version);
        let staged = hostkeeper_daemon::target::read(&dir)
            .expect("target.json")
            .expect("the version directory holds the entry it was staged for");
        let entry = built.entry();
        assert_eq!(
            (
                staged.installer.as_str(),
                staged.digest.as_str(),
                staged.repository.as_deref(),
                staged.package_version.as_deref(),
            ),
            (
                entry.installer.as_str(),
                entry.digest.as_str(),
                entry.repository.as_deref(),
                entry.package_version.as_deref(),
            ),
            "the version directory holds the entry it was ordered through"
        );
        assert!(
            fs::read(dir.join(APT_RELEASE_BINARY)).expect("the tree's hostkeeper") == binary,
            "the tree of {version} holds the binary of the package"
        );
        assert_eq!(
            Layout::package(&self.host.root().packages().join(HK))
                .current()
                .expect("read current")
                .as_deref(),
            Some(version.as_str()),
            "{}",
            self.host.logs()
        );
        assert_eq!(
            self.placed_install(),
            Some(("apt".to_owned(), Some(version.clone())))
        );
        assert!(
            fs::symlink_metadata(super::linux::LINKED_UNIT).is_err(),
            "no unit of an archive is linked: the package's is the host's"
        );
        assert!(
            fs::symlink_metadata(DEB_UNIT).is_ok_and(|meta| meta.is_file()),
            "the package's unit is the host's"
        );
    }

    /// Checks that the host holds hostkeeper as the `.deb` of `version` still, whichever binary
    /// the daemon runs: dpkg's version and the hold, `/usr/bin/hostkeeper` the binary of `build`,
    /// and `placed.json` recording `apt` at the version. What a failed or abandoned update to
    /// another version leaves.
    pub fn assert_package_is(&self, version: &str, build: Build) {
        assert_eq!(
            self.host.package_version().as_deref(),
            Some(version),
            "dpkg has hostkeeper at {version}\n{}",
            self.host.logs()
        );
        assert!(self.host.package_held(), "hostkeeper is held by apt");
        assert!(
            fs::read("/usr/bin/hostkeeper").expect("/usr/bin/hostkeeper") == self.binary(build),
            "/usr/bin/hostkeeper is the binary of {version}"
        );
        assert_eq!(
            self.placed_install(),
            Some(("apt".to_owned(), Some(version.to_owned())))
        );
    }

    /// What the host says of the package, for a failed test's view.
    pub(super) fn native_view(&self) -> String {
        format!(
            "package: {:?} held: {}\nplaced: {:?}\n",
            self.host.package_version(),
            self.host.package_held(),
            self.placed_install()
        )
    }
}
