//! `release targets`: the `targets` of the set that deploys one package of a release
//! (docs/design/packages.md, section 3.7; docs/design/packages-native.md, section 10.5). For each
//! platform the package is released for ([`Package::platforms`]), one native entry and nothing
//! else: a release is set with native entries alone, so a Linux host installs it from the
//! project's package repository and a Mac from the release's signed installer (stage 9 of
//! packages-native.md section 10.6).
//!
//! **The `macos-arm64` list is the signed `.pkg`'s `macos-pkg` entry alone.** A Mac whose daemon
//! announces `macos-pkg` (its signers file `/etc/hostkeeper/macos-pkg.toml` loaded, and
//! `/usr/sbin/installer` and `/usr/sbin/pkgutil` present) takes it; a Mac that does not announce
//! it is ordered nothing (the coordinator's pick gives it `installer_dropped` or
//! `no_build_for_host`). mdm has no Mac build, and hello's failing build no `.pkg`, so neither
//! has a macOS list.
//!
//! The native packages are the release's `.deb`s and `.pkg`s ([`Package::native_name`]), which
//! the release publishes to the project's repository, suite `testing`, and to the installers'
//! `testing` directory ([`crate::channels::spec`]). hostkeeper's are the daemon's `.deb` and its
//! installer, the packages that bootstrap a host (section 10.4).
//!
//! **Hello's failing build is listed only when asked for** (`--fail-ready`): its `.deb`
//! `hello_<v>-fail-ready_<arch>.deb` is in the suite `testing-fail`, which lists it alone, under
//! the Debian version `<v>~fail.ready` ([`Package::deb_version`]), so a normal set never takes
//! it, and a call without `--fail-ready` reads none of its files.
//!
//! ```text
//! entry    installer apt, repository komira-testing (the host's entry of the testing suite;
//!          the failing build's: komira-testing-fail, [`Package::repository_entry`]),
//!          package_version the .deb's Debian version ([`Package::deb_version`]: the release's,
//!          the failing build's `<v>~fail.ready`), digest the .deb's SHA-256 (the pin); no
//!          location, no size, no attestation
//! manifest compiled in ([`manifest`]): the package's hook with the command
//!          `<path the .deb installs its program at> update-hook` (hello's /usr/lib/hello/hello,
//!          its failing build's too; mdm's /usr/lib/mdm/mdm; hostkeeper's package has no
//!          command, and its daemon runs built-in steps) and its bounds, its run units, health
//!          URL, params and min_hostkeeper; `package` the package's name (`hostkeeper`, `hello`,
//!          `mdm`; the failing build's is hello's); the installer's bounds ([`INSTALLER_BOUNDS`])
//! macOS    installer macos-pkg, location komira-testing hostkeeper/<release>/<service>-<release>-
//!          macos-arm64.pkg (the Mac's store of the testing directory, rooted at pkg/), digest
//!          and bytes the .pkg's; no attestation (the key and the pin bind the bytes,
//!          packages-native.md section 10.3), no repository, no package version; the manifest
//!          the Linux one with the .pkg's identifier, launchd labels and program
//!          ([`manifest::of_pkg`])
//! ```
//!
//! Each native package must be a regular file whose bundle lists its SHA-256, and a `.deb`'s
//! `control` must name the service, the version and the architecture; a `.pkg` has no control to
//! read.
//!
//! Everything is checked before anything is written, and a refusal lists every problem found:
//! each native package as above. Then the entries pass the coordinator's checks of a set's
//! targets and the daemon's of each entry's manifest for its installer (crate
//! `hostkeeper-release`). The answer is one line, `targets=<JSON>`, the targets as proto3
//! canonical JSON keyed by platform, appended to `--output` when given (on Actions,
//! `GITHUB_OUTPUT`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::Platform as IndexPlatform;
use hostkeeper_proto::hostkeeper::release::v1::{InstallerBounds, Location, Target, TargetList};
use hostkeeper_release::{Installer, manifest as release_manifest, target};
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor};
use serde_json::{Map, Value};

use self::files::{MAX_BUNDLE_BYTES, bundle_problem, read_capped, regular_file};
use super::contract::{Package, Platform, TESTING_ENTRY, Version, bundle_of};
use super::{Error, answer, file_sha256};
use crate::channels::apt::{control, deb};
use crate::channels::spec;

mod files;
mod manifest;

/// The bounds of a native installer's own steps, in seconds: the fetch and the checks of
/// `stage` (mdm's `.deb` carries its pinned servers), the package manager's install of
/// `activate`, and the purge of `remove`.
pub(crate) const INSTALLER_BOUNDS: InstallerBounds = InstallerBounds {
    stage_secs: 600,
    activate_secs: 300,
    remove_secs: 300,
};

