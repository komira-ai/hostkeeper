//! The command line of `hostkeeper-bootstrap` and its host: the ports a Mac provides.
//!
//! Every path has the Mac's own as its default, and a flag changes it (configuration is passed
//! as flags, never read from the environment). A run ends with exit status 0 when hostkeeper is
//! installed or was already there, 1 when it gave up (launchd starts the program again), and 2 for
//! a command line that is not understood.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use hostkeeper_install::tools::{Tool, ToolError, ToolOutput, Tools};
use hostkeeper_package::net::{Transport, UreqTransport};

use crate::run::{Paths, Pause, Ports, ThreadPause, run};
use crate::settings::{FALLBACK_FILE, MANAGED_PREFERENCES};

/// The flags of the program.
#[derive(Debug, Parser)]
#[command(
    name = "hostkeeper-bootstrap",
    about = "Installs hostkeeper's signed macOS pkg and exits; the daemon then enrolls."
)]
pub struct Args {
    /// The managed preferences of dev.hostkeeper.config: the first source of the farm values.
    #[arg(long, default_value = MANAGED_PREFERENCES)]
    pub managed_preferences: PathBuf,
    /// The settings file of a manual install: the source when there are no managed preferences.
    #[arg(long, default_value = FALLBACK_FILE)]
    pub fallback_file: PathBuf,
    /// The directory of hostkeeper.toml and macos-pkg.toml.
    #[arg(long, default_value = crate::run::CONFIG_DIR)]
    pub config_dir: PathBuf,
    /// The root-only directory the pkg is staged in.
    #[arg(long, default_value = crate::run::STAGING_DIR)]
    pub staging_dir: PathBuf,
    /// The user id that must own the settings files: root's.
    #[arg(long, default_value_t = 0)]
    pub trusted_uid: u32,
    /// The binary hostkeeper's pkg installs; the bootstrap does nothing when it is there.
    #[arg(long, default_value = hostkeeper_launchd::plist::INSTALLED_BINARY)]
    pub installed_binary: PathBuf,
    /// The update-account file, written when the settings name the update account.
    #[arg(long, default_value = crate::update_account::UPDATE_ACCOUNT_FILE)]
    pub update_account_file: PathBuf,
}

impl Args {
    fn paths(self) -> Paths {
        let mut paths = Paths::host();
        paths.sources.managed = self.managed_preferences;
        paths.sources.fallback = self.fallback_file;
        paths.sources.trusted_uid = self.trusted_uid;
        paths.config_dir = self.config_dir;
        paths.staging_dir = self.staging_dir;
        paths.binary = self.installed_binary;
        paths.update_account = self.update_account_file;
        paths
    }
}

/// The Mac's `pkgutil` and `installer`; on any other system, a tool that cannot be run.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostTools;

impl Tools for HostTools {
    fn run(
        &self,
        tool: Tool,
        args: &[String],
        deadline: Duration,
    ) -> Result<ToolOutput, ToolError> {
        run_on_host(tool, args, deadline)
    }
}

#[cfg(target_os = "macos")]
fn run_on_host(tool: Tool, args: &[String], deadline: Duration) -> Result<ToolOutput, ToolError> {
    hostkeeper_install::tools::HostTools.run(tool, args, deadline)
}

#[cfg(not(target_os = "macos"))]
fn run_on_host(tool: Tool, args: &[String], _deadline: Duration) -> Result<ToolOutput, ToolError> {
    Err(ToolError::Spawn {
        args: args.join(" "),
        why: format!("{} is a tool of a Mac, and this is not one", tool.path()),
    })
}

/// The DER certificates the system trusts, which the downloads' TLS trusts alone.
pub fn system_roots() -> Vec<Vec<u8>> {
    rustls_native_certs::load_native_certs()
        .certs
        .iter()
        .map(|der| der.as_ref().to_vec())
        .collect()
}

/// The program's entry point: [`execute`] on the host's ports.
pub fn main() -> ExitCode {
    execute(
        std::env::args_os(),
        &UreqTransport::new(&system_roots()),
        &HostTools,
        &ThreadPause,
    )
}

/// One run of the program with `args`, against the given ports.
pub fn execute<I: IntoIterator<Item = OsString>>(
    args: I,
    transport: &dyn Transport,
    tools: &dyn Tools,
    pause: &dyn Pause,
) -> ExitCode {
    let args = match Args::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            drop(error.print());
            return ExitCode::from(u8::from(error.use_stderr()) * 2);
        }
    };
    let log = |line: &str| eprintln!("hostkeeper-bootstrap: {line}");
    let ports = Ports {
        transport,
        tools,
        pause,
        log: &log,
    };
    match run(&args.paths(), &ports) {
        Ok(outcome) => {
            log(&outcome.to_string());
            ExitCode::SUCCESS
        }
        Err(gave_up) => {
            log(&gave_up.to_string());
            ExitCode::FAILURE
        }
    }
}
