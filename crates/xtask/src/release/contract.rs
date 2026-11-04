//! The release contract: every name and format a consumer of a hostkeeper release relies on, in
//! one module. A consumer checks a release's provenance before it trusts anything in it (design
//! section 4.5), so these are a public interface and change additively only:
//!
//! - the version `X.Y.Z` ([`Version`]) and the tag `v<X.Y.Z>` ([`Version::tag`]);
//! - the text a shipped binary prints for `--version`, `hostkeeper X.Y.Z+<12 hex>`
//!   ([`stamp_string`]), and [`RELEASE_COMMIT`], the build-time input that carries the commit;
//! - the identity that signs every attestation: [`SIGNER_WORKFLOW`] on [`SIGNER_REF`] of
//!   [`REPO`];
//! - the asset names of a release ([`asset_names`], the one function that lists them; the
//!   committed baseline `crates/xtask/tests/release/asset_names.baseline` holds them).
//!
//! There are exactly three platforms, linux-x86_64, linux-arm64 and macos-arm64
//! ([`Platform::ALL`]): one package per OS and CPU family, listed in the release index
//! (`index.json`, see `index`). A release ships the platforms of [`Platform::RELEASED`], the two
//! Linux ones until the macOS daemon exists.

use std::fmt;
use std::str::FromStr;

/// The repository a release is cut from and attested by.
pub(crate) const REPO: &str = "komira-ai/hostkeeper";

/// The branch every released commit is on.
pub(crate) const MAIN: &str = "main";

/// The ref every attestation names as its source.
pub(crate) const SIGNER_REF: &str = "refs/heads/main";

/// The one workflow whose runs sign: the attested signer workflow of every asset.
pub(crate) const SIGNER_WORKFLOW: &str = "komira-ai/hostkeeper/.github/workflows/artifacts.yml";

/// The workflow file that builds and signs the assets (the file of [`SIGNER_WORKFLOW`]).
pub(crate) const ARTIFACTS_WORKFLOW: &str = "artifacts.yml";

/// The workflow file that lints and tests.
pub(crate) const CI_WORKFLOW: &str = "ci.yml";

/// The workflow file that runs the privileged tests (real systemd, root).
pub(crate) const PRIVILEGED_WORKFLOW: &str = "privileged.yml";

/// The three workflows a commit needs a successful push run of on [`MAIN`] to be released.
pub(crate) const REQUIRED_WORKFLOWS: [&str; 3] =
    [CI_WORKFLOW, PRIVILEGED_WORKFLOW, ARTIFACTS_WORKFLOW];

/// The name of the build-time input that carries the full commit into a shipped binary, which
/// prints its first [`SHORT_COMMIT`] digits in [`stamp_string`]. The release notes name it.
pub(crate) const RELEASE_COMMIT: &str = "RELEASE_COMMIT";

/// The program name a shipped binary prints first for `--version`.
pub(crate) const PROGRAM: &str = "hostkeeper";

/// The coordinator's asset stem.
pub(crate) const COORDINATOR: &str = "hostkeeper-coordinator";

/// The file listing the SHA-256 of every asset but itself and the bundles.
pub(crate) const SUMS: &str = "SHA256SUMS";

/// The release index: which package file serves each platform (see `index`).
pub(crate) const INDEX: &str = "index.json";

/// What a provenance bundle's file name adds to its asset's.
pub(crate) const BUNDLE_SUFFIX: &str = ".sigstore.json";

/// What the hostkeeper package's file name ends in: the archive and its compression. The
/// compression itself is chosen in `package::compress`; the two change together.
pub(crate) const PACKAGE_SUFFIX: &str = ".tar.zst";

/// What the file holding the package's SHA-256 adds to the package's name.
pub(crate) const DIGEST_SUFFIX: &str = ".sha256";

/// How many hex digits of the commit a binary prints in [`stamp_string`].
pub(crate) const SHORT_COMMIT: usize = 12;

/// Why a text is not one of the values of this module.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BadValue {
    #[error("not a commit: {0:?} (expected 40 lowercase hex digits)")]
    Commit(String),
    #[error("not a version: {0:?} (expected X.Y.Z, decimal numbers without a leading zero)")]
    Version(String),
    #[error("not a base version: {0:?} (expected X.Y, decimal numbers without a leading zero)")]
    Base(String),
    #[error("not a repository: {0:?} (expected <owner>/<name>)")]
    Repo(String),
}

/// A full commit id: 40 lowercase hex digits.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Commit(String);

