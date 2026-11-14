//! The registry of package repositories the operator keeps on the coordinator
//! (docs/design/packages-repositories.md, section 3): each [`Repository`] with the registry
//! generation it was last put at, and the registry's generation, which grows by one at every
//! `Repositories.Put` and `Repositories.Remove`.
//!
//! The registry is part of the desired state ([`crate::desired::Held`]), kept beside it and
//! never in its file: the state file refuses a key it does not know, so a new key in it would
//! stop an older coordinator from starting. It is written whole, before the state file, when a
//! commit changes it: `<state-file>.repositories` on the file store ([`encode_file`],
//! [`parse_file`]), the tables `repositories` and `repository_generation` (migration `0007`,
//! `may-run`) on Postgres. An older coordinator never reads either.
//!
//! The rules of an entry are the crate `hostkeeper-release`'s ([`registry::check`]), the code
//! the daemon judges a pushed set by. A registry that is read back is judged by them again, so a
//! file or a row that breaks one is refused at start, naming the repository.
//!
//! - No repository is built in: a coordinator with no entry lists none, and every one, the
//!   organisation's `komira` included, is added through `Repositories.Add`.
//! - [`Registry::put`] and [`Registry::remove`] make the registry a change would leave, and
//!   refuse a repository that clashes with another or past [`MAX_REPOSITORIES`].
//! - [`Registry::private_channels`] is the channels the pick treats as private ([`crate::picks`]).
//! - [`Registry::entries_of`] is what a host is sent: the entries its services name, when the
//!   registry holds every one (`repository_push.rs` adds the generation and the digest).
//! - [`put_reason`] and [`remove_reason`] are the reasons of the audit entries
//!   `repository_put` and `repository_remove`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::descriptor;
use hostkeeper_release::registry::{self, MAX_REPOSITORIES};
use serde_json::{Map, Value, json};

use crate::desired::DesiredError;

/// A repository as the registry keeps it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Kept {
    /// The repository, with its keyring.
    pub(crate) repository: Repository,
    /// The registry's generation when it was last put.
    pub(crate) generation: u64,
}

impl Eq for Kept {}

impl Kept {
    /// The record as it is stored: `{"generation": n, "repository": {...}}`, the keyring as
    /// standard base64. `access`, `credential_file`, `base_uri` and `descriptor` (standard base64)
    /// are written only when set, so the record of a public repository put without a descriptor is
    /// the one the release before wrote, byte for byte; the release before reads a record that has
    /// them, naming the keys it knows only.
    #[must_use]
    pub(crate) fn to_json(&self) -> Value {
        let repository = &self.repository;
        let mut record = json!({
            "generation": self.generation,
            "repository": {
                "name": repository.name,
                "kind": repository.kind,
                "uri": repository.uri,
                "suites": repository.suites,
                "components": repository.components,
                "fingerprints": repository.fingerprints,
                "keyring": base64::engine::general_purpose::STANDARD.encode(&repository.keyring),
            },
        });
        for (key, value) in [
            ("access", &repository.access),
            ("credential_file", &repository.credential_file),
            ("base_uri", &repository.base_uri),
        ] {
            if !value.is_empty() {
                record["repository"][key] = json!(value);
            }
        }
        if !repository.descriptor.is_empty() {
            record["repository"]["descriptor"] =
                json!(base64::engine::general_purpose::STANDARD.encode(&repository.descriptor));
        }
        record
    }

