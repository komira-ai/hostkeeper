//! The paths and names that differ between a Linux host under systemd, a Linux host under OpenRC
//! and a Mac.
//!
//! Which service manager a Linux host runs is read from the host when the daemon starts
//! ([`Platform::host`]), never from the name of its distribution: systemd makes
//! [`SYSTEMD_MARKER`] when it boots the system (the test `sd_booted(3)` documents), and OpenRC makes
//! [`OPENRC_MARKER`] when it starts its first runlevel. A host with the first is a systemd host,
//! whatever else it has; a host with only the second is an OpenRC host; a host with neither is
//! taken for a systemd host, as every build before OpenRC's took it.

use std::path::{Path, PathBuf};

use crate::label::REVERSE_DNS_PREFIX;

/// The directory systemd makes at boot when it is the system's service manager.
pub const SYSTEMD_MARKER: &str = "/run/systemd/system";
/// The directory OpenRC makes when it starts its first runlevel: its state of the services.
pub const OPENRC_MARKER: &str = "/run/openrc";

/// Which service manager the host runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformKind {
    /// Linux under systemd.
    Linux,
    Macos,
    /// Linux under OpenRC (Alpine): the daemon is a `supervise-daemon` service.
    Openrc,
}

/// Where a host keeps its unit files and its run directory, and how it names its jobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub kind: PlatformKind,
    /// The directory the service manager reads unit files, plists or init scripts from.
    pub unit_dir: PathBuf,
    /// The end of a unit file's name (an OpenRC init script's name has none).
    pub unit_suffix: &'static str,
    /// The run directory: `/run` on Linux, `/var/run` on macOS. kbf's agent kept its
    /// registration socket in `updater` under it, which the daemon removes at start.
    pub run_dir: PathBuf,
    /// The reverse-DNS prefix of a job label; Linux unit names have none.
    pub label_prefix: Option<&'static str>,
}

impl Platform {
    /// A systemd host.
    pub fn linux() -> Platform {
        Platform {
            kind: PlatformKind::Linux,
            unit_dir: PathBuf::from("/etc/systemd/system"),
            unit_suffix: ".service",
            run_dir: PathBuf::from("/run"),
            label_prefix: None,
        }
    }

    /// A launchd host.
    pub fn macos() -> Platform {
        Platform {
            kind: PlatformKind::Macos,
            unit_dir: PathBuf::from("/Library/LaunchDaemons"),
            unit_suffix: ".plist",
            run_dir: PathBuf::from("/var/run"),
            label_prefix: Some(REVERSE_DNS_PREFIX),
        }
    }

    /// An OpenRC host: init scripts in `/etc/init.d`, the run directory a systemd host has.
    pub fn openrc() -> Platform {
        Platform {
            kind: PlatformKind::Openrc,
            unit_dir: PathBuf::from("/etc/init.d"),
            unit_suffix: "",
            run_dir: PathBuf::from("/run"),
            label_prefix: None,
        }
    }

    /// The platform of this host: [`Platform::detect`] for the system this binary was built
    /// for, its markers looked up on the host's filesystem.
    pub fn host() -> Platform {
        Platform::detect(cfg!(target_os = "macos"), &Path::is_dir)
    }

    /// The Mac platform when `macos` is set; otherwise the Linux platform whose service manager
    /// `is_dir` finds: systemd when [`SYSTEMD_MARKER`] is a directory, else OpenRC when
    /// [`OPENRC_MARKER`] is one, else systemd.
    pub fn detect(macos: bool, is_dir: &dyn Fn(&Path) -> bool) -> Platform {
        if macos {
            return Platform::macos();
        }
        let openrc = !is_dir(Path::new(SYSTEMD_MARKER)) && is_dir(Path::new(OPENRC_MARKER));
        match openrc {
            true => Platform::openrc(),
            false => Platform::linux(),
        }
    }

    /// The Mac platform when `macos` is set, the systemd one otherwise.
    pub fn for_os(macos: bool) -> Platform {
        match macos {
            true => Platform::macos(),
            false => Platform::linux(),
        }
    }
}
