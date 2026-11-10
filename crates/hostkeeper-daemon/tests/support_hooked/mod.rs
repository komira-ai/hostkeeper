//! A host whose services are `apt` packages, for the tests of what the daemon does with any
//! order: the repository installer's fake of apt and dpkg ([`crate::support_apt`]) answers the
//! daemon's tool runs, and each version a test orders is published in its repository and staged
//! by the daemon from the entry the order carries.
//!
//! A version is published from the TOML text of a manifest, as the daemon's test doubles have
//! always carried it ([`hostkeeper_daemon::testing::manifest`]): [`Farm::publish`] builds the
//! package (a program that prints the version, a unit file for each unit the manifest runs, and
//! the hook program at its fixed path when the manifest has a `[hook] command`) and the entry
//! that names it, pinned to the package's SHA-256. The orders [`Farm::install`], [`Farm::update`],
//! [`Farm::force_rollback`] and [`Farm::force`] carry the entry of the version they name.

#![allow(
    dead_code,
    unused_imports,
    reason = "each part of the support is used by some of the test files"
)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::boot::BootReport;
use hostkeeper_daemon::testing::manifest;
use hostkeeper_daemon::{Daemon, Placement, Ports};
use hostkeeper_install::repository::tree::hook_path;
use hostkeeper_placed::testing::{FakeUnitControl, FaultFs};
use hostkeeper_proto::hostkeeper::agent::v1::Order;
use hostkeeper_proto::hostkeeper::release::v1::Target;

use crate::support::versions::entry_of;
use crate::support::{self, Rig};
use crate::support_apt::{AptRig, Deb, with_entry};

/// The package version of the package that holds `version` of a service.
pub fn package_version(version: &str) -> String {
    format!("{version}-1")
}

/// The text of a unit that starts the package's program and is wanted by `multi-user.target`.
fn unit_text(service: &str) -> String {
    format!(
        "[Unit]\nDescription={service}\n[Service]\nExecStart=/usr/bin/{service}\n[Install]\nWantedBy={}\n",
        support::TARGET
    )
}