    /// The record stored as `value`, judged by [`registry::check`]. `access`, `credential_file`,
    /// `base_uri` and `descriptor` are optional: absent, they are empty (a public repository put
    /// without a descriptor, as every record the release before wrote).
    ///
    /// # Errors
    /// Why `value` is no record: a field missing or of the wrong type, a keyring that is not
    /// base64, or an entry that breaks a rule of the registry.
    pub(crate) fn from_json(value: &Value) -> Result<Self, String> {
        let generation = value
            .get("generation")
            .and_then(Value::as_u64)
            .ok_or("no generation")?;
        let stored = value.get("repository").ok_or("no repository")?;
        let text = |key: &str| {
            stored
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("no {key}"))
        };
        let optional = |key: &str| match stored.get(key) {
            None => Ok(String::new()),
            Some(value) => value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{key} is not text")),
        };
        let texts = |key: &str| -> Result<Vec<String>, String> {
            stored
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| format!("no {key}"))?
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("{key} holds a value that is not text"))
                })
                .collect()
        };
        let repository = Repository {
            name: text("name")?,
            kind: text("kind")?,
            uri: text("uri")?,
            suites: texts("suites")?,
            components: texts("components")?,
            fingerprints: texts("fingerprints")?,
            keyring: base64::engine::general_purpose::STANDARD
                .decode(text("keyring")?)
                .map_err(|_| "a keyring that is not base64".to_owned())?,
            access: optional("access")?,
            credential_file: optional("credential_file")?,
            descriptor: base64::engine::general_purpose::STANDARD
                .decode(optional("descriptor")?)
                .map_err(|_| "a descriptor that is not base64".to_owned())?,
            base_uri: optional("base_uri")?,
        };
        registry::check(&repository).map_err(|why| why.to_string())?;
        Ok(Self {
            repository,
            generation,
        })
    }
}

/// Why a `Put` is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PutError {
    /// A channel of the repository, `<name>-<suite>`, is a channel of another repository.
    #[error("the channel {0:?} is a channel of another repository")]
    Conflict(String),
    /// The registry holds [`MAX_REPOSITORIES`] and the repository is not one of them.
    #[error("the registry holds {MAX_REPOSITORIES} repositories")]
    Capacity,
}

/// The registry: its generation, and the repositories by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Registry {
    /// 0 until the first change; one more at each.
    pub(crate) generation: u64,
    /// The repositories by name.
    pub(crate) entries: BTreeMap<String, Kept>,
}

impl Registry {
    /// The registry of `generation` holding `entries`, judged as a file or a row read back is.
    ///
    /// # Errors
    /// Why it is no registry: past [`MAX_REPOSITORIES`], an entry kept under another name or at
    /// a generation the registry has not reached, or two entries whose channels clash.
    pub(crate) fn new(generation: u64, entries: BTreeMap<String, Kept>) -> Result<Self, String> {
        if entries.len() > MAX_REPOSITORIES {
            return Err(format!("more than {MAX_REPOSITORIES} repositories"));
        }
        let all: Vec<Repository> = entries.values().map(|k| k.repository.clone()).collect();
        for (name, kept) in &entries {
            if *name != kept.repository.name {
                return Err(format!(
                    "{name} holds the repository {}",
                    kept.repository.name
                ));
            }
            if kept.generation > generation {
                return Err(format!(
                    "{name} is at generation {}, past the registry's {generation}",
                    kept.generation
                ));
            }
            if let Some(channel) = registry::conflict(&kept.repository, &all) {
                return Err(format!("{name}: the channel {channel} is another's too"));
            }
        }
        Ok(Self {
            generation,
            entries,
        })
    }

    /// The repository named `name`, if one was put.
    #[must_use]
    pub(crate) fn get(&self, name: &str) -> Option<&Kept> {
        self.entries.get(name)
    }

    /// Every repository the registry holds, by name.
    #[must_use]
    pub(crate) fn listed(&self) -> Vec<&Kept> {
        self.entries.values().collect()
    }

    /// The channels (`<name>-<suite>`) of every private repository of the registry.
    #[must_use]
    pub(crate) fn private_channels(&self) -> BTreeSet<String> {
        self.entries
            .values()
            .filter(|kept| registry::is_private(&kept.repository))
            .flat_map(|kept| registry::channels(&kept.repository))
            .collect()
    }

    /// The registry after a `Put` of `repository`: its generation one more, and the repository
    /// kept at it, replacing the one of its name.
    ///
    /// # Errors
    /// [`PutError::Conflict`], [`PutError::Capacity`] (a new name at [`MAX_REPOSITORIES`]).
    pub(crate) fn put(&self, repository: Repository) -> Result<Self, PutError> {
        let others: Vec<Repository> = self
            .listed()
            .into_iter()
            .map(|kept| kept.repository.clone())
            .collect();
        if let Some(channel) = registry::conflict(&repository, &others) {
            return Err(PutError::Conflict(channel));
        }
        if self.entries.len() >= MAX_REPOSITORIES && !self.entries.contains_key(&repository.name) {
            return Err(PutError::Capacity);
        }
        let mut next = self.clone();
        next.generation += 1;
        next.entries.insert(
            repository.name.clone(),
            Kept {
                repository,
                generation: next.generation,
            },
        );
        Ok(next)
    }

