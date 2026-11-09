//! The installer contract (packages.md section 5.1) and the registry of this build's installers:
//! the code that turns a verified artifact into installed software, and takes it away again.
//!
//! | Step       | Does                                                               | Must not |
//! |------------|--------------------------------------------------------------------|----------|
//! | `stage`    | Makes the version directory from the verified artifact             | Change anything outside the version's staging directory, or run a script of the package |
//! | `activate` | Makes the version the installed one; a rollback activates the kept previous version | Fetch anything; start, stop or restart the service's `run` units; touch another service |
//! | `retire`   | What a version that is neither `current` nor `previous` leaves once the core deleted its directory and seal | Call the format's tool on the host's installed package |
//! | `remove`   | Takes the whole service off the host: what its `activate` recorded | Remove anything `activate` did not record, or anything changed since |
//!
//! The host's side of the steps (the switch of `current` and the placer's links) is the
//! daemon's, behind [`Host`]: a step says what is done and in which order, and the host does it
//! with the write-ahead record it already keeps, so a step is idempotent and a step found
//! unfinished is run again. The deletion of a version's directory and seal is the same for every
//! installer and the core's (the prune, section 5.2); `retire` is what an installer does beyond
//! it. A format's own tool is run through the
//! step launcher ([`hostkeeper_steps::StepLauncher::run_tool`]), by absolute path, bounded, in a
//! unit with no network.
//!
//! The registry ([`steps_of`]) is the closed list of installers the daemon is built with, which
//! no manifest can extend (section 5.1, "Why built in, and not delivered"). This build holds one:
//!
//! - **`hostkeeper-archive`** ([`Archive`]): hostkeeper's own package, its manifest inside the
//!   attested bytes. `stage` is today's unpack, bounds and shape rules exactly
//!   ([`hostkeeper_unpack::unpack`]); `activate` is today's switch and placement; `retire` has
//!   nothing to do beyond the core's deletion; `remove` is today's unlinking of what
//!   `placed.json` records.
//!
//! The repository installer's `apt` backend (A7) and `macos-pkg` (A9) are added here, each a
//! module implementing [`InstallerSteps`].

use std::path::{Path, PathBuf};

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_config::manifest::Manifest;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_config::platform::Platform as PackagePlatform;
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;
use hostkeeper_unpack::{Limits, PackageFs, Platform, UnpackError, UnpackOrder, unpack};

/// Why a step the host does for an installer failed, as the host words it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HostError(pub String);

/// What the host does for an installer's steps: the daemon's placer and package directory, for
/// one service.
pub trait Host {
    /// Switches `current` to `version`, links what its manifest places and reloads the service
    /// manager, durably and so that a repeat changes nothing: today's switch.
    fn switch(&self, version: &Version) -> Result<(), HostError>;
    /// Removes every path the service placed, each only while it is still the link recorded.
    fn unplace(&self) -> Result<(), HostError>;
}

/// What `stage` is given: the order's service, version and package digest, and its entry.
#[derive(Clone, Copy, Debug)]
pub struct StageOrder<'a> {
    /// The service.
    pub service: &'a ServiceName,
    /// The version.
    pub version: &'a Version,
    /// The SHA-256 the artifact was verified to have.
    pub package_digest: &'a Sha256Hex,
    /// The entry the order carries, already judged on receipt; `None` for an order of the
    /// release channel's archive.
    pub entry: Option<&'a Target>,
}

/// How `stage` writes: the bounds, the package directory's port and the platform's checks.
pub struct StageEnv<'a> {
    /// `max_unpacked_bytes` and `max_entries`.
    pub limits: Limits,
    /// The port every write of the stage goes through.
    pub fs: &'a dyn PackageFs,
    /// The checks of the host the stage runs on (macOS: quarantine and code signatures).
    pub platform: Platform<'a>,
}

/// A version directory `stage` made, or found sealed for the same artifact.
#[derive(Clone, Debug, PartialEq)]
pub struct StagedVersion {
    /// `packages/<service>/<version>`.
    pub dir: PathBuf,
    /// `false` when the version was on the host already and nothing was staged.
    pub fresh: bool,
    /// The manifest the stage read from the artifact, for an installer whose manifest is inside
    /// it (`hostkeeper-archive`'s `manifest.toml`), checked against the order.
    pub manifest: Option<Manifest>,
}

/// Why `stage` made no version directory.
#[derive(Debug, thiserror::Error)]
pub enum StageError {
    /// The reader refused the artifact, or the disk failed: the unpack's own outcome and reason
    /// ([`UnpackError::outcome`]).
    #[error("{0}")]
    Unpack(UnpackError),
}

/// The four steps of one installer.
pub trait InstallerSteps: Send + Sync {
    /// The installer.
    fn installer(&self) -> Installer;
    /// Makes the version directory of `order` from the verified artifact at `artifact`.
    fn stage(
        &self,
        artifact: &Path,
        order: &StageOrder<'_>,
        env: StageEnv<'_>,
    ) -> Result<StagedVersion, StageError>;
    /// Makes `version` the installed one.
    fn activate(&self, host: &dyn Host, version: &Version) -> Result<(), HostError>;
    /// Drops what `version`, neither `current` nor `previous`, leaves once the core deleted its
    /// directory and seal.
    fn retire(&self, version: &Version) -> Result<(), HostError>;
    /// Takes the service off the host.
    fn remove(&self, host: &dyn Host) -> Result<(), HostError>;
}

/// The installers this build has the steps of, in the order of their spellings.
pub const REGISTERED: [Installer; 1] = [Installer::HostkeeperArchive];

/// The steps of `installer`, `None` for one this build has no steps of.
pub fn steps_of(installer: Installer) -> Option<&'static dyn InstallerSteps> {
    match installer {
        Installer::HostkeeperArchive => Some(&Archive),
        Installer::Apt | Installer::MacosPkg | Installer::OciImage | Installer::Tarball => None,
    }
}

/// `hostkeeper-archive`: hostkeeper's own package.
#[derive(Clone, Copy, Debug)]
pub struct Archive;

impl InstallerSteps for Archive {
    fn installer(&self) -> Installer {
        Installer::HostkeeperArchive
    }

    /// Today's unpack: one zstd frame of ustar in today's layout, its manifest loaded and
    /// checked against the order, sealed and published. The entry adds nothing: the manifest is
    /// the archive's own.
    fn stage(
        &self,
        artifact: &Path,
        order: &StageOrder<'_>,
        env: StageEnv<'_>,
    ) -> Result<StagedVersion, StageError> {
        let order = UnpackOrder {
            service: order.service.clone(),
            version: order.version.clone(),
            platform: PackagePlatform::current(),
            package_digest: order.package_digest.clone(),
        };
        unpack(artifact, &order, &env.limits, env.fs, env.platform)
            .map(|unpacked| StagedVersion {
                dir: unpacked.dir,
                fresh: unpacked.fresh,
                manifest: Some(unpacked.manifest),
            })
            .map_err(StageError::Unpack)
    }

    fn activate(&self, host: &dyn Host, version: &Version) -> Result<(), HostError> {
        host.switch(version)
    }

    /// Nothing: the version's directory and seal were all it had.
    fn retire(&self, _version: &Version) -> Result<(), HostError> {
        Ok(())
    }

    fn remove(&self, host: &dyn Host) -> Result<(), HostError> {
        host.unplace()
    }
}
