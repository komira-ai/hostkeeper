//! The release contract: every name and format a consumer of a hostkeeper release relies on, in
//! one module. A consumer checks a release's provenance before it trusts anything in it (design
//! section 4.5), so these are a public interface and change additively only:
//!
//! - the version `X.Y.Z` ([`Version`]) and the tag `v<X.Y.Z>` ([`Version::tag`]);
//! - the text a shipped binary prints for `--version`, `hostkeeper X.Y.Z+<12 hex>`
//!   ([`stamp_string`]), `hostkeeper-coordinator X.Y.Z+<12 hex>` and `hostkeeper-mdm
//!   X.Y.Z+<12 hex>` ([`stamp_of`]), and [`RELEASE_COMMIT`], the build-time input that carries
//!   the commit;
//! - the identity that signs every attestation: [`SIGNER_WORKFLOW`] on [`SIGNER_REF`] of
//!   [`REPO`], on a hosted runner;
//! - the asset names of a release ([`asset_names`], the one function that lists them; the
//!   committed baseline `crates/xtask/tests/release/asset_names.baseline` holds them).
//!
//! There are exactly three platforms, linux-x86_64, linux-arm64 and macos-arm64
//! ([`Platform::ALL`]): one package per OS and CPU family, listed in the release index of its
//! package (see `index`). A release ships every one of them ([`Platform::RELEASED`]), and a
//! release missing one is refused. macos-arm64 also ships the installer pkg of a Mac's first
//! install ([`installer_name`]); the two Linux platforms also ship the coordinator
//! ([`COORDINATOR`]) and the MDM kit's binary ([`MDM`]). A release carries four kinds of package ([`Package`]):
//! hostkeeper's own (`index.json`), `hello`, the example service of `examples/hello`
//! (`hello-index.json`), hello's failing build (`hello-fail-ready-index.json`), and `mdm`, the
//! MDM server stack as a service package (`mdm-index.json`), for linux-x86_64 only
//! ([`Package::platforms`]). It also
//! carries `netboot.json` ([`NETBOOT`]), the boot sets of network install for the two Linux
//! platforms, and `hkctl-<version>` ([`hkctl_name`]), the command line, one script for every
//! platform.

use std::fmt;
use std::str::FromStr;

use hostkeeper_config::channel_trust::{Repository, TrustedService};
use hostkeeper_config::daemon::TrustedSigners;

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

/// The name of the example service, `examples/hello`: its package name, binary and release index
/// stem.
pub(crate) const HELLO: &str = "hello";

/// The coordinator's asset stem, and the name of its binary.
pub(crate) const COORDINATOR: &str = "hostkeeper-coordinator";

/// The asset stem of the MDM kit's binary, and the name of that binary: the Linux host of the
/// MDM server stack runs it (`crates/hostkeeper-mdm`). Released for the two Linux platforms only,
/// as a tarball with its SBOM.
pub(crate) const MDM: &str = "hostkeeper-mdm";

/// The name of the MDM server stack's service package (docs/design/two-programs.md section 6.2):
/// its package name, the service of its release index and its program `bin/mdm`, which prints
/// `mdm X.Y.Z+<12 hex>` for `--version`. Built for linux-x86_64 only: the pinned tools it carries
/// are linux-amd64 builds.
pub(crate) const MDM_SERVICE: &str = "mdm";

/// Where the coordinator's container image holds the operator web app (`ui/` of the commit):
/// the destination of the Dockerfile's `COPY ui`, and the default UI directory of the
/// coordinator's `serve` (`hostkeeper_coordinator::config::DEFAULT_UI_DIR`).
pub(crate) const IMAGE_UI_DIR: &str = "/usr/share/hostkeeper-coordinator/ui";

/// The command line's program name and the stem of its asset, `hkctl-<version>`: one bash
/// script for every platform, which prints `hkctl X.Y.Z+<12 hex>` for `--version`
/// (`release hkctl`).
pub(crate) const HKCTL: &str = hostkeeper_hkctl::PROGRAM;

/// The binaries a release ships, by file name: each prints `<name> X.Y.Z+<12 hex>` for
/// `--version` ([`stamp_of`]).
pub(crate) const PROGRAMS: [&str; 3] = [PROGRAM, COORDINATOR, MDM];

/// The binaries `release check-binary` holds to their stamp ([`stamp_of`]): the shipped binaries
/// of [`PROGRAMS`] and `mdm`, the program the mdm package carries as `bin/mdm`.
pub(crate) const STAMPED: [&str; 4] = [PROGRAMS[0], PROGRAMS[1], PROGRAMS[2], MDM_SERVICE];

/// The file listing the SHA-256 of every asset but itself and the bundles.
pub(crate) const SUMS: &str = "SHA256SUMS";

