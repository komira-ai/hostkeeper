//! The pkg the gate installs on a Mac (docs/design/hostkeeper-mdm.md section 7.1): the
//! allowlist of pkg digests, and the inline manifest of `InstallEnterpriseApplication`.
//!
//! **The allowlist** is a TOML file, root's and writable by root alone, named by
//! [`ALLOWLIST_FLAG`] and read when the coordinator starts:
//!
//! ```toml
//! [[pkg]]
//! sha256  = "<the pkg's SHA-256, 64 lower-case hex digits>"
//! version = "0.1.66"
//! path    = "/var/lib/hostkeeper-mdm-pkg/<sha256>.pkg"
//! ```
//!
//! Each entry gives the digest, the version and the path of the pkg file in the kit's pkg
//! directory. The path's file name is `<sha256>.pkg`, the name the kit's proxy serves at
//! `/pkg/<sha256>.pkg`, so the URL the Mac fetches names the digest it checks. A digest is listed
//! once, and so is a version's release numbers ([`Version::release_numbers`]), so the newest
//! entry, the one the gate sends, is never in doubt. A file with no entry, or one entry that
//! breaks a rule, stops the start. A key the gate does not read is passed over, so a later
//! release can add one.
//!
//! **The digest is checked twice:** by the gate, which hashes the file before every send and
//! sends nothing when its bytes are not the listed digest, and by the Mac, which checks the
//! manifest's `sha256` when it downloads.
//!
//! **The manifest** ([`manifest`]) is inline, not a `ManifestURL`: one item, with one
//! `software-package` asset (its `url` and its whole-file `sha256`) and the `metadata`
//! `bundle-identifier`, `bundle-version`, `kind` `software` and `title`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_enrollment::fs::Fs;
use hostkeeper_wire::version::Version;
use plist::{Dictionary, Value};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// The flag of the pkg allowlist file.
pub const ALLOWLIST_FLAG: &str = "--mdm-pkg-allowlist";

/// The flag of the MDM host's DNS name.
pub const HOST_FLAG: &str = "--mdm-host";

/// The pkg identifier of the hostkeeper pkg: the daemon's launchd label, as
/// `cargo xtask release macos-pkg` builds it.
pub const PKG_IDENTIFIER: &str = "dev.hostkeeper.hostkeeper";

/// The SHA-256 of a pkg, as 64 lower-case hexadecimal digits: the form the kit's route
/// `/pkg/<sha256>.pkg` answers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PkgDigest(String);

impl PkgDigest {
    /// `text` as a digest; `None` unless it is 64 lower-case hexadecimal digits.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let fits = text.len() == 64
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        fits.then(|| Self(text.to_owned()))
    }

    /// The digest of `bytes`.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(
            Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        )
    }

    /// The 64 digits.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name of the pkg file, `<sha256>.pkg`.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.pkg", self.0)
    }
}

impl fmt::Display for PkgDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The MDM host's DNS name, as its proxy's certificate names it: two or more labels of ASCII
/// letters, digits and `-` (not first or last), each 1 to 63 long, at most 253 in all, kept in
/// lower case. No scheme, port or path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MdmHost(String);

/// Why the MDM host's name is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{HOST_FLAG}: {0:?} is not a DNS name (the MDM host's name alone, with no scheme, port or path)"
)]
pub struct MdmHostError(String);

impl MdmHost {
    /// `text` as the MDM host's name.
    ///
    /// # Errors
    /// [`MdmHostError`], naming the text (at most its first 253 characters).
    pub fn parse(text: &str) -> Result<Self, MdmHostError> {
        let label = |label: &str| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        };
        let fits = text.len() <= 253 && text.split('.').count() >= 2 && text.split('.').all(label);
        if fits {
            Ok(Self(text.to_ascii_lowercase()))
        } else {
            Err(MdmHostError(text.chars().take(253).collect()))
        }
    }

    /// The URL the kit's proxy serves the pkg of `digest` at: `https://<host>/pkg/<sha256>.pkg`.
    #[must_use]
    pub fn pkg_url(&self, digest: &PkgDigest) -> String {
        format!("https://{}/pkg/{}", self.0, digest.file_name())
    }
}

