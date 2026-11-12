//! The host side of the apt installer's device test: the signed fixture repository and the
//! host's repository configuration for it, the installer over the production launcher with real
//! tools, an in-memory record, and the host's own answers (`dpkg-query`, `apt-mark`, `systemctl`)
//! the assertions read. A [`Rig`] purges the package, lets go of its hold and takes away the
//! enablement link when it is made and again when it is dropped, whatever a test did before it
//! failed.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_install::repository::apt::{self, Apt};
use hostkeeper_install::repository::config::{self, Repositories};
use hostkeeper_install::repository::launcher::LauncherTools;
use hostkeeper_install::repository::tools::Sleep;
use hostkeeper_install::repository::{
    Bounds, HostDirs, InstallerLock, Order, PackageRecord, RecordError, Records,
    RepositoryInstaller,
};
use hostkeeper_privileged::{SYSTEMD_RUNTIME_DIR, check_host};
use hostkeeper_steps::{SystemdLauncher, SystemdRunProcess};
use hostkeeper_testkit::Scratch;

use super::repo::{self, Built, DEFAULT, Key, PACKAGE, SUITE, Signing, Spec};

/// The name of the repository entry: the stem of its `.sources` file.
pub const REPOSITORY: &str = "hkaptfix";
/// The link that enables the run unit, where the service manager reads it.
pub const LINK: &str = "/etc/systemd/system/multi-user.target.wants/hkaptfix.service";
/// Where the package installs its run unit.
pub const UNIT: &str = "/usr/lib/systemd/system/hkaptfix.service";
/// Every file and directory the package installs, which no purge may leave.
pub const FILES: [&str; 8] = [
    "/usr/bin/hkaptfix",
    UNIT,
    "/usr/lib/systemd/system/hkaptfix-extra.timer",
    "/usr/share/hkaptfix/version",
    "/usr/share/hkaptfix",
    "/usr/share/doc/hkaptfix",
    "/etc/hkaptfix/hkaptfix.conf",
    "/etc/hkaptfix",
];

/// The files the fixture packages write to show what their scripts met, which no test leaves.
pub const EVIDENCE: [&str; 4] = [
    "/run/hkaptfix-policy-seen",
    "/run/hkaptfix-policy-exit",
    "/run/hkaptfix-hook.log",
    "/run/hkaptfix-hook.fail-verify",
];

/// The tests share the host's one dpkg database and the one package in it, so they take turns.
static HOST: Mutex<()> = Mutex::new(());

/// Who signed the suite, against whose key the host's entry trusts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The suite is signed by the key the entry's `Signed-By` names.
    Entry,
    /// The suite is signed by another key.
    Other,
}

/// What a command printed and how it ended.
pub struct Printed {
    pub code: Option<i32>,
    pub text: String,
}

/// One test's host: see the module's documentation.
pub struct Rig {
    pub service: ServiceName,
    pub repo: PathBuf,
    pub built: Vec<Built>,
    pub records: MemRecords,
    pub launcher: SystemdLauncher<SystemdRunProcess>,
    lock: InstallerLock,
    repositories: Repositories,
    dirs: HostDirs,
    world: PathBuf,
    scratch_path: PathBuf,
    entry_key: Key,
    // Held so that its agent is stopped when the rig is dropped.
    _other_key: Key,
    // The last field: dropped after the keys' agents were stopped and the package was purged.
    _scratch: Scratch,
    _turn: MutexGuard<'static, ()>,
}

/// The records of the services, in memory.
#[derive(Default)]
pub struct MemRecords(Mutex<BTreeMap<String, PackageRecord>>);

impl MemRecords {
    fn map(&self) -> MutexGuard<'_, BTreeMap<String, PackageRecord>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The record of `service`.
    pub fn get(&self, service: &ServiceName) -> Option<PackageRecord> {
        self.map().get(service.as_str()).cloned()
    }
}

impl Records for MemRecords {
    fn load(&self, service: &ServiceName) -> Result<Option<PackageRecord>, RecordError> {
        Ok(self.get(service))
    }

    fn save(&self, service: &ServiceName, record: &PackageRecord) -> Result<(), RecordError> {
        self.map()
            .insert(service.as_str().to_owned(), record.clone());
        Ok(())
    }

    fn clear(&self, service: &ServiceName) -> Result<(), RecordError> {
        self.map().remove(service.as_str());
        Ok(())
    }

