//! What the bootstrap does on each start (docs/design/mac-bootstrap.md, sections 3, 6 and 9, with
//! the decisions of the owner's 2026-11-14 defaults): install hostkeeper first and let its daemon
//! enroll. The bootstrap does no enrollment and carries no certificate.
//!
//! 1. A Mac that has hostkeeper (the binary its pkg installs) or a host certificate
//!    ([`Paths::pki_current`]) is done: [`Outcome::AlreadySetUp`], nothing touched.
//! 2. The settings are read ([`crate::settings`]), the repository's descriptor and pointer name the
//!    pkg ([`crate::package::locate`]), and the pkg is downloaded to a root-only staging file, held
//!    to the pointer's size and SHA-256.
//! 3. `pkgutil --check-signature` must show a Developer ID Installer identity of the pinned Team ID
//!    that Apple issued ([`crate::signer`]). A pkg that fails any check is deleted and nothing
//!    else is written or run.
//! 4. `hostkeeper.toml` and `macos-pkg.toml` are written when absent ([`crate::files`]), then
//!    `installer -pkg <file> -target /` runs, whose postinstall loads the daemon's job; the daemon
//!    finds its config and enrolls through the normal approval flow.
//!
//! A failed attempt is retried after a pause until the sum of the pauses reaches `wait_secs`;
//! then [`run`] gives up with the last error, the program exits non-zero and launchd starts it
//! again. A step changes nothing it has not finished, so each attempt is safe to repeat.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_install::macos_pkg::signers::{Fingerprint, SignersError, parse};
use hostkeeper_install::tools::{Tool, ToolError, ToolOutput, Tools, last_line};
use hostkeeper_launchd::plist::INSTALLED_BINARY;
use hostkeeper_package::net::Transport;

use crate::files::{FileError, Written, write_if_absent};
use crate::package::{self, PackageError};
use crate::settings::{Bounds, Settings, SettingsError, Sources};
use crate::signer::{self, SignerError};

/// The current generation of the host certificate the daemon keeps
/// (`hostkeeper_enroll::PkiDir`): its presence means the Mac enrolled before.
pub const PKI_CURRENT: &str = "/etc/updater/pki/current";
/// The directory of the daemon's config files.
pub const CONFIG_DIR: &str = "/etc/hostkeeper";
/// The root-only directory the pkg is staged in.
pub const STAGING_DIR: &str = "/var/lib/hostkeeper-bootstrap";
/// The staged pkg's name, ending in `.pkg`, which `installer` requires.
const STAGED: &str = "hostkeeper.pkg";
/// The daemon config's file name, and the signers file's.
const DAEMON_CONFIG: &str = "hostkeeper.toml";
const SIGNERS_FILE: &str = "macos-pkg.toml";

/// Where the bootstrap reads and writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub sources: Sources,
    /// The binary the hostkeeper pkg installs.
    pub binary: PathBuf,
    /// The host certificate's `current`.
    pub pki_current: PathBuf,
    /// The directory of `hostkeeper.toml` and `macos-pkg.toml`.
    pub config_dir: PathBuf,
    /// The staging directory, made 0700 when it is not there.
    pub staging_dir: PathBuf,
}

impl Paths {
    /// The Mac's own paths.
    pub fn host() -> Paths {
        Paths {
            sources: Sources::host(),
            binary: PathBuf::from(INSTALLED_BINARY),
            pki_current: PathBuf::from(PKI_CURRENT),
            config_dir: PathBuf::from(CONFIG_DIR),
            staging_dir: PathBuf::from(STAGING_DIR),
        }
    }
}

/// The pause between attempts.
pub trait Pause {
    fn pause(&self, secs: u64);
}

/// The real pause: the thread sleeps.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadPause;

impl Pause for ThreadPause {
    fn pause(&self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }
}

/// What the bootstrap runs against.
pub struct Ports<'a> {
    /// The HTTPS transport of the downloads.
    pub transport: &'a dyn Transport,
    /// `pkgutil` and `installer`.
    pub tools: &'a dyn Tools,
    pub pause: &'a dyn Pause,
    /// Where progress and each failed attempt's reason are told, one line each.
    pub log: &'a dyn Fn(&str),
}

/// What a run did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Hostkeeper was there already; nothing was touched.
    AlreadySetUp,
    /// The pkg of `version` was installed.
    Installed {
        version: String,
        daemon_config: Written,
        signers_file: Written,
    },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::AlreadySetUp => f.write_str("hostkeeper is installed; nothing to do"),
            Outcome::Installed {
                version,
                daemon_config,
                signers_file,
            } => write!(
                f,
                "installed hostkeeper {version} (hostkeeper.toml {daemon_config:?}, macos-pkg.toml {signers_file:?})"
            ),
        }
    }
}