/// One pkg the gate may send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkgEntry {
    /// Its digest.
    pub sha256: PkgDigest,
    /// Its version, the manifest's `bundle-version`.
    pub version: Version,
    /// Its file, `<sha256>.pkg` in the kit's pkg directory.
    pub path: PathBuf,
}

/// The pkgs the gate may send, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PkgAllowlist(Vec<PkgEntry>);

/// Why the pkg allowlist is refused; the coordinator does not start.
#[derive(Debug, thiserror::Error)]
pub enum PkgAllowlistError {
    /// The file cannot be read.
    #[error("{ALLOWLIST_FLAG} {path}: {source}")]
    Read {
        /// The file.
        path: String,
        /// What the system said.
        #[source]
        source: std::io::Error,
    },
    /// The file is not root's alone.
    #[error(
        "{ALLOWLIST_FLAG} {path}: {why}; the pkg allowlist must be a file of root's that no one else can write"
    )]
    Untrusted {
        /// The file.
        path: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// The text breaks a rule of the allowlist.
    #[error("{ALLOWLIST_FLAG}: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
struct FileToml {
    #[serde(default)]
    pkg: Vec<EntryToml>,
}

#[derive(Deserialize)]
struct EntryToml {
    sha256: String,
    version: String,
    path: PathBuf,
}

impl PkgAllowlist {
    /// The allowlist in the TOML `text`.
    ///
    /// # Errors
    /// [`PkgAllowlistError::Invalid`]: the text is not TOML of `[[pkg]]` entries, lists no
    /// pkg, or an entry has a malformed digest or version, a path that is not absolute or not
    /// named `<sha256>.pkg`, or the digest or the release numbers of another entry.
    pub fn parse(text: &str) -> Result<Self, PkgAllowlistError> {
        let invalid = |why: String| PkgAllowlistError::Invalid(why);
        let file: FileToml = toml::from_str(text).map_err(|e| invalid(e.message().to_owned()))?;
        let mut entries: Vec<PkgEntry> = Vec::with_capacity(file.pkg.len());
        for row in file.pkg {
            let sha256 = PkgDigest::parse(&row.sha256).ok_or_else(|| {
                invalid(format!(
                    "sha256 {:?} is not 64 lower-case hex digits",
                    row.sha256
                ))
            })?;
            let version = Version::parse(&row.version)
                .map_err(|why| invalid(format!("version {:?}: {why}", row.version)))?;
            let named =
                row.path.is_absolute() && row.path.file_name() == Some(sha256.file_name().as_ref());
            if !named {
                return Err(invalid(format!(
                    "the path of {sha256} is not an absolute path to {}, the name the kit's proxy serves",
                    sha256.file_name()
                )));
            }
            if entries.iter().any(|entry| entry.sha256 == sha256) {
                return Err(invalid(format!("sha256 {sha256} is listed twice")));
            }
            let numbers = version.release_numbers();
            if let Some(other) = entries
                .iter()
                .find(|entry| entry.version.release_numbers() == numbers)
            {
                return Err(invalid(format!(
                    "versions {} and {} are the same release, so neither is the newest",
                    other.version.as_str(),
                    version.as_str()
                )));
            }
            entries.push(PkgEntry {
                sha256,
                version,
                path: row.path,
            });
        }
        if entries.is_empty() {
            return Err(invalid("the allowlist lists no pkg ([[pkg]])".to_owned()));
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.version.release_numbers()));
        Ok(Self(entries))
    }

    /// The allowlist in the file `path` of `fs`, which must be a regular file of root's (uid 0)
    /// that neither its group nor others can write.
    ///
    /// # Errors
    /// [`PkgAllowlistError`]: the file cannot be read, is not root's alone, or
    /// [`Self::parse`] refuses it.
    pub fn load(fs: &dyn Fs, path: &Path) -> Result<Self, PkgAllowlistError> {
        let shown = path.display().to_string();
        let read = fs.read_private(path).and_then(|read| {
            read.ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        let (bytes, stat) = read.map_err(|source| PkgAllowlistError::Read {
            path: shown.clone(),
            source,
        })?;
        let untrusted = |why| PkgAllowlistError::Untrusted {
            path: shown.clone(),
            why,
        };
        if !stat.is_file {
            return Err(untrusted("it is not a regular file"));
        }
        if stat.uid != 0 {
            return Err(untrusted("it is not owned by root"));
        }
        if stat.mode & 0o022 != 0 {
            return Err(untrusted("its group or others can write it"));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| PkgAllowlistError::Invalid("the file is not UTF-8".to_owned()))?;
        Self::parse(&text)
    }

    /// The pkg the gate sends: the newest by its release numbers.
    #[must_use]
    pub fn newest(&self) -> &PkgEntry {
        // `parse` refuses an empty list.
        &self.0[0]
    }

    /// Every entry, newest first.
    #[must_use]
    pub fn entries(&self) -> &[PkgEntry] {
        &self.0
    }
}

/// What the gate needs to install the pkg: the allowlist, the MDM host the Mac fetches it
/// from, and the file system the pkg files are read from.
pub struct Install {
    /// The pkgs it may send.
    pub allowlist: PkgAllowlist,
    /// The MDM host whose proxy serves them.
    pub host: MdmHost,
    /// Where the pkg files are read, to check their digests.
    pub fs: Arc<dyn Fs>,
}

impl fmt::Debug for Install {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Install")
            .field("allowlist", &self.allowlist)
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// Why a pkg is not sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PkgFileError {
    /// The file is not there.
    #[error("the pkg {0} is missing")]
    Missing(String),
    /// The file cannot be read.
    #[error("the pkg {0} cannot be read: {1}")]
    Read(String, String),
    /// Its bytes are not the listed digest.
    #[error("the pkg {path}'s bytes are {actual}, not its listed sha256; it is not sent")]
    Digest {
        /// The file.
        path: String,
        /// The digest of its bytes.
        actual: PkgDigest,
    },
}

impl Install {
    /// The entry to send, once the bytes of its file are its listed digest.
    ///
    /// # Errors
    /// [`PkgFileError`]: the file is missing, unreadable, or its bytes are another digest.
    pub fn checked(&self) -> Result<&PkgEntry, PkgFileError> {
        let entry = self.allowlist.newest();
        let shown = entry.path.display().to_string();
        let bytes = self
            .fs
            .read(&entry.path)
            .map_err(|why| PkgFileError::Read(shown.clone(), why.to_string()))?
            .ok_or_else(|| PkgFileError::Missing(shown.clone()))?;
        let actual = PkgDigest::of(&bytes);
        if actual == entry.sha256 {
            Ok(entry)
        } else {
            Err(PkgFileError::Digest {
                path: shown,
                actual,
            })
        }
    }
}

/// The inline `Manifest` of `InstallEnterpriseApplication` for `entry`, served by `host`.
#[must_use]
pub fn manifest(entry: &PkgEntry, host: &MdmHost) -> Dictionary {
    let dict = |pairs: Vec<(&str, Value)>| -> Dictionary {
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    };
    let asset = dict(vec![
        ("kind", Value::from("software-package")),
        ("sha256", Value::from(entry.sha256.as_str())),
        ("url", Value::from(host.pkg_url(&entry.sha256))),
    ]);
    let metadata = dict(vec![
        ("bundle-identifier", Value::from(PKG_IDENTIFIER)),
        ("bundle-version", Value::from(entry.version.as_str())),
        ("kind", Value::from("software")),
        (
            "title",
            Value::from(format!("hostkeeper {}", entry.version.as_str())),
        ),
    ]);
    let item = dict(vec![
        ("assets", Value::Array(vec![Value::Dictionary(asset)])),
        ("metadata", Value::Dictionary(metadata)),
    ]);
    dict(vec![("items", Value::Array(vec![Value::Dictionary(item)]))])
}
