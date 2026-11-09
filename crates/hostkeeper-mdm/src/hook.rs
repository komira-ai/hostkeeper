//! The `mdm` package's update hook: `mdm update-hook <step> [<staged release>] --params <file>`
//! (docs/design/two-programs.md section 6.2). The contract is the one of the `hostkeeper-hook`
//! crate; this is what each step does for the MDM stack.
//!
//! The daemon has unpacked the archive package and links its units (`units/`, from the version
//! `current` names) into `/etc/systemd/system` immediately before `cutover` and back before
//! `rollback`; from the `.deb`, dpkg installs the binaries in `/usr/lib/mdm` and the units in
//! `/usr/lib/systemd/system/` at the daemon's `activate`, before `cutover`, and the daemon runs
//! the hook as the tree's `usr/lib/mdm/hostkeeper-hook`, which runs `mdm update-hook` beside it.
//! The program's path says which of the two builds runs ([`package::Layout::of_program`]). The
//! hook does what `hostkeeper-mdm install` did besides placing binaries and units, which are now
//! the package's:
//!
//! - **`prepare <staged release>`** checks the host as `install` does before anything changes
//!   (root; an x86-64 machine; no link at the state, settings or pkg directory, but not at the
//!   binary directory, which runs through the daemon's link `current` to the version that
//!   runs, so the staged release's programs are checked by their staged paths instead; an
//!   interface carrying `MDM_BIND`; the loopback ports and 443 free or held by the package's
//!   own active unit; the SCEP depot empty or holding a whole CA), the staged release (its
//!   `scepserver` and `lego` are files, in its `bin/` for the archive and its
//!   `tree/usr/lib/mdm/` for the `.deb`, and that lego takes every flag the certificate
//!   unit passes to `lego run` and the renewal unit to `lego renew`), and `/etc/mdm/dns.env`
//!   when it is there (a 0600 file of root's holding every variable the DNS provider reads;
//!   without it the certificate jobs wait). Then it makes, each
//!   only when the host differs: the user `mdm`; `/var/lib/mdm` and its subdirectories (0700,
//!   the user's), `/etc/mdm` (0750, root and the user's group) and the pkg directory (0755,
//!   root's); the three secrets, each made once and never shown; the SCEP CA, with the staged
//!   release's `scepserver`, unless the depot holds one; the Caddyfile and the drop-ins of the
//!   settings ([`hostkeeper_mdm_core::package::drop_ins`]). What each of those last files held
//!   before is kept in the record first, so `rollback` can put it back. Nothing is started or
//!   restarted: the services that run keep running what they ran.
//! - **`cutover`** reloads systemd, enables the proxy and the certificate timer, restarts the
//!   run units and the timer (they now run the new version's binaries), restarts the proxy
//!   when the TLS certificate is there, restarts depsyncer if it runs, and starts the first
//!   certificate job when there is no certificate yet and the DNS credentials are there.
//! - **`verify`** runs the checks of `hostkeeper-mdm verify` that an update can break: the run
//!   units active and each listening on its loopback port only (and, with the certificate
//!   there, the proxy and the timer active and the proxy on `MDM_BIND:443` only); nothing on
//!   port 80; the state, settings and pkg directories and the secrets with their modes and
//!   owners; the Caddyfile and the drop-ins as the settings render them. A service that is not
//!   up yet defers the step (exit 75, `not_ready`), so the daemon asks again; any other check
//!   that fails fails it (`check_failed`). The certificate's remaining validity, the DNS
//!   credentials' file and the enrollment profile are printed, not judged: no update changes
//!   them.
//! - **`rollback`** puts back every file `prepare` changed, and after a `cutover` reloads
//!   systemd and restarts what runs on the version the daemon has linked back; on a first
//!   install (no previous release) it stops the units and removes the links of the units it
//!   enabled instead.
//! - **`cleanup`**, refused until `verify` succeeded, forgets the kept files.
//! - **`uninstall`** stops every unit of the package, removes the links of the units the hook
//!   or `mdm dep define-profile` enabled, moves the Caddyfile and the drop-ins to
//!   `/var/lib/mdm/removed/<unix seconds>/` and reloads systemd. The state, the secrets, the
//!   pkg directory and the user stay, as with `install --uninstall`.
//!
//! Every step refuses a params file the contract's reader refuses (`Params::validate`), or
//! whose `settings` is not the manifest's ([`hostkeeper_mdm_core::package::SETTINGS_FILE`]);
//! `prepare` refuses a release argument that is not byte for byte `updater.staged_release`.
//! Every step takes the record directory's lock first and acts on the record. Run again,
//! `prepare`, `rollback` and `cleanup` change nothing more; `verify` checks again; `cutover`
//! (which needs only `prepare` recorded) and `uninstall` issue their systemctl commands again,
//! which restarts the units once more and leaves them as the first run did. The last line on standard output is the report; the plan and the
//! commands go to standard error. Exit 0 success, 1 failure, 75 deferred, 2 a command line the
//! hook cannot read.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use hostkeeper_hook::{
    Class, EXIT_DEFERRED, EXIT_FAILURE, Invocation, Params, Report, STAGED_RELEASE_KEY, Scalar,
};
use hostkeeper_mdm_core::package;

