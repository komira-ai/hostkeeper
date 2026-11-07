//! The boot menus: the fixed `grub.cfg`, the install menu of each platform and the local-disk
//! menu, each against its golden by exact equality; and the choice between them for every case
//! of network-install.md section 11 ("Boot menu").

mod support;

use std::cell::Cell;

use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_pxe_seed::inputs::{LinuxPlatform, Record};
use hostkeeper_pxe_seed::layout::MENU_DIR;
use hostkeeper_pxe_seed::menu::{
    GRUB_CFG, LOCAL_DISK_MENU, LocalDiskReason, MenuChoice, MenuRequest, Mode, install_menu,
};
use support::{record, server, server_on};

const MAC: &str = "52:54:00:12:34:56";

fn mac() -> MacAddress {
    MacAddress::parse(MAC).expect("a MAC")
}

/// The choice for `path` when the answer set holds `record` for [`MAC`] and nothing else.
fn choose(path: &str, mode: Mode, set: Option<&Record>) -> MenuChoice {
    MenuRequest::parse(path)
        .expect("a menu path")
        .choose(mode, &|asked| set.filter(|_| asked == mac()))
}

const MODES: [Mode; 2] = [Mode::Proxy, Mode::Router];

/// Asserts `choice` is an install of `platform` on the machine of `mac`, carrying `record`.
fn assert_install(choice: &MenuChoice, mac: MacAddress, record: &Record, platform: LinuxPlatform) {
    let MenuChoice::Install(install) = choice else {
        panic!("not an install: {choice:?}");
    };
    assert_eq!(
        (install.mac(), install.record(), install.platform()),
        (mac, record, platform)
    );
}

/// Catches a `grub.cfg` that differs between machines or loads anything but the menu of
/// GRUB's CPU and MAC from the menu directory.
#[test]
fn grub_cfg_is_the_golden() {
    assert_eq!(GRUB_CFG, include_str!("golden/grub.cfg"));
    assert!(GRUB_CFG.contains(&format!("/{MENU_DIR}/${{grub_cpu}}/${{net_default_mac}}")));
}

/// Catches an install menu that loads another platform's kernel, waits at a menu, or leaves
/// the `;` of `ds=nocloud;s=` unescaped, where it would end GRUB's command.
#[test]
fn the_install_menu_of_each_platform_is_its_golden() {
    let goldens = [
        (
            LinuxPlatform::X86_64,
            include_str!("golden/install-x86_64.cfg"),
        ),
        (
            LinuxPlatform::Arm64,
            include_str!("golden/install-arm64.cfg"),
        ),
    ];
    for (platform, golden) in goldens {
        assert_eq!(
            install_menu(&server(), platform, mac()),
            golden,
            "{platform}"
        );
    }
    assert!(install_menu(&server(), LinuxPlatform::X86_64, mac()).contains("ds=nocloud\\;s="));
}

/// Catches a boot server on a port other than 80 whose port is dropped from the URLs, and a
/// port 80 that is written out, which a GRUB that reads no port in `(http,...)` would not take.
#[test]
fn a_port_other_than_80_is_in_every_url_of_the_menu() {
    let menu = install_menu(&server_on(8080), LinuxPlatform::X86_64, mac());
    assert_eq!(menu, include_str!("golden/install-x86_64-port-8080.cfg"));
    assert!(!install_menu(&server(), LinuxPlatform::X86_64, mac()).contains(":80"));
}

/// Catches a local-disk menu that waits, or does anything but hand the boot back to the
/// firmware.
#[test]
fn the_local_disk_menu_is_the_golden() {
    assert_eq!(LOCAL_DISK_MENU, include_str!("golden/local-disk.cfg"));
}

/// Catches an install of a record whose platform GRUB agrees with being refused, in either
/// mode, on either platform, and a menu naming another MAC's seed.
#[test]
fn a_mac_in_the_answer_set_whose_platform_agrees_gets_the_install_menu() {
    for platform in LinuxPlatform::ALL {
        let node = record("node-a", None, Some(platform));
        let path = format!("grub/hostkeeper/{}/{MAC}", platform.grub_cpu());
        for mode in MODES {
            let choice = choose(&path, mode, Some(&node));
            assert_install(&choice, mac(), &node, platform);
            assert_eq!(
                choice.text(&server()),
                install_menu(&server(), platform, mac())
            );
        }
    }
}

