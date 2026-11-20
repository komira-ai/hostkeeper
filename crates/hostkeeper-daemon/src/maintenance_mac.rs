//! The collector `mac` of the maintenance look ([`crate::maintenance_collect`],
//! docs/design/maintenance-updates.md section 1): what a macOS update needs of a Mac
//! (`MacUpdateFacts`), as far as this collector reads it: FileVault's state (`fdesetup status`) and
//! whether it can restart authenticated (`fdesetup supportsauthrestart`), the free bytes of the
//! data volume (`statvfs` of [`DATA_VOLUME`], through a [`FreeSpace`] port), and the Command Line
//! Tools: their receipt and its version (`pkgutil --pkg-info`, as [`crate::xcode_clt`] looks for
//! it) and their compiler on disk ([`CLANG`]). The update account and the device-management
//! settings are not read here; they stay at their defaults.
//!
//! `fdesetup status` writes `FileVault is On.` or `FileVault is Off.` (also `FileVault is Off, but
//! will be enabled after the next restart.`), then, while the volume is being converted,
//! `Encryption in progress: ...` or `Decryption in progress: ...`, which win over the first line;
//! `Deferred enablement appears to be active for user ...` is known and gives nothing. Any other
//! line is counted in the section's unparsed lines; with no state line the state is
//! `FILE_VAULT_STATE_UNSPECIFIED`. `fdesetup supportsauthrestart` writes `true` or `false`;
//! anything else is counted and read as `false`.
//!
//! Each program runs through a [`Launchctl`] port: on a Mac, as root, with an empty environment, no
//! standard input, a deadline and capped output. A program that does not run, does not end by its
//! deadline or (but `pkgutil`, whose non-zero exit means no receipt) exits non-zero fails the look
//! with the step's name; so does a data volume whose free space cannot be read.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::MAX_MAC_TEXT;

use crate::login_account::run;
use crate::maintenance_collect::{Collector, HostRoot, Look, Section, clip, listing};
use crate::xcode_clt::{CLANG, CLT_PACKAGE, LOOK_DEADLINE, PKGUTIL, version_of};

/// The tool that answers for FileVault.
pub const FDESETUP: &str = "/usr/bin/fdesetup";

/// The data volume, whose free space an update needs.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The arguments that ask FileVault's state.
const STATUS_ARGS: [&str; 1] = ["status"];

/// The arguments that ask whether FileVault can restart authenticated.
const AUTHRESTART_ARGS: [&str; 1] = ["supportsauthrestart"];

/// The lines of `fdesetup status` that name the state, and the state each names.
const STATE_LINES: [(&str, pb::FileVaultState); 3] = [
    ("FileVault is On.", pb::FileVaultState::On),
    ("FileVault is Off.", pb::FileVaultState::Off),
    (
        "FileVault is Off, but will be enabled after the next restart.",
        pb::FileVaultState::Off,
    ),
];

/// The starts of the lines of `fdesetup status` that name a conversion, and the state each names.
const CONVERSION_LINES: [(&str, pb::FileVaultState); 2] = [
    ("Encryption in progress:", pb::FileVaultState::Encrypting),
    ("Decryption in progress:", pb::FileVaultState::Decrypting),
];

/// The start of the line of `fdesetup status` that says enablement is deferred.
const DEFERRED_LINE: &str = "Deferred enablement appears to be active for user";

/// The free space of a volume.
pub trait FreeSpace: Send + Sync {
    /// The bytes free to an unprivileged writer.
    ///
    /// # Errors
    /// Why it could not be read, naming the volume.
    fn free_bytes(&self) -> Result<u64, String>;
}

/// The free space of the volume holding a path, as `statvfs` gives it: its available blocks
/// times its fragment size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statvfs(PathBuf);

impl Statvfs {
    /// The volume holding `path`.
    #[must_use]
    pub fn at(path: &Path) -> Statvfs {
        Statvfs(path.to_owned())
    }

    /// The Mac's data volume, [`DATA_VOLUME`].
    #[must_use]
    pub fn data_volume() -> Statvfs {
        Statvfs::at(Path::new(DATA_VOLUME))
    }
}

impl FreeSpace for Statvfs {
    fn free_bytes(&self) -> Result<u64, String> {
        let stat = rustix::fs::statvfs(self.0.as_path())
            .map_err(|error| format!("statvfs of {} failed: {error}", self.0.display()))?;
        Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
    }
}