/// The release index: which package file serves each platform (see `index`).
pub(crate) const INDEX: &str = "index.json";

/// The release index of the hello packages: the same schema as [`INDEX`], `service` `hello`.
pub(crate) const HELLO_INDEX: &str = "hello-index.json";

/// The release index of the failing build of hello: the same schema as [`INDEX`], `service`
/// `hello`, its version the release's followed by [`FAIL_READY_SUFFIX`].
pub(crate) const HELLO_FAIL_READY_INDEX: &str = "hello-fail-ready-index.json";

/// The release index of the mdm package: the same schema as [`INDEX`], `service` `mdm`, its one
/// platform linux-x86_64.
pub(crate) const MDM_INDEX: &str = "mdm-index.json";

/// The boot sets of network install: for each Linux platform the Ubuntu ISO, shim, network
/// GRUB, kernel and initrd a bare machine boots, each with its size and SHA-256 (see `netboot`).
/// Attested, with a bundle of its own.
pub(crate) const NETBOOT: &str = "netboot.json";

/// What a provenance bundle's file name adds to its asset's.
pub(crate) const BUNDLE_SUFFIX: &str = ".sigstore.json";

/// What the hostkeeper package's file name ends in: the archive and its compression. The
/// compression itself is chosen in `package::compress`; the two change together.
pub(crate) const PACKAGE_SUFFIX: &str = ".tar.zst";

/// What the file holding the package's SHA-256 adds to the package's name.
pub(crate) const DIGEST_SUFFIX: &str = ".sha256";

/// What the file name of the macOS installer ends in: a flat installer package.
pub(crate) const INSTALLER_SUFFIX: &str = ".pkg";

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

    /// The grammar of the trust list's [`Repository`], so the two cannot differ.
    fn from_str(text: &str) -> Result<Repo, BadValue> {
        Repository::new(text)
            .map(|r| Repo::from(&r))
            .ok_or_else(|| BadValue::Repo(text.to_owned()))
    }
}

impl From<&Repository> for Repo {
    fn from(repository: &Repository) -> Repo {
        Repo(repository.as_str().to_owned())
    }
}

impl fmt::Display for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The identity whose attestations a repository's release is checked against: a workflow of the
/// repository, on the refs and runners of a [`TrustedSigners`], the daemon config's type.
/// Hostkeeper's own is [`Signer::hostkeeper`], the [`SIGNER_WORKFLOW`] of [`REPO`] on
/// [`SIGNER_REF`]; `release channel-sync` holds each service it stages to the signer of its
/// entry in the trust list ([`Signer::of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signer {
    /// The repository whose attestations count.
    pub(crate) repo: String,
    /// The workflow that signs, `<owner>/<repo>/<path>`.
    pub(crate) workflow: String,
    /// The workflow's path in the repository, the refs it may run on (globs), and whether it must
    /// run on a hosted runner; `organisation` is the repository's owner.
    pub(crate) signers: TrustedSigners,
}

impl Signer {
    /// The signer of the trust list's entry `trusted`: its workflow in its repository.
    pub(crate) fn of(trusted: &TrustedService) -> Signer {
        Signer {
            repo: trusted.repository.to_string(),
            workflow: format!("{}/{}", trusted.repository, trusted.signers.workflow),
            signers: trusted.signers.clone(),
        }
    }

    /// The signer of hostkeeper's own releases: [`SIGNER_WORKFLOW`] of [`REPO`] on
    /// [`SIGNER_REF`], on a hosted runner.
    pub(crate) fn hostkeeper() -> Signer {
        Signer {
            repo: REPO.to_owned(),
            workflow: SIGNER_WORKFLOW.to_owned(),
            signers: TrustedSigners {
                organisation: REPO.split('/').next().unwrap_or_default().to_owned(),
                workflow: format!(".github/workflows/{ARTIFACTS_WORKFLOW}"),
                refs: vec![SIGNER_REF.to_owned()],
                hosted_runner: true,
            },
        }
    }

    /// The one ref the signing run must have run on, when the signer allows exactly one and it
    /// is not a glob: what `gh attestation verify --source-ref` is given. Otherwise the ref is
    /// matched against the globs after the verifier answers (`check::judge`).
    pub(crate) fn exact_ref(&self) -> Option<&str> {
        match self.signers.refs.as_slice() {
            [one] if !one.contains('*') => Some(one),
            _ => None,
        }
    }

    /// The refs the signer allows, for a message: `a`, or `a or b`.
    pub(crate) fn refs_text(&self) -> String {
        self.signers.refs.join(" or ")
    }
}

