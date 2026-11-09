//! `release targets`: the `targets` of the set that deploys one package of a release, read from
//! the release's assets (docs/design/packages.md, section 3.7, build item A5). For each platform
//! the package's release index lists, one `hostkeeper-archive` entry:
//!
//! ```text
//! installer    hostkeeper-archive
//! locations    channel  S/V/S-V-{platform}.tar.zst                       (the release channel)
//!              github   <repo>/releases/download/v<release>/S-V-{platform}.tar.zst
//! digest       the archive's SHA-256, bytes its size: the index's, measured again
//! attestation  the archive's `.sigstore.json` bundle: its SHA-256, its size, the same two
//!              stores at the bundle's path
//! manifest     { min_hostkeeper }: the `min_hostkeeper` of the archive's own manifest.toml
//! ```
//!
//! The entries are the same bytes the index lists, so the set's index and its targets agree as
//! the coordinator requires (section 3.4). A `hostkeeper-archive` manifest carries nothing but
//! `min_hostkeeper`: the hook, units, files and params are in the attested `manifest.toml` inside
//! the archive, and its `min_hostkeeper` is the one copied, so the two never differ.
//!
//! The `channel` location comes first: it is the store a daemon has when it has no
//! `stores.toml` (section 7.2), the one every archive is fetched from today. The `github`
//! location is the release asset itself, for a host whose `stores.toml` defines that store. A
//! daemon skips a store it does not define (section 7.4).
//!
//! **The native entry** (docs/design/packages-native.md section 10.6, item A21). hello's
//! (stage 7) and mdm's (stage 8) lists hold the native entry of the platform before the archive
//! one, so a host whose daemon announces the native installer moves to it and any other host
//! keeps its archive. The native packages are the release's assets ([`Package::native_name`])
//! that the release publishes to the project's repository: a `.deb` to the suite `testing`, a
//! `.pkg` to the installers' `testing` directory ([`crate::channels::spec`]).
//!
//! ```text
//! Linux   installer apt, repository komira-testing (the host's entry of the testing suite),
//!         package_version the release's, digest the .deb's SHA-256 (the pin); no location,
//!         no size, no attestation
//! macOS   installer macos-pkg, location komira-testing hostkeeper/<release>/hello-<release>-
//!         macos-arm64.pkg (the Mac's store of the testing directory, rooted at pkg/),
//!         digest and bytes the .pkg's; no attestation (the key and the pin bind the bytes,
//!         packages-native.md section 10.3)
//! manifest the archive's own manifest.toml as an entry's manifest: its hook without the
//!         command (the package's hook is its program at the fixed path), its run units, health
//!         URL, params and min_hostkeeper; `package` the package's name (`hello`, `mdm`, or the
//!         .pkg's identifier); the installer's bounds ([`INSTALLER_BOUNDS`])
//! ```
//!
//! Each native package must be a regular file whose bundle lists its SHA-256, and a `.deb`'s
//! `control` must name the service, the version and the architecture. hostkeeper's own list
//! gains its native entry with item A23, and hello's failing build has none: its version is not
//! the `X.Y.Z` the shared repository takes, so it is not published there.
//!
//! Everything is checked before anything is written, and a refusal lists every problem found:
//! the index as `release channel-stage` checks it (canonical, of the service at the version);
//! that it lists every platform the package is released for; each archive a regular file with
//! the index's size and SHA-256; each bundle a regular file of at most 1 MiB listing that
//! SHA-256; each archive holding a `manifest.toml` of the service, version and platform with a
//! `min_hostkeeper`; each native package as above. Then the entries pass the coordinator's
//! checks of a set's targets and the daemon's of each entry's manifest for its installer (crate
//! `hostkeeper-release`). The answer is one line, `targets=<JSON>`, the targets as proto3
//! canonical JSON keyed by platform, appended to `--output` when given (on Actions,
//! `GITHUB_OUTPUT`).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use hostkeeper_config::index::{PackageEntry, attestation_path, package_path};
use hostkeeper_config::manifest::{HostkeeperVersion, MAX_MANIFEST_BYTES};
use hostkeeper_config::names::{ServiceName, Version as ServiceVersion};
use hostkeeper_config::platform::Platform as IndexPlatform;
use hostkeeper_proto::hostkeeper::release::v1::{
    Blob, Hook, InstallerBounds, Location, Manifest, Param, StepBounds, Target, TargetList, param,
};
use hostkeeper_release::{Installer, manifest, target};
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use serde_json::{Map, Value};

