//! Apple's Command Line Tools for Xcode on a Mac (`xcode-command-line-tools`,
//! [`crate::prerequisites`]), looked for and installed headless, as root, through a port for each
//! program (`pkgutil`, `xcode-select`, `xcodebuild`, `sw_vers` and `softwareupdate`) and a port
//! for the one file it looks at ([`FileCheck`]).
//!
//! **Look** ([`CommandLineTools::check`]): the tools are present in any of three ways.
//!
//! 1. Their receipt: `pkgutil --pkg-info=com.apple.pkg.CLTools_Executables` exits 0, and its
//!    `version:` line is the version. Then `xcode-select -p` must exit 0; when it does not,
//!    `xcode-select --switch /Library/Developer/CommandLineTools` and `xcode-select -p` again must
//!    both exit 0.
//! 2. With no receipt (any other exit of `pkgutil`), the tools on disk: the compiler [`CLANG`] is a
//!    file and `xcode-select -p` exits 0. The version is empty, since no receipt names it.
//! 3. With no receipt and no compiler there, a ready Xcode: the directory `xcode-select -p` names
//!    is an Xcode's (it ends with `.app/Contents/Developer`), `xcodebuild -license check` and
//!    `xcodebuild -checkFirstLaunchStatus` exit 0 (its licence is accepted and its first launch
//!    ran), and `xcodebuild -version` exits 0; its first line (`Xcode 16.1`) is the version. An Xcode
//!    carries the same compilers and SDKs.
//!
//! Anything else is absent: no receipt and `xcode-select -p` exiting non-zero, or a directory that
//! is neither, or an Xcode whose licence is not accepted or whose first launch did not run.
//!
//! **Install** ([`CommandLineTools::install`]): the marker file [`INSTALL_MARKER`] is made, never
//! through a link and never truncating a file that is there, which makes `softwareupdate` offer the tools; `softwareupdate --list` names them
//! in `* Label: ` lines, as `Command Line Tools for Xcode-<version>` (the form of older catalogs) or
//! `Command Line Tools for Xcode <version>-<version>` (the form of current ones, the last version
//! being the tools'); the label of the highest version is installed with
//! `softwareupdate --install <label>`; the marker is removed whatever happened after it was made (a
//! removal that fails is logged, not an error of the install); then the look must find the tools.
//!
//! An error names the step, its exit code and the first line of the program's standard error. A
//! listing that exits non-zero, or that names no label of the tools, fails with a reason that also
//! says what the Mac was offered, so that the coordinator shows the cause with no shell on the Mac:
//! the macOS version and build (`sw_vers`), the first line of the listing's standard output and of
//! its standard error that is not blank, and every label that holds `Command Line Tools` in any
//! case or, with none, every label it listed; each part cut to its own bound, and the whole to
//! [`MAX_PREREQUISITE_REASON`].

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_link::{Log, MAX_PREREQUISITE_REASON};

use crate::login_account::{exit_failure, run, run_ok};

/// The tool that reads the installed packages.
pub const PKGUTIL: &str = "/usr/sbin/pkgutil";

/// The tool that names the developer directory.
pub const XCODE_SELECT: &str = "/usr/bin/xcode-select";

/// The tool that answers for the Xcode the developer directory names.
pub const XCODEBUILD: &str = "/usr/bin/xcodebuild";

/// The tool that names the version and build of macOS.
pub const SW_VERS: &str = "/usr/bin/sw_vers";

/// The tool that lists and installs Apple's updates.
pub const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";

/// The file whose presence makes `softwareupdate` offer the Command Line Tools.
pub const INSTALL_MARKER: &str = "/tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress";

/// The developer directory the tools install.
pub const DEVELOPER_DIR: &str = "/Library/Developer/CommandLineTools";

/// The compiler the tools install: a file there means the tools are on disk, receipt or not.
pub const CLANG: &str = "/Library/Developer/CommandLineTools/usr/bin/clang";

/// The package the tools are recorded as.
pub(crate) const CLT_PACKAGE: &str = "com.apple.pkg.CLTools_Executables";

/// What the developer directory of an Xcode ends with.
const XCODE_DEVELOPER: &str = ".app/Contents/Developer";

