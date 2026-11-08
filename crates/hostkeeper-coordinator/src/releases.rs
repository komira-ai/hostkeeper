//! The releases a service's versions were set with (docs/design/packages.md, sections 3.4 to
//! 3.6): for each platform, the artifacts in CI's order of preference, one per installer, as a
//! `SetDesired` with `targets` named them, and the digest of the release index set with them.
//!
//! A release is kept beside the registry of digests, never in it: `IndexRecord` and the state
//! file refuse unknown keys, so a new key in either would stop an older coordinator from
//! starting. A version set with targets is written to the state file and the registry as a set
//! with neither a package digest nor an index (the third form): an older coordinator reads it,
//! never reads the releases, and shows the version `no_digest`.
//!
//! - [`checked`] judges a set's targets: the rules of `hostkeeper-release` (`target_invalid`),
//!   then no package digest beside them and an index that agrees with their archive entries
//!   (`index_invalid`).
//! - [`Release`] is what is kept, and [`record`] keeps it under the registry's cap
//!   ([`MAX_REGISTRY`]), dropping the oldest release no mark keeps.
//! - [`encode_file`] and [`parse_file`] are the file `<state-file>.releases`; a release's
//!   JSON ([`Release::to_json`], [`Release::from_json`]) is also the Postgres store's.
//! - [`audit_reason`] is the reason of a set's `set_desired` audit entry.
//!
//! The targets are written as proto3 canonical JSON. They are read without refusing a field this
//! build does not know, and a release read is written back as it was read, so a coordinator rolled
//! back keeps what a newer one wrote.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use hostkeeper_config::index::{PackageEntry, ReleaseIndex};
use hostkeeper_config::names::{ServiceName, Version as IndexVersion};
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::TargetList;
use hostkeeper_release::Installer;
use hostkeeper_wire::version::Version;
use prost::Message as _;
use prost_reflect::{DescriptorPool, DeserializeOptions, DynamicMessage, MessageDescriptor};
use serde_json::{Map, Value, json};

use crate::desired::DesiredError;
use crate::ops_file;
use crate::registry::{Digest, MAX_REGISTRY, oldest_droppable};
use crate::service::SetError;

/// A set's targets, by the platform's spelling.
pub(crate) type Targets = BTreeMap<String, TargetList>;

/// The releases of one service, by version.
pub(crate) type Book = BTreeMap<String, Arc<Release>>;

/// The releases of every service that has one, by service.
pub(crate) type Books = BTreeMap<String, Book>;

/// The release a version was set with.
#[derive(Clone, Debug)]
pub(crate) struct Release {
    targets: Targets,
    index_digest: Option<Digest>,
    /// The targets as they are written: as read, when the release was read.
    json: Value,
}

/// Two releases are one when their targets encode to the same bytes (each list's entries in
/// order, a manifest's params in the order of their names) and they name the same index digest.
impl PartialEq for Release {
    fn eq(&self, other: &Self) -> bool {
        self.index_digest == other.index_digest && encoded(&self.targets) == encoded(&other.targets)
    }
}

impl Eq for Release {}

fn encoded(targets: &Targets) -> Vec<(&str, Vec<u8>)> {
    targets
        .iter()
        .map(|(platform, list)| (platform.as_str(), list.encode_to_vec()))
        .collect()
}

/// The descriptor of `TargetList`, by which the targets are written and read as JSON.
static TARGET_LIST: LazyLock<Option<MessageDescriptor>> = LazyLock::new(|| {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .ok()?
        .get_message_by_name("hostkeeper.release.v1.TargetList")
});

impl Release {
    /// The release of `targets`, set with the index of SHA-256 `index_digest`, if any.
    #[must_use]
    pub(crate) fn new(targets: Targets, index_digest: Option<Digest>) -> Self {
        let json = targets
            .iter()
            .map(|(platform, list)| (platform.clone(), list_json(list)))
            .collect::<Map<_, _>>()
            .into();
        Self {
            targets,
            index_digest,
            json,
        }
    }

    /// The targets, each platform's entries in the order set.
    #[must_use]
    pub(crate) fn targets(&self) -> &Targets {
        &self.targets
    }

    /// The digest of the release index set with the targets.
    #[must_use]
    pub(crate) fn index_digest(&self) -> Option<&Digest> {
        self.index_digest.as_ref()
    }

