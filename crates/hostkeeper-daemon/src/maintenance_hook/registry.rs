//! The kinds of maintenance a platform has work for ([`Registry::for_platform`]). Each is a stub
//! ([`Unsupported`]) until its platform's work is written; that work is a file of its own beside
//! this one, put in its place by [`Registry::at_host`]: the `linux` kind's stage ([`Linux`]).

use std::collections::BTreeMap;

use hostkeeper_launchd::PlatformKind;

use super::kind::{KindError, MaintenanceKind, Unsupported};
use super::linux::Linux;
use super::params::Kind;

/// The work of each kind a host has; a kind it does not hold fails `unsupported_kind`.
#[derive(Default)]
pub struct Registry {
    kinds: BTreeMap<Kind, Box<dyn MaintenanceKind>>,
}

impl Registry {
    /// No kind: each fails `unsupported_kind`.
    pub fn new() -> Registry {
        Registry::default()
    }

    /// The kinds a host of `platform` has: `macos`, `xcode` and `reboot` on a Mac; `linux` and
    /// `reboot` on Linux, under systemd or OpenRC.
    pub fn for_platform(platform: PlatformKind) -> Registry {
        let kinds: &[Kind] = match platform {
            PlatformKind::Macos => &[Kind::Macos, Kind::Xcode, Kind::Reboot],
            PlatformKind::Linux | PlatformKind::Openrc => &[Kind::Linux, Kind::Reboot],
        };
        kinds.iter().fold(Registry::new(), |registry, kind| {
            registry.with(*kind, Box::new(Unsupported(*kind)))
        })
    }

    /// The kinds of a host of `platform`, as [`Registry::for_platform`] has them, with the work
    /// written so far in place of its stub: on Linux, the `linux` kind's stage ([`Linux`]), which
    /// waits `lock_wait_secs` at most for a held package manager's lock.
    pub fn at_host(platform: PlatformKind, lock_wait_secs: u64) -> Registry {
        let registry = Registry::for_platform(platform);
        match platform {
            PlatformKind::Linux | PlatformKind::Openrc => {
                registry.with(Kind::Linux, Box::new(Linux::at_host(lock_wait_secs)))
            }
            PlatformKind::Macos => registry,
        }
    }

    /// This registry with `work` as the work of `kind`, in place of what it held.
    pub fn with(mut self, kind: Kind, work: Box<dyn MaintenanceKind>) -> Registry {
        self.kinds.insert(kind, work);
        self
    }

    /// The kinds this registry holds work for, in [`Kind::ALL`]'s order.
    pub fn kinds(&self) -> impl Iterator<Item = Kind> + '_ {
        self.kinds.keys().copied()
    }

    /// The work of `kind`, or the `unsupported_kind` failure when this registry holds none.
    pub fn get(&self, kind: Kind) -> Result<&dyn MaintenanceKind, KindError> {
        self.kinds
            .get(&kind)
            .map(|work| &**work)
            .ok_or_else(|| KindError::unsupported(kind))
    }
}
