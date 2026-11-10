//! The packages of the device tests' fixture services, as the daemon installs every service: a
//! `.deb` in a signed apt repository on disk, the host's entry of that repository, and the one
//! `apt` entry an order carries for a version of it (docs/design/packages-native.md, section 10).
//!
//! - **The package** ([`Deb`]) is built at test time with the runner's `dpkg-deb`: the files the
//!   test gives, in the layout a service's package has (its programs in `/usr/bin/`, its hook the
//!   program `usr/lib/<package>/hostkeeper-hook`, its units in `/usr/lib/systemd/system/`, its own
//!   files in `/usr/share/<package>/`), with the control fields `Package`, `Version` and
//!   `Architecture` and no maintainer script. Nothing binary is checked in.
//! - **The repository** ([`AptRepo`]) is the one of the apt device test (`install_apt/repo.rs`:
//!   the suite `hk`, its one component `main`, `InRelease` clear-signed by a key made for the
//!   test) in a directory the test gives, which lists every package published so far.
//! - **The host's entry** ([`AptRepo::install_entry`]) is the `.sources` file and keyring the
//!   daemon reads at `/etc/hostkeeper/repositories/` (a fixed place of the daemon, with no flag),
//!   which a test that starts the daemon as a program makes and takes away again.
//! - **The entry of an order** ([`entry`]) is the one `apt` entry `release targets` writes for a
//!   package: the repository's name, the package version, the `.deb`'s SHA-256 as the pin, no
//!   location, no size and no attestation, and the manifest of the service (its package, `run`
//!   units, health URL, params and the bounds of its hook and of the installer).
//!
//! The module is included by path by each test file that needs a package, with its own copy of
//! the apt device test's repository builder.

#![allow(dead_code, reason = "each test file uses part of the fixture packages")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError};

use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, Param, StepBounds, Target, TargetList, param,
};
use sha2::{Digest, Sha256};

#[path = "../install_apt/repo.rs"]
pub mod repo;

use repo::{Key, SUITE, Signing, architecture, run, write_index};

/// The directory of the host's repository entries, a fixed place of the daemon.
pub const REPOSITORIES_DIR: &str = hostkeeper_install::repository::config::REPOSITORIES_DIR;
/// The `min_hostkeeper` of an entry's manifest: below every version a device test runs.
pub const MIN_HOSTKEEPER: &str = "0.1.0";

/// One file of a package, its path relative to the root of the filesystem it is installed on.
#[derive(Clone, Debug)]
pub struct File {
    pub path: String,
    pub bytes: Vec<u8>,
    pub mode: u32,
}

impl File {
    /// The file `path` with `bytes` and mode `mode`.
    pub fn new(path: &str, bytes: impl Into<Vec<u8>>, mode: u32) -> File {
        File {
            path: path.to_owned(),
            bytes: bytes.into(),
            mode,
        }
    }
}

/// A package: its name, its version and its files.
#[derive(Clone, Debug)]
pub struct Deb {
    pub package: String,
    pub version: String,
    pub files: Vec<File>,
}

/// A package the repository lists.
#[derive(Clone, Debug)]
pub struct Published {
    pub package: String,
    pub version: String,
    /// The SHA-256 of the `.deb`, lower-case hex: the pin of its entry.
    pub sha256: String,
    /// The `.deb` in the repository's pool.
    pub file: PathBuf,
}

/// One package of the repository's index.
struct Stanza {
    package: String,
    version: String,
    text: String,
}

/// A signed apt repository in a directory of the test's (the module says what is in it).
pub struct AptRepo {
    dir: PathBuf,
    trees: PathBuf,
    key: Key,
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

/// Builds `deb` with `dpkg-deb` from a tree made in `trees` into the directory `pool`, and
/// answers the package as built and its control file's text. No key and no repository are needed.
///
/// # Panics
/// The package cannot be built.
pub fn build(deb: &Deb, trees: &Path, pool: &Path) -> (Published, String) {
    let architecture = architecture();
    let tree = trees.join(format!("{}-{}", deb.package, deb.version));
    let _ = fs::remove_dir_all(&tree);
    fs::create_dir(&tree).expect("make the package tree");
    for file in &deb.files {
        put(&tree, &file.path, &file.bytes, file.mode);
    }
    let control = format!(
        "Package: {}\nVersion: {}\nArchitecture: {architecture}\n\
         Maintainer: hostkeeper tests <tests@example.invalid>\n\
         Description: a fixture package of the device tests\n",
        deb.package, deb.version
    );
    put(&tree, "DEBIAN/control", control.as_bytes(), 0o644);
    open_directories(&tree);
    let file = pool.join(format!(
        "{}_{}_{architecture}.deb",
        deb.package, deb.version
    ));
    run(Command::new("dpkg-deb")
        .args(["--root-owner-group", "-Zgzip", "-z1", "--build"])
        .arg(&tree)
        .arg(&file));
    fs::remove_dir_all(&tree).expect("remove the package tree");
    let sha256 = hex::encode(Sha256::digest(fs::read(&file).expect("read the package")));
    let published = Published {
        package: deb.package.clone(),
        version: deb.version.clone(),
        sha256,
        file,
    };
    (published, control)
}

impl AptRepo {
    /// The repository below `base` (a directory that exists, which apt reads as root) and its
    /// key, named `key_name`.
    ///
    /// # Panics
    /// A directory cannot be made, or `gpg` fails.
    pub fn new(base: &Path, key_name: &str) -> AptRepo {
        let dir = base.join("apt-repo");
        let trees = base.join("apt-trees");
        for made in [&dir, &trees] {
            fs::create_dir(made).expect("make a directory of the repository");
            fs::set_permissions(made, fs::Permissions::from_mode(0o755)).expect("its mode");
        }
        let key = Key::new(&base.join("apt-gpg"), key_name);
        AptRepo {
            dir,
            trees,
            key,
            stanzas: Mutex::new(Vec::new()),
        }
    }