use super::channel_stage::{
    MAX_BUNDLE_BYTES, Measure, bundle_problem, file_name, load_index, measure_file, read_capped,
    regular_file,
};
use super::contract::{Package, Platform, Repo, Version, bundle_of};
use super::native::pkg::IDENTIFIER as HELLO_PKG_IDENTIFIER;
use super::{Error, answer};
use crate::channels::apt::{control, deb};
use crate::channels::spec;

/// The store of the release channel: the one a daemon without `stores.toml` has.
pub(crate) const CHANNEL_STORE: &str = "channel";
/// The store of GitHub's release assets, as a host's `stores.toml` names it.
pub(crate) const GITHUB_STORE: &str = "github";
/// The name of the archive's manifest, at the top of the archive.
const MANIFEST_ENTRY: &str = "manifest.toml";

/// The channel a native entry names: on Linux the host's repository entry of the suite
/// `testing` (packages-repositories.md section 3.1), on a Mac its store of the installers'
/// `testing` directory. A release is set from `testing`; its promotion to `stable` copies the
/// same bytes, so the pin is the same from either.
pub(crate) const TESTING: &str = "komira-testing";

/// The bounds of a native installer's own steps, in seconds: the fetch and the checks of
/// `stage` (mdm's `.deb` carries its pinned servers), the package manager's install of
/// `activate`, and the purge of `remove`.
pub(crate) const INSTALLER_BOUNDS: InstallerBounds = InstallerBounds {
    stage_secs: 600,
    activate_secs: 300,
    remove_secs: 300,
};

/// The keys of the archive's manifest a native entry's manifest is made from, and the ones it
/// checks or drops: `schema`, `name`, `version` and `platform` were checked against the release,
/// and `files`, the paths an archive places, has no native counterpart (the package installs its
/// own files), so an archive that lists one is refused.
const MANIFEST_KEYS: [&str; 9] = [
    "schema",
    "name",
    "version",
    "platform",
    "min_hostkeeper",
    "hook",
    "units",
    "health",
    "params",
];

/// What `release targets` reads.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    /// The package whose entries are written.
    pub(crate) package: Package,
    /// The release, `X.Y.Z`: its tag `v<X.Y.Z>` holds the assets.
    pub(crate) version: Version,
    /// The directory of the release's assets.
    pub(crate) dir: PathBuf,
    /// The repository whose release it is, `owner/name`.
    pub(crate) repo: Repo,
    /// A file the `targets=` line is appended to.
    pub(crate) output: Option<PathBuf>,
}

/// The descriptor of `TargetList`, by which the entries are written as JSON.
static TARGET_LIST: LazyLock<Option<MessageDescriptor>> = LazyLock::new(|| {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .ok()?
        .get_message_by_name("hostkeeper.release.v1.TargetList")
});

