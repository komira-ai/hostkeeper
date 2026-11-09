//! hostkeeper's package in the cross-release rig on a Mac: the `hostkeeper-archive` package of
//! each version, signed and published on the release channel, and ordered the way CI's set of a
//! release still orders a Mac.
//!
//! No daemon announces `macos-pkg` yet (`hostkeeper_install::announce`), so a Mac can take no
//! native entry and the Mac half of the rig keeps its archive orders: hostkeeper's package of
//! each version is built as the release builder lays it out
//! (`crates/xtask/src/release/package.rs`: `bin/hostkeeper`, `files/trusted_root.json`,
//! `manifest.toml`; this build's also `bin/hook`), signed and put on the loopback release channel
//! with its index, and a set carries the one `hostkeeper-archive` entry and the index with its
//! digest ([`Rig::order`]). A daemon that announces no `target-v1` is ordered the package and the
//! index digest and fetches the index from the channel; one that does is ordered the entry and
//! fetches through it. The Linux half orders native entries alone (`super::deb`).
//!
//! The released version's package is built the same way around the released binary, whose hook
//! is the release's own, `bin/hostkeeper update-hook`, which the released daemon does not run (it
//! runs its built-in steps at that floor); this build's is the workload fixture `bin/hook` (ending
//! every step 0), because a dev build's floor is at or below the daemon's legacy floor and a
//! daemon then runs the hook's command, which this build's binary has no verb for.

use std::collections::BTreeMap;
use std::fs;

use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Location, Manifest, Target, TargetList};

use super::super::e2e::package::{ArchiveEntry, compressed_archive};
use super::{Build, HK, Host, Rig, floor};

/// The workload fixture, the hook of the packages of this build.
pub const HOOK: &str = env!("CARGO_BIN_EXE_hostkeeper-fixture-hook");

/// The manifest of hostkeeper's package of `build` at `version` with the floor `floor`, as the
/// release builder writes it; this build's hook is the workload fixture `bin/hook` in mode `ok`
/// ([`HOOK`]) in place of `bin/hostkeeper update-hook`, which its binary has no verb for.
pub fn manifest(version: &str, floor: &str, build: Build) -> String {
    let platform = Platform::current();
    let units = if cfg!(target_os = "linux") {
        "\n[units]\nrun = [\"hostkeeper.service\"]\n"
    } else {
        ""
    };
    let (command, params) = match build {
        Build::Released => ("\"bin/hostkeeper\", \"update-hook\"", ""),
        Build::This | Build::Failing => ("\"bin/hook\"", "\n[params]\n\"fixture.mode\" = \"ok\"\n"),
    };
    format!(
        "schema = 1\n\
         name = \"{HK}\"\n\
         version = \"{version}\"\n\
         platform = \"{platform}\"\n\
         min_hostkeeper = \"{floor}\"\n\
         \n\
         [hook]\n\
         command = [{command}]\n\
         verify_deadline_secs = 600\n\
         \n\
         [hook.timeout_secs]\n\
         prepare = 300\n\
         cutover = 300\n\
         verify = 300\n\
         rollback = 300\n\
         cleanup = 300\n\
         {params}{units}"
    )
}

/// The store of the release channel, which the entries of the rig's sets name: the one a daemon
/// without a `stores.toml` has.
pub const CHANNEL_STORE: &str = "channel";

/// A package of hostkeeper the rig built and published.
#[derive(Clone, Debug)]
pub struct Built {
    pub version: String,
    /// The archive's SHA-256.
    pub sha256: String,
    /// The archive's size.
    pub bytes: u64,
    /// The archive's name on the channel, `<service>-<version>-<platform>.tar.zst`.
    pub file_name: String,
    /// The SHA-256 of the attestation bundle beside the archive.
    pub bundle_sha256: String,
    /// The bundle's size.
    pub bundle_bytes: u64,
    /// The `min_hostkeeper` of the package's manifest, and of its entry's.
    pub floor: String,
    /// The text of the version's `index.json` on the channel, and its SHA-256.
    pub index: String,
    pub index_sha256: String,
}

impl Built {
    /// The path of the version's release index on the channel, as a request for it is logged.
    pub fn index_path(&self) -> String {
        format!("/{HK}/{}/index.json", self.version)
    }

    /// The path of the archive in the channel's store, as a location names it: `S/V/S-V-platform.tar.zst`,
    /// below the base of the release channel (the form of `release targets`).
    pub fn archive_locator(&self) -> String {
        format!("{HK}/{}/{}", self.version, self.file_name)
    }

    /// The path of the bundle in the channel's store: the archive's, with `.sigstore.json`.
    pub fn bundle_locator(&self) -> String {
        format!("{}.sigstore.json", self.archive_locator())
    }