use crate::{MdmError, System};

mod record;
mod steps;

/// The exit status of a command line the hook cannot read.
pub const EXIT_USAGE: u8 = 2;

/// How long `verify` asks the daemon to wait before it runs again, when a service is not up.
pub const RETRY_AFTER_SECS: u64 = 5;

/// The params key naming the settings file.
pub const SETTINGS_KEY: &str = "settings";

/// The params key naming the record directory.
pub const RECORD_DIR_KEY: &str = "record_dir";

/// How a step ended other than in success: its exit code and its report.
#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) code: u8,
    pub(crate) report: Report,
}

impl Failure {
    fn new(code: u8, class: Class, reason: &str, detail: String) -> Failure {
        Failure {
            code,
            report: Report {
                class,
                reason: Some(reason.to_owned()),
                detail: Some(detail),
                ..Report::default()
            },
        }
    }

    /// A failure the host or its settings cause: no release fixes it.
    pub(crate) fn environment(reason: &str, detail: String) -> Failure {
        Failure::new(fail_code(), Class::Environment, reason, detail)
    }

    /// A failure of the release.
    pub(crate) fn release(reason: &str, detail: String) -> Failure {
        Failure::new(fail_code(), Class::Release, reason, detail)
    }

    /// Not yet: ask again after [`RETRY_AFTER_SECS`].
    pub(crate) fn deferred(reason: &str, detail: String) -> Failure {
        let mut deferred = Failure::new(defer_code(), Class::Release, reason, detail);
        deferred.report.retry_after_secs = Some(RETRY_AFTER_SECS);
        deferred
    }

    /// This failure, stating that nothing it did needs undoing beyond a rollback.
    pub(crate) fn reversible(mut self) -> Failure {
        self.report = self.report.with_reversible(true);
        self
    }
}

/// The exit code of a failure, as the contract numbers it.
fn fail_code() -> u8 {
    u8::try_from(EXIT_FAILURE).unwrap_or(1)
}

/// The exit code of a deferral, as the contract numbers it.
fn defer_code() -> u8 {
    u8::try_from(EXIT_DEFERRED).unwrap_or(75)
}

impl From<MdmError> for Failure {
    /// A refusal of the kit's checks or a step that failed, as the hook reports it: the host's
    /// (class `environment`), with a reason token for the checks an operator acts on.
    fn from(e: MdmError) -> Failure {
        let reason = match &e {
            MdmError::Config(_) | MdmError::Package(_) | MdmError::NoFile(_) => "bad_settings",
            MdmError::NeedsRoot { .. } => "not_root",
            MdmError::Architecture { .. } => "wrong_machine",
            MdmError::NotCarried(_) => "bind_not_carried",
            MdmError::PortHeld(_) => "port_held",
            MdmError::DepotPartial { .. } | MdmError::DepotDamaged { .. } => "depot_damaged",
            MdmError::LegoHelp { .. } | MdmError::LegoFlag { .. } => "bad_lego",
            e if e.exit_code() == 2 => "refused",
            _ => "step_failed",
        };
        let class = match reason {
            "bad_lego" => Class::Release,
            _ => Class::Environment,
        };
        Failure::new(fail_code(), class, reason, e.to_string())
    }
}