/// What `release targets` reads.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    /// The package whose entries are written.
    pub(crate) package: Package,
    /// The release, `X.Y.Z`: its tag `v<X.Y.Z>` holds the assets.
    pub(crate) version: Version,
    /// The directory of the release's assets.
    pub(crate) dir: PathBuf,
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
    // Every list is its platform's native entry alone: a Linux platform's `.deb`, the Mac's
    // signed `.pkg` (the failing build has none, so no macOS list).
    let mut natives: Vec<_> = package
        .platforms()
        .iter()
        .filter_map(|&platform| {
            let (arch, index) = match platform {
                Platform::LinuxX86_64 => (Some("amd64"), IndexPlatform::LinuxX86_64),
                Platform::LinuxArm64 => (Some("arm64"), IndexPlatform::LinuxArm64),
                Platform::MacosArm64 => (None, IndexPlatform::MacosArm64),
            };
            let name = package.native_name(&request.version, platform)?;
            Some((index, arch, name))
        })
        .collect();
    // The platforms in the order of their spellings.
    natives.sort_by_key(|(platform, _, _)| platform.as_str());
    let mut problems = Vec::new();
    let mut lists = Vec::new();
    for (platform, arch, name) in natives {
        let native = native_of(request, &service, (arch, &name), &mut problems)?;
        lists.push((platform, native.into_iter().collect()));
    }
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    checked(&service, &lists)?;
    let json = json_of(&named(&lists))?;
    answer(request.output.as_deref(), vec![format!("targets={json}")])
}

/// The native entry of `request`'s package with its installer, from its native package `name` in
/// the release's directory (a `.deb` for the Debian architecture `arch`, a Mac's `.pkg` for none)
/// and the manifest compiled in for the package (none when the package is missing, which is a
/// problem already); every problem goes to `problems`.
fn native_of(
    request: &Request,
    service: &ServiceName,
    (arch, name): (Option<&str>, &str),
    problems: &mut Vec<String>,
) -> Result<Option<(Installer, Target)>, Error> {
    let package = request.package;
    let dir = &request.dir;
    let missing = "missing: the release ships the native package of every platform it lists";
    let measured = if regular_file(dir, name, missing, problems)? {
        Some(file_sha256(&dir.join(name))?)
    } else {
        None
    };
    let bundle_name = bundle_of(name);
    let missing = "missing: every native package carries its attestation bundle";
    if regular_file(dir, &bundle_name, missing, problems)?
        && let Some(sha256) = &measured
    {
        let bytes = read_capped(&dir.join(&bundle_name), MAX_BUNDLE_BYTES)?;
        if let Some(why) = bundle_problem(&bytes, name, sha256) {
            problems.push(format!("{bundle_name}: {why}"));
        }
    }
    let version = package.deb_version(&request.version);
    // Only a `.deb` has a control to read; a `.pkg` is held to its bundle and its digest.
    if let (Some(arch), Some(_)) = (arch, &measured) {
        let bytes = std::fs::read(dir.join(name))
            .map_err(Error::io(format!("read {}", dir.join(name).display())))?;
        problems.extend(
            control_problems(&bytes, service.as_str(), &version, arch)
                .into_iter()
                .map(|why| format!("{name}: {why}")),
        );
    }
    let Some(digest) = measured else {
        return Ok(None);
    };
    if arch.is_none() {
        let path = dir.join(name);
        let bytes = std::fs::metadata(&path)
            .map_err(Error::io(format!("read {}", path.display())))?
            .len();
        let installer = Installer::MacosPkg;
        return Ok(Some((
            installer,
            Target {
                installer: installer.as_str().to_owned(),
                locations: vec![Location {
                    store: TESTING_ENTRY.to_owned(),
                    locator: spec::HOSTKEEPER.pkg_locator(&request.version.to_string(), name),
                }],
                digest,
                bytes,
                // The key and the pin bind the bytes: a `macos-pkg` entry carries no attestation.
                attestation: None,
                manifest: Some(manifest::of_pkg(package, &request.version)),
                repository: None,
                package_version: None,
            },
        )));
    }
    let installer = Installer::Apt;
    Ok(Some((
        installer,
        Target {
            installer: installer.as_str().to_owned(),
            locations: Vec::new(),
            digest,
            // A repository entry is pinned by its digest alone: no size, no location.
            bytes: 0,
            attestation: None,
            // A `.deb` is installed by its control's `Package`, the service.
            manifest: Some(manifest::of(package, &request.version)),
            repository: Some(package.repository_entry().to_owned()),
            package_version: Some(version),
        },
    )))
}

/// What is wrong with the `.deb` `bytes` as the package of `service` at the Debian version
/// `version` for `arch`: its `control` read back out of it must name each, since the host's
/// package manager installs it by those fields and the daemon holds `package_version` to its
/// `Version`.
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
            release_manifest::check_for(service, *platform, installer, &found).map_err(|e| {
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
