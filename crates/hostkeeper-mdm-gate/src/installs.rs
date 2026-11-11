//! The named installs (docs/design/coordinator-mdm.md sections 4.7 and 4.8): the packages the
//! gate may send a Mac by `InstallEnterpriseApplication`, each by a name.
//!
//! An install is `name`, `bundle_id`, `sha256`, `version`, `path` and, optionally, `url`, the
//! origin the package proxy pulls the file from (the proxy is a later task; the gate only keeps
//! and shows the field). The list is the MDM setting `installs`, kept as the JSON text
//! [`Installs::to_json`] makes and [`Installs::parse`] reads, so the operator's setup page edits
//! it and the coordinator keeps it in its store. A key the reader does not know is passed over, so
//! a later release can add one (B8).
//!
//! **The install named [`HOSTKEEPER_INSTALL`]** is the hostkeeper pkg, the one the gate sends
//! before the daemon runs ([`crate::install`]). Every other install is a baseline entry
//! ([`crate::baseline`]): sent to each Mac of a host group that has a baseline, once, and again
//! when the entry changes ([`InstallEntry::digest`]).
//!
//! **The old file.** `--mdm-pkg-allowlist` ([`crate::pkg::PkgAllowlist`]) is read once, when the
//! setting has never been set: its newest pkg becomes the install named `hostkeeper`
//! ([`crate::pkg::PkgAllowlist::installs`]).

use std::collections::BTreeSet;
use std::path::PathBuf;

use hostkeeper_wire::version::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::pkg::PkgDigest;

/// The name of the install of the hostkeeper pkg.
pub const HOSTKEEPER_INSTALL: &str = "hostkeeper";

/// The name of the MDM setting that holds the installs.
pub const SETTING_NAME: &str = "installs";

/// One install the gate may send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallEntry {
    /// Its name: 1 to 32 lower-case letters, digits and `-`, not first or last.
    pub name: String,
    /// The pkg's bundle identifier, the manifest's `bundle-identifier`.
    pub bundle_id: String,
    /// The digest of the pkg file.
    pub sha256: PkgDigest,
    /// Its version, the manifest's `bundle-version`.
    pub version: Version,
    /// Its file, `<sha256>.pkg` in the pkg directory.
    pub path: PathBuf,
    /// Where the package proxy pulls the file from, when it is not a file the host provides.
    pub url: Option<String>,
}

impl InstallEntry {
    /// The digest of what a Mac is sent for this entry (its name, bundle identifier, file digest
    /// and version, which the manifest carries): a change of any is a different install. The
    /// path and the origin `url` are where the file is found, not what is installed.
    #[must_use]
    pub fn digest(&self) -> String {
        let text = format!(
            "{}\n{}\n{}\n{}",
            self.name,
            self.bundle_id,
            self.sha256,
            self.version.as_str()
        );
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// The installs, in the order they were listed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Installs(Vec<InstallEntry>);

/// Why an installs text is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("installs: {0}")]
pub struct InstallsError(String);

#[derive(Deserialize)]
struct EntryJson {
    name: String,
    bundle_id: String,
    sha256: String,
    version: String,
    path: PathBuf,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Serialize)]
struct EntryOut<'a> {
    name: &'a str,
    bundle_id: &'a str,
    sha256: &'a str,
    version: &'a str,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
}

fn name_fits(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn bundle_id_fits(id: &str) -> bool {
    (1..=255).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

fn url_fits(url: &str) -> bool {
    url.len() <= 2048
        && url.starts_with("https://")
        && url.len() > "https://".len()
        && url.bytes().all(|b| b.is_ascii_graphic())
}

impl Installs {
    /// No install.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The installs `entries`, which the caller made from checked parts.
    pub(crate) fn from_entries(entries: Vec<InstallEntry>) -> Self {
        Self(entries)
    }

    /// The installs in the JSON `text`, an array of objects.
    ///
    /// # Errors
    /// [`InstallsError`]: the text is not such an array, or an entry has a name that is not 1 to
    /// 32 lower-case letters, digits and `-`, a bundle identifier of other characters, a digest
    /// that is not 64 lower-case hex digits, a version the daemon would not read, a path that is
    /// not absolute or not named `<sha256>.pkg`, or a `url` that is not `https://`; or a name or
    /// a digest is listed twice.
    pub fn parse(text: &str) -> Result<Self, InstallsError> {
        let invalid = |why: String| InstallsError(why);
        let rows: Vec<EntryJson> = serde_json::from_str(text)
            .map_err(|_| invalid("not a JSON array of installs".to_owned()))?;
        let mut names = BTreeSet::new();
        let mut digests = BTreeSet::new();
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            if !name_fits(&row.name) {
                return Err(invalid(format!(
                    "the name {:?} is not 1 to 32 lower-case letters, digits and '-'",
                    row.name.chars().take(40).collect::<String>()
                )));
            }
            let name = row.name;
            if !bundle_id_fits(&row.bundle_id) {
                return Err(invalid(format!(
                    "{name}: the bundle_id is not 1 to 255 letters, digits, '.', '-' and '_'"
                )));
            }
            let sha256 = PkgDigest::parse(&row.sha256).ok_or_else(|| {
                invalid(format!("{name}: sha256 is not 64 lower-case hex digits"))
            })?;
            let version = Version::parse(&row.version)
                .map_err(|why| invalid(format!("{name}: version {:?}: {why}", row.version)))?;
            let named =
                row.path.is_absolute() && row.path.file_name() == Some(sha256.file_name().as_ref());
            if !named {
                return Err(invalid(format!(
                    "{name}: the path is not an absolute path to {}",
                    sha256.file_name()
                )));
            }
            if row.url.as_deref().is_some_and(|url| !url_fits(url)) {
                return Err(invalid(format!("{name}: the url is not an https:// URL")));
            }
            if !names.insert(name.clone()) {
                return Err(invalid(format!("the name {name} is listed twice")));
            }
            if !digests.insert(sha256.clone()) {
                return Err(invalid(format!("{name}: sha256 {sha256} is listed twice")));
            }
            entries.push(InstallEntry {
                name,
                bundle_id: row.bundle_id,
                sha256,
                version,
                path: row.path,
                url: row.url,
            });
        }
        Ok(Self(entries))
    }

    /// The installs as the JSON text [`Self::parse`] reads back.
    #[must_use]
    pub fn to_json(&self) -> String {
        let rows: Vec<EntryOut<'_>> = self
            .0
            .iter()
            .map(|entry| EntryOut {
                name: &entry.name,
                bundle_id: &entry.bundle_id,
                sha256: entry.sha256.as_str(),
                version: entry.version.as_str(),
                path: entry.path.to_string_lossy().into_owned(),
                url: entry.url.as_deref(),
            })
            .collect();
        // Strings and options of strings always serialize.
        serde_json::to_string(&rows).unwrap_or_default()
    }

    /// The install called `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&InstallEntry> {
        self.0.iter().find(|entry| entry.name == name)
    }

    /// The install of the hostkeeper pkg, if listed.
    #[must_use]
    pub fn hostkeeper(&self) -> Option<&InstallEntry> {
        self.get(HOSTKEEPER_INSTALL)
    }

    /// Every install, in the order listed.
    #[must_use]
    pub fn entries(&self) -> &[InstallEntry] {
        &self.0
    }

    /// The installs the baseline sends: every one but the hostkeeper pkg.
    pub fn baseline(&self) -> impl Iterator<Item = &InstallEntry> {
        self.0
            .iter()
            .filter(|entry| entry.name != HOSTKEEPER_INSTALL)
    }
}
