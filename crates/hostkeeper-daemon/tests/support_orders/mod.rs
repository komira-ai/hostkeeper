//! A host whose daemon announces apt and runs any number of services installed from packages of
//! its repository, for the tests of orders (their checks, locks, try rules, steps, abort and
//! force): a service's version is a package `<service>` at `<version>-1`, published in the host's
//! one repository the first time an order or the host names it, with the units its manifest runs
//! and the hook program at the fixed path `usr/lib/<service>/hostkeeper-hook` (the launcher's
//! scripted steps stand for it). An order carries the entry of its version, whose digest is the
//! package's pin.
//!
//! The manifest text of a version is [`hostkeeper_daemon::testing::manifest`]'s unless the test
//! sets another with [`Host::manifest`] before the version is named: its `[hook]` bounds, `[units]
//! run`, `[health] url` and `[params]` are the entry's manifest (as
//! [`crate::support::versions::entry_of`] builds it); a manifest that runs no unit runs
//! `<service>.service`.

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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_daemon::boot::BootReport;
use hostkeeper_daemon::prune::Prune;
use hostkeeper_daemon::repository::backend_of_entry;
use hostkeeper_daemon::testing::{Signal, manifest};
use hostkeeper_daemon::{Daemon, Placement, Ports, Reply};
use hostkeeper_hook::Verdict;
use hostkeeper_install::repository::tree::hook_path;
use hostkeeper_placed::testing::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_proto::hostkeeper::release::v1::Target;

use crate::support::versions::entry_of;
use crate::support::{OWN, Rig, TARGET, config, roots};
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

/// What a version of a service is: its service and its version.
type Key = (String, String);

/// The rig of an apt host with services of any name.
pub struct Host {
    pub apt: AptRig,
    manifests: Mutex<BTreeMap<Key, String>>,
    pins: Mutex<BTreeMap<Key, String>>,
    shipped: Mutex<BTreeMap<Key, Vec<String>>>,
    /// How many tool runs of the fake were made when the host last set a service up.
    counted: AtomicUsize,
}

impl Deref for Host {
    type Target = Rig;

    fn deref(&self) -> &Rig {
        &self.apt.rig
    }
}

impl Host {
    pub fn new(name: &str) -> Host {
        Host {
            apt: AptRig::new(name),
            manifests: Mutex::default(),
            pins: Mutex::default(),
            shipped: Mutex::default(),
            counted: AtomicUsize::new(0),
        }
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

    /// Sets the manifest text of `service` at `version`, before it is named.
    pub fn manifest(&self, service: &str, version: &str, text: &str) {
        let key = (service.to_owned(), version.to_owned());
        assert!(
            !self.pins.lock().expect("the pins").contains_key(&key),
            "{service} {version} is published already"
        );
        self.manifests
            .lock()
            .expect("the manifests")
            .insert(key, text.to_owned());
    }

    /// The package of `service` at `version` also ships the unit file `unit`, which its manifest
    /// does not run; set before the version is named.
    pub fn ship(&self, service: &str, version: &str, unit: &str) {
        let key = (service.to_owned(), version.to_owned());
        assert!(
            !self.pins.lock().expect("the pins").contains_key(&key),
            "{service} {version} is published already"
        );
        let mut shipped = self.shipped.lock().expect("the shipped units");
        shipped.entry(key).or_default().push(unit.to_owned());
    }

    /// The manifest text of `service` at `version`: a manifest that runs no unit runs
    /// `<service>.service`.
    pub fn text(&self, service: &str, version: &str) -> String {
        let key = (service.to_owned(), version.to_owned());
        let set = self
            .manifests
            .lock()
            .expect("the manifests")
            .get(&key)
            .cloned();
        let text = set.unwrap_or_else(|| manifest(service, version));
        let runs = text
            .parse::<toml::Table>()
            .expect("a manifest of TOML")
            .get("units")
            .and_then(toml::Value::as_table)
            .and_then(|units| units.get("run"))
            .is_some();
        match runs {
            true => text,
            false => format!("{text}\n[units]\nrun = [\"{service}.service\"]\n"),
        }
    }

    /// The package of `service` at `version`: its program, its hook when the manifest has one,
    /// and a unit file for each unit the manifest runs.
    fn deb(&self, service: &str, version: &str) -> Deb {
        let (entry, hook) = entry_of(service, version, &self.text(service, version));
        let mut run = entry.manifest.map(|m| m.run).unwrap_or_default();
        let key = (service.to_owned(), version.to_owned());
        let shipped = self.shipped.lock().expect("the shipped units");
        run.extend(shipped.get(&key).cloned().unwrap_or_default());
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
        match hook {
            true => deb.file(&hook_path(service), 0o755, "#!/bin/sh\nexit 0\n"),
            false => deb,
        }
    }

    /// The pin of `service` at `version`: its package, published in the host's repository the
    /// first time it is asked.
    pub fn pin(&self, service: &str, version: &str) -> String {
        let key = (service.to_owned(), version.to_owned());
        let mut pins = self.pins.lock().expect("the pins");
        if let Some(pin) = pins.get(&key) {
            return pin.clone();
        }
        let pin = self.apt.publish(self.deb(service, version));
        pins.insert(key, pin.clone());
        pin
    }

    /// The entry of `service` at `version`: its manifest, pinned to its package.
    pub fn entry(&self, service: &str, version: &str) -> Target {
        let (mut entry, _) = entry_of(service, version, &self.text(service, version));
        entry.digest = self.pin(service, version);
        entry
    }

    /// `order` of `service` at `version`, carrying its entry.
    fn ordered(&self, service: &str, version: &str, order: impl FnOnce(&str) -> Order) -> Order {
        let entry = self.entry(service, version);
        with_entry(order(&entry.digest), &entry)
    }

    /// An `update` order.
    pub fn update(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.ordered(service, version, |pin| {
            crate::support::update(service, version, pin, order_try)
        })
    }

    /// An `install` order.
    pub fn install(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.ordered(service, version, |pin| {
            crate::support::install(service, version, pin, order_try)
        })
    }

    /// A `force_rollback` order.
    pub fn force_rollback(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.ordered(service, version, |pin| {
            crate::support::force_rollback(service, version, pin, order_try)
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
            crate::support::force(service, version, pin, order_try, halt_generation)
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
    /// version it left. The tools it ran are not counted by [`Host::tools`], nor its writes by `placed_log`.
    pub fn running(&self, service: &str, version: &str) -> PathBuf {
        let entry = self.entry(service, version);
        let backend = backend_of_entry(&entry).expect("a backend for apt");
        let name = ServiceName::new(service).expect("a service name");
        let at = Version::new(version).expect("a version");
        let daemon = self.daemon();
        let repository = daemon.ctx().repository();
        let staged = repository
            .stage(backend, &name, &at, &entry)
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

    /// `service` at `version` is for an order to install; a service with no `current` is run at
    /// [`crate::support::BASE`] first, so that an update of it runs.
    pub fn add(&self, service: &str, version: &str) -> PathBuf {
        self.pin(service, version);
        if fs::symlink_metadata(self.packages.join(service).join("current")).is_err() {
            self.running(service, crate::support::BASE);
        }
        self.version_dir(service, version)
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
}