/// Writes the targets of `request`; the line to print.
pub(crate) fn targets(request: &Request) -> Result<Vec<String>, Error> {
    let package = request.package;
    let service = ServiceName::new(package.service().name())
        .map_err(|e| Error::Failed(format!("the service: {e}")))?;
    let version = ServiceVersion::new(&package.version_text(&request.version))
        .map_err(|e| Error::Failed(format!("the version: {e}")))?;
    let index_path = request.dir.join(package.index_name());
    let (_, index) = load_index(&index_path, &service, &version)?;
    let listed: Vec<&str> = index
        .packages()
        .iter()
        .map(|p| p.platform.as_str())
        .collect();
    let mut released: Vec<&str> = package.platforms().iter().map(|p| p.name()).collect();
    // The index lists its packages in the order of the platforms' spellings.
    released.sort_unstable();
    if listed != released {
        return Err(Error::Failed(format!(
            "{}: lists {}, not every platform the package is released for: {}",
            index_path.display(),
            listed.join(", "),
            released.join(", ")
        )));
    }
    let tag = format!("{}/releases/download/v{}", request.repo, request.version);
    let mut problems = Vec::new();
    let mut lists = Vec::new();
    for entry in index.packages() {
        let (target, archive) = entry_of(request, &service, &version, &tag, entry, &mut problems)?;
        let mut entries = Vec::new();
        // hello's and mdm's lists hold their native entry first (stages 7 and 8); hostkeeper's
        // gains its own with item A23, and hello's failing build has none.
        if let Some(name) = package.native_name(&request.version, release_platform(entry.platform))
        {
            let native = native_of(
                request,
                &service,
                entry.platform,
                &name,
                archive.as_ref(),
                &mut problems,
            )?;
            entries.extend(native);
        }
        entries.push((Installer::HostkeeperArchive, target));
        lists.push((entry.platform, entries));
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    checked(&service, &lists)?;
    let json = json_of(&named(&lists))?;
    answer(request.output.as_deref(), vec![format!("targets={json}")])
}

/// The entry of the archive `entry` names, and the archive's floor and manifest when it was read;
/// every problem goes to `problems`.
fn entry_of(
    request: &Request,
    service: &ServiceName,
    version: &ServiceVersion,
    tag: &str,
    entry: &PackageEntry,
    problems: &mut Vec<String>,
) -> Result<(Target, Option<(String, toml::Table)>), Error> {
    let dir = &request.dir;
    let rel = package_path(service, version, entry.platform);
    let name = file_name(&rel);
    let listed = Measure {
        bytes: entry.bytes,
        sha256: entry.sha256.as_str().to_owned(),
    };
    let mut floor = String::new();
    let mut read = None;
    if regular_file(dir, name, "missing, but the index lists it", problems)? {
        let got = measure_file(&dir.join(name))?;
        if got != listed {
            problems.push(format!(
                "{name}: package_digest_mismatch: {} bytes with SHA-256 {}, but the index lists \
                 {} bytes with SHA-256 {}",
                got.bytes, got.sha256, listed.bytes, listed.sha256
            ));
        } else {
            // Read only from the bytes the entry names.
            match archive_floor(&dir.join(name), service, version, entry.platform) {
                Ok((found, table)) => {
                    floor = found.to_string();
                    read = Some((floor.clone(), table));
                }
                Err(why) => problems.push(format!("{name}: {why}")),
            }
        }
    }
    let bundle_rel = attestation_path(service, version, entry.platform);
    let bundle_name = file_name(&bundle_rel);
    let missing = "missing: every archive entry carries its attestation bundle";
    let bundle = if regular_file(dir, bundle_name, missing, problems)? {
        let bytes = read_capped(&dir.join(bundle_name), MAX_BUNDLE_BYTES)?;
        if let Some(why) = bundle_problem(&bytes, name, &listed.sha256) {
            problems.push(format!("{bundle_name}: {why}"));
        }
        Measure::of(&bytes)
    } else {
        // Never sent: the missing bundle refuses the release before any entry is written.
        Measure::of(b"")
    };
    let target = Target {
        installer: Installer::HostkeeperArchive.as_str().to_owned(),
        locations: locations(&rel, tag, name),
        digest: listed.sha256,
        bytes: listed.bytes,
        attestation: Some(Blob {
            digest: bundle.sha256,
            bytes: bundle.bytes,
            locations: locations(&bundle_rel, tag, bundle_name),
        }),
        manifest: Some(Manifest {
            min_hostkeeper: floor,
            ..Manifest::default()
        }),
        // An archive is an artifact entry: no repository, no package version.
        repository: None,
        package_version: None,
    };
    Ok((target, read))
}

/// The native entry of `request`'s package for `platform` with its installer, from its native
/// package `name` in the release's directory and the archive's floor and manifest `archive`
/// (none when the archive could not be read, which is a problem already); every problem goes to
/// `problems`.
fn native_of(
    request: &Request,
    service: &ServiceName,
    platform: IndexPlatform,
    name: &str,
    archive: Option<&(String, toml::Table)>,
    problems: &mut Vec<String>,
) -> Result<Option<(Installer, Target)>, Error> {
    let platform = release_platform(platform);
    let package = request.package;
    let dir = &request.dir;
    let missing = "missing: the release ships the native package of every platform it lists";
    let measured = if regular_file(dir, name, missing, problems)? {
        Some(measure_file(&dir.join(name))?)
    } else {
        None
    };
    let bundle_name = bundle_of(name);
    let missing = "missing: every native package carries its attestation bundle";
    if regular_file(dir, &bundle_name, missing, problems)?
        && let Some(found) = &measured
    {
        let bytes = read_capped(&dir.join(&bundle_name), MAX_BUNDLE_BYTES)?;
        if let Some(why) = bundle_problem(&bytes, name, &found.sha256) {
            problems.push(format!("{bundle_name}: {why}"));
        }
    }
    let version = package.version_text(&request.version);
    let linux = platform.linux_arches();
    if let (Some((arch, _)), Some(_)) = (linux, &measured) {
        let bytes = std::fs::read(dir.join(name))
            .map_err(Error::io(format!("read {}", dir.join(name).display())))?;
        problems.extend(
            control_problems(&bytes, service.as_str(), &version, arch)
                .into_iter()
                .map(|why| format!("{name}: {why}")),
        );
    }
    let (installer, package_name) = match linux {
        Some(_) => (Installer::Apt, service.as_str()),
        None => (Installer::MacosPkg, HELLO_PKG_IDENTIFIER),
    };
    let manifest = match archive.map(|(floor, table)| native_manifest(table, package_name, floor)) {
        Some(Ok(manifest)) => manifest,
        Some(Err(why)) => {
            let archive = package.package_name(&request.version, platform);
            problems.push(format!("{archive}: {MANIFEST_ENTRY}: {why}"));
            return Ok(None);
        }
        None => return Ok(None),
    };
    let Some(measured) = measured else {
        return Ok(None);
    };
    Ok(Some(match installer {
        Installer::Apt => (
            installer,
            Target {
                installer: installer.as_str().to_owned(),
                locations: Vec::new(),
                digest: measured.sha256,
                // A repository entry is pinned by its digest alone: no size, no location.
                bytes: 0,
                attestation: None,
                manifest: Some(manifest),
                repository: Some(TESTING.to_owned()),
                package_version: Some(version),
            },
        ),
        _ => (
            installer,
            Target {
                installer: installer.as_str().to_owned(),
                locations: vec![Location {
                    store: TESTING.to_owned(),
                    locator: spec::HOSTKEEPER.pkg_locator(&request.version.to_string(), name),
                }],
                digest: measured.sha256,
                bytes: measured.bytes,
                attestation: None,
                manifest: Some(manifest),
                repository: None,
                package_version: None,
            },
        ),
    }))
}

/// The release's platform of the index's `platform`.
fn release_platform(platform: IndexPlatform) -> Platform {
    match platform {
        IndexPlatform::LinuxX86_64 => Platform::LinuxX86_64,
        IndexPlatform::LinuxArm64 => Platform::LinuxArm64,
        IndexPlatform::MacosArm64 => Platform::MacosArm64,
    }
}

/// What is wrong with the `.deb` `bytes` as the package of `service` at `version` for `arch`:
/// its `control` read back out of it must name each, since the host's package manager installs
/// it by those fields and the daemon holds `package_version` to its `Version`.
fn control_problems(bytes: &[u8], service: &str, version: &str, arch: &str) -> Vec<String> {
    let fields = match deb::control_of(bytes).and_then(|text| control::parse(&text)) {
        Ok(fields) => fields,
        Err(e) => return vec![e.to_string()],
    };
    [
        ("Package", service),
        ("Version", version),
        ("Architecture", arch),
    ]
    .into_iter()
    .filter_map(|(field, want)| match control::get(&fields, field) {
        Some(found) if found == want => None,
        Some(found) => Some(format!("its control names the {field} {found}, not {want}")),
        None => Some(format!("its control has no {field}")),
    })
    .collect()
}

/// The manifest of a native entry from the archive's manifest `table` and its floor `floor` (as
/// [`archive_floor`] read them), naming the package `package`; or why it cannot be one.
fn native_manifest(table: &toml::Table, package: &str, floor: &str) -> Result<Manifest, String> {
    if let Some(key) = table.keys().find(|k| !MANIFEST_KEYS.contains(&k.as_str())) {
        return Err(format!(
            "{key} has no counterpart in a native entry, whose package installs its own files"
        ));
    }
    let hook = sub_table(table, "hook")?.ok_or("there is no [hook]")?;
    let timeouts = sub_table(hook, "timeout_secs")?.ok_or("there is no [hook.timeout_secs]")?;
    known(
        hook,
        "hook",
        &[
            "command",
            "boot",
            "verify_deadline_secs",
            "promotion_ceiling_secs",
            "cleanup_retries",
            "timeout_secs",
        ],
    )?;
    known(
        timeouts,
        "hook.timeout_secs",
        &[
            "prepare",
            "cutover",
            "verify",
            "rollback",
            "cleanup",
            "uninstall",
        ],
    )?;
    let bound = |key: &str| number(timeouts, "hook.timeout_secs", key);
    let units = sub_table(table, "units")?;
    if let Some(units) = units {
        known(units, "units", &["run"])?;
    }
    let run = match units.and_then(|u| u.get("run")) {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|i| i.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or("run of [units] is not a list of strings")?,
    };
    let health = sub_table(table, "health")?;
    if let Some(health) = health {
        known(health, "health", &["url"])?;
    }
    let health_url = health
        .and_then(|h| h.get("url"))
        .map(|u| {
            u.as_str()
                .map(str::to_owned)
                .ok_or("health.url is not a string")
        })
        .transpose()?;
    let mut params = BTreeMap::new();
    for (key, value) in sub_table(table, "params")?.into_iter().flatten() {
        let value = match value {
            toml::Value::String(text) => param::Value::Text(text.clone()),
            toml::Value::Integer(n) => param::Value::Integer(*n),
            toml::Value::Float(x) => param::Value::Float(*x),
            toml::Value::Boolean(b) => param::Value::Flag(*b),
            _ => {
                return Err(format!(
                    "params.{key} is not a string, an integer, a float or a boolean"
                ));
            }
        };
        params.insert(key.clone(), Param { value: Some(value) });
    }
    Ok(Manifest {
        package: Some(package.to_owned()),
        hook: Some(Hook {
            // The package's hook is its program at the fixed path: an entry names no command.
            command: Vec::new(),
            boot: optional_string(hook, "hook", "boot")?,
            timeout_secs: Some(StepBounds {
                prepare: optional_number(timeouts, "hook.timeout_secs", "prepare")?.unwrap_or(0),
                cutover: bound("cutover")?,
                verify: bound("verify")?,
                rollback: bound("rollback")?,
                cleanup: bound("cleanup")?,
                uninstall: optional_number(timeouts, "hook.timeout_secs", "uninstall")?,
            }),
            verify_deadline_secs: number(hook, "hook", "verify_deadline_secs")?,
            promotion_ceiling_secs: optional_number(hook, "hook", "promotion_ceiling_secs")?,
            cleanup_retries: optional_number(hook, "hook", "cleanup_retries")?
                .map(u32::try_from)
                .transpose()
                .map_err(|_| "hook.cleanup_retries is larger than 4294967295")?,
        }),
        run,
        health_url,
        place: Vec::new(),
        params: params.into_iter().collect(),
        min_hostkeeper: floor.to_owned(),
        containers: Vec::new(),
        installer_bounds: Some(INSTALLER_BOUNDS),
    })
}

/// The table `key` of `table`, if there is one.
fn sub_table<'a>(table: &'a toml::Table, key: &str) -> Result<Option<&'a toml::Table>, String> {
    table
        .get(key)
        .map(|v| v.as_table().ok_or_else(|| format!("{key} is not a table")))
        .transpose()
}

