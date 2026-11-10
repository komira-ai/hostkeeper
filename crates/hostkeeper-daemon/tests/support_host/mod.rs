//! The native rig of the daemon's tests: a host whose daemon announces apt and whose services
//! are packages of its repository. The repository installer's fake of apt, dpkg and dpkg-deb
//! ([`crate::support_apt`]) answers the daemon's tool runs; the launcher's scripted steps stand
//! for the hook program of each package.
//!
//! A version is published from the text of a manifest, as the daemon's test doubles have always
//! carried it ([`hostkeeper_daemon::testing::manifest`]): [`NativeHost::publish_manifest`] builds
//! the package `<service>` at `<version>-1` (a program, a unit file for each unit the manifest
//! runs, and the hook program at its fixed path when the manifest has a `[hook] command`) and the
//! entry that names it, pinned to the package's SHA-256. A manifest that runs no unit runs
//! `<service>.service`. A version no test published is published, with the manifest set for it
//! with [`NativeHost::manifest`] or else the daemon's test manifest, the first time an order or
//! the host names it.
//!
//! The orders [`NativeHost::install`], [`NativeHost::update`], [`NativeHost::force_rollback`] and
//! [`NativeHost::force`] carry the entry of the version they name, so the daemon stages the
//! version from the repository as it does on a host. A version is on the host before any order in
//! one of two ways: [`NativeHost::seeded`] runs it through the installer's stage and activate,
//! and [`NativeHost::add_staged`] makes its version directory as the installer leaves it, sealed,
//! so that an order for it stages nothing. [`NativeHost::like`] makes a rig that is another one as
//! it is, with no write to disk made again.

#![allow(
    dead_code,
    unused_imports,
    reason = "each part of the support is used by some of the test files"
)]

use std::collections::BTreeMap;
use std::fs;
use std::ops::Deref;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::support::versions::hook_path;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::boot::BootReport;
use hostkeeper_daemon::prune::Prune;
use hostkeeper_daemon::repository::{Native, backend_of_entry};
use hostkeeper_daemon::testing::{Signal, manifest};
use hostkeeper_daemon::{Daemon, Placement, Ports, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_placed::InstalledEntry;
use hostkeeper_placed::testing::{FakeUnitControl, FaultFs};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_proto::hostkeeper::release::v1::Target;

use crate::support::versions::entry_of;
use crate::support::{self, BASE, OWN, Rig, TARGET, config, roots, unit_text};
use crate::support_apt::{AptRig, Deb, REPO, bridge, with_entry};

/// The package version of the service's version `version`.
pub fn pv(version: &str) -> String {
    format!("{version}-1")
}

/// A reply that ran to `Succeeded`, now.
pub fn succeeded() -> Reply {
    Reply::Finished {
        verdict: Verdict::Succeeded {
            promotion_pending: false,
        },
        reason: None,
        resent: false,
    }
}

/// The manifest text of `service` at `version` (the daemon's test double of it) that runs the
/// units `run`.
pub fn runs(service: &str, version: &str, run: &[&str]) -> String {
    let run: Vec<String> = run.iter().map(|unit| format!("\"{unit}\"")).collect();
    format!(
        "{}\n[units]\nrun = [{}]\n",
        manifest(service, version),
        run.join(", ")
    )
}

/// `service`'s manifest at `version` with the bound of its `uninstall` step when `uninstall` is
/// given, running the unit `<service>.service`.
pub fn placed_manifest(service: &str, version: &str, uninstall: Option<u64>) -> String {
    let mut text = manifest(service, version);
    if let Some(bound) = uninstall {
        text.push_str(&format!("uninstall = {bound}\n"));
    }
    text.push_str(&format!("\n[units]\nrun = [\"{service}.service\"]\n"));
    text
}

/// What a version of a service is: its service and its version.
type Key = (String, String);

/// The rig of an apt host with services of any name.
pub struct NativeHost {
    pub apt: AptRig,
    manifests: Mutex<BTreeMap<Key, String>>,
    shipped: Mutex<BTreeMap<Key, Vec<String>>>,
    /// The entry of each version published, and each package published.
    entries: Mutex<BTreeMap<Key, Target>>,
    debs: Mutex<Vec<Deb>>,
    /// The package versions marked installed on the host by [`NativeHost::add_staged`].
    installed: Mutex<Vec<(String, String)>>,
    /// How many tool runs of the fake were made when the host last set a service up.
    counted: AtomicUsize,
}

impl Deref for NativeHost {
    type Target = Rig;

    fn deref(&self) -> &Rig {
        &self.apt.rig
    }
}

/// The contents of the directory `from` made under the directory `to`, which exists: files and
/// links as they are, directories with their modes (set last, so that a read-only one is filled
/// first). Nothing is synced to disk.
fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("a directory to copy").flatten() {
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let meta = fs::symlink_metadata(&source).expect("what to copy");
        if meta.is_symlink() {
            let link = fs::read_link(&source).expect("a link to copy");
            drop(fs::remove_file(&target));
            std::os::unix::fs::symlink(link, &target).expect("the link");
        } else if meta.is_dir() {
            if !target.exists() {
                fs::create_dir(&target).expect("a directory");
            }
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).expect("its mode");
            copy_tree(&source, &target);
            fs::set_permissions(&target, meta.permissions()).expect("its mode");
        } else {
            drop(fs::remove_file(&target));
            fs::copy(&source, &target).expect("a file");
        }
    }
}