/// Runs the step `args` names (the words after `update-hook`) for the package's build `build`
/// (the layout of its settings and of a staged release). The report line is written to `out`
/// last; the plan, the commands and the detail go to `err`. The exit code.
pub fn run(
    args: &[OsString],
    build: package::Layout,
    system: &System<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let (code, report) = match execute(args, build, system, err) {
        Ok(report) => (0, report),
        Err(failure) => (failure.code, failure.report),
    };
    if let Some(detail) = &report.detail {
        let _ = writeln!(err, "mdm update-hook: {detail}");
    }
    let _ = report.write_to(out);
    code
}

/// What the manifest's params say: where the settings and the record are.
pub(crate) struct Where {
    /// The settings file.
    pub(crate) settings: String,
    /// The record directory.
    pub(crate) record_dir: PathBuf,
}

/// An absolute path the params give under `key`.
fn path_param(params: &Params, key: &str) -> Result<String, Failure> {
    match params.get(key) {
        Some(Scalar::Text(path)) if path.starts_with('/') => Ok(path.clone()),
        Some(_) => Err(Failure::environment(
            "params",
            format!("the params key {key} is not an absolute path"),
        )),
        None => Err(Failure::environment(
            "params",
            format!("the params file has no key {key}"),
        )),
    }
}

fn execute(
    args: &[OsString],
    build: package::Layout,
    system: &System<'_>,
    err: &mut dyn Write,
) -> Result<Report, Failure> {
    let call = Invocation::parse(args.iter().cloned()).map_err(|why| Failure {
        code: EXIT_USAGE,
        report: Report {
            class: Class::Environment,
            reason: Some("usage".to_owned()),
            detail: Some(why.to_string()),
            ..Report::default()
        },
    })?;
    let file = call.params.clone().ok_or_else(|| {
        Failure::environment("params", "the step needs --params <file>".to_owned())
    })?;
    let params = read_params(&file)?;
    params
        .validate()
        .map_err(|why| Failure::environment("params", format!("the params file: {why}")))?;
    let at = Where {
        settings: path_param(&params, SETTINGS_KEY)?,
        record_dir: PathBuf::from(path_param(&params, RECORD_DIR_KEY)?),
    };
    if at.settings != package::SETTINGS_FILE {
        return Err(Failure::environment(
            "params",
            format!(
                "the params key {SETTINGS_KEY} is {:?}, not the package's {:?}",
                at.settings,
                package::SETTINGS_FILE
            ),
        ));
    }
    // The argument of `prepare` is byte for byte the params' staged release (the contract).
    let staged = match call.artifact.as_ref() {
        None => None,
        Some(artifact) => Some(staged_release(&params, artifact)?),
    };
    let record = record::Record::open(&at.record_dir)?;
    let mut steps = steps::Steps {
        system,
        build,
        record: &record,
        params: &params,
        settings: &at.settings,
        log: err,
    };
    steps.run(call.step, staged)
}

/// The staged release `prepare` was given, once it is the params' `updater.staged_release`.
fn staged_release(params: &Params, artifact: &Path) -> Result<String, Failure> {
    match params.staged_release() {
        Some(staged) if staged.as_os_str() == artifact.as_os_str() => {
            Ok(staged.to_string_lossy().into_owned())
        }
        _ => Err(Failure::environment(
            "params",
            format!(
                "prepare was given {}, which is not the params' {STAGED_RELEASE_KEY}",
                artifact.display()
            ),
        )),
    }
}

fn read_params(file: &Path) -> Result<Params, Failure> {
    Params::read(file).map_err(|why| {
        Failure::environment(
            "params",
            format!("the params file {}: {why}", file.display()),
        )
    })
}
