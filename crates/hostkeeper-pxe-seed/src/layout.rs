//! Where the files a menu and a seed name are served: the contract between this crate and
//! `hostkeeper-pxe serve`, which serves each of these paths and refuses every other.
//!
//! By TFTP, below GRUB's `$prefix`: the fixed `grub.cfg`, and the menu of a MAC at
//! `hostkeeper/<grub cpu>/<mac>` ([`MENU_DIR`]). By HTTP: the kernel, the initrd and the
//! live-server ISO of each platform under `/boot/<platform>/`, the deb of each platform under
//! `/deb/`, and each machine's seed under `/seed/<mac>/`.

use hostkeeper_enrollment::names::MacAddress;

use crate::inputs::LinuxPlatform;

/// The directory, below GRUB's `$prefix`, that holds the menu of each MAC.
pub const MENU_DIR: &str = "hostkeeper";

/// The HTTP path of the platform's kernel.
pub const fn kernel_path(platform: LinuxPlatform) -> &'static str {
    match platform {
        LinuxPlatform::X86_64 => "/boot/linux-x86_64/vmlinuz",
        LinuxPlatform::Arm64 => "/boot/linux-arm64/vmlinuz",
    }
}

/// The HTTP path of the platform's initrd.
pub const fn initrd_path(platform: LinuxPlatform) -> &'static str {
    match platform {
        LinuxPlatform::X86_64 => "/boot/linux-x86_64/initrd",
        LinuxPlatform::Arm64 => "/boot/linux-arm64/initrd",
    }
}

/// The HTTP path of the platform's Ubuntu 24.04 live-server ISO.
pub const fn iso_path(platform: LinuxPlatform) -> &'static str {
    match platform {
        LinuxPlatform::X86_64 => "/boot/linux-x86_64/live-server.iso",
        LinuxPlatform::Arm64 => "/boot/linux-arm64/live-server.iso",
    }
}

/// The HTTP path of the platform's hostkeeper deb.
pub const fn deb_path(platform: LinuxPlatform) -> &'static str {
    match platform {
        LinuxPlatform::X86_64 => "/deb/hostkeeper_amd64.deb",
        LinuxPlatform::Arm64 => "/deb/hostkeeper_arm64.deb",
    }
}

/// The HTTP directory of a machine's seed, `/seed/<mac>/`, which holds `user-data` and
/// `meta-data`. The MAC is in its one spelling, lower-case and colon-separated.
pub fn seed_dir(mac: MacAddress) -> String {
    format!("/seed/{mac}/")
}

/// The one apt mirror a seed names: a path the boot server never serves. The installer finds
/// no usable mirror, so it installs from the ISO's own pool (`fallback: offline-install`) and
/// reaches no archive on the internet.
pub const NO_MIRROR_PATH: &str = "/no-mirror/";
