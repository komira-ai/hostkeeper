//! The descriptor of a package repository (docs/deploy/package-repositories.md, "hostkeeper
//! repository format, version 1"): the small file `<base>/hostkeeper-repository.json` through which one URL says
//! what a repository serves, so that adding it needs a name and that URL and nothing else.
//!
//! The descriptor names the channels (apt suites, rpm directories, pointer names), the keyring
//! that signs the apt repository and its fingerprints, and the formats the repository holds: an
//! `apt` repository (required in version 1), an `rpm` one (listed for display), and `macos-pkg`
//! installers with the platforms they are built for and the directory each package's installers
//! are in. It is read by the coordinator, when an operator adds a repository, and written by the
//! publishing pipeline (`cargo xtask channels descriptor`), which judges what it writes with
//! [`check`] too.
//!
//! Version 1 is additive for ever: a reader ignores every key it does not know, at any level, and
//! refuses a `version` higher than its own ([`DescriptorError::Newer`]). The descriptor's
//! `key.fingerprints` is a claim, which [`crate::registry::keys`] checks against the keyring; the
//! trust in a key is the operator's confirmation, never the descriptor's word.
//!
//! [`check`] is pure: bytes in, the first rule broken or the parsed [`Descriptor`] out.

use std::collections::BTreeMap;

use hostkeeper_config::platform::Platform;
use serde_json::Value;

use crate::grammar::{LocatorError, check_locator};
use crate::registry::{MAX_COMPONENTS, MAX_FINGERPRINTS, MAX_SUITES};

/// The path of the descriptor under a repository's base URL.
pub const FILE: &str = "hostkeeper-repository.json";
/// The version of the descriptor this build reads and writes.
pub const VERSION: u64 = 1;
/// The most bytes of a descriptor.
pub const MAX_BYTES: usize = 64 * 1024;
/// The most bytes of a description.
pub const MAX_DESCRIPTION: usize = 200;
/// The most packages of the `macos-pkg` format.
pub const MAX_PACKAGES: usize = 64;
/// The most architectures of a format.
pub const MAX_ARCHITECTURES: usize = 16;
/// The key of the apt format.
pub const APT: &str = "apt";
/// The key of the rpm format.
pub const RPM: &str = "rpm";
/// The key of the macOS installer format.
pub const MACOS_PKG: &str = "macos-pkg";

/// The apt format: a standard apt repository, one suite per channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Apt {
    /// Where it is under the base, a locator.
    pub path: String,
    /// The components of every suite.
    pub components: Vec<String>,
    /// The architectures it holds (`amd64`, `arm64`), for display; empty means both Linux ones.
    pub architectures: Vec<String>,
}

/// The rpm format, listed for display: hostkeeper installs no rpm yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rpm {
    /// Where it is under the base, a locator.
    pub path: String,
    /// The architectures it holds.
    pub architectures: Vec<String>,
}

/// The macOS installer format: `<path>/<dir>/<version>/<package>-<version>-<platform>.pkg`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacosPkg {
    /// Where it is under the base, a locator.
    pub path: String,
    /// The platforms the installers are built for, each a hostkeeper platform of the Mac.
    pub platforms: Vec<String>,
    /// The package names whose installers are in a directory of another name, by package.
    pub packages: BTreeMap<String, String>,
}

impl MacosPkg {
    /// The directory of `package`'s installers under [`MacosPkg::path`]: the one the descriptor
    /// maps it to, else the package's own name.
    pub fn dir<'a>(&'a self, package: &'a str) -> &'a str {
        self.packages.get(package).map_or(package, String::as_str)
    }
}

/// A parsed descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// The repository's own name for itself: a suggestion, the operator's name wins. Empty when
    /// the descriptor names none.
    pub name: String,
    /// A line of display text, at most [`MAX_DESCRIPTION`] bytes; may be empty.
    pub description: String,
    /// The channels: the apt suites, the rpm directories, the pointer names.
    pub channels: Vec<String>,
    /// The keyring, a locator under the base, the binary OpenPGP keyring apt reads.
    pub keyring: String,
    /// The fingerprints the descriptor claims the keyring holds.
    pub fingerprints: Vec<String>,
    /// The apt format, required in version 1.
    pub apt: Apt,
    /// The rpm format, when the repository has one.
    pub rpm: Option<Rpm>,
    /// The macOS installer format, when the repository has one.
    pub macos_pkg: Option<MacosPkg>,
}