/// Catches a proxy-mode record with no platform that is not installed, or installed with a
/// platform other than the one GRUB states.
#[test]
fn proxy_mode_installs_a_record_with_no_platform_as_the_platform_grub_states() {
    let node = record("node-a", None, None);
    for platform in LinuxPlatform::ALL {
        let path = format!("grub/hostkeeper/{}/{MAC}", platform.grub_cpu());
        assert_install(
            &choose(&path, Mode::Proxy, Some(&node)),
            mac(),
            &node,
            platform,
        );
    }
}

/// Catches a router-mode install of a record that states no platform.
#[test]
fn router_mode_boots_the_disk_of_a_record_with_no_platform() {
    let node = record("node-a", None, None);
    for platform in LinuxPlatform::ALL {
        let path = format!("grub/hostkeeper/{}/{MAC}", platform.grub_cpu());
        let choice = choose(&path, Mode::Router, Some(&node));
        assert_eq!(
            choice,
            MenuChoice::LocalDisk(LocalDiskReason::NoPlatform {
                mac: mac(),
                host_id: node.host_id.clone(),
            })
        );
        assert_eq!(choice.text(&server()), LOCAL_DISK_MENU);
    }
}

/// Catches the wrong kernel booted on a machine whose record was typed with the wrong
/// platform, in each direction and each mode.
#[test]
fn a_platform_that_disagrees_with_grub_boots_the_disk() {
    let cases = [
        (LinuxPlatform::X86_64, LinuxPlatform::Arm64),
        (LinuxPlatform::Arm64, LinuxPlatform::X86_64),
    ];
    for (recorded, stated) in cases {
        let node = record("node-a", None, Some(recorded));
        let path = format!("grub/hostkeeper/{}/{MAC}", stated.grub_cpu());
        for mode in MODES {
            let choice = choose(&path, mode, Some(&node));
            assert_eq!(
                choice,
                MenuChoice::LocalDisk(LocalDiskReason::PlatformDisagrees {
                    mac: mac(),
                    host_id: node.host_id.clone(),
                    recorded,
                    stated,
                })
            );
            assert_eq!(choice.text(&server()), LOCAL_DISK_MENU);
        }
    }
}

/// Catches an install of a MAC that is not in the answer set: enrolled, retired, pending, or
/// never recorded.
#[test]
fn a_mac_not_in_the_answer_set_boots_the_disk() {
    for platform in LinuxPlatform::ALL {
        let path = format!("grub/hostkeeper/{}/{MAC}", platform.grub_cpu());
        for mode in MODES {
            assert_eq!(
                choose(&path, mode, None),
                MenuChoice::LocalDisk(LocalDiskReason::NotInAnswerSet { mac: mac() })
            );
        }
    }
}

/// Catches a malformed MAC or a CPU of a platform this design does not install that reaches
/// the answer set or an install: the answer set is not asked, and the disk boots.
#[test]
fn a_malformed_mac_or_an_unknown_cpu_boots_the_disk_without_asking_the_answer_set() {
    let node = record("node-a", None, Some(LinuxPlatform::X86_64));
    let cases = [
        ("grub/hostkeeper/x86_64/", LocalDiskReason::MalformedMac),
        (
            "grub/hostkeeper/x86_64/52:54:00:12:34",
            LocalDiskReason::MalformedMac,
        ),
        (
            "grub/hostkeeper/x86_64/52:54:00:12:34:5g",
            LocalDiskReason::MalformedMac,
        ),
        (
            "grub/hostkeeper/x86_64/52:54-00:12:34:56",
            LocalDiskReason::MalformedMac,
        ),
        (
            "grub/hostkeeper/x86_64/${net_default_mac}",
            LocalDiskReason::MalformedMac,
        ),
        (
            "grub/hostkeeper/i386/52:54:00:12:34:56",
            LocalDiskReason::UnknownCpu { mac: mac() },
        ),
        (
            "grub/hostkeeper/arm/52:54:00:12:34:56",
            LocalDiskReason::UnknownCpu { mac: mac() },
        ),
        (
            "grub/hostkeeper/X86_64/52:54:00:12:34:56",
            LocalDiskReason::UnknownCpu { mac: mac() },
        ),
        (
            "grub/hostkeeper//52:54:00:12:34:56",
            LocalDiskReason::UnknownCpu { mac: mac() },
        ),
    ];
    for (path, reason) in cases {
        for mode in MODES {
            let asked = Cell::new(false);
            let choice = MenuRequest::parse(path)
                .expect("a menu path")
                .choose(mode, &|_| {
                    asked.set(true);
                    Some(&node)
                });
            assert_eq!(choice, MenuChoice::LocalDisk(reason.clone()), "{path}");
            assert!(!asked.get(), "{path}: the answer set was asked");
        }
    }
}

