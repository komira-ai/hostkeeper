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
//! Everything is checked before anything is written, and a refusal lists every problem found:
//! the index as `release channel-stage` checks it (canonical, of the service at the version);
//! that it lists every platform the package is released for; each archive a regular file with
//! the index's size and SHA-256; each bundle a regular file of at most 1 MiB listing that
//! SHA-256; each archive holding a `manifest.toml` of the service, version and platform with a
//! `min_hostkeeper`. Then the entries pass the coordinator's checks of a set's targets and the
//! daemon's of a `hostkeeper-archive` manifest (crate `hostkeeper-release`). The answer is one
//! line, `targets=<JSON>`, the targets as proto3 canonical JSON keyed by platform, appended to
//! `--output` when given (on Actions, `GITHUB_OUTPUT`).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use hostkeeper_config::index::{PackageEntry, attestation_path, package_path};
use hostkeeper_config::manifest::{HostkeeperVersion, MAX_MANIFEST_BYTES};
use hostkeeper_config::names::{ServiceName, Version as ServiceVersion};
use hostkeeper_config::platform::Platform as IndexPlatform;
use hostkeeper_proto::hostkeeper::release::v1::{Blob, Location, Manifest, Target, TargetList};
use hostkeeper_release::{Installer, manifest, target};
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use serde_json::{Map, Value};

use super::channel_stage::{
    MAX_BUNDLE_BYTES, Measure, bundle_problem, file_name, load_index, measure_file, read_capped,
    regular_file,
};
use super::contract::{Package, Repo, Version};
use super::{Error, answer};

/// The store of the release channel: the one a daemon without `stores.toml` has.
pub(crate) const CHANNEL_STORE: &str = "channel";
/// The store of GitHub's release assets, as a host's `stores.toml` names it.
pub(crate) const GITHUB_STORE: &str = "github";
/// The name of the archive's manifest, at the top of the archive.
const MANIFEST_ENTRY: &str = "manifest.toml";

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
        let target = entry_of(request, &service, &version, &tag, entry, &mut problems)?;
        lists.push((entry.platform, target));
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    checked(&service, &lists)?;
    let json = json_of(&named(&lists))?;
    answer(request.output.as_deref(), vec![format!("targets={json}")])
}

/// The entry of the archive `entry` names; every problem goes to `problems`.
fn entry_of(
    request: &Request,
    service: &ServiceName,
    version: &ServiceVersion,
    tag: &str,
    entry: &PackageEntry,
    problems: &mut Vec<String>,
) -> Result<Target, Error> {
    let dir = &request.dir;
    let rel = package_path(service, version, entry.platform);
    let name = file_name(&rel);
    let listed = Measure {
        bytes: entry.bytes,
        sha256: entry.sha256.as_str().to_owned(),
    };
    let mut floor = String::new();
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
                Ok(found) => floor = found.to_string(),
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
    Ok(Target {
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
    })
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

/// The `min_hostkeeper` of the archive at `path`: its `manifest.toml`, which must name the
/// service, the version and the platform; or why it has none.
fn archive_floor(
    path: &Path,
    service: &ServiceName,
    version: &ServiceVersion,
    platform: IndexPlatform,
) -> Result<HostkeeperVersion, String> {
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
    HostkeeperVersion::parse(floor).map_err(|e| format!("{MANIFEST_ENTRY}: min_hostkeeper {e}"))
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

/// The platforms' lists of one entry each, keyed by the platforms' spellings.
fn named(lists: &[(IndexPlatform, Target)]) -> BTreeMap<String, TargetList> {
    lists
        .iter()
        .map(|(platform, target)| {
            let list = TargetList {
                entries: vec![target.clone()],
            };
            (platform.as_str().to_owned(), list)
        })
        .collect()
}

/// Refuses the entries unless their lists pass the coordinator's checks of a set's targets for
/// `service`, and each manifest the daemon's for a `hostkeeper-archive` entry.
fn checked(service: &ServiceName, lists: &[(IndexPlatform, Target)]) -> Result<(), Error> {
    target::check(service, &named(lists))
        .map_err(|e| Error::Failed(format!("target_invalid: {e}")))?;
    for (platform, entry) in lists {
        let found = entry.manifest.clone().unwrap_or_default();
        manifest::check_for(service, *platform, Installer::HostkeeperArchive, &found)
            .map_err(|e| Error::Failed(format!("manifest_invalid: {platform}: {e}")))?;
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
