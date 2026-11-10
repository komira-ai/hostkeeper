//! The parts of the cross-release rig (`tests/self_update_across_releases.rs`) that need no root:
//! the pin of the previous release, the check of its downloaded tarball against the pin, and the
//! version a program of this build runs as.
//!
//! **The pin.** `previous-release.toml` beside this crate's manifest ([`PIN_TEXT`]) names one
//! published release of hostkeeper: its `tag`, its `version`, and for each release platform
//! ([`Platform::ALL`]) a table with the `url` of the release's binary tarball and the `sha256` of
//! that tarball. [`Pin::parse`] refuses anything else: an unknown or missing key, a tag that is
//! not `v<version>`, a URL that is not the release's own asset for that platform, a digest that
//! is not 64 lower-case hex digits.
//!
//! **The download.** A workflow step before the test downloads the URL of its platform and checks
//! it against the digest, into `target/previous-release/hostkeeper.tar.gz` ([`DOWNLOAD_DIR`],
//! [`DOWNLOAD_FILE`]). The test fetches nothing: [`checked_binary`] reads that file, checks its
//! SHA-256 against the pin again and gives the one member [`MEMBER`], the daemon binary. A file
//! that is missing, or that differs from the pin, fails by name ([`PinError::NotDownloaded`],
//! [`PinError::Differs`]), so a stale cache or a moved pin never runs another binary than the
//! pinned one.
//!
//! **The version of a program.** The programs that stand for this build in the rig run as the
//! version named by the file they run from ([`program_version`]): the daemon keeps version `<v>`
//! of its own binary at `versions/<v>` of its binary directory, and `current` names it, so one
//! build staged under two names runs as two versions. A released binary runs as the version it
//! was built with.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::platform::Platform;
use sha2::{Digest, Sha256};

/// The pin, as checked in beside this crate's manifest.
pub const PIN_TEXT: &str = include_str!("../previous-release.toml");
/// The directory of the target directory the workflow downloads the pinned tarball into.
pub const DOWNLOAD_DIR: &str = "previous-release";
/// The name the workflow gives the tarball there, on every platform.
pub const DOWNLOAD_FILE: &str = "hostkeeper.tar.gz";
/// The one member of a release's binary tarball: the `hostkeeper` binary, at the archive's root.
pub const MEMBER: &str = "hostkeeper";
/// Where every release's assets are published, before `<tag>/<asset>`.
pub const RELEASES: &str = "https://github.com/komira-ai/hostkeeper/releases/download";

/// Why the pin cannot be read, or the downloaded tarball is not the pinned one.
#[derive(Debug, thiserror::Error)]
pub enum PinError {
    /// The pin is not TOML.
    #[error("the pin is not TOML: {0}")]
    Unparseable(String),
    /// The pin has a key it does not define.
    #[error("the pin has the key {0:?}, which it does not define")]
    UnknownKey(String),
    /// The pin lacks a key it needs.
    #[error("the pin has no {0}")]
    MissingKey(String),
    /// A key of the pin holds something other than the text or table it should.
    #[error("the pin's {0} is not {1}")]
    WrongType(String, &'static str),
    /// The pin's version is not `major.minor.patch`.
    #[error("the pin's version {0:?} is not major.minor.patch")]
    BadVersion(String),
    /// The pin's tag is not `v<version>`.
    #[error("the pin's tag {tag:?} is not v{version}")]
    TagMismatch { tag: String, version: String },
    /// A platform's URL is not the release's binary tarball for that platform.
    #[error("the pin's url of {platform} is {url:?}, not {expected:?}")]
    WrongUrl {
        platform: String,
        url: String,
        expected: String,
    },
    /// A platform's digest is not 64 lower-case hex digits.
    #[error("the pin's sha256 of {platform} is {sha256:?}, not 64 lower-case hex digits")]
    BadSha256 { platform: String, sha256: String },
    /// The tarball is not where the workflow downloads it.
    #[error(
        "the pinned release is not downloaded at {}: privileged.yml's step `Check the previous release (pinned checksum)` puts it there",
        .0.display()
    )]
    NotDownloaded(PathBuf),
    /// The tarball cannot be read.
    #[error("{}: {source}", .path.display())]
    Unreadable { path: PathBuf, source: io::Error },
    /// The tarball is not the pinned one.
    #[error("{} differs from the pin: its sha256 is {actual}, the pin's {expected}", .path.display())]
    Differs {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// The tarball is not a gzip'd tar whose one member is [`MEMBER`].
    #[error("{} is not a release's binary tarball: {why}", .path.display())]
    NotATarball { path: PathBuf, why: String },
}

/// One platform's asset of the pinned release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    /// The binary tarball's URL.
    pub url: String,
    /// Its SHA-256, 64 lower-case hex digits.
    pub sha256: String,
}

/// The pinned release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// The release's tag, `v<version>`.
    pub tag: String,
    /// The version its binary runs as.
    pub version: HostkeeperVersion,
    /// Each release platform's asset, by the platform's name.
    pub assets: BTreeMap<&'static str, Asset>,
}

/// The URL of the binary tarball of the release `tag` at `version` for `platform`.
pub fn asset_url(tag: &str, version: &str, platform: &str) -> String {
    format!("{RELEASES}/{tag}/hostkeeper-{version}-{platform}.tar.gz")
}