    /// The registry after a `Remove` of `name`: its generation one more; none when the registry
    /// does not hold it.
    #[must_use]
    pub(crate) fn remove(&self, name: &str) -> Option<Self> {
        let mut next = self.clone();
        next.entries.remove(name)?;
        next.generation += 1;
        Some(next)
    }

    /// The repositories named `names`, by name, when the registry holds every one; none when it
    /// lacks one.
    #[must_use]
    pub(crate) fn entries_of<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Option<Vec<Repository>> {
        let mut out: Vec<Repository> = names
            .into_iter()
            .map(|name| self.get(name).map(|kept| kept.repository.clone()))
            .collect::<Option<_>>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out.dedup_by(|a, b| a.name == b.name);
        Some(out)
    }
}

/// The platforms `repository` serves: those of its descriptor (the Linux ones its apt format's
/// architectures name, and its macOS format's, which a private repository does not serve); both
/// Linux platforms for an entry put without a descriptor, or whose descriptor does not read.
#[must_use]
pub(crate) fn platforms_of(repository: &Repository) -> Vec<String> {
    let linux = || vec!["linux-x86_64".to_owned(), "linux-arm64".to_owned()];
    let Ok(parsed) = descriptor::check(&repository.descriptor) else {
        return linux();
    };
    let mut platforms = parsed.platforms();
    if registry::is_private(repository) {
        platforms.retain(|platform| !platform.starts_with("macos-"));
    }
    platforms
}

/// The file of the registry beside the desired-state file at `state`: `<state>.repositories`.
#[must_use]
pub(crate) fn repositories_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".repositories");
    PathBuf::from(path)
}

/// The text of the repositories file holding `registry`:
/// `{"generation": n, "repositories": {name: record}}`.
///
/// # Errors
/// The registry could not be serialised.
pub(crate) fn encode_file(registry: &Registry) -> serde_json::Result<Vec<u8>> {
    let repositories: Map<String, Value> = registry
        .entries
        .iter()
        .map(|(name, kept)| (name.clone(), kept.to_json()))
        .collect();
    let mut text = serde_json::to_vec_pretty(&json!({
        "generation": registry.generation,
        "repositories": repositories,
    }))?;
    text.push(b'\n');
    Ok(text)
}

/// The registry in the text of a repositories file.
///
/// # Errors
/// [`DesiredError::Malformed`]: not the file's JSON, a record that does not read, or a registry
/// that breaks a rule of [`Registry::new`], naming the repository.
pub(crate) fn parse_file(bytes: &[u8]) -> Result<Registry, DesiredError> {
    let malformed = |why: String| DesiredError::Malformed(format!("the repositories: {why}"));
    let value: Value = serde_json::from_slice(bytes).map_err(|e| malformed(e.to_string()))?;
    let generation = value
        .get("generation")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed("no generation".to_owned()))?;
    let stored = value
        .get("repositories")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("no repositories".to_owned()))?;
    let mut entries = BTreeMap::new();
    for (name, record) in stored {
        let kept = Kept::from_json(record).map_err(|e| malformed(format!("{name}: {e}")))?;
        entries.insert(name.clone(), kept);
    }
    Registry::new(generation, entries).map_err(malformed)
}

/// The reason of the `repository_put` audit entry: the operator's `why`, then the repository's
/// name, URI, suites and fingerprints, and, for a private repository, `access hosts credential`
/// and the credential file's path (a path, never the file's content).
#[must_use]
pub(crate) fn put_reason(why: &str, repository: &Repository) -> String {
    let private = if registry::is_private(repository) {
        format!(
            " access {} credential {}",
            registry::ACCESS_HOSTS,
            repository.credential_file
        )
    } else {
        String::new()
    };
    format!(
        "{why}: {} {} suites {} fingerprints {}{private}",
        repository.name,
        repository.uri,
        repository.suites.join(","),
        repository.fingerprints.join(",")
    )
}

/// The reason of the `repository_remove` audit entry: the operator's `why`, then the name.
#[must_use]
pub(crate) fn remove_reason(why: &str, name: &str) -> String {
    format!("{why}: {name}")
}