/// Refuses a key of the table `name` that is not one of `keys`: a field the native entry would
/// silently drop.
fn known(table: &toml::Table, name: &str, keys: &[&str]) -> Result<(), String> {
    match table.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(key) => Err(format!("{name}.{key} is not a key a native entry carries")),
        None => Ok(()),
    }
}

/// The number `key` of the table `name`, which must be there.
fn number(table: &toml::Table, name: &str, key: &str) -> Result<u64, String> {
    optional_number(table, name, key)?.ok_or_else(|| format!("{name}.{key} is missing"))
}

/// The number `key` of the table `name`, if it is there.
fn optional_number(table: &toml::Table, name: &str, key: &str) -> Result<Option<u64>, String> {
    table
        .get(key)
        .map(|v| {
            v.as_integer()
                .and_then(|n| u64::try_from(n).ok())
                .ok_or_else(|| format!("{name}.{key} is not a whole number of 0 or more"))
        })
        .transpose()
}

/// The string `key` of the table `name`, if it is there.
fn optional_string(table: &toml::Table, name: &str, key: &str) -> Result<Option<String>, String> {
    table
        .get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name}.{key} is not a string"))
        })
        .transpose()
}

/// The two locations of a file: its path in the release channel, then its release asset.
fn locations(channel_path: &str, tag: &str, name: &str) -> Vec<Location> {
    vec![
        Location {
            store: CHANNEL_STORE.to_owned(),
            locator: channel_path.to_owned(),
        },
        Location {
            store: GITHUB_STORE.to_owned(),
            locator: format!("{tag}/{name}"),
        },
    ]
}