/// Why an attempt failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error("the pkg is refused: {0}")]
    Signer(#[from] SignerError),
    #[error(transparent)]
    File(#[from] FileError),
    #[error("{tool} could not be run: {source}")]
    Tool {
        tool: &'static str,
        #[source]
        source: ToolError,
    },
    #[error("installer exited {code:?}: {last}")]
    Installer { code: Option<i32>, last: String },
    #[error("the signers file text is refused: {0}")]
    Signers(#[from] SignersError),
    #[error("the staging directory {path}: {source}")]
    Staging {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is not UTF-8")]
    NotUtf8(String),
}

/// The run that ended without an install: every attempt failed until the wait was used up.
#[derive(Debug, thiserror::Error)]
#[error("gave up after {attempts} attempt(s); the last failed: {last}")]
pub struct GaveUp {
    pub attempts: u32,
    #[source]
    pub last: Error,
}

/// Whether hostkeeper is on this Mac: the binary its pkg installs is there, or the Mac holds a
/// host certificate. The pkg lays its payload down before its postinstall runs, so a binary that
/// is there is a Mac the pkg reached.
pub fn is_set_up(paths: &Paths) -> bool {
    let there = |path: &Path| fs::symlink_metadata(path).is_ok();
    there(&paths.binary) || there(&paths.pki_current)
}

/// One run, as the module says.
pub fn run(paths: &Paths, ports: &Ports<'_>) -> Result<Outcome, GaveUp> {
    let mut waited = 0u64;
    let mut attempts = 0u32;
    loop {
        if is_set_up(paths) {
            return Ok(Outcome::AlreadySetUp);
        }
        attempts += 1;
        let settings = paths.sources.load();
        let bounds = settings
            .as_ref()
            .map_or_else(|_| Bounds::default(), |s| s.bounds);
        let result = settings
            .map_err(Error::from)
            .and_then(|settings| install(paths, ports, &settings));
        let error = match result {
            Ok(outcome) => return Ok(outcome),
            Err(error) => error,
        };
        (ports.log)(&format!("attempt {attempts} failed: {error}"));
        if waited.saturating_add(bounds.retry_secs) > bounds.wait_secs {
            return Err(GaveUp {
                attempts,
                last: error,
            });
        }
        (ports.log)(&format!("trying again in {} seconds", bounds.retry_secs));
        ports.pause.pause(bounds.retry_secs);
        waited += bounds.retry_secs;
    }
}

/// `path` as the text of a tool's argument.
fn arg(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::NotUtf8(path.display().to_string()))
}

fn run_tool(
    ports: &Ports<'_>,
    tool: Tool,
    args: &[String],
    deadline: u64,
) -> Result<ToolOutput, Error> {
    let name = match tool {
        Tool::Installer => "installer",
        _ => "pkgutil",
    };
    ports
        .tools
        .run(tool, args, Duration::from_secs(deadline))
        .map_err(|source| Error::Tool { tool: name, source })
}

/// The text of the signers file: the settings' signers, else the leaf of the checked pkg.
fn signers_text(settings: &Settings, leaf: &Fingerprint) -> Result<String, Error> {
    let own: Vec<String> = match settings.macos_pkg_signers.is_empty() {
        true => vec![leaf.as_str().to_owned()],
        false => settings.macos_pkg_signers.clone(),
    };
    let mut table = toml::Table::new();
    table.insert(
        "signers".into(),
        toml::Value::Array(own.into_iter().map(Into::into).collect()),
    );
    let text =
        format!("# Written by hostkeeper-bootstrap (docs/design/mac-bootstrap.md).\n{table}");
    parse(text.as_bytes())?;
    Ok(text)
}

/// One attempt, with the staged pkg removed however it ends.
fn install(paths: &Paths, ports: &Ports<'_>, settings: &Settings) -> Result<Outcome, Error> {
    let staging = &paths.staging_dir;
    let staging_error = |source| Error::Staging {
        path: staging.display().to_string(),
        source,
    };
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(staging)
            .map_err(staging_error)?;
    }
    let pkg = staging.join(STAGED);
    drop(fs::remove_file(&pkg));
    let outcome = stage_and_install(paths, ports, settings, &pkg);
    drop(fs::remove_file(&pkg));
    outcome
}

fn stage_and_install(
    paths: &Paths,
    ports: &Ports<'_>,
    settings: &Settings,
    pkg: &Path,
) -> Result<Outcome, Error> {
    let located = package::locate(ports.transport, settings)?;
    (ports.log)(&format!(
        "fetching hostkeeper {} from {}",
        located.version, located.url
    ));
    package::download(ports.transport, settings, &located, pkg)?;
    let pkg_arg = arg(pkg)?;
    let checked = run_tool(
        ports,
        Tool::Pkgutil,
        &["--check-signature".to_owned(), pkg_arg.clone()],
        settings.bounds.tool_timeout_secs,
    )?;
    let leaf = signer::check(
        checked.code,
        &String::from_utf8_lossy(&checked.stdout),
        &settings.team_id,
    )?;
    let signers = signers_text(settings, &leaf)?;
    let daemon_config = write_if_absent(
        &paths.config_dir.join(DAEMON_CONFIG),
        &settings.daemon_config(),
        0o600,
    )?;
    let signers_file = write_if_absent(&paths.config_dir.join(SIGNERS_FILE), &signers, 0o644)?;
    let installed = run_tool(
        ports,
        Tool::Installer,
        &[
            "-pkg".to_owned(),
            pkg_arg,
            "-target".to_owned(),
            "/".to_owned(),
        ],
        settings.bounds.installer_timeout_secs,
    )?;
    if installed.code != Some(0) {
        return Err(Error::Installer {
            code: installed.code,
            last: last_line(&installed),
        });
    }
    Ok(Outcome::Installed {
        version: located.version,
        daemon_config,
        signers_file,
    })
}