/// The text of `key` in `table`, refused when missing or not text.
fn text_of<'a>(table: &'a toml::Table, key: &str, name: &str) -> Result<&'a str, PinError> {
    match table.get(key) {
        None => Err(PinError::MissingKey(name.to_owned())),
        Some(value) => value
            .as_str()
            .ok_or_else(|| PinError::WrongType(name.to_owned(), "text")),
    }
}

/// Whether `text` is 64 lower-case hex digits.
fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Pin {
    /// Reads the pin's text (the module says what it holds).
    ///
    /// # Errors
    /// The text is not TOML or is not a pin, as [`PinError`] names.
    pub fn parse(text: &str) -> Result<Pin, PinError> {
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| PinError::Unparseable(e.message().to_owned()))?;
        let platforms = Platform::ALL.map(Platform::as_str);
        if let Some(key) = table.keys().find(|key| {
            !["tag", "version"].contains(&key.as_str()) && !platforms.contains(&key.as_str())
        }) {
            return Err(PinError::UnknownKey(key.clone()));
        }
        let tag = text_of(&table, "tag", "tag")?;
        let written = text_of(&table, "version", "version")?;
        let version = HostkeeperVersion::parse(written)
            .map_err(|_| PinError::BadVersion(written.to_owned()))?;
        if tag != format!("v{version}") {
            return Err(PinError::TagMismatch {
                tag: tag.to_owned(),
                version: version.to_string(),
            });
        }
        let mut assets = BTreeMap::new();
        for platform in platforms {
            let entry = table
                .get(platform)
                .ok_or_else(|| PinError::MissingKey(format!("[{platform}]")))?
                .as_table()
                .ok_or_else(|| PinError::WrongType(format!("[{platform}]"), "a table"))?;
            if let Some(key) = entry
                .keys()
                .find(|key| !["url", "sha256"].contains(&key.as_str()))
            {
                return Err(PinError::UnknownKey(format!("{platform}.{key}")));
            }
            let url = text_of(entry, "url", &format!("{platform}.url"))?;
            let expected = asset_url(tag, &version.to_string(), platform);
            if url != expected {
                return Err(PinError::WrongUrl {
                    platform: platform.to_owned(),
                    url: url.to_owned(),
                    expected,
                });
            }
            let sha256 = text_of(entry, "sha256", &format!("{platform}.sha256"))?;
            if !is_sha256(sha256) {
                return Err(PinError::BadSha256 {
                    platform: platform.to_owned(),
                    sha256: sha256.to_owned(),
                });
            }
            assets.insert(
                platform,
                Asset {
                    url: url.to_owned(),
                    sha256: sha256.to_owned(),
                },
            );
        }
        Ok(Pin {
            tag: tag.to_owned(),
            version,
            assets,
        })
    }

    /// The asset of `platform`. Every release platform has one ([`Pin::parse`] holds it).
    pub fn asset(&self, platform: Platform) -> &Asset {
        &self.assets[platform.as_str()]
    }
}

/// Where the workflow downloads the pinned tarball, under the target directory `target`.
pub fn download_path(target: &Path) -> PathBuf {
    target.join(DOWNLOAD_DIR).join(DOWNLOAD_FILE)
}

/// The bytes of the daemon binary of the tarball at `tarball`, once its SHA-256 is `asset`'s.
///
/// # Errors
/// The tarball is not there ([`PinError::NotDownloaded`]) or cannot be read, its SHA-256 is not
/// the pin's ([`PinError::Differs`]), or it is not a gzip'd tar whose one member is [`MEMBER`].
pub fn checked_binary(tarball: &Path, asset: &Asset) -> Result<Vec<u8>, PinError> {
    let bytes = match fs::read(tarball) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(PinError::NotDownloaded(tarball.to_path_buf()));
        }
        Err(source) => {
            return Err(PinError::Unreadable {
                path: tarball.to_path_buf(),
                source,
            });
        }
    };
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != asset.sha256 {
        return Err(PinError::Differs {
            path: tarball.to_path_buf(),
            expected: asset.sha256.clone(),
            actual,
        });
    }
    member(&bytes).map_err(|why| PinError::NotATarball {
        path: tarball.to_path_buf(),
        why,
    })
}

/// The bytes of [`MEMBER`], the one member of the gzip'd tar `bytes`.
fn member(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut members = members(bytes).map_err(|e| e.to_string())?;
    if members.is_empty() {
        return Err(format!("it holds no {MEMBER:?}"));
    }
    let extra = members
        .iter()
        .find(|(name, _)| name != MEMBER)
        .or_else(|| members.get(1));
    if let Some((name, _)) = extra {
        return Err(format!("it holds {name:?}, not only one {MEMBER:?}"));
    }
    Ok(members.remove(0).1)
}

/// Every member of the gzip'd tar `bytes`: its name and its bytes, in order.
fn members(bytes: &[u8]) -> io::Result<Vec<(String, Vec<u8>)>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut out = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        out.push((name, data));
    }
    Ok(out)
}

/// The version a program of this build runs as when run from `exe`, its links resolved: the
/// file's name, a `major.minor.patch` (the module says why).
///
/// # Errors
/// `exe` cannot be resolved, or its file name is not a version; the message names it.
pub fn program_version(exe: &Path) -> Result<HostkeeperVersion, String> {
    let real = fs::canonicalize(exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let name = real
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    HostkeeperVersion::parse(&name).map_err(|_| {
        format!(
            "the daemon runs from versions/<major.minor.patch> of a binary directory, not from {}",
            real.display()
        )
    })
}

#[cfg(test)]
mod tests;