/// Why a descriptor is refused: the first rule it breaks.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    /// More than [`MAX_BYTES`].
    #[error("the descriptor is {0} bytes, at most {MAX_BYTES}")]
    TooLarge(usize),
    /// Not JSON.
    #[error("the descriptor is not JSON: {0}")]
    NotJson(String),
    /// JSON, but not an object.
    #[error("the descriptor is not a JSON object")]
    NotObject,
    /// No `version`, or one that is not a whole number from 1.
    #[error("the descriptor's version is missing or not a whole number from 1")]
    Version,
    /// A `version` past [`VERSION`].
    #[error("the descriptor is version {0}, newer than this coordinator reads ({VERSION})")]
    Newer(u64),
    /// A field of the wrong type or missing.
    #[error("{0} is missing or of the wrong type")]
    Field(String),
    /// A value that breaks its grammar or bound.
    #[error("{field}: {rule}")]
    Rule {
        /// The field, as a path (`formats.apt.path`).
        field: String,
        /// The rule it breaks.
        rule: String,
    },
    /// A path that is not a locator.
    #[error("{field} {source}")]
    Path {
        /// The field.
        field: String,
        /// Why it is no locator.
        source: LocatorError,
    },
    /// No `formats.apt`: version 1 requires it.
    #[error("formats.apt is required: version 1 describes an apt repository")]
    NoApt,
}

/// A rule broken at `field`.
fn rule(field: &str, rule: impl Into<String>) -> DescriptorError {
    DescriptorError::Rule {
        field: field.to_owned(),
        rule: rule.into(),
    }
}

/// The member `key` of `object`, which must be present.
fn member<'a>(object: &'a Value, key: &str, path: &str) -> Result<&'a Value, DescriptorError> {
    object
        .get(key)
        .ok_or_else(|| DescriptorError::Field(format!("{path}{key}")))
}

/// The text member `key` of `object`: `None` when absent, an error when not text.
fn optional_text(object: &Value, key: &str, path: &str) -> Result<Option<String>, DescriptorError> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|text| Some(text.to_owned()))
            .ok_or_else(|| DescriptorError::Field(format!("{path}{key}"))),
    }
}

/// The text member `key` of `object`, which must be present.
fn text(object: &Value, key: &str, path: &str) -> Result<String, DescriptorError> {
    member(object, key, path)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| DescriptorError::Field(format!("{path}{key}")))
}

/// The array of texts at `key`, `min` to `max` of them, each passing `ok`, none twice.
fn texts(
    object: &Value,
    key: &str,
    path: &str,
    bounds: (usize, usize),
    ok: impl Fn(&str) -> bool,
) -> Result<Vec<String>, DescriptorError> {
    let field = format!("{path}{key}");
    let items = member(object, key, path)?
        .as_array()
        .ok_or_else(|| DescriptorError::Field(field.clone()))?;
    if !(bounds.0..=bounds.1).contains(&items.len()) {
        return Err(rule(
            &field,
            format!("{} entries, not {} to {}", items.len(), bounds.0, bounds.1),
        ));
    }
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        let value = item
            .as_str()
            .ok_or_else(|| DescriptorError::Field(field.clone()))?;
        if !ok(value) {
            return Err(rule(
                &field,
                format!("{value:?} breaks the rule of its form"),
            ));
        }
        if seen.iter().any(|earlier| earlier == value) {
            return Err(rule(&field, format!("{value:?} is listed twice")));
        }
        seen.push(value.to_owned());
    }
    Ok(seen)
}

/// A path member: a locator.
fn path_of(object: &Value, key: &str, prefix: &str) -> Result<String, DescriptorError> {
    let value = text(object, key, prefix)?;
    check_locator(&value).map_err(|source| DescriptorError::Path {
        field: format!("{prefix}{key}"),
        source,
    })?;
    Ok(value)
}

/// Whether `value` is 1 to `max` bytes of `[a-z0-9-]`.
fn is_lower_name(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | (b == b'-'))
}

/// Whether `value` is 1 to 64 bytes of `[A-Za-z0-9._-]`.
fn is_component(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `value` is 40 or 64 upper-case hex characters.
fn is_fingerprint(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() | (b'A'..=b'F').contains(&b))
}

/// Whether `value` is an architecture's spelling: 1 to 16 bytes of `[a-z0-9_-]`.
fn is_architecture(value: &str) -> bool {
    (1..=16).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() | b.is_ascii_digit() | matches!(b, b'_' | b'-'))
}

/// Whether `value` is a hostkeeper platform of a Mac.
fn is_mac_platform(value: &str) -> bool {
    value.starts_with("macos-") && value.parse::<Platform>().is_ok()
}

/// The architectures of a format: optional, up to [`MAX_ARCHITECTURES`].
fn architectures(format: &Value, path: &str) -> Result<Vec<String>, DescriptorError> {
    if format.get("architectures").is_none() {
        return Ok(Vec::new());
    }
    texts(
        format,
        "architectures",
        path,
        (0, MAX_ARCHITECTURES),
        is_architecture,
    )
}

fn apt(formats: &Value) -> Result<Apt, DescriptorError> {
    let format = formats.get(APT).ok_or(DescriptorError::NoApt)?;
    let path = "formats.apt.";
    Ok(Apt {
        path: path_of(format, "path", path)?,
        components: texts(
            format,
            "components",
            path,
            (1, MAX_COMPONENTS),
            is_component,
        )?,
        architectures: architectures(format, path)?,
    })
}