    /// The repository's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Builds `deb`, adds it to the repository's index (replacing a package of that name and
    /// version) and signs the suite again.
    ///
    /// # Panics
    /// The package cannot be built.
    pub fn publish(&self, deb: &Deb) -> Published {
        let architecture = architecture();
        let pool = self.dir.join("pool/main");
        fs::create_dir_all(&pool).expect("make the pool");
        let (published, control) = build(deb, &self.trees, &pool);
        let bytes = fs::read(&published.file).expect("read the package");
        let text = format!(
            "{control}Filename: pool/main/{}\nSize: {}\nSHA256: {}\n\n",
            published
                .file
                .file_name()
                .expect("a name")
                .to_string_lossy(),
            bytes.len(),
            published.sha256
        );
        let index = {
            let mut stanzas = self.stanzas.lock().unwrap_or_else(PoisonError::into_inner);
            stanzas.retain(|s| !(s.package == deb.package && s.version == deb.version));
            stanzas.push(Stanza {
                package: deb.package.clone(),
                version: deb.version.clone(),
                text,
            });
            stanzas.iter().map(|s| s.text.as_str()).collect::<String>()
        };
        write_index(&self.dir, &architecture, &index, Signing::Signed(&self.key));
        published
    }

    /// The host's entry of this repository, `<name>.sources` and its keyring `<name>.gpg`, in
    /// [`REPOSITORIES_DIR`] (root's, directory mode 0755, files 0644): where the daemon reads it
    /// at its start. The directory and its parent are made when missing.
    ///
    /// # Panics
    /// A file or a directory cannot be made.
    pub fn install_entry(&self, name: &str) {
        self.install_entry_in(Path::new(REPOSITORIES_DIR), name);
    }

    /// [`AptRepo::install_entry`] in `config`, for a daemon whose repositories directory is
    /// `config` (its `Signed-By` names the keyring there).
    pub fn install_entry_in(&self, config: &Path, name: &str) {
        fs::create_dir_all(config).expect("the repository configuration directory");
        for dir in [config.parent().expect("its parent"), config] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
        }
        let keyring = config.join(format!("{name}.gpg"));
        self.export_keyring(&keyring);
        let sources = config.join(format!("{name}.sources"));
        fs::write(
            &sources,
            self.sources_text(&format!("file:{}", self.dir.display()), &keyring),
        )
        .expect("write the entry");
        fs::set_permissions(&sources, fs::Permissions::from_mode(0o644)).expect("entry mode");
    }

    /// The public key of the repository, binary, into `path` (mode 0644): the keyring an entry's
    /// `Signed-By` names.
    pub fn export_keyring(&self, path: &Path) {
        self.key.export(path);
    }

    /// The text of the host's entry for this repository at the address `uri` (`file:` and a path,
    /// or `https://`), `Signed-By` the keyring `keyring`.
    pub fn sources_text(&self, uri: &str, keyring: &Path) -> String {
        format!(
            "Types: deb\nURIs: {uri}\nSuites: {SUITE}\nComponents: main\nSigned-By: {}\n",
            keyring.display()
        )
    }
}

/// The `targets` of a set of one version: `entry` alone, for this host's platform.
pub fn targets(entry: Target) -> BTreeMap<String, TargetList> {
    BTreeMap::from([(
        Platform::current().to_string(),
        TargetList {
            entries: vec![entry],
        },
    )])
}

/// A param of the manifest that is text.
pub fn text(value: &str) -> Param {
    Param {
        value: Some(param::Value::Text(value.to_owned())),
    }
}

/// A param of the manifest that is a flag.
pub fn flag(value: bool) -> Param {
    Param {
        value: Some(param::Value::Flag(value)),
    }
}

/// What the manifest of a service's entry says besides its package.
#[derive(Clone, Debug, Default)]
pub struct Service {
    /// The entry's `repository`: the name of the host's repository entry.
    pub repository: String,
    /// The units the service runs.
    pub run: Vec<String>,
    /// The health URL the observer probes.
    pub health_url: Option<String>,
    /// The manifest's `params`, which only a hook reads.
    pub params: BTreeMap<String, Param>,
    /// The manifest's `hook.command`: the program the package installs, by its absolute path, and
    /// its leading arguments; empty for the built-in steps.
    pub hook_command: Vec<String>,
    /// The bound of each step of the hook (the built-in steps when the package has no hook).
    pub steps: StepBounds,
    /// How long `verify` may keep deferring: at least `verify` plus `rollback`.
    pub verify_deadline_secs: u64,
}

/// The `apt` entry of `published` for the service `service`: the one `release targets` writes for
/// a package (the module says what is in it). The entry's `hook.command` is the service's
/// ([`Service::hook_command`]): the program the package installs as its hook, when it has one.
pub fn entry(service: &Service, published: &Published) -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: published.sha256.clone(),
        manifest: Some(Manifest {
            min_hostkeeper: MIN_HOSTKEEPER.to_owned(),
            package: Some(published.package.clone()),
            run: service.run.clone(),
            health_url: service.health_url.clone(),
            params: service.params.clone().into_iter().collect(),
            hook: Some(Hook {
                command: service.hook_command.clone(),
                timeout_secs: Some(service.steps),
                verify_deadline_secs: service.verify_deadline_secs,
                ..Hook::default()
            }),
            installer_bounds: Some(InstallerBounds {
                stage_secs: 300,
                activate_secs: 300,
                remove_secs: 300,
            }),
            ..Manifest::default()
        }),
        repository: Some(service.repository.clone()),
        package_version: Some(published.version.clone()),
        ..Target::default()
    }
}
