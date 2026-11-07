//! The architecture a client states, the platform that takes, and the boot sets there are.

use std::collections::BTreeMap;
use std::fmt;

use hostkeeper_wire::Platform;

/// The client system architecture of DHCP option 93 (RFC 4578 and the IANA registry of
/// "Processor Architecture Types"): the first value of the option.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientArch(u16);

impl ClientArch {
    /// 7: registered as EFI byte code; x86-64 UEFI firmware commonly sends it, so it is taken
    /// as x86-64 (network-install.md section 3, which marks the board models' values for the
    /// runbook to record).
    pub const EFI_BC: Self = Self(7);
    /// 9: x86-64 UEFI.
    pub const X86_64_UEFI: Self = Self(9);
    /// 11: arm64 UEFI.
    pub const ARM64_UEFI: Self = Self(11);
    /// 16: x86-64 UEFI HTTP boot.
    pub const X86_64_UEFI_HTTP: Self = Self(16);
    /// 19: arm64 UEFI HTTP boot.
    pub const ARM64_UEFI_HTTP: Self = Self(19);

    /// The architecture of this option 93 value.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// The option 93 value.
    #[must_use]
    pub const fn value(self) -> u16 {
        self.0
    }

    /// The platform a client of this architecture is installed as: `linux-x86_64` for 7, 9 and
    /// 16, `linux-arm64` for 11 and 19, and `None` for every other value (0, legacy BIOS; 10,
    /// 32-bit ARM UEFI; the rest), which this design does not install.
    #[must_use]
    pub const fn platform(self) -> Option<Platform> {
        match self.0 {
            7 | 9 | 16 => Some(Platform::LinuxX86_64),
            11 | 19 => Some(Platform::LinuxArm64),
            _ => None,
        }
    }
}

impl fmt::Display for ClientArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for ClientArch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClientArch({})", self.0)
    }
}

/// The names a platform's boot loaders are served by, by TFTP and under the HTTP root alike.
/// Both are in the directory of `hostkeeper/boot.efi` ([`crate::tftp::BOOT_EFI`]): the shim
/// reads GRUB by GRUB's own name from the directory it was itself read from (network-install.md
/// section 3), so a shim served as `hostkeeper/boot.efi` reads `hostkeeper/grubx64.efi`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoaderNames {
    /// The signed shim: `hostkeeper/shimx64.efi` or `hostkeeper/shimaa64.efi`.
    pub shim: &'static str,
    /// The signed network GRUB: `hostkeeper/grubx64.efi` or `hostkeeper/grubaa64.efi`.
    pub grub: &'static str,
}

impl LoaderNames {
    /// The names of `platform`'s boot loaders; `None` for a platform network install does not
    /// boot (`darwin-arm64`).
    #[must_use]
    pub const fn of(platform: Platform) -> Option<Self> {
        match platform {
            Platform::LinuxX86_64 => Some(Self {
                shim: "hostkeeper/shimx64.efi",
                grub: "hostkeeper/grubx64.efi",
            }),
            Platform::LinuxArm64 => Some(Self {
                shim: "hostkeeper/shimaa64.efi",
                grub: "hostkeeper/grubaa64.efi",
            }),
            Platform::DarwinArm64 => None,
        }
    }
}

/// A boot set named for a platform network install does not boot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "no boot set is made for the platform {0}: only linux-x86_64 and linux-arm64 boot from the network"
)]
pub struct BootSetsError(pub Platform);

/// The platforms the sealed directory holds a boot set for (network-install.md section 5),
/// with the names their boot loaders are served by.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BootSets(BTreeMap<Platform, LoaderNames>);

impl BootSets {
    /// The boot sets of `platforms`.
    ///
    /// # Errors
    /// [`BootSetsError`] for a platform that has no boot loaders ([`LoaderNames::of`]).
    pub fn new(platforms: impl IntoIterator<Item = Platform>) -> Result<Self, BootSetsError> {
        let mut sets = BTreeMap::new();
        for platform in platforms {
            let names = LoaderNames::of(platform).ok_or(BootSetsError(platform))?;
            sets.insert(platform, names);
        }
        Ok(Self(sets))
    }

    /// The names of `platform`'s boot loaders when it has a boot set; `None` when it has none.
    #[must_use]
    pub fn loaders(&self, platform: Platform) -> Option<LoaderNames> {
        self.0.get(&platform).copied()
    }

    /// The platforms that have a boot set, in [`Platform`]'s order: what `serve` logs at start.
    pub fn platforms(&self) -> impl Iterator<Item = Platform> + '_ {
        self.0.keys().copied()
    }
}