impl Commit {
    /// The first [`SHORT_COMMIT`] digits.
    pub(crate) fn short(&self) -> &str {
        &self.0[..SHORT_COMMIT]
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Commit {
    type Err = BadValue;

    fn from_str(text: &str) -> Result<Commit, BadValue> {
        let hex = text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if (text.len() == 40) & hex {
            Ok(Commit(text.to_owned()))
        } else {
            Err(BadValue::Commit(text.to_owned()))
        }
    }
}

impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One number of a version: decimal digits, and no leading zero but in `0` itself.
fn number(part: &str) -> Option<u64> {
    let digits = !part.is_empty() & part.bytes().all(|b| b.is_ascii_digit());
    let no_leading_zero = (part.len() == 1) | !part.starts_with('0');
    (digits & no_leading_zero)
        .then(|| part.parse().ok())
        .flatten()
}

/// `text` as exactly `N` dot-separated [`number`]s.
fn numbers<const N: usize>(text: &str) -> Option<[u64; N]> {
    let parts: Vec<Option<u64>> = text.split('.').map(number).collect();
    let all: Option<Vec<u64>> = parts.into_iter().collect();
    all.and_then(|all| <[u64; N]>::try_from(all).ok())
}

/// A release version, `X.Y.Z`: `X.Y` from the `VERSION` file, `Z` from the history (see
/// `version`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Version {
    pub(crate) major: u64,
    pub(crate) minor: u64,
    pub(crate) patch: u64,
}

impl Version {
    /// The release tag of this version: `v<X.Y.Z>`.
    pub(crate) fn tag(&self) -> String {
        format!("v{self}")
    }

    /// The version a release tag names, none for anything that is not `v<X.Y.Z>`.
    pub(crate) fn of_tag(tag: &str) -> Option<Version> {
        tag.strip_prefix('v').and_then(|v| v.parse().ok())
    }
}

impl FromStr for Version {
    type Err = BadValue;