/// `dir` and the directories above it that are not there yet, each mode 0755: the step runner
/// trusts no directory writable by group or others.
fn make_dirs(dir: &Path) {
    if let Some(parent) = dir.parent().filter(|parent| !parent.exists()) {
        make_dirs(parent);
    }
    if !dir.exists() {
        fs::create_dir(dir).expect("a directory");
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
}

impl NativeHost {
    pub fn new(name: &str) -> NativeHost {
        NativeHost {
            apt: AptRig::new(name),
            manifests: Mutex::default(),
            shipped: Mutex::default(),
            entries: Mutex::default(),
            debs: Mutex::default(),
            installed: Mutex::default(),
            counted: AtomicUsize::new(0),
        }
    }

    /// A rig named `name` that is `template` as it is now: the same files under its scratch
    /// directory (copied, not made again), the same packages in the repository, the same
    /// packages installed and the same entries. What a test does once to set a host up is not
    /// redone, with its writes to disk, for each rig that starts from it. The fakes of the
    /// service manager and the launcher start empty, as `template`'s did.
    pub fn like(name: &str, template: &NativeHost) -> NativeHost {
        let rig = NativeHost::new(name);
        copy_tree(template.scratch.path(), rig.scratch.path());
        for deb in template.debs.lock().expect("the packages").iter() {
            rig.keep(deb.clone());
        }
        for (service, version) in template.installed.lock().expect("the installed").iter() {
            rig.mark_installed(service, version);
        }
        *rig.entries.lock().expect("the entries") =
            template.entries.lock().expect("the entries").clone();
        *rig.manifests.lock().expect("the manifests") =
            template.manifests.lock().expect("the manifests").clone();
        *rig.shipped.lock().expect("the shipped units") =
            template.shipped.lock().expect("the shipped units").clone();
        rig
    }

    pub fn rig(&self) -> &Rig {
        &self.apt.rig
    }

    /// The daemon's ports: apt announced, the entry `main` configured.
    pub fn ports(&self) -> Ports {
        self.apt.ports()
    }

    /// A daemon on the rig's directories, with no dead-men.
    pub fn daemon(&self) -> Daemon {
        Daemon::new(config(), self.dirs(), self.ports(), OWN)
    }

    /// A daemon on the rig's directories whose start pass ran: the boot id of the rig's clock is
    /// written.
    pub fn booted(&self) -> (Daemon, BootReport) {
        fs::create_dir_all(&self.state).expect("the state dir");
        fs::set_permissions(&self.state, fs::Permissions::from_mode(0o700)).expect("its mode");
        let daemon = self.daemon();
        let report = daemon.boot_pass().expect("the start pass");
        (daemon, report)
    }

    /// A daemon on the rig whose placement goes through `fs` and whose old versions are pruned by
    /// `prune`.
    pub fn daemon_on(&self, fs: Arc<FaultFs>, prune: Arc<dyn Prune>) -> Daemon {
        let ports = Ports {
            placement: Placement { roots: roots(), fs },
            prune,
            ..self.ports()
        };
        Daemon::new(config(), self.dirs(), ports, OWN)
    }

    /// A daemon of the host whose placement goes through `fs` and whose service manager is
    /// `units`.
    pub fn daemon_with(&self, fs: Arc<FaultFs>, units: Arc<FakeUnitControl>) -> Daemon {
        let ports = Ports {
            units,
            placement: Placement { roots: roots(), fs },
            ..self.ports()
        };
        Daemon::new(config(), self.dirs(), ports, OWN)
    }

    /// The tools the package manager was run with since the host last set a service up, by label:
    /// what an order that stages nothing leaves empty.
    pub fn tools(&self) -> Vec<String> {
        let calls = self.apt.host.calls();
        let seen = self.counted.load(Ordering::SeqCst);
        calls[seen..]
            .iter()
            .map(|call| call.label.clone())
            .collect()
    }

    /// How many packages the host's package manager was asked to download.
    pub fn downloads(&self) -> usize {
        let downloads = |label: &String| label == "apt-get download";
        self.tools().iter().filter(|label| downloads(label)).count()
    }

    fn key(service: &str, version: &str) -> Key {
        (service.to_owned(), version.to_owned())
    }

    /// Panics when `service` at `version` is published: what describes it comes first.
    fn unpublished(&self, service: &str, version: &str) {
        assert!(
            !self
                .entries
                .lock()
                .expect("the entries")
                .contains_key(&Self::key(service, version)),
            "{service} {version} is published already"
        );
    }

    /// Sets the manifest text of `service` at `version`, before it is named.
    pub fn manifest(&self, service: &str, version: &str, text: &str) {
        self.unpublished(service, version);
        self.manifests
            .lock()
            .expect("the manifests")
            .insert(Self::key(service, version), text.to_owned());
    }

    /// The package of `service` at `version` also ships the unit file `unit`, which its manifest
    /// does not run; set before the version is named.
    pub fn ship(&self, service: &str, version: &str, unit: &str) {
        self.unpublished(service, version);
        let mut shipped = self.shipped.lock().expect("the shipped units");
        shipped
            .entry(Self::key(service, version))
            .or_default()
            .push(unit.to_owned());
    }

    /// `text`, a manifest of `service`, as a manifest that runs a unit: one that runs none runs
    /// `<service>.service`.
    fn with_units(service: &str, text: &str) -> String {
        let runs = text
            .parse::<toml::Table>()
            .expect("a manifest of TOML")
            .get("units")
            .and_then(toml::Value::as_table)
            .and_then(|units| units.get("run"))
            .is_some();
        match runs {
            true => text.to_owned(),
            false => format!("{text}\n[units]\nrun = [\"{service}.service\"]\n"),
        }
    }

    /// The manifest text of `service` at `version`: the one set for it, else the daemon's test
    /// manifest; one that runs no unit runs `<service>.service`.
    pub fn text(&self, service: &str, version: &str) -> String {
        let set = self
            .manifests
            .lock()
            .expect("the manifests")
            .get(&Self::key(service, version))
            .cloned();
        Self::with_units(service, &set.unwrap_or_else(|| manifest(service, version)))
    }

    /// The package of `service` at `version` and the entry that names it, built from `manifest`:
    /// its program, its hook when the manifest has one, and a unit file for each unit the
    /// manifest runs and each unit [`NativeHost::ship`] names. `tune` may change the package's
    /// files. Whether the package has a hook program comes last.
    fn built(
        &self,
        service: &str,
        version: &str,
        manifest: &str,
        tune: impl FnOnce(Deb) -> Deb,
    ) -> (Target, Deb, bool) {
        let manifest = Self::with_units(service, manifest);
        let (mut entry, hook) = entry_of(service, version, &manifest);
        let mut run = entry
            .manifest
            .as_ref()
            .map(|m| m.run.clone())
            .unwrap_or_default();
        let shipped = self.shipped.lock().expect("the shipped units");
        run.extend(
            shipped
                .get(&Self::key(service, version))
                .cloned()
                .unwrap_or_default(),
        );
        drop(shipped);
        let mut deb = Deb::new(service, &pv(version), "amd64")
            .file(
                &format!("usr/bin/{service}"),
                0o755,
                &format!("#!/bin/sh\necho {service} {version}\n"),
            )
            .link(
                &format!("usr/lib/{service}/current"),
                &format!("../../bin/{service}"),
            );
        for unit in run {
            deb = deb.file(
                &format!("usr/lib/systemd/system/{unit}"),
                0o644,
                &format!(
                    "[Unit]\nDescription={unit}\n[Service]\nExecStart=/usr/bin/{service}\n[Install]\nWantedBy={TARGET}\n"
                ),
            );
        }
        if hook {
            deb = deb.file(&hook_path(service), 0o755, "#!/bin/sh\nexit 0\n");
        }
        let deb = tune(deb);
        entry.package_version = Some(deb.version.clone());
        entry.digest = deb.pin();
        (entry, deb, hook)
    }

    /// Publishes `deb` in the host's repository.
    fn keep(&self, deb: Deb) {
        self.debs.lock().expect("the packages").push(deb.clone());
        self.apt.publish(deb);
    }

    /// `version` of `service` built from `manifest` and published: the package, whose files
    /// `tune` may change, and its entry, which is answered. A package published again replaces
    /// the entry.
    fn publish_built(
        &self,
        service: &str,
        version: &str,
        manifest: &str,
        tune: impl FnOnce(Deb) -> Deb,
    ) -> (Target, Deb, bool) {
        let (entry, deb, hook) = self.built(service, version, manifest, tune);
        self.keep(deb.clone());
        self.entries
            .lock()
            .expect("the entries")
            .insert(Self::key(service, version), entry.clone());
        (entry, deb, hook)
    }

    /// [`NativeHost::publish_manifest`] with the package's files tuned by `tune`.
    pub fn publish_tuned(
        &self,
        service: &str,
        version: &str,
        manifest: &str,
        tune: impl FnOnce(Deb) -> Deb,
    ) -> Target {
        self.publish_built(service, version, manifest, tune).0
    }

    /// `version` of `service`, built from the manifest text `manifest` and published.
    pub fn publish_manifest(&self, service: &str, version: &str, manifest: &str) -> Target {
        self.publish_tuned(service, version, manifest, |deb| deb)
    }

    /// `version` of `service` published with the manifest set for it, else the daemon's test
    /// manifest.
    pub fn publish(&self, service: &str, version: &str) -> Target {
        self.publish_manifest(service, version, &self.text(service, version))
    }

    /// `version` of `service` published with the unit `<service>.service` it runs, and, when
    /// `uninstall` is given, that bound for the `uninstall` step ([`placed_manifest`]).
    pub fn publish_placed(&self, service: &str, version: &str, uninstall: Option<u64>) -> Target {
        self.publish_manifest(
            service,
            version,
            &placed_manifest(service, version, uninstall),
        )
    }

    /// The entry of `service` at `version`: its manifest, pinned to its package, which is
    /// published the first time it is asked.
    pub fn entry(&self, service: &str, version: &str) -> Target {
        let known = self
            .entries
            .lock()
            .expect("the entries")
            .get(&Self::key(service, version))
            .cloned();
        known.unwrap_or_else(|| self.publish(service, version))
    }

    /// The pin of `service` at `version`: the digest of its package.
    pub fn pin(&self, service: &str, version: &str) -> String {
        self.entry(service, version).digest
    }

    /// `order` of `service` at `version`, carrying its entry.
    fn ordered(&self, service: &str, version: &str, order: impl FnOnce(&str) -> Order) -> Order {
        let entry = self.entry(service, version);
        with_entry(order(&entry.digest), &entry)
    }

    /// An `update` order.
    pub fn update(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.ordered(service, version, |pin| {
            support::update(service, version, pin, order_try)
        })
    }

    /// An `install` order.
    pub fn install(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.ordered(service, version, |pin| {
            support::install(service, version, pin, order_try)
        })
    }

    /// A `force_rollback` order.
    pub fn force_rollback(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.ordered(service, version, |pin| {
            support::force_rollback(service, version, pin, order_try)
        })
    }

    /// A `force` order of the halt generation `halt_generation`.
    pub fn force(
        &self,
        service: &str,
        version: &str,
        order_try: u64,
        halt_generation: u64,
    ) -> Order {
        self.ordered(service, version, |pin| {
            support::force(service, version, pin, order_try, halt_generation)
        })
    }

    /// Where the version directory of `service` at `version` is.
    pub fn version_dir(&self, service: &str, version: &str) -> PathBuf {
        self.packages.join(service).join(version)
    }

    /// `service` runs `version` as an earlier order of this daemon left it, with no order made
    /// and no step run: the version staged and activated by the repository installer (the
    /// version directory with its entry, its seal and its copy of the index, the package
    /// installed and held, its record in `placed.json`), and `current` naming it, `previous` the
    /// version it left. The tools it ran are not counted by [`NativeHost::tools`], nor its writes
    /// by `placed_log`.
    pub fn seeded(&self, service: &str, version: &str) -> PathBuf {
        let entry = self.entry(service, version);
        let backend = backend_of_entry(&entry).expect("a backend for apt");
        let name = ServiceName::new(service).expect("a service name");
        let at = Version::new(version).expect("a version");
        let daemon = self.daemon();
        let repository = daemon.ctx().repository();
        let staged = repository
            .stage(Native::Repository(backend), &name, &at, &entry)
            .expect("the version stages");
        let dir = staged.dir().to_path_buf();
        repository
            .activate_version(&name, &at, &dir, &entry)
            .expect("the version activates");
        drop(repository.take_repaired(&name));
        self.counted
            .store(self.apt.host.calls().len(), Ordering::SeqCst);
        self.placed_log.clear();
        dir
    }

    /// `service` at `version` is for an order to install; a service with no `current` is
    /// [`NativeHost::seeded`] at [`BASE`] first, so that an update of it runs.
    pub fn add(&self, service: &str, version: &str) -> PathBuf {
        self.pin(service, version);
        if fs::symlink_metadata(self.packages.join(service).join("current")).is_err() {
            self.seeded(service, BASE);
        }
        self.version_dir(service, version)
    }

    fn mark_installed(&self, service: &str, package_version: &str) {
        self.apt
            .host
            .set_installed(service, package_version, 'i', true);
        self.installed
            .lock()
            .expect("the installed")
            .push((service.to_owned(), package_version.to_owned()));
    }

    /// `service` at `version` as the installer leaves it on the host, with no order: published
    /// in the repository from `manifest`, and its version directory made, with its `target.json`,
    /// its tree (the hook program and the units of the package), the installer's copy of the
    /// index and the package under `apt/`, and its seal, so that an order for it finds the sealed
    /// version of its entry and stages nothing, and the installer's `activate` of it finds what
    /// it installs from.
    pub fn stage_native(&self, service: &str, version: &str, manifest: &str) -> Target {
        let (entry, deb, hook) = self.publish_built(service, version, manifest, |deb| deb);
        let dir = self.store.add_entry(service, version, &entry, hook);
        let units = entry
            .manifest
            .as_ref()
            .map(|manifest| manifest.run.clone())
            .unwrap_or_default();
        let tree = dir.join("tree");
        for unit in units {
            let file = tree.join("usr/lib/systemd/system").join(&unit);
            make_dirs(file.parent().expect("a parent"));
            fs::write(&file, unit_text()).expect("a unit");
            fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("its mode");
        }
        let meta = dir.join("apt");
        make_dirs(&meta.join("lists"));
        make_dirs(&meta.join("archives"));
        fs::write(
            meta.join("main.sources"),
            "Types: deb\nURIs: file:/srv/hostkeeper-apt\n",
        )
        .expect("the sources");
        fs::write(
            meta.join("lists").join("main_Packages"),
            format!(
                "Package: {service}\nVersion: {}\nArchitecture: amd64\n\n",
                deb.version
            ),
        )
        .expect("the index");
        fs::write(
            meta.join("archives")
                .join(format!("{service}_{}_amd64.deb", deb.version)),
            deb.bytes(),
        )
        .expect("the package");
        self.store.seal(service, version, &entry.digest);
        entry
    }

    /// `service` at `version`, with the manifest every rule accepts, is on the host as
    /// [`NativeHost::stage_native`] leaves it, and a service with no `current` runs [`BASE`] (a
    /// package with no hook program, so that its steps are the built-in ones), installed and
    /// held: what an update to `version` needs.
    pub fn add_staged(&self, service: &str, version: &str) {
        self.stage_native(service, version, &manifest(service, version));
        if fs::symlink_metadata(self.packages.join(service).join("current")).is_err() {
            let bare =
                manifest(service, BASE).replace("command = [\"bin/hook\", \"update-hook\"]\n", "");
            let entry = self.stage_native(service, BASE, &bare);
            self.store.link(service, "current", BASE);
            let package_version = entry.package_version.clone().expect("a version");
            self.mark_installed(service, &package_version);
            let mut record = self.placed_record();
            let mut installed = InstalledEntry::new("apt");
            installed.package = Some(service.to_owned());
            installed.version = Some(package_version);
            installed.repository = Some(REPO.to_owned());
            record.installed.insert(service.to_owned(), installed);
            self.save_placed(&record);
        }
    }

    /// `daemon` forces `web` to `version` with `order_try`, and that runs to `Succeeded`; then
    /// the log of placed writes and unit calls is cleared.
    pub fn forced(&self, daemon: &Daemon, version: &str, order_try: u64) {
        let reply = daemon.order(&self.force("web", version, order_try, 0));
        assert_eq!(reply, succeeded(), "the force of {version}");
        self.placed_log.clear();
    }

    /// A signal of each package the host's package manager is asked to download from now on,
    /// as `<package>=<version>`: an order that has got as far as its stage.
    pub fn asked(&self) -> Signal<String> {
        let (notice, signal) = Signal::new();
        let inner = bridge(Arc::clone(&self.apt.host));
        self.launcher.answer_tools(Arc::new(move |call| {
            let args: Vec<String> = call
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            if args.iter().any(|arg| arg == "download") {
                notice.send(args.last().cloned().unwrap_or_default());
            }
            inner(call)
        }));
        signal
    }

    /// Makes every directory of the packages directory writable by its owner: an uninstall
    /// deletes the sealed, read-only version directories, which root does through its mode on a
    /// host and the test's own user cannot.
    pub fn open_up(&self) {
        crate::support_apt::open_up(&self.apt.rig.packages);
    }
}
