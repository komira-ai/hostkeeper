//! The collector `macos` of the maintenance look ([`crate::maintenance_collect`],
//! docs/design/maintenance-updates.md section 1): what a Mac runs (`macOS`, its version and build
//! from `sw_vers`, its hardware serial from `ioreg`) and which macOS updates are due, from
//! `softwareupdate --list` and `softwareupdate --list-full-installers`, both read by
//! [`parse_softwareupdate_list`] and judged by [`macos_due`]: the newest major upgrade either
//! lists is reported whatever the host's group allows, which is the coordinator's to judge.
//!
//! Each program runs through a [`Launchctl`] port: on a Mac, as root, with an empty environment, no
//! standard input, a deadline, and each output stream kept to its bound
//! ([`crate::maintenance_collect::listing`]). Nothing is installed or downloaded: both listings
//! only ask Apple's catalog. A program that does not run, does not end by its deadline or exits
//! non-zero fails the look with the step's name; so does a `sw_vers` that names no version, and an
//! `ioreg` whose serial is missing or malformed.

use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::MAX_OS_TEXT;

use crate::mac::{IOREG, TOOL_DEADLINE, ioreg_args};
use crate::maintenance_collect::{Collector, Look, Section, clip, listing};
use crate::maintenance_softwareupdate::{macos_due, parse_softwareupdate_list};
use crate::xcode_clt::{LIST_DEADLINE, LOOK_DEADLINE, SOFTWAREUPDATE, SW_VERS};

/// The name a Mac reports as its operating system's.
pub const MACOS_NAME: &str = "macOS";

/// The arguments of the listing of updates.
const LIST_ARGS: [&str; 1] = ["--list"];

/// The arguments of the listing of full installers.
const FULL_ARGS: [&str; 1] = ["--list-full-installers"];

/// The programs the collector `macos` runs.
pub struct MacosPorts {
    /// `sw_vers`, for the version and build.
    pub sw_vers: Box<dyn Launchctl>,
    /// `ioreg`, for the hardware serial.
    pub ioreg: Box<dyn Launchctl>,
    /// `softwareupdate`, for `--list`.
    pub list: Box<dyn Launchctl>,
    /// `softwareupdate`, for `--list-full-installers`.
    pub full: Box<dyn Launchctl>,
}

/// The collector `macos`.
pub struct Macos {
    ports: MacosPorts,
}

impl Macos {
    /// The collector over `ports`.
    #[must_use]
    pub fn new(ports: MacosPorts) -> Macos {
        Macos { ports }
    }

    /// The collector over the Mac's own programs: [`SW_VERS`] bounded by
    /// [`LOOK_DEADLINE`], [`IOREG`] by [`TOOL_DEADLINE`], and both listings of [`SOFTWAREUPDATE`]
    /// by [`LIST_DEADLINE`], as the Command Line Tools' look runs them.
    #[must_use]
    pub fn at_host() -> Macos {
        let tool = |program: &str, deadline: Duration| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), deadline))
        };
        Macos::new(MacosPorts {
            sw_vers: tool(SW_VERS, LOOK_DEADLINE),
            ioreg: tool(IOREG, TOOL_DEADLINE),
            list: tool(SOFTWAREUPDATE, LIST_DEADLINE),
            full: tool(SOFTWAREUPDATE, LIST_DEADLINE),
        })
    }

    /// The hardware serial, as `ioreg` names it.
    fn serial(&self) -> Result<String, String> {
        let args = ioreg_args();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (_, text) = listing(&*self.ports.ioreg, "ioreg", &args, &[0])?;
        hostkeeper_enroll::parse_ioreg(&text)
            .map(|ids| ids.serial)
            .map_err(|error| error.to_string())
    }
}

impl Collector for Macos {
    fn name(&self) -> &'static str {
        "macos"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        let (_, text) = listing(&*self.ports.sw_vers, "sw_vers", &[], &[0])?;
        let (version, build) = parse_sw_vers(&text);
        if version.is_empty() {
            return Err("sw_vers named no ProductVersion".to_owned());
        }
        let serial = self.serial()?;
        let (_, text) = listing(&*self.ports.list, "softwareupdate --list", &LIST_ARGS, &[0])?;
        let mut listed = parse_softwareupdate_list(&text);
        let (_, text) = listing(
            &*self.ports.full,
            "softwareupdate --list-full-installers",
            &FULL_ARGS,
            &[0],
        )?;
        let full = parse_softwareupdate_list(&text);
        listed.updates.extend(full.updates);
        listed.installers.extend(full.installers);
        listed.unparsed = listed.unparsed.saturating_add(full.unparsed);
        Ok(Section {
            host: pb::MaintenanceHost {
                os_name: MACOS_NAME.to_owned(),
                os_version: clip(&version, MAX_OS_TEXT),
                os_build: clip(&build, MAX_OS_TEXT),
                hardware_serial: clip(&serial, MAX_OS_TEXT),
                ..pb::MaintenanceHost::default()
            },
            due: macos_due(&version, &listed),
            unparsed: listed.unparsed,
            ..Section::default()
        })
    }
}

/// The `ProductVersion` and the `BuildVersion` of what `sw_vers` wrote (`ProductVersion:\t26.1`),
/// each trimmed, each empty when there is no such line.
#[must_use]
pub fn parse_sw_vers(text: &str) -> (String, String) {
    let value = |key: &str| -> String {
        text.lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.trim() == key)
            .map(|(_, value)| value.trim().to_owned())
            .unwrap_or_default()
    };
    (value("ProductVersion"), value("BuildVersion"))
}

#[cfg(test)]
#[path = "maintenance_macos_tests.rs"]
mod tests;