/// What a label of the tools starts with, after the `* Label: ` of `softwareupdate --list`; a `-`
/// or a space follows it, then the version.
const LABEL_PREFIX: &str = "Command Line Tools for Xcode";

/// What a label a failed listing's reason names holds, in any case.
const TOOLS_WORDS: &str = "command line tools";

/// The reason of a listing with no label of the tools, before what it listed.
const NONE_OFFERED: &str = "softwareupdate --list offered no Command Line Tools for Xcode";

/// The longest first line of a stream a listing's reason quotes, in bytes.
const LINE_MAX: usize = 48;

/// The longest label a listing's reason quotes, in bytes.
const LABEL_MAX: usize = 64;

/// The longest macOS version or build a listing's reason quotes, in bytes.
const VERSION_PART_MAX: usize = 16;

/// The longest `pkgutil`, `xcode-select`, `xcodebuild` and `sw_vers` run.
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

/// Says whether a file is there, the one thing the look asks of the file system.
pub trait FileCheck: Send + Sync {
    /// Whether `path` names a regular file, links followed; `false` for anything else, and for a
    /// path that cannot be read.
    fn is_file(&self, path: &Path) -> bool;
}

/// The files of the host itself.
pub struct HostFiles;

impl FileCheck for HostFiles {
    fn is_file(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|meta| meta.is_file())
    }
}

/// What the Command Line Tools are looked for and installed through: a port for each program, the
/// file port, and the path of the marker file.
pub struct Ports {
    /// `pkgutil`, for the receipt.
    pub pkgutil: Box<dyn Launchctl>,
    /// `xcode-select`, for the developer directory and its switch.
    pub xcode_select: Box<dyn Launchctl>,
    /// `xcodebuild`, for the selected Xcode's licence, first launch and version.
    pub xcodebuild: Box<dyn Launchctl>,
    /// `sw_vers`, for the macOS version and build of a failed listing's reason.
    pub sw_vers: Box<dyn Launchctl>,
    /// `softwareupdate`, for its list.
    pub list: Box<dyn Launchctl>,
    /// `softwareupdate`, for its install.
    pub install: Box<dyn Launchctl>,
    /// The files the look asks about ([`CLANG`]).
    pub files: Box<dyn FileCheck>,
    /// Where the marker file is made.
    pub marker: PathBuf,
}

/// The Command Line Tools of a Mac, over a port for each program.
pub struct CommandLineTools {
    pkgutil: Box<dyn Launchctl>,
    xcode_select: Box<dyn Launchctl>,
    xcodebuild: Box<dyn Launchctl>,
    sw_vers: Box<dyn Launchctl>,
    list: Box<dyn Launchctl>,
    install: Box<dyn Launchctl>,
    files: Box<dyn FileCheck>,
    marker: PathBuf,
}

impl CommandLineTools {
    /// The steps over `ports`.
    #[must_use]
    pub fn new(ports: Ports) -> CommandLineTools {
        let Ports {
            pkgutil,
            xcode_select,
            xcodebuild,
            sw_vers,
            list,
            install,
            files,
            marker,
        } = ports;
        CommandLineTools {
            pkgutil,
            xcode_select,
            xcodebuild,
            sw_vers,
            list,
            install,
            files,
            marker,
        }
    }

