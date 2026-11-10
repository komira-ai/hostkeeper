//! The boot menus (docs/design/network-install.md, section 4, "The boot chain").
//!
//! GRUB reads the fixed [`GRUB_CFG`], the same for every machine, whose one command loads the
//! menu of the machine's MAC from `hostkeeper/<grub cpu>/<mac>` below GRUB's `$prefix`.
//! [`MenuRequest::parse`] reads that path and [`MenuRequest::choose`] decides the menu: the
//! install menu of a platform, or the local-disk menu. Both menus have one entry and no wait.

use std::fmt;

use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_wire::names::HostId;

use crate::inputs::{BootServer, IsoName, LinuxPlatform, Record};
use crate::layout::{MENU_DIR, initrd_path, iso_path, kernel_path, seed_dir};

/// The fixed `grub.cfg`: one command, which loads the menu of GRUB's CPU and of the MAC GRUB
/// booted from, from the directory GRUB read this file from.
pub const GRUB_CFG: &str = "configfile ${prefix}/hostkeeper/${grub_cpu}/${net_default_mac}\n";

/// The menu of every MAC that is not installed: one entry, GRUB's `exit`, which hands the boot
/// back to the firmware to try its next boot option.
pub const LOCAL_DISK_MENU: &str = "\
set timeout=0
set default=0
menuentry \"Boot the local disk\" {
\texit
}
";

/// Whether `hostkeeper-pxe` gives the boot file by proxyDHCP or the router names it
/// (network-install.md section 3, "Two modes, one rule").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `--mode proxy`: only a MAC of the answer set gets a proxy offer, so a record with no
    /// platform installs the platform its firmware and GRUB state.
    Proxy,
    /// `--mode router`: every network-boot client of the LAN reaches GRUB, so the record is
    /// where the platform of an install is stated, and a record with none is not installed.
    Router,
}

/// A request for the menu of a MAC: the last two components of a TFTP path
/// `.../hostkeeper/<cpu>/<mac>`, as GRUB filled them in, not yet checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MenuRequest<'n> {
    cpu: &'n str,
    mac: &'n str,
}

impl<'n> MenuRequest<'n> {
    /// The request of a TFTP path whose last three `/`-separated components are
    /// [`MENU_DIR`], the CPU and the MAC; `None` for every other path, which is no menu.
    pub fn parse(path: &'n str) -> Option<MenuRequest<'n>> {
        let mut components = path.rsplit('/');
        let (Some(mac), Some(cpu), Some(MENU_DIR)) =
            (components.next(), components.next(), components.next())
        else {
            return None;
        };
        Some(MenuRequest { cpu, mac })
    }

    /// The menu of this request. `in_answer_set` gives the record of a MAC that may be
    /// installed now, and `None` for every other MAC (`pxe serve` decides the answer
    /// set, network-install.md section 3). It is not called for a MAC that is not well-formed
    /// or a CPU this design does not install. It is a `dyn` closure, so `choose` has one
    /// compiled body and each of its arms is measured once.
    ///
    /// The install menu is chosen exactly when the MAC is well-formed and in the answer set,
    /// the CPU is that of a Linux platform, and the record's platform is that platform; or, in
    /// proxy mode, the record has no platform. Every other request gets the local-disk menu,
    /// with the reason.
    pub fn choose<'r>(
        &self,
        mode: Mode,
        in_answer_set: &dyn Fn(MacAddress) -> Option<&'r Record>,
    ) -> MenuChoice {
        let Ok(mac) = MacAddress::parse(self.mac) else {
            return MenuChoice::LocalDisk(LocalDiskReason::MalformedMac);
        };
        let Some(stated) = LinuxPlatform::from_grub_cpu(self.cpu) else {
            return MenuChoice::LocalDisk(LocalDiskReason::UnknownCpu { mac });
        };
        let Some(record) = in_answer_set(mac) else {
            return MenuChoice::LocalDisk(LocalDiskReason::NotInAnswerSet { mac });
        };
        match (record.platform, mode) {
            (Some(recorded), _) if recorded == stated => MenuChoice::Install(Install {
                mac,
                record: record.clone(),
                platform: stated,
            }),
            (Some(recorded), _) => MenuChoice::LocalDisk(LocalDiskReason::PlatformDisagrees {
                mac,
                host_id: record.host_id.clone(),
                recorded,
                stated,
            }),
            (None, Mode::Proxy) => MenuChoice::Install(Install {
                mac,
                record: record.clone(),
                platform: stated,
            }),
            (None, Mode::Router) => MenuChoice::LocalDisk(LocalDiskReason::NoPlatform {
                mac,
                host_id: record.host_id.clone(),
            }),
        }
    }
}