    fn holder(
        &self,
        package: &str,
        service: &ServiceName,
    ) -> Result<Option<ServiceName>, RecordError> {
        let found = self
            .map()
            .iter()
            .find(|(name, record)| name.as_str() != service.as_str() && record.package == package)
            .map(|(name, _)| name.clone());
        Ok(found.map(|name| ServiceName::new(&name).expect("a recorded service name")))
    }
}

/// Runs `program` with `args` and answers what it printed, standard error after standard output.
pub fn print(program: &str, args: &[&str]) -> Printed {
    let out = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .output()
        .unwrap_or_else(|e| panic!("run {program}: {e}"));
    Printed {
        code: out.status.code(),
        text: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// Puts the host as it was before the package: unheld, purged, unlinked, reloaded.
fn clean_host() {
    // A daemon test starts the unit (`hkaptfix serve` runs until it is stopped).
    print("systemctl", &["stop", "hkaptfix.service"]);
    print("apt-mark", &["unhold", PACKAGE]);
    print("dpkg", &["--purge", PACKAGE]);
    let _ = fs::remove_file(LINK);
    for file in EVIDENCE {
        let _ = fs::remove_file(file);
    }
    // A killed run of the installer leaves its policy file, which would hold the next test's
    // maintainer scripts back.
    let _ = hostkeeper_install::repository::policy::clear_stale(Path::new("/"));
    print("systemctl", &["daemon-reload"]);
}

/// `dpkg-query`'s version of the package when it is fully installed; `None` when the database
/// knows no such package or holds only what a removal left.
pub fn dpkg_version() -> Option<String> {
    let shown = print(
        "dpkg-query",
        &["-W", "-f=${Version}\\t${db:Status-Abbrev}\\n", PACKAGE],
    );
    if shown.code != Some(0) {
        return None;
    }
    let (version, status) = shown.text.trim_end().split_once('\t')?;
    // The abbreviation is the wanted state, the state and the error: `hi ` is installed and held.
    (status.as_bytes().get(1) == Some(&b'i')).then(|| version.to_owned())
}

/// The packages `apt-mark showhold` lists.
pub fn held() -> Vec<String> {
    print("apt-mark", &["showhold"])
        .text
        .lines()
        .map(str::to_owned)
        .collect()
}

/// What the installed program prints.
pub fn program_says() -> String {
    print("/usr/bin/hkaptfix", &[]).text.trim().to_owned()
}

/// The text the entry of the host's configuration holds.
fn entry_text(repo: &Path, keyring: &Path) -> String {
    format!(
        "Types: deb\nURIs: file:{}\nSuites: {SUITE}\nComponents: main\nSigned-By: {}\n",
        repo.display(),
        keyring.display()
    )
}

impl Rig {
    /// The host of the test `name`, which refuses to go on but as root with systemd as the
    /// service manager. The repository lists [`DEFAULT`], signed as `trust` says.
    pub fn new(name: &str, trust: Trust) -> Rig {
        let turn = HOST.lock().unwrap_or_else(|e| e.into_inner());
        let root = rustix::process::geteuid().is_root();
        if let Err(e) = check_host(root, Path::new(SYSTEMD_RUNTIME_DIR)) {
            panic!("{e}");
        }
        clean_host();
        let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
            .expect("the scratch directory");
        let base = scratch.path().to_path_buf();
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).expect("scratch mode");
        let dirs: Vec<PathBuf> = ["repo", "trees", "config", "work", "staged", "world"]
            .iter()
            .map(|d| base.join(d))
            .collect();
        for dir in &dirs {
            fs::create_dir(dir).expect("make a directory of the rig");
        }
        let (repo, trees, config_dir, work, world) =
            (&dirs[0], &dirs[1], &dirs[2], &dirs[3], &dirs[5]);
        let entry_key = Key::new("entry");
        let other_key = Key::new("other");
        let keyring = config_dir.join("hkaptfix.gpg");
        entry_key.export(&keyring);
        fs::write(
            config_dir.join("hkaptfix.sources"),
            entry_text(repo, &keyring),
        )
        .expect("write the entry");
        fs::set_permissions(
            config_dir.join("hkaptfix.sources"),
            fs::Permissions::from_mode(0o644),
        )
        .expect("entry mode");
        fs::set_permissions(config_dir, fs::Permissions::from_mode(0o755)).expect("config mode");
        let repositories = config::load(config_dir, 0).expect("the repository configuration");
        let signer = match trust {
            Trust::Entry => &entry_key,
            Trust::Other => &other_key,
        };
        let built = repo::publish(repo, trees, &DEFAULT, Signing::Signed(signer));
        let rig = Rig {
            service: ServiceName::new(PACKAGE).expect("the service name"),
            repo: repo.clone(),
            built,
            records: MemRecords::default(),
            launcher: SystemdLauncher::system(),
            lock: InstallerLock::new(&base.join("installer.lock")),
            repositories,
            dirs: HostDirs {
                root: PathBuf::from("/"),
                work: work.clone(),
                mirror: base.join("mirror"),
            },
            world: world.clone(),
            scratch_path: base,
            entry_key,
            _other_key: other_key,
            _scratch: scratch,
            _turn: turn,
        };
        rig.prepare_world();
        rig
    }

    /// Publishes the repository again with `specs`, signed by the entry's key.
    pub fn republish(&mut self, specs: &[Spec]) {
        self.built = repo::publish(
            &self.repo,
            &self.scratch_path.join("trees"),
            specs,
            Signing::Signed(&self.entry_key),
        );
    }

    /// The host's repository configuration: the one entry, for the rig's repository.
    pub fn repositories(&self) -> &Repositories {
        &self.repositories
    }

    /// The pin of the package at `package_version`, as the repository was built.
    pub fn pin(&self, package_version: &str) -> String {
        self.order(package_version).pin
    }

    /// The tools of the service, run by the production launcher.
    pub fn tools(&self) -> LauncherTools<'_> {
        LauncherTools::new(&self.launcher, PACKAGE)
    }

    /// The installer: the `apt` backend with the host's real tools, the in-memory record, the
    /// test's lock and work directory, and the host's root.
    pub fn installer<'a>(&'a self, tools: &'a LauncherTools<'a>) -> RepositoryInstaller<'a> {
        RepositoryInstaller {
            backend: &Apt,
            tools,
            records: &self.records,
            lock: &self.lock,
            repositories: &self.repositories,
            dirs: &self.dirs,
            pause: &Sleep,
            lock_wait_secs: hostkeeper_daemon::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS,
            mirror: None,
        }
    }

    /// The order of the package at `package_version`, pinned to the digest the repository was
    /// built with.
    pub fn order(&self, package_version: &str) -> Order {
        let pin = self
            .built
            .iter()
            .find(|b| b.version == package_version)
            .unwrap_or_else(|| panic!("the repository was built with no {package_version}"))
            .sha256
            .clone();
        self.order_pinned(package_version, &pin)
    }

    /// The order of the package at `package_version`, pinned to `pin`.
    pub fn order_pinned(&self, package_version: &str, pin: &str) -> Order {
        let upstream = package_version.split('-').next().unwrap_or_default();
        Order {
            service: self.service.clone(),
            version: Version::new(&format!("{upstream}.0")).expect("a version"),
            package: PACKAGE.to_owned(),
            package_version: package_version.to_owned(),
            repository: REPOSITORY.to_owned(),
            pin: pin.to_owned(),
            run: vec!["hkaptfix.service".to_owned()],
            hook: None,
            bounds: Bounds {
                stage: 120,
                activate: 120,
                remove: 120,
            },
            max_package_bytes: 10 * 1024 * 1024,
        }
    }

    /// A new, empty staging directory named `name`, as the core makes one.
    /// The service's work directory, where its stage runs apt.
    pub fn work(&self) -> PathBuf {
        self.dirs.work.join(self.service.as_str())
    }

    pub fn staging(&self, name: &str) -> PathBuf {
        let dir = self.scratch_path.join("staged").join(name);
        fs::create_dir(&dir).expect("make a staging directory");
        dir
    }

    /// The host's apt, by hand, over a world of its own that names the fixture repository alone
    /// (the lists and the archives in the scratch directory): the update and the options of
    /// section 4.2 in [`apt::options`], then `args`.
    pub fn apt_by_hand(&self, args: &[&str]) -> Printed {
        let mut all = apt::options(&self.world, 60);
        all.extend(args.iter().map(|a| (*a).to_owned()));
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        print("apt-get", &all)
    }

    /// The world's directories and its copy of the entry.
    fn prepare_world(&self) {
        for dir in ["sources", "empty", "lists/partial", "archives/partial"] {
            fs::create_dir_all(self.world.join(dir)).expect("make the world's directories");
        }
        let keyring = self.scratch_path.join("config/hkaptfix.gpg");
        fs::write(
            self.world.join("sources/hkaptfix.sources"),
            entry_text(&self.repo, &keyring),
        )
        .expect("write the world's entry");
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        clean_host();
    }
}
