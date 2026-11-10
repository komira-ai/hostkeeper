//! What the start-pass, watcher, observer and startup tests share: a rig whose host has apt
//! ([`crate::support_apt`]) and whose updates are of native versions. Each version an order
//! names is published as a `.deb` in the host's repository, with the hook program its manifest
//! has, and the order carries its entry, so that the daemon stages the version from the
//! repository as it does on a host, and a version made by [`Rig::stage`] or [`Rig::add_with`] is
//! one that was installed before.

#![allow(
    dead_code,
    unused_imports,
    reason = "each part of the support is used by some of the test files"
)]

use std::collections::BTreeMap;
use std::fs;
use std::ops::Deref;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;

use hostkeeper_daemon::boot::BootReport;
use hostkeeper_daemon::{Daemon, Ports};
use hostkeeper_install::repository::tree::hook_path;
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_proto::hostkeeper::release::v1::Target;

use crate::support::versions::entry_of;
use crate::support::{BASE, Rig, unit_text, update};
use crate::support_apt::{AptRig, Deb, REPO, with_entry};
use hostkeeper_daemon::testing::manifest as manifest_text;
use hostkeeper_placed::InstalledEntry;

/// A rig on a host with apt that has published the versions its orders name.
pub struct Bench {
    pub apt: AptRig,
    entries: Mutex<BTreeMap<(String, String), Target>>,
}

impl Deref for Bench {
    type Target = Rig;

    fn deref(&self) -> &Rig {
        &self.apt.rig
    }
}

/// `dir` and the directories above it that are not there yet, each mode 0755: the step runner
/// trusts no directory writable by group or others.
fn make_dirs(dir: &std::path::Path) {
    if let Some(parent) = dir.parent().filter(|parent| !parent.exists()) {
        make_dirs(parent);
    }
    if !dir.exists() {
        fs::create_dir(dir).expect("a directory");
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
}

impl Bench {
    pub fn new(name: &str) -> Bench {
        Bench {
            apt: AptRig::new(name),
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// The daemon's ports: apt announced, the entry `main` configured.
    pub fn ports(&self) -> Ports {
        self.apt.ports()
    }

    /// A daemon on the rig's directories, with no dead-men.
    pub fn daemon(&self) -> Daemon {
        self.apt.daemon()
    }

    /// A daemon on the rig's directories whose start pass ran: the boot id of the rig's clock
    /// is written.
    pub fn booted(&self) -> (Daemon, BootReport) {
        fs::create_dir_all(&self.state).expect("the state dir");
        fs::set_permissions(&self.state, fs::Permissions::from_mode(0o700)).expect("its mode");
        let daemon = self.daemon();
        let report = daemon.boot_pass().expect("the start pass");
        (daemon, report)
    }

    /// The entry of `service` at `version` built from `manifest` (the TOML text of a manifest),
    /// with the package of it: its hook program at its fixed path when the manifest has a hook
    /// command, and each unit of `[units] run` (`<service>.service` when there is none) as a
    /// unit wanted by the multi-user target. The entry is pinned to the package's digest.
    fn package_of(&self, service: &str, version: &str, manifest: &str) -> (Target, Deb, bool) {
        let (mut entry, hook) = entry_of(service, version, manifest);
        let package_version = entry.package_version.clone().expect("a package version");
        let mut deb = Deb::new(service, &package_version, "amd64");
        if hook {
            deb = deb.file(&hook_path(service), 0o755, "#!hook\n");
        }
        // A manifest names one run unit at least (the entry judges 1 to 64): a package whose
        // manifest names none runs the unit of its service.
        if let Some(manifest) = entry.manifest.as_mut().filter(|m| m.run.is_empty()) {
            manifest.run.push(format!("{service}.service"));
        }
        let run = entry
            .manifest
            .as_ref()
            .map(|manifest| manifest.run.clone())
            .unwrap_or_default();
        for unit in run {
            deb = deb.file(
                &format!("usr/lib/systemd/system/{unit}"),
                0o644,
                &unit_text(),
            );
        }
        entry.digest = deb.pin();
        (entry, deb, hook)
    }

    fn register(&self, service: &str, version: &str, entry: &Target) {
        self.entries
            .lock()
            .expect("the entries")
            .insert((service.to_owned(), version.to_owned()), entry.clone());
    }

    /// Publishes `service` at `version` in the host's repository, its entry built from
    /// `manifest`; an order for the version carries the entry, and the daemon stages the version
    /// from the repository.
    pub fn publish(&self, service: &str, version: &str, manifest: &str) -> Target {
        let (entry, deb, _) = self.package_of(service, version, manifest);
        self.apt.publish(deb);
        self.register(service, version, &entry);
        entry
    }

    /// `service` at `version` as the installer leaves it on the host, with no order: published
    /// in the repository, and its version directory made, with its `target.json`, its tree (the
    /// hook program and the units of the package), the installer's copy of the index and the
    /// package under `apt/`, and its seal, so that an order for it finds the sealed version of
    /// its entry and stages nothing, and the installer's `activate` of it finds what it
    /// installs from.
    pub fn stage_native(&self, service: &str, version: &str, manifest: &str) -> Target {
        let (entry, deb, hook) = self.package_of(service, version, manifest);
        self.apt.publish(deb.clone());
        self.register(service, version, &entry);
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
    /// [`Bench::stage_native`] leaves it, and a service with no `current` runs [`BASE`] (a
    /// package with no hook program, so that its steps are the built-in ones), installed and
    /// held: what an update to `version` needs.
    pub fn add(&self, service: &str, version: &str) {
        self.stage_native(service, version, &manifest_text(service, version));
        if fs::symlink_metadata(self.packages.join(service).join("current")).is_err() {
            let bare = manifest_text(service, BASE)
                .replace("command = [\"bin/hook\", \"update-hook\"]\n", "");
            let entry = self.stage_native(service, BASE, &bare);
            self.store.link(service, "current", BASE);
            let package_version = entry.package_version.clone().expect("a version");
            self.apt
                .host
                .set_installed(service, &package_version, 'i', true);
            let mut record = self.placed_record();
            let mut installed = InstalledEntry::new("apt");
            installed.package = Some(service.to_owned());
            installed.version = Some(package_version);
            installed.repository = Some(REPO.to_owned());
            record.installed.insert(service.to_owned(), installed);
            self.save_placed(&record);
        }
    }

    /// The `update` order of `service` to `version`, which [`Bench::publish`] or [`Bench::add`]
    /// published: the same order, with the same entry, each time it is asked for.
    pub fn update(&self, service: &str, version: &str, order_try: u64) -> Order {
        let entry = self
            .entries
            .lock()
            .expect("the entries")
            .get(&(service.to_owned(), version.to_owned()))
            .cloned()
            .expect("a version that was published");
        with_entry(update(service, version, &entry.digest, order_try), &entry)
    }

    /// The version directory of `service` at `version`.
    pub fn version_dir(&self, service: &str, version: &str) -> PathBuf {
        self.packages.join(service).join(version)
    }
}