/// The units of a manifest's `[units] run`.
fn run_of(text: &str) -> Vec<String> {
    let table: toml::Table = text.parse().expect("a manifest of TOML");
    table
        .get("units")
        .and_then(toml::Value::as_table)
        .and_then(|units| units.get("run"))
        .and_then(toml::Value::as_array)
        .map(|run| {
            run.iter()
                .map(|unit| unit.as_str().expect("a unit").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// `text`, a manifest of `service`, with the bound of its `uninstall` step and the unit
/// `<service>.service` it runs.
pub fn placed_manifest(service: &str, version: &str, uninstall: Option<u64>) -> String {
    let mut text = manifest(service, version);
    if let Some(bound) = uninstall {
        text.push_str(&format!("uninstall = {bound}\n"));
    }
    text.push_str(&format!("\n[units]\nrun = [\"{service}.service\"]\n"));
    text
}

/// A host that runs apt, with the packages its tests publish.
pub struct Farm {
    pub apt: AptRig,
    published: Mutex<BTreeMap<(String, String), Target>>,
}

impl Farm {
    pub fn new(name: &str) -> Farm {
        Farm {
            apt: AptRig::new(name),
            published: Mutex::default(),
        }
    }

    pub fn rig(&self) -> &Rig {
        &self.apt.rig
    }

    /// The daemon's ports: apt announced, the entry `main` configured.
    pub fn ports(&self) -> Ports {
        self.apt.ports()
    }

    /// A daemon of the host.
    pub fn daemon(&self) -> Daemon {
        self.apt.daemon()
    }

    /// A daemon of the host whose start pass ran: the boot id of the rig's clock is written.
    pub fn booted(&self) -> (Daemon, BootReport) {
        let rig = self.rig();
        fs::create_dir_all(&rig.state).expect("the state dir");
        fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700)).expect("its mode");
        let daemon = self.daemon();
        let report = daemon.boot_pass().expect("the start pass");
        (daemon, report)
    }

    /// A daemon of the host whose placement goes through `fs` and whose service manager is
    /// `units`.
    pub fn daemon_with(&self, fs: Arc<FaultFs>, units: Arc<FakeUnitControl>) -> Daemon {
        let ports = Ports {
            units,
            placement: Placement {
                roots: support::roots(),
                fs,
            },
            ..self.ports()
        };
        Daemon::new(support::config(), self.rig().dirs(), ports, support::OWN)
    }

    /// `version` of `service`, built from the manifest text `manifest` and published: the
    /// package, whose files `tune` may change, and its entry, which is answered.
    pub fn publish_tuned(
        &self,
        service: &str,
        version: &str,
        manifest: &str,
        tune: impl FnOnce(Deb) -> Deb,
    ) -> Target {
        // An entry's manifest runs at least one unit: a manifest that names none runs
        // `<service>.service`.
        let manifest = match run_of(manifest).is_empty() {
            true => format!("{manifest}\n[units]\nrun = [\"{service}.service\"]\n"),
            false => manifest.to_owned(),
        };
        let manifest = manifest.as_str();
        let (mut entry, hook) = entry_of(service, version, manifest);
        let mut deb = Deb::new(service, &package_version(version), "amd64").file(
            &format!("usr/bin/{service}"),
            0o755,
            &format!("#!/bin/sh\necho {version}\n"),
        );
        for unit in run_of(manifest) {
            deb = deb.file(
                &format!("usr/lib/systemd/system/{unit}"),
                0o644,
                &unit_text(service),
            );
        }
        if hook {
            deb = deb.file(&hook_path(service), 0o755, "#!hook\n");
        }
        let deb = tune(deb);
        entry.package_version = Some(deb.version.clone());
        entry.digest = self.apt.publish(deb);
        self.published
            .lock()
            .expect("the published entries")
            .insert((service.to_owned(), version.to_owned()), entry.clone());
        entry
    }

    /// `version` of `service`, built from the manifest text `manifest` and published.
    pub fn publish_manifest(&self, service: &str, version: &str, manifest: &str) -> Target {
        self.publish_tuned(service, version, manifest, |deb| deb)
    }

    /// `version` of `service` with the manifest every rule accepts
    /// ([`hostkeeper_daemon::testing::manifest`]), published.
    pub fn publish(&self, service: &str, version: &str) -> Target {
        self.publish_manifest(service, version, &manifest(service, version))
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

    /// The entry of `version` of `service`, published before.
    pub fn entry(&self, service: &str, version: &str) -> Target {
        self.published
            .lock()
            .expect("the published entries")
            .get(&(service.to_owned(), version.to_owned()))
            .cloned()
            .unwrap_or_else(|| panic!("{version} of {service} is not published"))
    }

    /// `order`, of a version published before, with its entry.
    fn carrying(&self, service: &str, version: &str, order: impl FnOnce(&str) -> Order) -> Order {
        let entry = self.entry(service, version);
        with_entry(order(&entry.digest), &entry)
    }

    /// The `install` of `version` of `service`.
    pub fn install(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.carrying(service, version, |pin| {
            support::install(service, version, pin, order_try)
        })
    }

    /// The `update` of `service` to `version`.
    pub fn update(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.carrying(service, version, |pin| {
            support::update(service, version, pin, order_try)
        })
    }

    /// The `force_rollback` of `service` to `version`.
    pub fn force_rollback(&self, service: &str, version: &str, order_try: u64) -> Order {
        self.carrying(service, version, |pin| {
            support::force_rollback(service, version, pin, order_try)
        })
    }

    /// The `force` of `service` to `version` under `halt_generation`.
    pub fn force(
        &self,
        service: &str,
        version: &str,
        order_try: u64,
        halt_generation: u64,
    ) -> Order {
        self.carrying(service, version, |pin| {
            support::force(service, version, pin, order_try, halt_generation)
        })
    }

    /// Makes every directory of the packages directory writable by its owner: an uninstall
    /// deletes the sealed, read-only version directories, which root does through its mode on a
    /// host and the test's own user cannot.
    pub fn open_up(&self) {
        crate::support_apt::open_up(&self.apt.rig.packages);
    }
}
