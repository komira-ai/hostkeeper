//! The directories the record and the switch work in, as the host names them (design section 4.7,
//! "on the host"). Every path here is the host's own spelling (`/etc/systemd/system/...`); the
//! [`crate::Fs`] the caller gives decides where on disk it is read and written, which is how the
//! tests run under a scratch directory with no real root.

use std::path::{Path, PathBuf};

/// The daemon's state directory, which holds `placed.json`.
pub const STATE_DIR: &str = "/var/lib/hostkeeper";
/// The directory of every service's package directories.
pub const PACKAGES_DIR: &str = "/var/lib/hostkeeper/packages";
/// The systemd unit directory on Linux.
pub const SYSTEMD_UNIT_DIR: &str = "/etc/systemd/system";
/// The name of the record of installed services in the state directory.
pub const PLACED_FILE: &str = "placed.json";
/// The name of the lock file in the state directory: taken while `placed.json` is read and
/// written, and around `daemon-reload` (design section 8.7).
pub const LOCK_FILE: &str = "placed.lock";

/// The package directories of every service (design section 4.7):
///
/// ```text
/// <packages>/<service>/<version>/   a version directory
/// <packages>/<service>/current  -> <version>
/// <packages>/<service>/previous -> <version>
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDirs {
    dir: PathBuf,
}

impl PackageDirs {
    pub fn new(dir: &Path) -> PackageDirs {
        PackageDirs {
            dir: dir.to_path_buf(),
        }
    }

    /// The directory of every service's package directories.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<packages>/<service>`.
    pub fn service(&self, service: &str) -> PathBuf {
        self.dir.join(service)
    }

    /// `<packages>/<service>/current`, the link to the version the host runs.
    pub fn current(&self, service: &str) -> PathBuf {
        self.service(service).join("current")
    }

    /// `<packages>/<service>/previous`, the link to the version run before the last switch.
    pub fn previous(&self, service: &str) -> PathBuf {
        self.service(service).join("previous")
    }

    /// `<packages>/<service>/<version>`, a version directory.
    pub fn version(&self, service: &str, version: &str) -> PathBuf {
        self.service(service).join(version)
    }
}

/// The directories of one host: the state directory, the unit directory and the package
/// directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    state_dir: PathBuf,
    unit_dir: PathBuf,
    packages: PackageDirs,
}

impl Roots {
    /// The directories of a host whose unit directory is `unit_dir` (the launchd directory on
    /// macOS, passed by the caller).
    pub fn new(state_dir: &Path, unit_dir: &Path, packages: &Path) -> Roots {
        Roots {
            state_dir: state_dir.to_path_buf(),
            unit_dir: unit_dir.to_path_buf(),
            packages: PackageDirs::new(packages),
        }
    }

    /// The directories of a Linux host: [`STATE_DIR`], [`SYSTEMD_UNIT_DIR`], [`PACKAGES_DIR`].
    pub fn linux() -> Roots {
        Roots::new(
            Path::new(STATE_DIR),
            Path::new(SYSTEMD_UNIT_DIR),
            Path::new(PACKAGES_DIR),
        )
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn unit_dir(&self) -> &Path {
        &self.unit_dir
    }

    pub fn packages(&self) -> &PackageDirs {
        &self.packages
    }

    /// `<state dir>/placed.json`.
    pub fn placed_file(&self) -> PathBuf {
        self.state_dir.join(PLACED_FILE)
    }

    /// `<state dir>/placed.lock`.
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join(LOCK_FILE)
    }
}