/// The text the hostkeeper binary prints for `--version`: `hostkeeper X.Y.Z+<12 hex>`, the
/// release version and the first [`SHORT_COMMIT`] digits of the commit it was built from.
pub(crate) fn stamp_string(version: &Version, commit: &Commit) -> String {
    stamp_of(PROGRAM, version, commit)
}

/// The text the shipped binary `program` (one of [`STAMPED`]) prints for `--version`:
/// `<program> X.Y.Z+<12 hex>`. The coordinator's is `hostkeeper-coordinator X.Y.Z+<12 hex>`, the
/// MDM kit's `hostkeeper-mdm X.Y.Z+<12 hex>`.
pub(crate) fn stamp_of(program: &str, version: &Version, commit: &Commit) -> String {
    format!("{program} {version}+{}", commit.short())
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

    /// The platforms a release ships: all three. A release that lacks the assets of one of them
    /// is refused (`check`), and so is an index without its package (`index`).
    pub(crate) const RELEASED: [Platform; 3] = [
        Platform::LinuxX86_64,
        Platform::LinuxArm64,
        Platform::MacosArm64,
    ];

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
    pub(crate) fn linux_arches(self) -> Option<(&'static str, &'static str)> {
        match self {
            Platform::LinuxX86_64 => Some(("amd64", "x86_64")),
            Platform::LinuxArm64 => Some(("arm64", "aarch64")),
            Platform::MacosArm64 => None,
        }
    }
}

/// A service whose package `release package` and `release index` build. Hostkeeper is the
/// default of every verb that takes one, so a verb given no service does what it did before there
/// were two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Service {
    /// The daemon's own package, for its self-update.
    #[default]
    #[value(name = "hostkeeper")]
    Hostkeeper,
    /// The example service of `examples/hello`.
    #[value(name = "hello")]
    Hello,
    /// The MDM server stack's service package.
    #[value(name = "mdm")]
    Mdm,
}

impl Service {
    /// The service's name: its package name, its binary and the `service` of its release index.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Service::Hostkeeper => PROGRAM,
            Service::Hello => HELLO,
            Service::Mdm => MDM_SERVICE,
        }
    }
}

/// What a version is followed by in the version of the failing build of hello.
pub(crate) const FAIL_READY_SUFFIX: &str = "-fail-ready";

/// One kind of package a release carries: each has a package per platform it is built for
/// ([`Package::platforms`]) and a release index of its own. The release carries hostkeeper's
/// package, hello's and hello's failing build, the same package started with `--fail-ready`,
/// which a host can be rolled back from (a host trusts only packages this repository's workflow
/// attested, so the failing build is released and attested like the others), and mdm's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Package {
    Hostkeeper,
    Hello,
    HelloFailReady,
    Mdm,
}

/// The platforms of the mdm package.
const MDM_PLATFORMS: [Platform; 1] = [Platform::LinuxX86_64];

impl Package {
    /// Every package, hostkeeper's first.
    pub(crate) const ALL: [Package; 4] = [
        Package::Hostkeeper,
        Package::Hello,
        Package::HelloFailReady,
        Package::Mdm,
    ];

    /// The platforms a release ships the package for, in [`Platform::RELEASED`] order: every one,
    /// but linux-x86_64 alone for mdm.
    pub(crate) fn platforms(self) -> &'static [Platform] {
        match self {
            Package::Hostkeeper | Package::Hello | Package::HelloFailReady => &Platform::RELEASED,
            Package::Mdm => &MDM_PLATFORMS,
        }
    }

    /// The package `service` gives with or without `--fail-ready`; none for hostkeeper's failing
    /// build, which does not exist.
    pub(crate) fn of(service: Service, fail_ready: bool) -> Option<Package> {
        match (service, fail_ready) {
            (Service::Hostkeeper, false) => Some(Package::Hostkeeper),
            (Service::Hello, false) => Some(Package::Hello),
            (Service::Hello, true) => Some(Package::HelloFailReady),
            (Service::Mdm, false) => Some(Package::Mdm),
            (Service::Hostkeeper | Service::Mdm, true) => None,
        }
    }

    /// The service the package is of: its name is the package's, binary's and index's `service`.
    pub(crate) fn service(self) -> Service {
        match self {
            Package::Hostkeeper => Service::Hostkeeper,
            Package::Hello | Package::HelloFailReady => Service::Hello,
            Package::Mdm => Service::Mdm,
        }
    }

    /// The version the package, its manifest and its index carry for the release `version`: the
    /// release's own, or for the failing build the release's followed by [`FAIL_READY_SUFFIX`].
    pub(crate) fn version_text(self, version: &Version) -> String {
        match self {
            Package::Hostkeeper | Package::Hello | Package::Mdm => version.to_string(),
            Package::HelloFailReady => format!("{version}{FAIL_READY_SUFFIX}"),
        }
    }

    /// The file name of the package's release index.
    pub(crate) fn index_name(self) -> &'static str {
        match self {
            Package::Hostkeeper => INDEX,
            Package::Hello => HELLO_INDEX,
            Package::HelloFailReady => HELLO_FAIL_READY_INDEX,
            Package::Mdm => MDM_INDEX,
        }
    }

    /// The file name of the package of the release `version` for `platform`.
    pub(crate) fn package_name(self, version: &Version, platform: Platform) -> String {
        format!(
            "{}-{}-{}{PACKAGE_SUFFIX}",
            self.service().name(),
            self.version_text(version),
            platform.name()
        )
    }
}

