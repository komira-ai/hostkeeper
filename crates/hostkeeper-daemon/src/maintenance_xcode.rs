//! The collector `xcode` of the maintenance look ([`crate::maintenance_collect`]): the Xcodes
//! installed on a Mac and the one `xcode-select -p` names (docs/design/maintenance-updates.md,
//! sections 1 and 2.4). An Xcode is each directory `/Applications/Xcode*.app`; its version and
//! build are the `CFBundleShortVersionString` and `ProductBuildVersion` of its
//! `Contents/version.plist`, read through the [`Plists`] port. An Xcode whose directory is not
//! named `Xcode-<version>.app` is reported `foreign`: one a person, the App Store or device
//! management put there, which a run never removes.
//!
//! `xcode-select -p` is run only when an Xcode is installed. Its exit code 2 (no developer
//! directory is set) is taken to be what a Mac without one answers; this is unverified on the
//! farm's macOS [V], and any other code but 0 fails the collector. An Xcode is selected when the
//! developer directory is its directory or is under it, so `Xcode-27.2.app/Contents/Developer`
//! selects `/Applications/Xcode-27.2.app` and not `/Applications/Xcode-27.2.app.old`, and the
//! Command Line Tools' directory selects none.
//!
//! The collector only reports; the coordinator decides what is due. A text is never cut: one over
//! its bound makes the look's report check refuse the report and the look fail naming the field,
//! since a cut version would name another Xcode. More Xcodes than the report holds fail the
//! collector before any file is read, since a partial list would make the coordinator see one
//! missing.

use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::MAX_XCODES;

use crate::maintenance_collect::{
    APPLICATIONS, Collector, HostRoot, Look, Section, listing, xcode_apps,
};
use crate::maintenance_plist::{HostPlists, Plists};

/// The program that names the selected developer directory.
pub const XCODE_SELECT: &str = "/usr/bin/xcode-select";

/// How long `xcode-select` is given.
pub const DEADLINE: Duration = Duration::from_secs(60);

/// The collector `xcode`: the Xcodes installed and the one selected.
pub struct Xcodes {
    xcode_select: Box<dyn Launchctl>,
    plists: Box<dyn Plists>,
    root: HostRoot,
}

impl Xcodes {
    /// The collector over `xcode_select`, the property lists of `plists` and the files under
    /// `root`.
    #[must_use]
    pub fn new(
        xcode_select: Box<dyn Launchctl>,
        plists: Box<dyn Plists>,
        root: HostRoot,
    ) -> Xcodes {
        Xcodes {
            xcode_select,
            plists,
            root,
        }
    }

    /// The collector over the host's own `xcode-select` (bounded by [`DEADLINE`]), property lists
    /// and files.
    #[must_use]
    pub fn at_host() -> Xcodes {
        Xcodes::new(
            Box::new(LaunchctlProcess::new(Path::new(XCODE_SELECT), DEADLINE)),
            Box::new(HostPlists::at_host()),
            HostRoot::host(),
        )
    }

    /// The version and the build of the Xcode whose `Contents/version.plist` is `plist`.
    fn version_and_build(&self, plist: &str) -> Result<(String, String), String> {
        let dictionary = self
            .plists
            .dictionary(plist)?
            .ok_or_else(|| format!("{plist} is missing"))?;
        let text = |key: &str| {
            dictionary
                .get(key)
                .and_then(plist::Value::as_string)
                .map(str::to_owned)
                .ok_or_else(|| format!("{plist} has no {key}"))
        };
        Ok((
            text("CFBundleShortVersionString")?,
            text("ProductBuildVersion")?,
        ))
    }

    /// The developer directory `xcode-select -p` names, or `None` when none is set.
    fn developer_dir(&self) -> Result<Option<String>, String> {
        let (code, text) = listing(
            self.xcode_select.as_ref(),
            "xcode-select -p",
            &["-p"],
            &[0, 2],
        )?;
        if code == 2 {
            return Ok(None);
        }
        Ok(text.lines().next().map(|line| line.trim().to_owned()))
    }
}

impl Collector for Xcodes {
    fn name(&self) -> &'static str {
        "xcode"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        let names = xcode_apps(&self.root)?;
        if names.is_empty() {
            return Ok(Section::default());
        }
        if names.len() > MAX_XCODES {
            return Err(format!(
                "{} Xcodes are in {APPLICATIONS}, more than {MAX_XCODES}",
                names.len()
            ));
        }
        let mut found = Vec::new();
        for name in names {
            let path = format!("{APPLICATIONS}/{name}");
            let plist = format!("{path}/Contents/version.plist");
            let (version, build) = self.version_and_build(&plist)?;
            let foreign = name != format!("Xcode-{version}.app");
            found.push(pb::InstalledXcode {
                version,
                build,
                path,
                selected: false,
                foreign,
            });
        }
        if let Some(developer) = self.developer_dir()? {
            for xcode in &mut found {
                xcode.selected =
                    developer == xcode.path || developer.starts_with(&format!("{}/", xcode.path));
            }
        }
        Ok(Section {
            xcodes: found,
            ..Section::default()
        })
    }
}

#[cfg(test)]
#[path = "maintenance_xcode_tests.rs"]
mod tests;
