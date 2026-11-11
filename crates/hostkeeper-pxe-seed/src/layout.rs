//! Where the files a menu and a seed name are served: the contract between this crate and
//! `pxe serve`, which serves each of these paths and refuses every other.
//!
//! By TFTP, below GRUB's `$prefix`: the fixed `grub.cfg`, and the menu of a MAC at
//! `hostkeeper/<grub cpu>/<mac>` ([`MENU_DIR`]). By HTTP, at the names the release's
//! `netboot.json` serves them as (network-install.md section 5): the kernel
//! `/vmlinuz-<platform>`, the initrd `/initrd-<platform>`, and the live-server ISO under its own
//! file name ([`crate::inputs::IsoName`]); then the deb of each platform under `/deb/`, and each
//! machine's seed under `/seed/<mac>/`.

use hostkeeper_enrollment::names::MacAddress;

use crate::inputs::{IsoName, LinuxPlatform};

/// The directory, below GRUB's `$prefix`, that holds the menu of each MAC.
pub const MENU_DIR: &str = "hostkeeper";

/// The HTTP path of the platform's kernel: `/` and its served name in `netboot.json`,
/// `vmlinuz-<platform>`.
pub const fn kernel_path(platform: LinuxPlatform) -> &'static str {
    match platform {
        LinuxPlatform::X86_64 => "/vmlinuz-linux-x86_64",
        LinuxPlatform::Arm64 => "/vmlinuz-linux-arm64",
    }
}

/// The HTTP path of the platform's initrd: `/` and its served name in `netboot.json`,
/// `initrd-<platform>`.
pub const fn initrd_path(platform: LinuxPlatform) -> &'static str {
    match platform {
        LinuxPlatform::X86_64 => "/initrd-linux-x86_64",
        LinuxPlatform::Arm64 => "/initrd-linux-arm64",
    }
}

/// The HTTP path of a live-server ISO: `/` and its served name.
pub fn iso_path(iso: &IsoName) -> String {
    format!("/{}", iso.as_str())
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