/// The file name of the installer pkg of `version`, the first install of a Mac (design section
/// 5.9): `hostkeeper-<v>-macos-arm64.pkg`.
pub(crate) fn installer_name(version: &Version) -> String {
    format!(
        "{PROGRAM}-{version}-{}{INSTALLER_SUFFIX}",
        Platform::MacosArm64.name()
    )
}

/// The file name of the command line of `version`, `hkctl-<v>`.
pub(crate) fn hkctl_name(version: &Version) -> String {
    format!("{HKCTL}-{version}")
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
/// - `hello-<v>-<p>.tar.zst`, the hello package, and its `.sha256`;
/// - `hello-<v>-fail-ready-<p>.tar.zst`, the failing build of hello, and its `.sha256`;
/// - `mdm-<v>-<p>.tar.zst`, the mdm package, and its `.sha256`, for the platforms of
///   [`Package::platforms`] only (linux-x86_64);
/// - `hostkeeper-<v>-<p>.cdx.json`, the SBOM;
///
/// and on Linux, with the deb and rpm architecture `<deb>` and `<rpm>`:
///
/// - `hostkeeper-coordinator-<v>-<p>.tar.gz`, the coordinator's binary;
/// - `hostkeeper_<v>_<deb>.deb` and `hostkeeper-<v>-1.<rpm>.rpm`, the Linux packages;
/// - `hostkeeper-coordinator-<v>-<p>.image.tar`, the coordinator's OCI image;
/// - `hostkeeper-coordinator-<v>-<p>.cdx.json`, its SBOM;
/// - `hostkeeper-mdm-<v>-<p>.tar.gz`, the MDM kit's binary, and `hostkeeper-mdm-<v>-<p>.cdx.json`,
///   its SBOM;
///
/// and on macOS `hostkeeper-<v>-macos-arm64.pkg`, the installer ([`installer_name`]);
///
/// the bundle of each binary tarball (the three programs'), Linux package, installer, hostkeeper package, hello package,
/// hello's failing build, mdm package and image; and once, [`INDEX`] (the release index of the
/// hostkeeper packages), [`HELLO_INDEX`] (that of the hello packages), [`HELLO_FAIL_READY_INDEX`]
/// (that of the failing build), [`MDM_INDEX`] (that of the mdm package), [`NETBOOT`] (the boot sets of network install) and its bundle, `hkctl-<v>`
/// (the command line, [`hkctl_name`]) and its bundle, and `SHA256SUMS`.
pub(crate) fn asset_names(version: &Version, platforms: &[Platform]) -> Vec<String> {
    let hkctl = hkctl_name(version);
    let mut names = vec![
        SUMS.to_owned(),
        NETBOOT.to_owned(),
        bundle_of(NETBOOT),
        bundle_of(&hkctl),
        hkctl,
    ];
    names.extend(Package::ALL.map(|p| p.index_name().to_owned()));
    for &platform in platforms {
        let p = platform.name();
        let mut attested = vec![format!("{PROGRAM}-{version}-{p}.tar.gz")];
        for kind in Package::ALL
            .iter()
            .filter(|k| k.platforms().contains(&platform))
        {
            let package = kind.package_name(version, platform);
            names.push(format!("{package}{DIGEST_SUFFIX}"));
            attested.push(package);
        }
        names.push(format!("{PROGRAM}-{version}-{p}.cdx.json"));
        if let Some((deb, rpm)) = platform.linux_arches() {
            attested.extend([
                format!("{COORDINATOR}-{version}-{p}.tar.gz"),
                format!("{PROGRAM}_{version}_{deb}.deb"),
                format!("{PROGRAM}-{version}-1.{rpm}.rpm"),
                format!("{COORDINATOR}-{version}-{p}.image.tar"),
                format!("{MDM}-{version}-{p}.tar.gz"),
            ]);
            names.push(format!("{COORDINATOR}-{version}-{p}.cdx.json"));
            names.push(format!("{MDM}-{version}-{p}.cdx.json"));
        }
        if platform == Platform::MacosArm64 {
            attested.push(installer_name(version));
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
