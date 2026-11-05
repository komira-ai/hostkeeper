//! The paths and names that differ between a Linux host and a Mac.

use std::path::PathBuf;

use crate::label::REVERSE_DNS_PREFIX;

/// Which service manager the host runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatformKind {
    Linux,
    Macos,
}

/// Where a host keeps its unit files and the admin socket, and how it names its jobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    pub kind: PlatformKind,
    /// The directory the service manager reads unit files or plists from.
    pub unit_dir: PathBuf,
    /// The end of a unit file's name.
    pub unit_suffix: &'static str,
    pub admin_socket: PathBuf,
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
            admin_socket: PathBuf::from("/run/hostkeeper/admin.sock"),
            label_prefix: None,
        }
    }

    /// A launchd host.
    pub fn macos() -> Platform {
        Platform {
            kind: PlatformKind::Macos,
            unit_dir: PathBuf::from("/Library/LaunchDaemons"),
            unit_suffix: ".plist",
            admin_socket: PathBuf::from("/var/run/hostkeeper/admin.sock"),
            label_prefix: Some(REVERSE_DNS_PREFIX),
        }
    }

    /// The platform this binary was built for.
    pub fn host() -> Platform {
        Platform::for_os(cfg!(target_os = "macos"))
    }

    /// The Mac platform when `macos` is set, the Linux one otherwise.
    pub fn for_os(macos: bool) -> Platform {
        match macos {
            true => Platform::macos(),
            false => Platform::linux(),
        }
    }
}