    fn from_str(text: &str) -> Result<Version, BadValue> {
        let [major, minor, patch] =
            numbers::<3>(text).ok_or_else(|| BadValue::Version(text.to_owned()))?;
        Ok(Version {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The text of the `VERSION` file: `X.Y`, with one optional line break after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Base {
    pub(crate) major: u64,
    pub(crate) minor: u64,
}

impl Base {
    /// The version with this base and `patch`.
    pub(crate) fn with_patch(self, patch: u64) -> Version {
        Version {
            major: self.major,
            minor: self.minor,
            patch,
        }
    }
}

impl FromStr for Base {
    type Err = BadValue;

    fn from_str(text: &str) -> Result<Base, BadValue> {
        let line = text.strip_suffix('\n').unwrap_or(text);
        let [major, minor] = numbers::<2>(line).ok_or_else(|| BadValue::Base(text.to_owned()))?;
        Ok(Base { major, minor })
    }
}

/// A GitHub repository, `<owner>/<name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Repo(String);

impl Repo {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Repo {
    type Err = BadValue;

    fn from_str(text: &str) -> Result<Repo, BadValue> {
        let part = |p: &str| {
            !p.is_empty()
                & p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'-' | b'_' | b'.'))
        };
        match text.split_once('/') {
            Some((owner, name)) if part(owner) & part(name) => Ok(Repo(text.to_owned())),
            _ => Err(BadValue::Repo(text.to_owned())),
        }
    }
}

impl fmt::Display for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The text a shipped binary prints for `--version`: `hostkeeper X.Y.Z+<12 hex>`, the release
/// version and the first [`SHORT_COMMIT`] digits of the commit it was built from.
pub(crate) fn stamp_string(version: &Version, commit: &Commit) -> String {
    format!("{PROGRAM} {version}+{}", commit.short())
}

/// A platform: an OS and CPU family. The set is closed; the daemon knows its own from the target
/// it was compiled for. The instruction-set levels inside a family are the build's business, held
/// inside one package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, clap::ValueEnum)]
pub(crate) enum Platform {
    #[value(name = "linux-x86_64")]
    LinuxX86_64,
    #[value(name = "linux-arm64")]
    LinuxArm64,
    #[value(name = "macos-arm64")]
    MacosArm64,
}

impl Platform {
    /// Every platform, in the order of their names.
    pub(crate) const ALL: [Platform; 3] = [
        Platform::LinuxArm64,
        Platform::LinuxX86_64,
        Platform::MacosArm64,
    ];

    /// The platforms a release ships today: the Linux ones. The macOS artifacts wait for the
    /// macOS daemon (design build item B18), which adds macos-arm64 here.
    pub(crate) const RELEASED: [Platform; 2] = [Platform::LinuxX86_64, Platform::LinuxArm64];

    /// The platform's name, in asset names, the release index and a manifest.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::MacosArm64 => "macos-arm64",
        }
    }

    /// The platform a name names, none for any other text.
    pub(crate) fn of_name(name: &str) -> Option<Platform> {
        Platform::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The architecture as a deb and as an rpm name it, for a Linux platform; none for macOS,
    /// which has neither.
    fn linux_arches(self) -> Option<(&'static str, &'static str)> {
        match self {
            Platform::LinuxX86_64 => Some(("amd64", "x86_64")),
            Platform::LinuxArm64 => Some(("arm64", "aarch64")),
            Platform::MacosArm64 => None,
        }
    }
}

/// The file name of the hostkeeper package of `version` for `platform`.
pub(crate) fn package_name(version: &Version, platform: Platform) -> String {
    format!("{PROGRAM}-{version}-{}{PACKAGE_SUFFIX}", platform.name())
}

/// The file name of the bundle of the asset `name`.
pub(crate) fn bundle_of(name: &str) -> String {
    format!("{name}{BUNDLE_SUFFIX}")
}

/// Whether the file name `name` is a provenance bundle's.
pub(crate) fn is_bundle(name: &str) -> bool {
    name.ends_with(BUNDLE_SUFFIX)
}

/// Every asset of the release of `version` for `platforms`, sorted. This is the one list of
/// asset names; [`SUMS`] lists every one of them that is neither itself nor a bundle, and every
/// asset with a bundle (`<asset>.sigstore.json`) is attested. Per platform `<p>`:
///
/// - `hostkeeper-<v>-<p>.tar.gz`, the binary;
/// - `hostkeeper-<v>-<p>.tar.zst`, the hostkeeper package, and its `.sha256`;
/// - `hostkeeper-<v>-<p>.cdx.json`, the SBOM;
///
/// and on Linux, with the deb and rpm architecture `<deb>` and `<rpm>`:
///
/// - `hostkeeper-coordinator-<v>-<p>.tar.gz`, the coordinator's binary;
/// - `hostkeeper_<v>_<deb>.deb` and `hostkeeper-<v>-1.<rpm>.rpm`, the Linux packages;
/// - `hostkeeper-coordinator-<v>-<p>.image.tar`, the coordinator's OCI image;
/// - `hostkeeper-coordinator-<v>-<p>.cdx.json`, its SBOM;
///
/// the bundle of each binary tarball, Linux package, hostkeeper package and image; and once,
/// [`INDEX`] (the release index of the hostkeeper packages) and `SHA256SUMS`.
pub(crate) fn asset_names(version: &Version, platforms: &[Platform]) -> Vec<String> {
    let mut names = vec![SUMS.to_owned(), INDEX.to_owned()];
    for &platform in platforms {
        let p = platform.name();
        let package = package_name(version, platform);
        let mut attested = vec![format!("{PROGRAM}-{version}-{p}.tar.gz"), package.clone()];
        names.push(format!("{package}{DIGEST_SUFFIX}"));
        names.push(format!("{PROGRAM}-{version}-{p}.cdx.json"));
        if let Some((deb, rpm)) = platform.linux_arches() {
            attested.extend([
                format!("{COORDINATOR}-{version}-{p}.tar.gz"),
                format!("{PROGRAM}_{version}_{deb}.deb"),
                format!("{PROGRAM}-{version}-1.{rpm}.rpm"),
                format!("{COORDINATOR}-{version}-{p}.image.tar"),
            ]);
            names.push(format!("{COORDINATOR}-{version}-{p}.cdx.json"));
        }
        names.extend(attested.iter().map(|a| bundle_of(a)));
        names.extend(attested);
    }
    names.sort();
    names
}

/// The assets of `version` that [`SUMS`] lists: all but itself and the bundles.
pub(crate) fn summed_names(version: &Version) -> Vec<String> {
    asset_names(version, &Platform::RELEASED)
        .into_iter()
        .filter(|n| (n != SUMS) & !is_bundle(n))
        .collect()
}

/// The assets of `version` that are attested: those with a bundle.
pub(crate) fn attested_names(version: &Version) -> Vec<String> {
    asset_names(version, &Platform::RELEASED)
        .iter()
        .filter_map(|n| n.strip_suffix(BUNDLE_SUFFIX))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests;
