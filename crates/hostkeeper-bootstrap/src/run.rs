//! What the bootstrap does on each start (docs/design/mac-bootstrap.md, sections 3, 6 and 9, with
//! the decisions of the owner's 2026-11-14 defaults): install hostkeeper first and let its daemon
//! enroll. The bootstrap does no enrollment and carries no certificate.
//!
//! 1. A Mac that has hostkeeper (the binary its pkg installs) or a host certificate
//!    ([`Paths::pki_current`]) is not installed again, but launchd is asked whether the daemon's
//!    job is loaded (`launchctl print system/dev.hostkeeper.hostkeeper`). A loaded job is done:
//!    [`Outcome::AlreadySetUp`], nothing touched. A job that is not loaded (launchd does not load
//!    a job marked disabled in its override database, so a Mac whose job was left disabled has
//!    none running after a restart) is enabled (`launchctl enable`) and loaded (`launchctl
//!    bootstrap system` of [`Paths::daemon_plist`]): [`Outcome::Loaded`]. A bootstrap that fails
//!    is followed by one more `print`: a job loaded by then (launchd loads the jobs of a booting
//!    Mac while this one starts) is success, and one still not loaded is an error of the attempt.
//!    No other tool runs, and no file is written. Settings are not needed for this step; their
//!    tool timeout is used when they can be read, else the default.
//! 2. The settings are read ([`crate::settings`]), the repository's descriptor and pointer name the
//!    pkg ([`crate::package::locate`]), and the pkg is downloaded to a root-only staging file, held
//!    to the pointer's size and SHA-256.
//! 3. `pkgutil --check-signature` must show a Developer ID Installer identity of the pinned Team ID
//!    that Apple issued ([`crate::signer`]). A pkg that fails any check is deleted and nothing
//!    else is written or run.
//! 4. `hostkeeper.toml` and `macos-pkg.toml` are written when absent ([`crate::files`]), and so
//!    is the update-account file when the settings name the update account
//!    ([`crate::update_account`], root's, mode 0600; one that is there must be root's with mode
//!    0600 and is kept). Then `installer -pkg <file> -target /` runs, whose postinstall loads the
//!    daemon's job; the daemon finds its config and enrolls through the normal approval flow. When
//!    a write or `installer` fails, the files this attempt made are removed.
//!
//! A failed attempt, whether it checked the job or installed, is retried after a pause until the
//! sum of the pauses reaches `wait_secs`; then [`run`] gives up with the last error, the program
//! exits non-zero and launchd starts it again. A step changes nothing it has not finished, so each attempt is safe to repeat.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_install::macos_pkg::signers::{Fingerprint, SignersError, parse};
use hostkeeper_install::tools::{Tool, ToolError, ToolOutput, Tools, last_line};
use hostkeeper_launchd::Label;
use hostkeeper_launchd::launchctl::{
    EXIT_NOT_LOADED, NOT_LOADED_TEXT, bootstrap_args, enable_args, print_args,
};
use hostkeeper_launchd::plist::INSTALLED_BINARY;
use hostkeeper_launchd::postinstall::Postinstall;
use hostkeeper_package::net::Transport;

use crate::files::{FileError, Written, write_if_absent, write_private};
use crate::package::{self, PackageError};
use crate::settings::{Bounds, Settings, SettingsError, Sources};
use crate::signer::{self, SignerError};
use crate::update_account::UPDATE_ACCOUNT_FILE;

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
/// The marker's name.
const INSTALLED: &str = "installed";
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
    /// The update-account file.
    pub update_account: PathBuf,
    /// The daemon's launchd plist, which the hostkeeper pkg installs and `launchctl bootstrap`
    /// loads when the job is not loaded.
    pub daemon_plist: PathBuf,
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
            update_account: PathBuf::from(UPDATE_ACCOUNT_FILE),
            daemon_plist: Postinstall::pkg().plist,
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
    /// `pkgutil`, `installer` and `launchctl`.
    pub tools: &'a dyn Tools,
    pub pause: &'a dyn Pause,
    /// Where progress and each failed attempt's reason are told, one line each.
    pub log: &'a dyn Fn(&str),
}