/// Catches a MAC that GRUB or a firmware spells in upper case or with `-` that is not found in
/// the answer set, or whose seed URL is not the one spelling.
#[test]
fn every_spelling_of_a_mac_is_the_one_mac() {
    let node = record("node-a", None, Some(LinuxPlatform::X86_64));
    for spelling in [
        "52:54:00:12:34:56",
        "52:54:00:12:34:5A",
        "52-54-00-12-34-56",
        "525400123456",
    ] {
        let expected = MacAddress::parse(spelling).expect("a MAC");
        let path = format!("grub/hostkeeper/x86_64/{spelling}");
        let choice = MenuRequest::parse(&path)
            .expect("a menu path")
            .choose(Mode::Router, &|asked| (asked == expected).then_some(&node));
        assert_install(&choice, expected, &node, LinuxPlatform::X86_64);
        assert!(
            choice
                .text(&server())
                .contains(&format!("/seed/{expected}/"))
        );
    }
}

/// Catches a TFTP path that is not a menu path read as one, so that `hostkeeper-pxe` would
/// serve a menu for a file it should refuse; and the prefixes GRUB may use read as the menu.
#[test]
fn only_a_path_ending_in_the_menu_directory_cpu_and_mac_is_a_menu_request() {
    for path in [
        "",
        "grub/grub.cfg",
        "hostkeeper/x86_64",
        "x86_64/52:54:00:12:34:56",
        "grub/menus/x86_64/52:54:00:12:34:56",
        "grub/hostkeeper/x86_64/52:54:00:12:34:56/extra",
        "grub/Hostkeeper/x86_64/52:54:00:12:34:56",
    ] {
        assert_eq!(MenuRequest::parse(path), None, "{path:?}");
    }
    let node = record("node-a", None, Some(LinuxPlatform::X86_64));
    for path in [
        "hostkeeper/x86_64/52:54:00:12:34:56",
        "/grub/hostkeeper/x86_64/52:54:00:12:34:56",
        "grub/hostkeeper/x86_64/52:54:00:12:34:56",
        "boot/grub/hostkeeper/x86_64/52:54:00:12:34:56",
    ] {
        assert!(
            matches!(
                choose(path, Mode::Router, Some(&node)),
                MenuChoice::Install(_)
            ),
            "{path}"
        );
    }
}

/// Catches a log line that loses the MAC, the host id or either platform of a refusal.
#[test]
fn each_reason_reads_whole() {
    let host_id = "node-a".parse().expect("a host id");
    let cases = [
        (
            LocalDiskReason::MalformedMac,
            "the menu path's MAC is not well-formed".to_owned(),
        ),
        (
            LocalDiskReason::UnknownCpu { mac: mac() },
            format!(
                "{MAC}: the menu path's CPU is not x86_64 or arm64, a platform this server installs"
            ),
        ),
        (
            LocalDiskReason::NotInAnswerSet { mac: mac() },
            format!("{MAC}: not in the answer set"),
        ),
        (
            LocalDiskReason::PlatformDisagrees {
                mac: mac(),
                host_id: "node-a".parse().expect("a host id"),
                recorded: LinuxPlatform::Arm64,
                stated: LinuxPlatform::X86_64,
            },
            format!(
                "{MAC}: the record of host node-a is linux-arm64, and GRUB states linux-x86_64"
            ),
        ),
        (
            LocalDiskReason::NoPlatform {
                mac: mac(),
                host_id,
            },
            format!("{MAC}: the record of host node-a has no platform, which router mode needs"),
        ),
    ];
    for (reason, text) in cases {
        assert_eq!(reason.to_string(), text);
    }
}