    /// The release as it is stored: `{"targets": {...}, "release_index_digest": "..."}`, the
    /// digest only when there is one.
    #[must_use]
    pub(crate) fn to_json(&self) -> Value {
        let mut out = json!({ "targets": self.json });
        if let Some(digest) = &self.index_digest {
            out["release_index_digest"] = digest.as_str().into();
        }
        out
    }

    /// The release stored as `value`; a key or a field this build does not know is kept as read.
    ///
    /// # Errors
    /// Why `value` is no release: no `targets` object, a platform's list that does not read, or a
    /// digest that is not 64 lower-case hex characters.
    pub(crate) fn from_json(value: &Value) -> Result<Self, String> {
        let json = value
            .get("targets")
            .filter(|targets| targets.is_object())
            .ok_or("no targets")?;
        let mut targets = Targets::new();
        for (platform, list) in json.as_object().into_iter().flatten() {
            let read =
                list_of(list).ok_or_else(|| format!("the list of {platform} does not read"))?;
            targets.insert(platform.clone(), read);
        }
        let index_digest = match value.get("release_index_digest") {
            None => None,
            Some(digest) => Some(
                digest
                    .as_str()
                    .and_then(|text| Digest::parse(text).ok())
                    .ok_or("a release_index_digest that is no digest")?,
            ),
        };
        Ok(Self {
            targets,
            index_digest,
            json: json.clone(),
        })
    }
}

/// `list` as proto3 canonical JSON.
fn list_json(list: &TargetList) -> Value {
    TARGET_LIST
        .as_ref()
        .and_then(|descriptor| {
            let mut message = DynamicMessage::new(descriptor.clone());
            message.transcode_from(list).ok()?;
            serde_json::to_value(&message).ok()
        })
        .unwrap_or_default()
}

/// The list `value` holds as proto3 JSON, fields this build does not know left out.
fn list_of(value: &Value) -> Option<TargetList> {
    let descriptor = TARGET_LIST.as_ref()?.clone();
    let options = DeserializeOptions::new().deny_unknown_fields(false);
    DynamicMessage::deserialize_with_options(descriptor, value.clone(), &options)
        .ok()?
        .transcode_to()
        .ok()
}

/// The release of a set of `service` at `version` with `targets`, once they pass: the rules of
/// the release ([`SetError::TargetInvalid`]); no package digest beside them
/// (`has_package_digest`, [`DesiredError::IndexInvalid`]); and, when the set carried the release
/// index `index` of SHA-256 `index_digest`, an index that lists exactly the platforms whose list
/// holds a `hostkeeper-archive` entry, each with that entry's size and digest (`index_invalid`).
///
/// # Errors
/// [`SetError`], as above.
pub(crate) fn checked(
    service: &str,
    version: &Version,
    targets: Targets,
    has_package_digest: bool,
    index: Option<(&str, &Digest)>,
) -> Result<Release, SetError> {
    if has_package_digest {
        return Err(DesiredError::IndexInvalid("targets are not set with a package digest").into());
    }
    let name = ServiceName::new(service).map_err(|_| DesiredError::UnknownService)?;
    hostkeeper_release::target::check(&name, &targets).map_err(SetError::TargetInvalid)?;
    let index_digest = match index {
        None => None,
        Some((text, digest)) => {
            let parsed = IndexVersion::new(version.as_str())
                .ok()
                .and_then(|at| ReleaseIndex::parse(text, &name, &at).ok())
                .ok_or(DesiredError::IndexInvalid(
                    "release_index is not the release index of the service at the version",
                ))?;
            if !agrees(parsed.packages(), &targets) {
                return Err(DesiredError::IndexInvalid(
                    "release_index does not list exactly the archive entries of the targets",
                )
                .into());
            }
            Some(digest.clone())
        }
    };
    Ok(Release::new(targets, index_digest))
}

/// Whether `packages` are exactly the `hostkeeper-archive` entries of `targets`: one per platform
/// whose list holds one, with its digest and size, and no other.
fn agrees(packages: &[PackageEntry], targets: &Targets) -> bool {
    let archive = Installer::HostkeeperArchive.as_str();
    let entries: BTreeMap<&str, (&str, u64)> = targets
        .iter()
        .filter_map(|(platform, list)| {
            let entry = list.entries.iter().find(|e| e.installer == archive)?;
            Some((platform.as_str(), (entry.digest.as_str(), entry.bytes)))
        })
        .collect();
    let listed: BTreeMap<&str, (&str, u64)> = packages
        .iter()
        .map(|package| {
            let platform: Platform = package.platform;
            (platform.as_str(), (package.sha256.as_str(), package.bytes))
        })
        .collect();
    listed == entries
}