/// The `min_hostkeeper` of the archive at `path` and its `manifest.toml`, which must name the
/// service, the version and the platform; or why it has none.
fn archive_floor(
    path: &Path,
    service: &ServiceName,
    version: &ServiceVersion,
    platform: IndexPlatform,
) -> Result<(HostkeeperVersion, toml::Table), String> {
    let text = archive_manifest(path)?;
    let table: toml::Table =
        toml::from_str(&text).map_err(|e| format!("{MANIFEST_ENTRY} is not TOML: {e}"))?;
    let string = |key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("{MANIFEST_ENTRY} has no string {key}"))
    };
    for (key, expected) in [
        ("name", service.as_str()),
        ("version", version.as_str()),
        ("platform", platform.as_str()),
    ] {
        let found = string(key)?;
        if found != expected {
            return Err(format!(
                "{MANIFEST_ENTRY} names the {key} {found}, not {expected}"
            ));
        }
    }
    let floor = string("min_hostkeeper")?;
    let floor = HostkeeperVersion::parse(floor)
        .map_err(|e| format!("{MANIFEST_ENTRY}: min_hostkeeper {e}"))?;
    Ok((floor, table))
}

/// The text of the top-level `manifest.toml` of the zstd tar at `path`.
fn archive_manifest(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let stream = ruzstd::decoding::StreamingDecoder::new(BufReader::new(file))
        .map_err(|e| format!("not a zstd archive: {e}"))?;
    let mut archive = tar::Archive::new(stream);
    let not_tar = |e: std::io::Error| format!("not a tar archive: {e}");
    for entry in archive.entries().map_err(not_tar)? {
        let entry = entry.map_err(not_tar)?;
        if entry.path_bytes().as_ref() != MANIFEST_ENTRY.as_bytes() {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(format!("{MANIFEST_ENTRY} is not a regular file"));
        }
        let mut bytes = Vec::new();
        entry
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(not_tar)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(format!(
                "{MANIFEST_ENTRY} is longer than {MAX_MANIFEST_BYTES} bytes"
            ));
        }
        return String::from_utf8(bytes).map_err(|_| format!("{MANIFEST_ENTRY} is not UTF-8"));
    }
    Err(format!("no {MANIFEST_ENTRY} at the top of the archive"))
}

