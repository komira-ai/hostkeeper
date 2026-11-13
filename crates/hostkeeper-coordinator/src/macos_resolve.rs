//! Where a Mac finds the `.pkg` a release names by version (docs/design/packages-repositories.md
//! section 4.7): a `macos-pkg` repository entry, `{installer macos-pkg, repository
//! "komira-testing", packageVersion, digest, bytes, manifest}`, is turned into the artifact entry
//! daemons install, with the implicit `coordinator` store as its location and a locator that the
//! coordinator serves ([`crate::repository_serve`]) from the repository's origin.
//!
//! The resolution is pure: a function of the entry, the service, the platform and what the
//! registry holds ([`Source`], one per channel of a repository whose descriptor has a `macos-pkg`
//! format). It reads no network and no file. The daemon, its `stores.toml` and the MDM change
//! nothing: it fetches from the coordinator, checks the size and SHA-256 the release names (the
//! pin) and the package's signature against its signers, as for an entry with explicit locations.
//! The repository is trusted for where the file is, never for its bytes.
//!
//! An entry that does not resolve (a channel no repository holds, a repository with no macOS
//! format, a platform it does not build for, a private repository) is left out of the release the
//! picks read, so the host is picked no entry (`no_build_for_host`); the release the coordinator
//! stored is as CI sent it.

use std::collections::BTreeMap;
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::release::v1::{Location, Target, TargetList};
use hostkeeper_release::descriptor;
use hostkeeper_release::installer::Installer;
use hostkeeper_release::registry;

use crate::picks::COORDINATOR_STORE;
use crate::releases::Release;
use crate::repositories::Registry;

/// The directory of the macOS format in a locator the coordinator serves: the segment after the
/// repository's name, `repositories/<name>/pkg/<dir>/<version>/<file>`.
pub(crate) const PKG: &str = "pkg";

/// What the coordinator knows of where a channel's macOS installers are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    /// The repository's name.
    pub(crate) repository: String,
    /// The platforms the installers are built for.
    pub(crate) platforms: Vec<String>,
    /// The package names whose installers are in a directory of another name.
    pub(crate) packages: BTreeMap<String, String>,
}

impl Source {
    /// The directory of `package`'s installers.
    pub(crate) fn dir<'a>(&'a self, package: &'a str) -> &'a str {
        self.packages.get(package).map_or(package, String::as_str)
    }
}

/// The macOS source of each channel (`<name>-<suite>`) of the public repositories of `registry`
/// whose descriptor has a `macos-pkg` format.
pub(crate) fn sources_of(registry: &Registry) -> BTreeMap<String, Source> {
    let mut sources = BTreeMap::new();
    for kept in registry.listed() {
        let repository = &kept.repository;
        if registry::is_private(repository) {
            continue;
        }
        let Some(mac) = descriptor::check(&repository.descriptor)
            .ok()
            .and_then(|parsed| parsed.macos_pkg)
        else {
            continue;
        };
        for channel in registry::channels(repository) {
            sources.insert(
                channel,
                Source {
                    repository: repository.name.clone(),
                    platforms: mac.platforms.clone(),
                    packages: mac.packages.clone(),
                },
            );
        }
    }
    sources
}

/// Whether `entry` is the named-version form of a Mac's entry: installer `macos-pkg` with a
/// `repository`.
pub(crate) fn is_named(entry: &Target) -> bool {
    entry.installer == Installer::MacosPkg.as_str() && entry.repository.is_some()
}

/// Whether `release` has a named-version entry of a Mac in any platform's list.
pub(crate) fn has_named(release: &Release) -> bool {
    release
        .targets()
        .values()
        .flat_map(|list| &list.entries)
        .any(is_named)
}

/// The artifact entry `entry` of `service` resolves to for `platform`: the same digest, size and
/// manifest, no attestation, one location in the coordinator's store. `None` when the entry's
/// channel has no source, the source does not build for `platform`, or the entry names no version.
pub(crate) fn resolve(
    entry: &Target,
    service: &str,
    platform: &str,
    sources: &BTreeMap<String, Source>,
) -> Option<Target> {
    let source = sources.get(entry.repository.as_deref()?)?;
    if !source.platforms.iter().any(|built| built == platform) {
        return None;
    }
    let version = entry.package_version.as_deref()?;
    let file = format!("{service}-{version}-{platform}.pkg");
    let locator = format!(
        "repositories/{}/{PKG}/{}/{version}/{file}",
        source.repository,
        source.dir(service)
    );
    Some(Target {
        repository: None,
        package_version: None,
        locations: vec![Location {
            store: COORDINATOR_STORE.to_owned(),
            locator,
        }],
        ..entry.clone()
    })
}

/// `release` of `service` as the picks read it: each named-version entry of a platform's list
/// resolved by [`resolve`], or left out when it does not resolve. A release with no such entry is
/// returned as it is (the same `Arc`).
pub(crate) fn resolved(
    service: &str,
    release: &Arc<Release>,
    sources: &BTreeMap<String, Source>,
) -> Arc<Release> {
    if !has_named(release) {
        return Arc::clone(release);
    }
    let targets = release
        .targets()
        .iter()
        .map(|(platform, list)| {
            let entries = list
                .entries
                .iter()
                .filter_map(|entry| {
                    if is_named(entry) {
                        resolve(entry, service, platform, sources)
                    } else {
                        Some(entry.clone())
                    }
                })
                .collect();
            (platform.clone(), TargetList { entries })
        })
        .collect();
    Arc::new(Release::new(targets, release.index_digest().cloned()))
}