/// An install [`MenuRequest::choose`] decided: the machine's MAC, its record, and the platform
/// to install, which is the record's, or in proxy mode the one GRUB stated when the record has
/// none. Only [`MenuRequest::choose`] makes one, so a seed ([`crate::seed::seed`]) cannot be made
/// for a platform its menu did not choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Install {
    mac: MacAddress,
    record: Record,
    platform: LinuxPlatform,
}

impl Install {
    /// The MAC the machine booted from.
    pub fn mac(&self) -> MacAddress {
        self.mac
    }

    /// The machine's record.
    pub fn record(&self) -> &Record {
        &self.record
    }

    /// The platform to install.
    pub fn platform(&self) -> LinuxPlatform {
        self.platform
    }

    /// The install menu of this install ([`install_menu`]), whose ISO is `iso`: the served name
    /// of the live-server ISO of [`Install::platform`].
    pub fn menu(&self, server: &BootServer, iso: &IsoName) -> String {
        install_menu(server, self.platform, iso, self.mac)
    }
}

/// The menu [`MenuRequest::choose`] gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuChoice {
    /// Install on the machine.
    Install(Install),
    /// Boot the local disk.
    LocalDisk(LocalDiskReason),
}

/// Why a request got the local-disk menu, for `hostkeeper-pxe`'s log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalDiskReason {
    /// The MAC component is not a MAC address.
    MalformedMac,
    /// The CPU component is not that of a Linux platform this design installs.
    UnknownCpu { mac: MacAddress },
    /// The MAC is not in the answer set.
    NotInAnswerSet { mac: MacAddress },
    /// The record's platform is not the platform GRUB states.
    PlatformDisagrees {
        mac: MacAddress,
        host_id: HostId,
        recorded: LinuxPlatform,
        stated: LinuxPlatform,
    },
    /// Router mode, and the record has no platform.
    NoPlatform { mac: MacAddress, host_id: HostId },
}

impl fmt::Display for LocalDiskReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalDiskReason::MalformedMac => f.write_str("the menu path's MAC is not well-formed"),
            LocalDiskReason::UnknownCpu { mac } => write!(
                f,
                "{mac}: the menu path's CPU is not x86_64 or arm64, a platform this server installs"
            ),
            LocalDiskReason::NotInAnswerSet { mac } => {
                write!(f, "{mac}: not in the answer set")
            }
            LocalDiskReason::PlatformDisagrees {
                mac,
                host_id,
                recorded,
                stated,
            } => write!(
                f,
                "{mac}: the record of host {host_id} is {recorded}, and GRUB states {stated}"
            ),
            LocalDiskReason::NoPlatform { mac, host_id } => write!(
                f,
                "{mac}: the record of host {host_id} has no platform, which router mode needs"
            ),
        }
    }
}

/// The install menu of `platform` for the machine of `mac`: one entry, no wait. It loads the
/// platform's kernel and initrd from the boot server by HTTP, with the URL of the live-server
/// ISO served as `iso`, `autoinstall`, and the NoCloud seed of the MAC. The `;` of
/// `ds=nocloud;s=...` is escaped, where it would otherwise end GRUB's command.
pub fn install_menu(
    server: &BootServer,
    platform: LinuxPlatform,
    iso: &IsoName,
    mac: MacAddress,
) -> String {
    let device = server.grub_device();
    format!(
        "\
set timeout=0
set default=0
menuentry \"Install Ubuntu 24.04 and hostkeeper ({platform})\" {{
\tlinux {device}{kernel} ip=dhcp url={iso} autoinstall ds=nocloud\\;s={seed}
\tinitrd {device}{initrd}
}}
",
        kernel = kernel_path(platform),
        initrd = initrd_path(platform),
        iso = server.url(&iso_path(iso)),
        seed = server.url(&seed_dir(mac)),
    )
}