fn rpm(formats: &Value) -> Result<Option<Rpm>, DescriptorError> {
    let Some(format) = formats.get(RPM) else {
        return Ok(None);
    };
    let path = "formats.rpm.";
    Ok(Some(Rpm {
        path: path_of(format, "path", path)?,
        architectures: architectures(format, path)?,
    }))
}

fn macos_pkg(formats: &Value) -> Result<Option<MacosPkg>, DescriptorError> {
    let Some(format) = formats.get(MACOS_PKG) else {
        return Ok(None);
    };
    let prefix = "formats.macos-pkg.";
    let path = path_of(format, "path", prefix)?;
    let platforms = texts(
        format,
        "platforms",
        prefix,
        (1, Platform::ALL.len()),
        is_mac_platform,
    )?;
    let mut packages = BTreeMap::new();
    if let Some(listed) = format.get("packages") {
        let field = format!("{prefix}packages");
        let object = listed
            .as_object()
            .ok_or_else(|| DescriptorError::Field(field.clone()))?;
        if object.len() > MAX_PACKAGES {
            return Err(rule(
                &field,
                format!("{} packages, at most {MAX_PACKAGES}", object.len()),
            ));
        }
        for (name, package) in object {
            if !is_lower_name(name, 64) {
                return Err(rule(
                    &field,
                    format!("the package {name:?} is not 1 to 64 bytes of [a-z0-9-]"),
                ));
            }
            let dir = text(package, "dir", &format!("{field}.{name}."))?;
            if !is_lower_name(&dir, 64) {
                return Err(rule(
                    &field,
                    format!("the dir {dir:?} of {name} is not 1 to 64 bytes of [a-z0-9-]"),
                ));
            }
            packages.insert(name.clone(), dir);
        }
    }
    Ok(Some(MacosPkg {
        path,
        platforms,
        packages,
    }))
}

/// Judges `bytes` as a descriptor and parses it: the first rule it breaks, in the order of the
/// file's fields.
///
/// # Errors
/// [`DescriptorError`].
pub fn check(bytes: &[u8]) -> Result<Descriptor, DescriptorError> {
    if bytes.len() > MAX_BYTES {
        return Err(DescriptorError::TooLarge(bytes.len()));
    }
    let root: Value =
        serde_json::from_slice(bytes).map_err(|why| DescriptorError::NotJson(why.to_string()))?;
    if !root.is_object() {
        return Err(DescriptorError::NotObject);
    }
    match root.get("version").and_then(Value::as_u64) {
        None | Some(0) => return Err(DescriptorError::Version),
        Some(version) if version > VERSION => return Err(DescriptorError::Newer(version)),
        Some(_) => {}
    }
    let name = optional_text(&root, "name", "")?.unwrap_or_default();
    if !name.is_empty() && !is_lower_name(&name, 32) {
        return Err(rule(
            "name",
            format!("{name:?} is not 1 to 32 bytes of [a-z0-9-]"),
        ));
    }
    let description = optional_text(&root, "description", "")?.unwrap_or_default();
    if description.len() > MAX_DESCRIPTION {
        return Err(rule(
            "description",
            format!("{} bytes, at most {MAX_DESCRIPTION}", description.len()),
        ));
    }
    let channels = texts(&root, "channels", "", (1, MAX_SUITES), |c| {
        is_lower_name(c, 31)
    })?;
    let key = member(&root, "key", "")?;
    let keyring = path_of(key, "keyring", "key.")?;
    let fingerprints = texts(
        key,
        "fingerprints",
        "key.",
        (1, MAX_FINGERPRINTS),
        is_fingerprint,
    )?;
    let formats = member(&root, "formats", "")?;
    Ok(Descriptor {
        name,
        description,
        channels,
        keyring,
        fingerprints,
        apt: apt(formats)?,
        rpm: rpm(formats)?,
        macos_pkg: macos_pkg(formats)?,
    })
}

impl Descriptor {
    /// The platforms the repository serves, in a fixed order: the Linux ones the apt format's
    /// architectures name (both when it names none), then those of the macOS format.
    pub fn platforms(&self) -> Vec<String> {
        let mut out = Vec::new();
        let arches = &self.apt.architectures;
        for (arch, platform) in [
            ("amd64", Platform::LinuxX86_64),
            ("arm64", Platform::LinuxArm64),
        ] {
            if arches.is_empty() || arches.iter().any(|a| a == arch) {
                out.push(platform.as_str().to_owned());
            }
        }
        if let Some(mac) = &self.macos_pkg {
            out.extend(mac.platforms.iter().cloned());
        }
        out
    }

    /// The apt repository's root under `base`: `<base>/<apt.path>`.
    pub fn apt_uri(&self, base: &str) -> String {
        format!("{base}/{}", self.apt.path)
    }
}