/// What the collector `mac` reads through.
pub struct MacPorts {
    /// `fdesetup`, for `status` and `supportsauthrestart`.
    pub fdesetup: Box<dyn Launchctl>,
    /// `pkgutil`, for the receipt of the Command Line Tools.
    pub pkgutil: Box<dyn Launchctl>,
    /// The data volume's free space.
    pub space: Box<dyn FreeSpace>,
    /// The files the collector asks about ([`CLANG`]).
    pub root: HostRoot,
}

/// The collector `mac`.
pub struct MacReadiness {
    ports: MacPorts,
}

impl MacReadiness {
    /// The collector over `ports`.
    #[must_use]
    pub fn new(ports: MacPorts) -> MacReadiness {
        MacReadiness { ports }
    }

    /// The collector over the Mac's own programs, each bounded by [`LOOK_DEADLINE`], its data
    /// volume and its own files.
    #[must_use]
    pub fn at_host() -> MacReadiness {
        let tool = |program: &str, deadline: Duration| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), deadline))
        };
        MacReadiness::new(MacPorts {
            fdesetup: tool(FDESETUP, LOOK_DEADLINE),
            pkgutil: tool(PKGUTIL, LOOK_DEADLINE),
            space: Box::new(Statvfs::data_volume()),
            root: HostRoot::host(),
        })
    }

    /// The Command Line Tools: the receipt when `pkgutil` exits 0, with the version it names, and
    /// whether the compiler is a file.
    fn command_line_tools(&self) -> Result<pb::CommandLineToolsState, String> {
        let output = run(
            &*self.ports.pkgutil,
            "pkgutil --pkg-info",
            &[&format!("--pkg-info={CLT_PACKAGE}")],
        )?;
        let receipt = output.code == Some(0);
        let version = if receipt {
            clip(
                &version_of(&String::from_utf8_lossy(&output.stdout)),
                MAX_MAC_TEXT,
            )
        } else {
            String::new()
        };
        Ok(pb::CommandLineToolsState {
            receipt,
            files: self.ports.root.is_file(CLANG),
            version,
        })
    }
}

impl Collector for MacReadiness {
    fn name(&self) -> &'static str {
        "mac"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        let (_, status) = listing(&*self.ports.fdesetup, "fdesetup status", &STATUS_ARGS, &[0])?;
        let (filevault, status_unparsed) = parse_fdesetup_status(&status);
        let (_, answer) = listing(
            &*self.ports.fdesetup,
            "fdesetup supportsauthrestart",
            &AUTHRESTART_ARGS,
            &[0],
        )?;
        let (authrestart_supported, answer_unparsed) = parse_supportsauthrestart(&answer);
        let data_free_bytes = self.ports.space.free_bytes()?;
        let command_line_tools = self.command_line_tools()?;
        Ok(Section {
            mac: Some(pb::MacUpdateFacts {
                filevault: filevault.into(),
                authrestart_supported,
                data_free_bytes: Some(data_free_bytes),
                command_line_tools: Some(command_line_tools),
                ..pb::MacUpdateFacts::default()
            }),
            unparsed: status_unparsed.saturating_add(answer_unparsed),
            ..Section::default()
        })
    }
}

/// FileVault's state in what `fdesetup status` wrote, and how many of its lines were not known,
/// as the module says.
#[must_use]
pub fn parse_fdesetup_status(text: &str) -> (pb::FileVaultState, u32) {
    let mut state = pb::FileVaultState::Unspecified;
    let mut converting = false;
    let mut unparsed: u32 = 0;
    for line in text.lines().map(str::trim) {
        if let Some((_, named)) = STATE_LINES.iter().find(|(known, _)| line == *known) {
            if !converting {
                state = *named;
            }
        } else if let Some((_, named)) = CONVERSION_LINES
            .iter()
            .find(|(start, _)| line.starts_with(*start))
        {
            state = *named;
            converting = true;
        } else if !(line.is_empty() || line.starts_with(DEFERRED_LINE)) {
            unparsed = unparsed.saturating_add(1);
        }
    }
    (state, unparsed)
}

/// Whether `fdesetup supportsauthrestart` said `true`, and 1 when it said neither `true` nor
/// `false` (then read as `false`).
#[must_use]
pub fn parse_supportsauthrestart(text: &str) -> (bool, u32) {
    match text.trim() {
        "true" => (true, 0),
        "false" => (false, 0),
        _ => (false, 1),
    }
}

#[cfg(test)]
#[path = "maintenance_mac_tests.rs"]
mod tests;