/// Keeps `release` as the release of `version` in `book`. Past [`MAX_REGISTRY`] releases, the
/// oldest that is not `version` and that `keep` does not keep is dropped, as the registry drops
/// a version ([`crate::registry::Registry::record`]); when `keep` keeps them all, none is.
pub(crate) fn record(
    book: &mut Book,
    version: &Version,
    release: Arc<Release>,
    keep: &dyn Fn(&Version) -> bool,
) {
    book.insert(version.as_str().to_owned(), release);
    while book.len() > MAX_REGISTRY {
        let Some(oldest) = oldest_droppable(book.keys(), version, keep) else {
            return;
        };
        book.remove(oldest.as_str());
    }
}

/// The file of the releases beside the desired-state file at `state`: `<state>.releases`.
#[must_use]
pub(crate) fn releases_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".releases");
    PathBuf::from(path)
}

/// The text of the releases file holding `books`: `{"services": {service: {version: release}}}`.
///
/// # Errors
/// The releases could not be serialised.
pub(crate) fn encode_file(books: &Books) -> serde_json::Result<Vec<u8>> {
    let services: Map<String, Value> = books
        .iter()
        .map(|(service, book)| {
            let versions: Map<String, Value> = book
                .iter()
                .map(|(version, release)| (version.clone(), release.to_json()))
                .collect();
            (service.clone(), versions.into())
        })
        .collect();
    let mut text = serde_json::to_vec_pretty(&json!({ "services": services }))?;
    text.push(b'\n');
    Ok(text)
}

/// The releases in the text of a releases file.
///
/// # Errors
/// [`DesiredError::Malformed`]: not the file's JSON, a version that is not one, or a release that
/// does not read, named by service and version.
pub(crate) fn parse_file(bytes: &[u8]) -> Result<Books, DesiredError> {
    let malformed = |why: String| DesiredError::Malformed(format!("the releases: {why}"));
    let value: Value = serde_json::from_slice(bytes).map_err(|e| malformed(e.to_string()))?;
    let services = value
        .get("services")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("no services".to_owned()))?;
    let mut books = Books::new();
    for (service, versions) in services {
        let versions = versions
            .as_object()
            .ok_or_else(|| malformed(format!("{service} holds no versions")))?;
        let mut book = Book::new();
        for (version, release) in versions {
            Version::parse(version).map_err(|e| malformed(format!("{service} {version}: {e}")))?;
            let release = Release::from_json(release)
                .map_err(|e| malformed(format!("{service} {version}: {e}")))?;
            book.insert(version.clone(), Arc::new(release));
        }
        if !book.is_empty() {
            books.insert(service.clone(), book);
        }
    }
    Ok(books)
}

/// The reason of the `set_desired` audit entry of a set of `version` with `release`, made by CI
/// with the workflow `workflow` when it is CI's: the version, `with targets`, CI's workflow
/// (`by workflow <owner>/<repo>/<path>@<ref>`) and the release index (`and release index
/// <digest>`) when there are, then for each platform in the order of its spelling its entries in
/// order, each as its installer, its artifact's digest, and `manifest` with the SHA-256 of the
/// manifest's encoding.
#[must_use]
pub(crate) fn audit_reason(version: &Version, release: &Release, workflow: Option<&str>) -> String {
    let mut reason = format!("{version} with targets");
    if let Some(workflow) = workflow {
        reason.push_str(&format!(" by workflow {workflow}"));
    }
    if let Some(digest) = &release.index_digest {
        reason.push_str(&format!(" and release index {digest}"));
    }
    let platforms: Vec<String> = release
        .targets
        .iter()
        .map(|(platform, list)| {
            let entries: Vec<String> = list
                .entries
                .iter()
                .map(|entry| {
                    let manifest = entry
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.encode_to_vec())
                        .unwrap_or_default();
                    format!(
                        "{} {} manifest {}",
                        entry.installer,
                        entry.digest,
                        ops_file::digest(&manifest)
                    )
                })
                .collect();
            format!("{platform} {}", entries.join(", "))
        })
        .collect();
    format!("{reason}: {}", platforms.join("; "))
}
