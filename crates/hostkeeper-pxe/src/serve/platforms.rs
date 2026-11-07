//! The one platform under its three spellings: [`BootPlatform`] (a sealed boot set),
//! [`LinuxPlatform`] (a menu and a seed, `hostkeeper-pxe-seed`) and [`Platform`] (a packet,
//! `hostkeeper-pxe-packets`, and the release). Only the two Linux platforms boot from the network.

use hostkeeper_pxe_packets::dhcp::Mode;
use hostkeeper_pxe_packets::{BootSets, LoaderNames};
use hostkeeper_pxe_seed::inputs::LinuxPlatform;
use hostkeeper_pxe_seed::menu::Mode as MenuMode;
use hostkeeper_wire::Platform;

use crate::platform::BootPlatform;

/// The platform of a sealed boot set.
pub fn linux_of_boot(platform: BootPlatform) -> LinuxPlatform {
    match platform {
        BootPlatform::LinuxX86_64 => LinuxPlatform::X86_64,
        BootPlatform::LinuxArm64 => LinuxPlatform::Arm64,
    }
}

/// The release's spelling of a Linux platform.
pub fn wire_of(platform: LinuxPlatform) -> Platform {
    match platform {
        LinuxPlatform::X86_64 => Platform::LinuxX86_64,
        LinuxPlatform::Arm64 => Platform::LinuxArm64,
    }
}

/// The names a Linux platform's boot loaders are served by. Both Linux platforms have them
/// ([`LoaderNames::of`]); the empty names of the fallback match no sealed file, so a platform
/// without them could never pass the seal check.
pub fn loaders_of(platform: LinuxPlatform) -> LoaderNames {
    LoaderNames::of(wire_of(platform)).unwrap_or(LoaderNames { shim: "", grub: "" })
}

/// The menu crate's spelling of the mode.
pub fn menu_mode(mode: Mode) -> MenuMode {
    match mode {
        Mode::Proxy => MenuMode::Proxy,
        Mode::Router => MenuMode::Router,
    }
}

/// The boot sets of the packet crate for `platforms`. Every Linux platform has boot loaders
/// ([`BootSets::new`] refuses only a Mac's), so the fallback, no boot set, is never taken.
pub fn boot_sets(platforms: impl IntoIterator<Item = LinuxPlatform>) -> BootSets {
    BootSets::new(platforms.into_iter().map(wire_of)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each sealed platform is the same Linux platform for the menus, the packets and the
    /// release, with the loader names of its own architecture.
    ///
    /// Catches an arm64 boot set served under the x86-64 names, or the reverse.
    #[test]
    fn each_platform_keeps_its_architecture_across_its_spellings() {
        assert_eq!(
            linux_of_boot(BootPlatform::LinuxX86_64),
            LinuxPlatform::X86_64
        );
        assert_eq!(
            linux_of_boot(BootPlatform::LinuxArm64),
            LinuxPlatform::Arm64
        );
        assert_eq!(wire_of(LinuxPlatform::X86_64), Platform::LinuxX86_64);
        assert_eq!(wire_of(LinuxPlatform::Arm64), Platform::LinuxArm64);
        assert_eq!(
            loaders_of(LinuxPlatform::X86_64).shim,
            "hostkeeper/shimx64.efi"
        );
        assert_eq!(
            loaders_of(LinuxPlatform::Arm64).grub,
            "hostkeeper/grubaa64.efi"
        );
        assert_eq!(menu_mode(Mode::Proxy), MenuMode::Proxy);
        assert_eq!(menu_mode(Mode::Router), MenuMode::Router);
        let sets = boot_sets([LinuxPlatform::Arm64]);
        assert_eq!(sets.platforms().collect::<Vec<_>>(), [Platform::LinuxArm64]);
    }
}