/// What a run did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Hostkeeper was there already and launchd has the daemon's job loaded; nothing was touched.
    AlreadySetUp,
    /// Hostkeeper was there already and launchd did not have the daemon's job loaded: the job was
    /// enabled and loaded.
    Loaded,
    /// The pkg of `version` was installed.
    Installed {
        version: String,
        daemon_config: Written,
        signers_file: Written,
        /// The update-account file; `None` when the settings name no update account.
        update_account: Option<Written>,
    },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::AlreadySetUp => f.write_str("hostkeeper is installed; nothing to do"),
            Outcome::Loaded => f.write_str(
                "hostkeeper is installed and its job was not loaded: enabled and loaded it",
            ),
            Outcome::Installed {
                version,
                daemon_config,
                signers_file,
                update_account,
            } => {
                write!(
                    f,
                    "installed hostkeeper {version} (hostkeeper.toml {daemon_config:?}, macos-pkg.toml {signers_file:?}, "
                )?;
                match update_account {
                    Some(written) => write!(f, "update-account {written:?})"),
                    None => f.write_str("no update account)"),
                }
            }
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
    /// A `launchctl` command of the check of the daemon's job (`print`, `enable` or `bootstrap`)
    /// that ended with an exit code that is not its success.
    #[error("launchctl {verb} exited {code:?}: {last}")]
    Launchctl {
        verb: &'static str,
        code: Option<i32>,
        last: String,
    },
    #[error("the signers file text is refused: {0}")]
    Signers(#[from] SignersError),
    #[error("the staging directory {path}: {source}")]
    Staging {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the staging directory {path} must be owned by uid {uid} and not writable by others")]
    StagingNotTrusted { path: String, uid: u32 },
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

/// The marker of a finished install, in the staging directory: made after `installer` exited 0.
pub fn installed_marker(paths: &Paths) -> PathBuf {
    paths.staging_dir.join(INSTALLED)
}

/// Whether hostkeeper's files are on this Mac (the daemon's job may still not be loaded, which
/// [`run`] checks): the Mac holds a host certificate, or the binary its pkg
/// installs is there and an install of ours finished (the marker). The pkg lays its payload down
/// before its postinstall runs, so a binary alone can be an install that failed half way; that Mac
/// is installed again, which loads the job.
pub fn is_set_up(paths: &Paths) -> bool {
    let there = |path: &Path| fs::symlink_metadata(path).is_ok();
    there(&paths.pki_current) || (there(&paths.binary) && there(&installed_marker(paths)))
}

/// One run, as the module says.
pub fn run(paths: &Paths, ports: &Ports<'_>) -> Result<Outcome, GaveUp> {
    let mut waited = 0u64;
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let settings = paths.sources.load();
        let bounds = settings
            .as_ref()
            .map_or_else(|_| Bounds::default(), |s| s.bounds);
        let result = if is_set_up(paths) {
            ensure_job_loaded(paths, ports, &bounds)
        } else {
            settings
                .map_err(Error::from)
                .and_then(|settings| install(paths, ports, &settings))
        };
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
    // The program's own name, the last part of its absolute path.
    let name = tool.path().rsplit('/').next().unwrap_or_default();
    ports
        .tools
        .run(tool, args, Duration::from_secs(deadline))
        .map_err(|source| Error::Tool { tool: name, source })
}

/// Whether launchd has the daemon's job loaded: `launchctl print system/<label>` exits 0 for a job
/// it holds, and for one it does not hold exits [`EXIT_NOT_LOADED`] or says [`NOT_LOADED_TEXT`]
/// (the reading `hostkeeper_launchd::launchctl::print` makes). Any other end is an error.
fn job_is_loaded(ports: &Ports<'_>, label: &Label, deadline: u64) -> Result<bool, Error> {
    let printed = run_tool(ports, Tool::Launchctl, &print_args(label), deadline)?;
    if printed.code == Some(0) {
        return Ok(true);
    }
    let says = |bytes: &[u8]| String::from_utf8_lossy(bytes).contains(NOT_LOADED_TEXT);
    if printed.code == Some(EXIT_NOT_LOADED) || says(&printed.stdout) || says(&printed.stderr) {
        return Ok(false);
    }
    Err(Error::Launchctl {
        verb: "print",
        code: printed.code,
        last: last_line(&printed),
    })
}

/// Step 1: a Mac that has hostkeeper has the daemon's job loaded, else it is enabled and loaded.
/// The `enable` comes first because `launchctl bootstrap` does not load a job that is marked
/// disabled.
fn ensure_job_loaded(paths: &Paths, ports: &Ports<'_>, bounds: &Bounds) -> Result<Outcome, Error> {
    let label = Label::daemon();
    let deadline = bounds.tool_timeout_secs;
    if job_is_loaded(ports, &label, deadline)? {
        return Ok(Outcome::AlreadySetUp);
    }
    (ports.log)(&format!("{label} is not loaded; enabling and loading it"));
    let enabled = run_tool(ports, Tool::Launchctl, &enable_args(&label), deadline)?;
    if enabled.code != Some(0) {
        return Err(Error::Launchctl {
            verb: "enable",
            code: enabled.code,
            last: last_line(&enabled),
        });
    }
    let loaded = run_tool(
        ports,
        Tool::Launchctl,
        &bootstrap_args(&paths.daemon_plist),
        deadline,
    )?;
    // A failed bootstrap is not an error when launchd has the job by now.
    if loaded.code != Some(0) && !job_is_loaded(ports, &label, deadline)? {
        return Err(Error::Launchctl {
            verb: "bootstrap",
            code: loaded.code,
            last: last_line(&loaded),
        });
    }
    Ok(Outcome::Loaded)
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
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(staging).map_err(staging_error)?;
        // One test of both: the owner differing, or any write bit of the group or others.
        if (meta.uid() ^ paths.sources.trusted_uid) | (meta.mode() & 0o022) != 0 {
            return Err(Error::StagingNotTrusted {
                path: staging.display().to_string(),
                uid: paths.sources.trusted_uid,
            });
        }
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
    let mut made = Vec::new();
    let written = write_and_install(paths, ports, settings, &signers, pkg_arg, &mut made);
    if written.is_err() {
        // The files this attempt made are not authoritative: the next attempt reads the
        // settings again, so a corrected value is not shadowed by a stale file.
        for path in made {
            drop(fs::remove_file(path));
        }
    }
    let (daemon_config, signers_file, update_account) = written?;
    let marker = installed_marker(paths);
    fs::write(&marker, b"").map_err(|source| Error::Staging {
        path: marker.display().to_string(),
        source,
    })?;
    Ok(Outcome::Installed {
        version: located.version,
        daemon_config,
        signers_file,
        update_account,
    })
}

/// What [`write_and_install`] wrote: the daemon config, the signers file and the update-account
/// file.
type WrittenFiles = (Written, Written, Option<Written>);

/// Writes the files of step 4, each path it made pushed to `made`, then runs `installer`.
fn write_and_install(
    paths: &Paths,
    ports: &Ports<'_>,
    settings: &Settings,
    signers: &str,
    pkg_arg: String,
    made: &mut Vec<PathBuf>,
) -> Result<WrittenFiles, Error> {
    let mut note = |path: PathBuf, written: Written| {
        if written == Written::Made {
            made.push(path);
        }
        written
    };
    let config = paths.config_dir.join(DAEMON_CONFIG);
    let daemon_config = note(
        config.clone(),
        write_if_absent(&config, &settings.daemon_config(), 0o600)?,
    );
    let signers_path = paths.config_dir.join(SIGNERS_FILE);
    let signers_file = note(
        signers_path.clone(),
        write_if_absent(&signers_path, signers, 0o644)?,
    );
    let update_account = match &settings.update_account {
        Some(keys) => Some(note(
            paths.update_account.clone(),
            write_private(
                &paths.update_account,
                &keys.file_text(),
                paths.sources.trusted_uid,
            )?,
        )),
        None => None,
    };
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
    Ok((daemon_config, signers_file, update_account))
}