    /// The steps over the Mac's own programs, each bounded by its deadline, its own files, and the
    /// marker [`INSTALL_MARKER`].
    #[must_use]
    pub fn at_host() -> CommandLineTools {
        let tool = |program: &str, deadline: Duration| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), deadline))
        };
        CommandLineTools::new(Ports {
            pkgutil: tool(PKGUTIL, LOOK_DEADLINE),
            xcode_select: tool(XCODE_SELECT, LOOK_DEADLINE),
            xcodebuild: tool(XCODEBUILD, LOOK_DEADLINE),
            sw_vers: tool(SW_VERS, LOOK_DEADLINE),
            list: tool(SOFTWAREUPDATE, LIST_DEADLINE),
            install: tool(SOFTWAREUPDATE, INSTALL_DEADLINE),
            files: Box::new(HostFiles),
            marker: PathBuf::from(INSTALL_MARKER),
        })
    }

    /// The version of the tools when they are present, or `None` when they are not, as the module
    /// says.
    ///
    /// # Errors
    /// [`CommandLineToolsError`] when a program does not run, or the package is installed and no
    /// developer directory is selected.
    pub fn check(&self) -> Result<Option<String>, CommandLineToolsError> {
        let info = run(
            &*self.pkgutil,
            "pkgutil --pkg-info",
            &[&format!("--pkg-info={CLT_PACKAGE}")],
        )
        .or_else(fail)?;
        if info.code != Some(0) {
            return self.without_receipt();
        }
        let version = version_of(&String::from_utf8_lossy(&info.stdout));
        if self.selected("xcode-select -p")?.is_some() {
            return Ok(Some(version));
        }
        run_ok(
            &*self.xcode_select,
            "xcode-select --switch",
            &["--switch", DEVELOPER_DIR],
        )
        .or_else(fail)?;
        if self.selected("xcode-select -p after its switch")?.is_some() {
            Ok(Some(version))
        } else {
            fail("xcode-select -p names no developer directory after its switch".to_owned())
        }
    }

    /// The developer directory `xcode-select -p` names when it exits 0 (the first line of its
    /// standard output that is not blank), or `None` when it does not.
    fn selected(&self, what: &str) -> Result<Option<String>, CommandLineToolsError> {
        let output = run(&*self.xcode_select, what, &["-p"]).or_else(fail)?;
        Ok((output.code == Some(0))
            .then(|| first_line(&String::from_utf8_lossy(&output.stdout)).to_owned()))
    }

    /// The look of a Mac with no receipt: the tools on disk, or a ready Xcode, as the module says.
    fn without_receipt(&self) -> Result<Option<String>, CommandLineToolsError> {
        let on_disk = self.files.is_file(Path::new(CLANG));
        let Some(directory) = self.selected("xcode-select -p")? else {
            return Ok(None);
        };
        if on_disk {
            return Ok(Some(String::new()));
        }
        if !is_xcode(&directory) {
            return Ok(None);
        }
        self.ready_xcode()
    }

    /// The version of the selected Xcode when its licence is accepted and its first launch ran,
    /// else `None`.
    fn ready_xcode(&self) -> Result<Option<String>, CommandLineToolsError> {
        let steps: [(&str, &[&str]); 3] = [
            ("xcodebuild -license check", &["-license", "check"]),
            (
                "xcodebuild -checkFirstLaunchStatus",
                &["-checkFirstLaunchStatus"],
            ),
            ("xcodebuild -version", &["-version"]),
        ];
        let mut stdout = Vec::new();
        for (what, args) in steps {
            let output = run(&*self.xcodebuild, what, args).or_else(fail)?;
            if output.code != Some(0) {
                return Ok(None);
            }
            stdout = output.stdout;
        }
        Ok(Some(
            first_line(&String::from_utf8_lossy(&stdout)).to_owned(),
        ))
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

    /// `softwareupdate --list` and `softwareupdate --install` of the highest label it names; a
    /// listing that exits non-zero or names none fails with [`listing_reason`].
    fn install_label(&self) -> Result<(), CommandLineToolsError> {
        let output = run(&*self.list, "softwareupdate --list", &["--list"]).or_else(fail)?;
        let listed = String::from_utf8_lossy(&output.stdout);
        let head = if output.code == Some(0) {
            if let Some(label) = highest_label(&listed) {
                return run_ok(
                    &*self.install,
                    "softwareupdate --install",
                    &["--install", label],
                )
                .map(drop)
                .or_else(fail);
            }
            NONE_OFFERED.to_owned()
        } else {
            exit_failure("softwareupdate --list", &output, false)
        };
        fail(listing_reason(
            &head,
            &self.macos(),
            &listed,
            &output.stderr_text(),
        ))
    }

    /// `macOS <version> <build>` from `sw_vers`, `?` for a part it does not name, or
    /// `macOS unknown` when it does not run or exits non-zero.
    fn macos(&self) -> String {
        match run(&*self.sw_vers, "sw_vers", &[]) {
            Ok(output) if output.code == Some(0) => {
                let text = String::from_utf8_lossy(&output.stdout);
                format!(
                    "macOS {} {}",
                    sw_vers_value(&text, "ProductVersion"),
                    sw_vers_value(&text, "BuildVersion")
                )
            }
            _ => "macOS unknown".to_owned(),
        }
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
pub(crate) fn version_of(text: &str) -> String {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("version:"))
        .map(|version| version.trim().to_owned())
        .unwrap_or_default()
}

/// The labels of the `* Label: ` lines of `softwareupdate --list`, in its order.
fn labels(listing: &str) -> impl Iterator<Item = &str> {
    listing
        .lines()
        .filter_map(|line| line.trim().strip_prefix("* Label:").map(str::trim))
}

/// The version of the tools a label names: all that follows [`LABEL_PREFIX`] and a `-`, or, after a
/// space, the last of `<version>-<version>`; `None` for a label of other software or with no
/// version.
fn tools_version(label: &str) -> Option<&str> {
    let rest = label.strip_prefix(LABEL_PREFIX)?;
    let version = match rest.strip_prefix('-') {
        Some(version) => version,
        None => rest.strip_prefix(' ')?.rsplit('-').next()?,
    };
    Some(version).filter(|version| !version.is_empty())
}

/// The reason of a listing that failed: `head` (the step's failure, or [`NONE_OFFERED`]), then
/// `macos`, the first line of `listing` and of `stderr` that is not blank (or that the stream is
/// empty), each cut to [`LINE_MAX`], then the labels that hold [`TOOLS_WORDS`] in any case or, with
/// none, every label listed, in its order, each cut to [`LABEL_MAX`]; the whole cut to
/// [`MAX_PREREQUISITE_REASON`] at a character boundary.
fn listing_reason(head: &str, macos: &str, listing: &str, stderr: &str) -> String {
    let mut reason = format!(
        "{head}; {macos}; {}; {}",
        stream("stdout", listing),
        stream("stderr", stderr)
    );
    let listed: Vec<&str> = labels(listing).collect();
    let tools: Vec<&str> = listed
        .iter()
        .copied()
        .filter(|label| label.to_lowercase().contains(TOOLS_WORDS))
        .collect();
    let (name, quoted) = if tools.is_empty() {
        ("it listed", listed)
    } else {
        ("Command Line Tools labels", tools)
    };
    for (index, label) in quoted.into_iter().enumerate() {
        if reason.len() >= MAX_PREREQUISITE_REASON {
            break;
        }
        let between = if index == 0 {
            format!("; {name}: ")
        } else {
            "; ".to_owned()
        };
        reason.push_str(&between);
        reason.push_str(cut(label, LABEL_MAX));
    }
    cut(&reason, MAX_PREREQUISITE_REASON).to_owned()
}

/// `<name>: <first line>` of a stream, its first line that is not blank cut to [`LINE_MAX`], or
/// `<name> empty` when it has none.
fn stream(name: &str, text: &str) -> String {
    match first_line(text) {
        "" => format!("{name} empty"),
        line => format!("{name}: {}", cut(line, LINE_MAX)),
    }
}

/// The first line of `text` that is not blank, trimmed; empty when there is none.
fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
}

/// The value of the `key:` line of what `sw_vers` wrote, trimmed and cut to [`VERSION_PART_MAX`];
/// `?` when there is none or it is empty.
fn sw_vers_value<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim() == key)
        .map(|(_, value)| cut(value.trim(), VERSION_PART_MAX))
        .filter(|value| !value.is_empty())
        .unwrap_or("?")
}

/// Whether the developer directory `directory` is an Xcode's: it ends with [`XCODE_DEVELOPER`],
/// a `/` after it or not.
fn is_xcode(directory: &str) -> bool {
    directory.trim_end_matches('/').ends_with(XCODE_DEVELOPER)
}

/// The longest start of `text` of at most `max` bytes that ends at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The label of the highest version among the labels of the tools in `softwareupdate --list`
/// ([`tools_version`]); the first of equal versions.
fn highest_label(listing: &str) -> Option<&str> {
    let mut best: Option<(&str, &str)> = None;
    for label in labels(listing) {
        let Some(version) = tools_version(label) else {
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
