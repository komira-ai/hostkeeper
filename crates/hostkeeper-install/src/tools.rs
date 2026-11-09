//! The tools an installer runs, by absolute path, and the port they run behind (packages design
//! section 5.1). On macOS no tool hostkeeper runs fetches (section 5.3); each runs with an empty
//! environment, no standard input and a deadline, through `hostkeeper-launchd`'s bounded
//! child-process runner, which also runs `codesign` for the daemon's own checks.

use std::time::Duration;

use hostkeeper_launchd::launchctl::OUTPUT_MAX;
use hostkeeper_launchd::{LaunchctlError, LaunchctlOutput};

/// A tool of the Mac's own, run by its absolute path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tool {
    /// `/usr/sbin/pkgutil`: the signature check, the expansion and the receipts.
    Pkgutil,
    /// `/usr/sbin/installer`: the install of a `.pkg`.
    Installer,
    /// `/usr/bin/lsbom`: the listing of a `Bom`.
    Lsbom,
    /// `/bin/launchctl`: a job's enablement.
    Launchctl,
}

impl Tool {
    /// Every tool.
    pub const ALL: [Tool; 4] = [Tool::Pkgutil, Tool::Installer, Tool::Lsbom, Tool::Launchctl];

    /// The tool's absolute path on a Mac.
    pub const fn path(self) -> &'static str {
        match self {
            Tool::Pkgutil => "/usr/sbin/pkgutil",
            Tool::Installer => "/usr/sbin/installer",
            Tool::Lsbom => "/usr/bin/lsbom",
            Tool::Launchctl => "/bin/launchctl",
        }
    }
}

/// What a tool printed, and how it exited (`None`: killed by a signal).
pub type ToolOutput = LaunchctlOutput;

/// Why a tool did not run to its end: it could not be started, or it passed its deadline.
pub type ToolError = LaunchctlError;

/// Runs a tool with `args` and waits for it at most `deadline`.
pub trait Tools: Send + Sync {
    fn run(&self, tool: Tool, args: &[String], deadline: Duration)
    -> Result<ToolOutput, ToolError>;
}

/// The host's tools, each a child process of the daemon. Compiled on macOS only, where the tools
/// are; everything that reads their output is measured on Linux against fakes.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Default)]
pub struct HostTools;

#[cfg(target_os = "macos")]
impl Tools for HostTools {
    fn run(
        &self,
        tool: Tool,
        args: &[String],
        deadline: Duration,
    ) -> Result<ToolOutput, ToolError> {
        use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
        LaunchctlProcess::new(std::path::Path::new(tool.path()), deadline).run(args)
    }
}

/// Whether a stream may have been cut at the runner's cap, so its end is unknown: a listing read
/// from it would be incomplete, and is refused instead.
pub fn may_be_cut(stream: &[u8]) -> bool {
    stream.len() >= OUTPUT_MAX
}

/// The last line of a tool's output that holds anything, from its standard error first, then its
/// standard output: what a failure report quotes of `installer`.
pub fn last_line(output: &ToolOutput) -> String {
    let pick = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(str::to_owned)
    };
    pick(&output.stderr)
        .or_else(|| pick(&output.stdout))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
