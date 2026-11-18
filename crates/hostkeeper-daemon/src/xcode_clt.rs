//! Apple's Command Line Tools for Xcode on a Mac (`xcode-command-line-tools`,
//! [`crate::prerequisites`]), looked for and installed headless, as root, through a port for each
//! program: `pkgutil`, `xcode-select` and `softwareupdate`.
//!
//! **Look** ([`CommandLineTools::check`]): `pkgutil --pkg-info=com.apple.pkg.CLTools_Executables`
//! exiting 0 means the package is installed, and its `version:` line is the version; any other exit
//! means it is not. When it is, `xcode-select -p` must exit 0; when it does not,
//! `xcode-select --switch /Library/Developer/CommandLineTools` and `xcode-select -p` again must
//! both exit 0.
//!
//! **Install** ([`CommandLineTools::install`]): the marker file [`INSTALL_MARKER`] is made, never
//! through a link and never truncating a file that is there, which makes `softwareupdate` offer the tools; `softwareupdate --list` names them
//! as `* Label: Command Line Tools for Xcode-<version>` lines; the label of the highest version is
//! installed with `softwareupdate --install <label>`; the marker is removed whatever happened after
//! it was made (a removal that fails is logged, not an error of the install); then the look must
//! find the tools.
//!
//! An error names the step, its exit code and the first line of the program's standard error.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_link::Log;

use crate::login_account::{run, run_ok};

/// The tool that reads the installed packages.
pub const PKGUTIL: &str = "/usr/sbin/pkgutil";

/// The tool that names the developer directory.
pub const XCODE_SELECT: &str = "/usr/bin/xcode-select";

/// The tool that lists and installs Apple's updates.
pub const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";

/// The file whose presence makes `softwareupdate` offer the Command Line Tools.
pub const INSTALL_MARKER: &str = "/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress";

/// The developer directory the tools install.
pub const DEVELOPER_DIR: &str = "/Library/Developer/CommandLineTools";

/// The package the tools are recorded as.
const PACKAGE: &str = "com.apple.pkg.CLTools_Executables";

/// What a label of the tools starts with, after the `* Label: ` of `softwareupdate --list`.
const LABEL_PREFIX: &str = "Command Line Tools for Xcode-";

/// The longest `pkgutil` and `xcode-select` run.
pub const LOOK_DEADLINE: Duration = Duration::from_secs(60);

/// The longest `softwareupdate --list` runs.
pub const LIST_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// The longest `softwareupdate --install` runs.
pub const INSTALL_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);

/// Why the tools could not be looked for or installed: the step, and why it failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CommandLineToolsError(String);

fn fail<T>(reason: String) -> Result<T, CommandLineToolsError> {
    Err(CommandLineToolsError(reason))
}

/// The Command Line Tools of a Mac, over a port for each program.
pub struct CommandLineTools {
    pkgutil: Box<dyn Launchctl>,
    xcode_select: Box<dyn Launchctl>,
    list: Box<dyn Launchctl>,
    install: Box<dyn Launchctl>,
    marker: PathBuf,
}

impl CommandLineTools {
    /// The steps over `pkgutil`, `xcode-select`, `softwareupdate` for its list, `softwareupdate`
    /// for its install, and the marker file at `marker`.
    #[must_use]
    pub fn new(
        pkgutil: Box<dyn Launchctl>,
        xcode_select: Box<dyn Launchctl>,
        list: Box<dyn Launchctl>,
        install: Box<dyn Launchctl>,
        marker: PathBuf,
    ) -> CommandLineTools {
        CommandLineTools {
            pkgutil,
            xcode_select,
            list,
            install,
            marker,
        }
    }

