//! The architectures of option 93, the platforms they take, and the boot sets.

use hostkeeper_pxe_packets::{BootSets, BootSetsError, ClientArch, LoaderNames, Mac};
use hostkeeper_wire::Platform;

/// Every option 93 value: 7, 9 and 16 are x86-64, 11 and 19 arm64, and no other is installed.
/// Catches: a BIOS (0) or 32-bit ARM (10) client given a boot file, and an arm64 value taken
/// for x86-64.
#[test]
fn each_option_93_value_takes_its_platform_or_none() {
    for value in 0..=u16::MAX {
        let want = match value {
            7 | 9 | 16 => Some(Platform::LinuxX86_64),
            11 | 19 => Some(Platform::LinuxArm64),
            _ => None,
        };
        let arch = ClientArch::new(value);
        assert_eq!(arch.platform(), want, "{value}");
        assert_eq!(arch.value(), value);
    }
    let named = [
        (ClientArch::EFI_BC, 7),
        (ClientArch::X86_64_UEFI, 9),
        (ClientArch::ARM64_UEFI, 11),
        (ClientArch::X86_64_UEFI_HTTP, 16),
        (ClientArch::ARM64_UEFI_HTTP, 19),
    ];
    for (arch, value) in named {
        assert_eq!(arch.value(), value);
    }
    assert_eq!(ClientArch::new(11).to_string(), "11");
    assert_eq!(format!("{:?}", ClientArch::new(11)), "ClientArch(11)");
}

/// The boot loaders of each Linux platform are in `hostkeeper/`, beside `boot.efi`; a Mac has
/// none.
#[test]
fn each_linux_platform_has_its_boot_loaders_beside_boot_efi() {
    assert_eq!(
        LoaderNames::of(Platform::LinuxX86_64),
        Some(LoaderNames {
            shim: "hostkeeper/shimx64.efi",
            grub: "hostkeeper/grubx64.efi"
        })
    );
    assert_eq!(
        LoaderNames::of(Platform::LinuxArm64),
        Some(LoaderNames {
            shim: "hostkeeper/shimaa64.efi",
            grub: "hostkeeper/grubaa64.efi"
        })
    );
    assert_eq!(LoaderNames::of(Platform::DarwinArm64), None);
}

/// Boot sets hold the Linux platforms given, and refuse a Mac's by name.
#[test]
fn boot_sets_hold_linux_platforms_and_refuse_a_mac() {
    let arm = BootSets::new([Platform::LinuxArm64]).expect("a boot set");
    assert_eq!(arm.platforms().collect::<Vec<_>>(), [Platform::LinuxArm64]);
    assert_eq!(arm.loaders(Platform::LinuxX86_64), None);
    assert_eq!(
        arm.loaders(Platform::LinuxArm64),
        LoaderNames::of(Platform::LinuxArm64)
    );
    let both = BootSets::new([Platform::LinuxArm64, Platform::LinuxX86_64]).expect("boot sets");
    assert_eq!(
        both.platforms().collect::<Vec<_>>(),
        [Platform::LinuxX86_64, Platform::LinuxArm64]
    );
    assert_eq!(BootSets::new([]).expect("none").platforms().count(), 0);
    assert_eq!(BootSets::default(), BootSets::new([]).expect("none"));
    let refused = BootSets::new([Platform::LinuxX86_64, Platform::DarwinArm64]);
    assert_eq!(refused, Err(BootSetsError(Platform::DarwinArm64)));
    assert_eq!(
        BootSetsError(Platform::DarwinArm64).to_string(),
        "no boot set is made for the platform darwin-arm64: only linux-x86_64 and linux-arm64 \
         boot from the network"
    );
}

#[test]
fn a_mac_is_shown_as_six_lower_case_pairs() {
    let mac = Mac::new([0x0c, 0xC4, 0x7a, 0x1f, 0xab, 0x02]);
    assert_eq!(mac.to_string(), "0c:c4:7a:1f:ab:02");
    assert_eq!(format!("{mac:?}"), "Mac(0c:c4:7a:1f:ab:02)");
    assert_eq!(mac.octets(), [0x0c, 0xc4, 0x7a, 0x1f, 0xab, 0x02]);
}