    /// The one `hostkeeper-archive` entry of this package for the host's platform, as CI's set
    /// writes it (`crates/xtask/src/release/targets.rs`) with the channel as the only store: the
    /// archive and its bundle at their locators, their digests and sizes, and the manifest's
    /// floor.
    pub fn entry(&self) -> Target {
        let at = |locator: String| Location {
            store: CHANNEL_STORE.to_owned(),
            locator,
        };
        Target {
            installer: "hostkeeper-archive".to_owned(),
            locations: vec![at(self.archive_locator())],
            digest: self.sha256.clone(),
            bytes: self.bytes,
            attestation: Some(Blob {
                digest: self.bundle_sha256.clone(),
                bytes: self.bundle_bytes,
                locations: vec![at(self.bundle_locator())],
            }),
            manifest: Some(Manifest {
                min_hostkeeper: self.floor.clone(),
                ..Manifest::default()
            }),
            repository: None,
            package_version: None,
        }
    }

    /// The `targets` of a set of this package: the entry for the host's platform.
    pub fn targets(&self) -> BTreeMap<String, TargetList> {
        BTreeMap::from([(
            Platform::current().to_string(),
            TargetList {
                entries: vec![self.entry()],
            },
        )])
    }
}

impl Rig {
    /// Builds hostkeeper's package at `version` carrying the `hostkeeper` of `build`, signs it
    /// and publishes it on the release channel.
    pub fn publish(&self, version: &str, build: Build) -> Built {
        let binary = self.binary(build);
        let floor = floor(build, &self.released);
        let manifest = manifest(version, &floor, build);
        let root = self.signer.trusted_root_jsonl();
        let unit = Host::package_unit(self.host.root());
        let hook = (build != Build::Released)
            .then(|| fs::read(HOOK).unwrap_or_else(|e| panic!("read {HOOK}: {e}")));
        let mut entries = vec![
            ArchiveEntry {
                name: "bin/",
                mode: 0o555,
                data: b"",
            },
            ArchiveEntry {
                name: "bin/hostkeeper",
                mode: 0o555,
                data: &binary,
            },
            ArchiveEntry {
                name: "files/",
                mode: 0o555,
                data: b"",
            },
            ArchiveEntry {
                name: "files/trusted_root.json",
                mode: 0o444,
                data: root.as_bytes(),
            },
            ArchiveEntry {
                name: "manifest.toml",
                mode: 0o444,
                data: manifest.as_bytes(),
            },
        ];
        if let Some(hook) = &hook {
            entries.push(ArchiveEntry {
                name: "bin/hook",
                mode: 0o555,
                data: hook,
            });
        }
        if let Some(unit) = &unit {
            entries.push(ArchiveEntry {
                name: "units/",
                mode: 0o555,
                data: b"",
            });
            entries.push(ArchiveEntry {
                name: "units/hostkeeper.service",
                mode: 0o444,
                data: unit.as_bytes(),
            });
        }
        let bytes = compressed_archive(&entries);
        let sha256 = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
        let file_name = format!("{HK}-{version}-{}.tar.zst", Platform::current());
        let bundle = self.signer.bundle(&file_name, &sha256);
        let (archive_len, bundle_len) = (bytes.len() as u64, bundle.len() as u64);
        let bundle_sha256 = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bundle));
        self.channel.publish(HK, version, &file_name, bytes, bundle);
        let index = self
            .channel
            .object(&format!("/{HK}/{version}/index.json"))
            .expect("the channel serves the index it just published");
        Built {
            version: version.to_owned(),
            sha256,
            bytes: archive_len,
            file_name,
            bundle_sha256,
            bundle_bytes: bundle_len,
            floor,
            index_sha256: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&index)),
            index: String::from_utf8(index).expect("the index is text"),
        }
    }

    /// Sets the desired version of hostkeeper to the published package `built` as CI's set does
    /// (design stage 2 and 3): by targets, the one `hostkeeper-archive` entry for the host's
    /// platform ([`Built::entry`]), and the version's release index with its digest, through the
    /// operator API. A daemon that announces no `target-v1` is ordered the package and the index
    /// digest of that platform and fetches the index from the channel; one that does is ordered
    /// the entry and fetches through it.
    pub fn order(&self, built: &Built) {
        self.coordinator.set_desired_targets(
            HK,
            &built.version,
            built.targets(),
            &built.index,
            &built.index_sha256,
        );
    }

    /// The released daemon's start as the host's daemon: its binary staged first and the unit or
    /// job started.
    pub(super) fn start_released(&self, first: &std::path::Path) {
        self.host.start(first, &self.released);
    }

    /// The released version's package installed by the coordinator's order: the install of the
    /// version that runs, which the daemon takes without a restart. It waits until the host has reported that order `OUTCOME_SUCCEEDED`, not only until the
    /// coordinator counts the host converged: the host runs that version from its start, so it is
    /// counted converged while the install order may still be running, and a test's next order
    /// sent then would be answered `busy` with it.
    pub(super) fn converge_on_released(&self) {
        let released = self.released.clone();
        let built = self.publish(&released, Build::Released);
        self.order(&built);
        self.wait_converged(&released);
        self.wait_reported(&released, "OUTCOME_SUCCEEDED");
    }

    /// Nothing more than the step record is shown of the package on a Mac.
    pub(super) fn native_view(&self) -> String {
        String::new()
    }
}