/// The platforms' lists of entries, each with its installer, in order.
type Lists = [(IndexPlatform, Vec<(Installer, Target)>)];

/// The platforms' lists, keyed by the platforms' spellings.
fn named(lists: &Lists) -> BTreeMap<String, TargetList> {
    lists
        .iter()
        .map(|(platform, entries)| {
            let list = TargetList {
                entries: entries.iter().map(|(_, e)| e.clone()).collect(),
            };
            (platform.as_str().to_owned(), list)
        })
        .collect()
}

/// Refuses the entries unless their lists pass the coordinator's checks of a set's targets for
/// `service`, and each manifest the daemon's for its entry's installer.
fn checked(service: &ServiceName, lists: &Lists) -> Result<(), Error> {
    target::check(service, &named(lists))
        .map_err(|e| Error::Failed(format!("target_invalid: {e}")))?;
    for (platform, entries) in lists {
        for (installer, entry) in entries {
            let installer = *installer;
            let found = entry.manifest.clone().unwrap_or_default();
            manifest::check_for(service, *platform, installer, &found).map_err(|e| {
                Error::Failed(format!("manifest_invalid: {platform}: {installer}: {e}"))
            })?;
        }
    }
    Ok(())
}

/// `lists` as one line of proto3 canonical JSON, an object keyed by platform.
fn json_of(lists: &BTreeMap<String, TargetList>) -> Result<String, Error> {
    let unwritable = || Error::Failed("the targets do not encode as JSON".to_owned());
    let descriptor = TARGET_LIST.as_ref().ok_or_else(unwritable)?;
    let mut out = Map::new();
    for (platform, list) in lists {
        let mut message = DynamicMessage::new(descriptor.clone());
        message.transcode_from(list).map_err(|_| unwritable())?;
        let value = serde_json::to_value(&message).map_err(|_| unwritable())?;
        out.insert(platform.clone(), value);
    }
    Ok(Value::Object(out).to_string())
}

#[cfg(test)]
mod tests;