    /// The steps over the Mac's own programs, each bounded by its deadline, and the marker
    /// [`INSTALL_MARKER`].
    #[must_use]
    pub fn at_host() -> CommandLineTools {
        let tool = |program: &str, deadline: Duration| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), deadline))
        };
        CommandLineTools::new(
            tool(PKGUTIL, LOOK_DEADLINE),
            tool(XCODE_SELECT, LOOK_DEADLINE),
            tool(SOFTWAREUPDATE, LIST_DEADLINE),
            tool(SOFTWAREUPDATE, INSTALL_DEADLINE),
            PathBuf::from(INSTALL_MARKER),
        )
    }

    /// The version of the installed tools, or `None` when the package is not installed, as the
    /// module says.
    ///
    /// # Errors
    /// [`CommandLineToolsError`] when a program does not run, or the package is installed and no
    /// developer directory is selected.
    pub fn check(&self) -> Result<Option<String>, CommandLineToolsError> {
        let info = run(
            &*self.pkgutil,
            "pkgutil --pkg-info",
            &[&format!("--pkg-info={PACKAGE}")],
        )
        .or_else(fail)?;
        if info.code != Some(0) {
            return Ok(None);
        }
        let version = version_of(&String::from_utf8_lossy(&info.stdout));
        if self.selected("xcode-select -p")? {
            return Ok(Some(version));
        }
        run_ok(
            &*self.xcode_select,
            "xcode-select --switch",
            &["--switch", DEVELOPER_DIR],
        )
        .or_else(fail)?;
        if self.selected("xcode-select -p after its switch")? {
            Ok(Some(version))
        } else {
            fail("xcode-select -p names no developer directory after its switch".to_owned())
        }
    }

    /// Whether `xcode-select -p` exits 0.
    fn selected(&self, what: &str) -> Result<bool, CommandLineToolsError> {
        let output = run(&*self.xcode_select, what, &["-p"]).or_else(fail)?;
        Ok(output.code == Some(0))
    }

    /// Installs the tools, as the module says, and returns their version.
    ///
    /// # Errors
    /// [`CommandLineToolsError`] for the first step that fails, or when the look after the install
    /// does not find the tools.
    pub fn install(&self, log: &dyn Log) -> Result<String, CommandLineToolsError> {
        make_marker(&self.marker).or_else(fail)?;
        let installed = self.install_label();
        match fs::remove_file(&self.marker) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => log.line(&format!(
                "error: host prerequisites: the install marker of the Command Line Tools could not be removed: {error}"
            )),
        }
        installed?;
        match self.check()? {
            Some(version) => Ok(version),
            None => {
                fail("the Command Line Tools are not installed after softwareupdate".to_owned())
            }
        }
    }

    /// `softwareupdate --list` and `softwareupdate --install` of the highest label it names.
    fn install_label(&self) -> Result<(), CommandLineToolsError> {
        let listed = run_ok(&*self.list, "softwareupdate --list", &["--list"]).or_else(fail)?;
        let Some(label) = highest_label(&listed) else {
            return fail(
                "softwareupdate --list offered no Command Line Tools for Xcode".to_owned(),
            );
        };
        run_ok(
            &*self.install,
            "softwareupdate --install",
            &["--install", label],
        )
        .or_else(fail)?;
        Ok(())
    }
}

/// Makes the marker at `path` through no link: a regular file that is there already is used as it
/// is (it is opened, never truncated or written, so a hard link a local user put there to another
/// file changes nothing of that file). Anything else there is refused by the open itself: a link
/// (`O_NOFOLLOW`), a directory, and a pipe or a socket (`O_NONBLOCK`, so that the open never waits
/// for a reader). Its removal after the install takes away the name alone.
fn make_marker(path: &Path) -> Result<(), String> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map(drop)
        .map_err(|error| {
            format!(
                "the install marker {} could not be made: {error}",
                path.display()
            )
        })
}

/// The value of the `version:` line of `pkgutil --pkg-info`; empty when there is none.
fn version_of(text: &str) -> String {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("version:"))
        .map(|version| version.trim().to_owned())
        .unwrap_or_default()
}

/// The label of the highest version among the `* Label: Command Line Tools for Xcode-<version>`
/// lines of `softwareupdate --list`; the first of equal versions.
fn highest_label(listing: &str) -> Option<&str> {
    let mut best: Option<(&str, &str)> = None;
    for line in listing.lines() {
        let Some(label) = line.trim().strip_prefix("* Label:").map(str::trim) else {
            continue;
        };
        let Some(version) = label.strip_prefix(LABEL_PREFIX) else {
            continue;
        };
        if best.is_none_or(|(_, held)| compare_versions(version, held).is_gt()) {
            best = Some((label, version));
        }
    }
    best.map(|(label, _)| label)
}

/// `left` against `right` as dotted versions: part by part, numbers by value and any other part as
/// text; the version with a part more is the higher.
fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(left), Some(right)) => {
                let order = match (left.parse::<u64>(), right.parse::<u64>()) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    _ => left.cmp(right),
                };
                if order.is_ne() {
                    return order;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "xcode_clt_tests.rs"]
mod tests;
