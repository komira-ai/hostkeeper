//! The apply and the check of the `macos` kind on a Mac (docs/design/maintenance-updates.md,
//! sections 5.2 step 3, 5.5 "The daemon installs it" and 5.6), on the order's path `install`
//! alone: the path `wait` (device management installs the update) is not built, and every step of
//! it, as of an order that names no path, fails `macos_path_not_built` before it reads or runs
//! anything.
//!
//! **`cutover`** ([`Apply::cutover`]) installs nothing; it makes sure the install can run, chooses
//! it and keeps the choice ([`super::plan`]), and answers [`Cutover::NeedsReboot`]. In order:
//!
//! 1. The update account's file ([`UPDATE_ACCOUNT_FILE`], root's, mode 0600), read and checked
//!    before any Apple program runs: a file that is wrong fails `update_account_refused`.
//! 2. FileVault (`fdesetup status`) must be off: with it on, the restart of `--restart` stops at the
//!    pre-boot login, and the authenticated restart of section 5.5 is not built, so anything else
//!    fails `filevault_not_off`.
//! 3. What the Mac runs (`sw_vers`) and what Apple offers it now (`softwareupdate --list`, and
//!    `--list-full-installers` only when the order's `macos_major` allows it), chosen as the stage
//!    chose ([`super::stage`]): with an upgrade allowed and offered, the newest full installer of
//!    a higher major, whose app the stage fetched into `/Applications` (exactly one
//!    `Install macOS *.app` with its `startosinstall`, or `macos_installer_not_found`), and the
//!    build its listing gives; otherwise the newest macOS update of the running major, by its
//!    label, and the build its label ends with (`<title>-<build>`). Nothing offered fails
//!    `macos_update_not_listed`.
//! 4. The plan written, durably, in the run's directory (`macos_plan_failed` when it cannot be).
//!
//! **The restart** ([`Apply::install`], the kind's [`super::super::MaintenanceKind::restart`]),
//! which the hook runs only after it recorded the reboot under way, and again in place of a
//! reboot asked in the boot the Mac still runs: the update account read again (refused as
//! above), the plan read, then the install with the account's password and a newline on the
//! program's standard input alone ([`UpdateAccount::run`]): a minor update by `softwareupdate
//! --install <label> --agree-to-license --restart --user <account> --stdinpass`, a major upgrade by
//! the installer app's `Contents/Resources/startosinstall --agreetolicense --nointeraction --user
//! <account> --stdinpass`; each restarts the Mac itself. The program's environment is empty and
//! nothing else is on its standard input ([`HostInstallers`]). A program that does not run, is
//! killed at [`INSTALL_DEADLINE`] or exits non-zero fails `macos_install_failed`, whose detail,
//! like the one log line of the call, names the step and the exit code alone.
//!
//! **`after_reboot`** answers the build `sw_vers` reports. **`verify`** ([`Apply::verify`]) reads
//! the plan, fails `macos_build_not_target` unless `sw_vers` reports its build, and only then
//! removes a major upgrade's installer app (one already gone is removed;
//! `macos_installer_not_removed` when it cannot be).
//!
//! **Failure.** Every failure of these steps is [`KindError::Failed`]: the Mac is drained, and the
//! run halts there (section 5.4). No detail holds the password: the account's file is named by
//! its path and its problem alone, a program by its step and exit code, and the plan holds none.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::{StdinProcess, StdinRun};
use hostkeeper_link::Log;
use hostkeeper_proto::hostkeeper::agent::v1::{FileVaultState, MacosPath};

use super::plan::{APP_SUFFIX, Install, Plan, PlanFile, installer_name};
use super::stage::{Programs, newest_upgrade};
use crate::maintenance_collect::{APPLICATIONS, HostRoot, compare_versions, listing};
use crate::maintenance_hook::kind::{Cutover, KindError};
use crate::maintenance_hook::params::MaintainParams;
use crate::maintenance_mac::{FDESETUP, parse_fdesetup_status};
use crate::maintenance_macos::parse_sw_vers;
use crate::maintenance_softwareupdate::{
    ListedUpdate, MACOS_TITLE, major_of, parse_softwareupdate_list,
};
use crate::update_account::{ROOT_UID, UPDATE_ACCOUNT_FILE, UpdateAccount};
use crate::xcode_clt::{LIST_DEADLINE, LOOK_DEADLINE, SOFTWAREUPDATE, SW_VERS};

/// The reason of an order whose macOS path is not `install`.
pub const REASON_PATH_NOT_BUILT: &str = "macos_path_not_built";
/// The reason of an update-account file that is missing or wrong.
pub const REASON_ACCOUNT_REFUSED: &str = "update_account_refused";
/// The reason of a Mac whose FileVault is not off.
pub const REASON_FILEVAULT_NOT_OFF: &str = "filevault_not_off";
/// The reason of a look (`fdesetup status`, `sw_vers`, a listing) that failed.
pub const REASON_LOOK_FAILED: &str = "macos_look_failed";
/// The reason of a Mac offered no macOS update and no upgrade the order allows.
pub const REASON_NOT_LISTED: &str = "macos_update_not_listed";
/// The reason of an update whose target build cannot be told.
pub const REASON_TARGET_UNKNOWN: &str = "macos_target_unknown";
/// The reason of a major upgrade with not exactly one installer app to run.
pub const REASON_INSTALLER_NOT_FOUND: &str = "macos_installer_not_found";
/// The reason of an install plan that could not be written or read.
pub const REASON_PLAN_FAILED: &str = "macos_plan_failed";
/// The reason of an install that did not run or did not succeed.
pub const REASON_INSTALL_FAILED: &str = "macos_install_failed";
/// The reason of a Mac that does not run the target build.
pub const REASON_NOT_TARGET: &str = "macos_build_not_target";
/// The reason of an installer app that could not be removed.
pub const REASON_INSTALLER_NOT_REMOVED: &str = "macos_installer_not_removed";

/// How long an install may run before it restarts the Mac: it prepares the update the stage
/// downloaded (a full installer is some 16 GB). One stopped at its deadline fails.
pub const INSTALL_DEADLINE: Duration = Duration::from_secs(3 * 60 * 60);

/// The step of a minor update, as its error and log line name it.
pub const STEP_UPDATE: &str = "softwareupdate --install";
/// The step of a major upgrade, as its error and log line name it.
pub const STEP_UPGRADE: &str = "startosinstall";

/// Where `startosinstall` is in an installer app.
const STARTOSINSTALL: &str = "Contents/Resources/startosinstall";

/// Where the install programs run.
pub trait Installers: Send + Sync {
    /// The runner of `program`, its standard input a pipe, bounded by `deadline`.
    fn runner(&self, program: &Path, deadline: Duration) -> Box<dyn StdinRun>;
}

/// The Mac's install programs: each with an empty environment, its standard input the bytes it is
/// given and nothing else, killed at its deadline ([`StdinProcess`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct HostInstallers;

impl Installers for HostInstallers {
    fn runner(&self, program: &Path, deadline: Duration) -> Box<dyn StdinRun> {
        Box::new(StdinProcess::new(program, deadline))
    }
}

/// The hook's log: its standard error, which the daemon logs under the step's unit
/// ([`crate::hook_log`]).
struct StderrLog;

impl Log for StderrLog {
    fn line(&self, text: &str) {
        use std::io::Write;
        let _unlogged = writeln!(std::io::stderr().lock(), "{text}");
    }
}

/// The update-account file and the owner it must have.
pub struct AccountFile {
    path: PathBuf,
    owner: u32,
}

impl AccountFile {
    /// The file at `path`, which must be owned by `owner`.
    pub fn new(path: &Path, owner: u32) -> AccountFile {
        AccountFile {
            path: path.to_owned(),
            owner,
        }
    }

    /// The Mac's: [`UPDATE_ACCOUNT_FILE`], root's.
    pub fn host() -> AccountFile {
        AccountFile::new(Path::new(UPDATE_ACCOUNT_FILE), ROOT_UID)
    }

    /// The account, or its refusal.
    fn read(&self) -> Result<UpdateAccount, KindError> {
        UpdateAccount::read(&self.path, self.owner)
            .map_err(|error| failed(REASON_ACCOUNT_REFUSED, error.to_string()))
    }
}

/// The failure of `reason` with `detail`.
fn failed(reason: &'static str, detail: String) -> KindError {
    KindError::Failed { reason, detail }
}

/// The failure of a look.
fn look_failed(detail: String) -> KindError {
    failed(REASON_LOOK_FAILED, detail)
}

/// The failure of an order whose path is not `install`, when it is not.
fn install_path(params: &MaintainParams) -> Result<(), KindError> {
    let path = match params.maintain().macos_path() {
        MacosPath::Install => return Ok(()),
        MacosPath::Wait => "wait",
        MacosPath::Unspecified => "unspecified",
    };
    Err(failed(
        REASON_PATH_NOT_BUILT,
        format!(
            "the order's macOS path is {path}, and this hostkeeper installs a macOS update only on \
             the path install"
        ),
    ))
}

/// The newest macOS update of the major version `running` among `updates`; of equal versions,
/// the first.
fn newest_minor(running: u64, updates: &[ListedUpdate]) -> Option<&ListedUpdate> {
    updates
        .iter()
        .filter(|update| {
            update.title.starts_with(MACOS_TITLE) && major_of(&update.version) == Some(running)
        })
        .reduce(
            |held, next| match compare_versions(&next.version, &held.version) {
                std::cmp::Ordering::Greater => next,
                _ => held,
            },
        )
}

/// The build a label of `softwareupdate --list` ends with: what follows its last `-`, letters and
/// digits.
fn label_build(label: &str) -> Option<&str> {
    label
        .rsplit_once('-')
        .map(|(_, build)| build)
        .filter(|build| !build.is_empty() && build.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// The apply and check of the `macos` kind.
pub struct Apply {
    programs: Box<dyn Programs>,
    installers: Box<dyn Installers>,
    account: AccountFile,
    plans: PlanFile,
    root: HostRoot,
    log: Box<dyn Log>,
}

impl Apply {
    /// The apply running the Mac's looks through `programs` and its installs through
    /// `installers`, with the update account of `account`, keeping its plan in `run_dir`, finding
    /// installer apps under `root` and logging to `log`.
    pub fn new(
        programs: Box<dyn Programs>,
        installers: Box<dyn Installers>,
        account: AccountFile,
        run_dir: Option<&Path>,
        root: HostRoot,
        log: Box<dyn Log>,
    ) -> Apply {
        Apply {
            programs,
            installers,
            account,
            plans: PlanFile::new(run_dir),
            root,
            log,
        }
    }

    /// The apply of this Mac, keeping its plan in `run_dir`.
    pub fn at_host(run_dir: Option<&Path>) -> Apply {
        Apply::new(
            Box::new(super::stage::HostPrograms),
            Box::new(HostInstallers),
            AccountFile::host(),
            run_dir,
            HostRoot::host(),
            Box::new(StderrLog),
        )
    }

    /// Checks, chooses and keeps the install of the run `params`, as the module says.
    ///
    /// # Errors
    /// The failure of the first check that does not pass.
    pub fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        install_path(params)?;
        self.account.read()?;
        self.filevault_off()?;
        let (version, _) = self.running()?;
        let running = major_of(&version).ok_or_else(|| {
            look_failed(format!(
                "sw_vers named the version \"{version}\", which has no major number"
            ))
        })?;
        let plan = self.choose(running, params.maintain().macos_major)?;
        self.plans
            .save(params.run_id(), &plan)
            .map_err(|why| failed(REASON_PLAN_FAILED, why))?;
        Ok(Cutover::NeedsReboot)
    }

    /// Installs the plan of the run `params` with the update account, as the module says; the
    /// program restarts the Mac.
    ///
    /// # Errors
    /// The refusal of the account, the plan's failure, or the install's, which names the step and
    /// the exit code alone.
    pub fn install(&self, params: &MaintainParams) -> Result<(), KindError> {
        install_path(params)?;
        let account = self.account.read()?;
        let plan = self.plan(params)?;
        let user = account.name().to_owned();
        let (program, step, args) = match &plan.install {
            Install::Update { label } => (
                PathBuf::from(SOFTWAREUPDATE),
                STEP_UPDATE,
                vec![
                    "--install",
                    label.as_str(),
                    "--agree-to-license",
                    "--restart",
                    "--user",
                    user.as_str(),
                    "--stdinpass",
                ],
            ),
            Install::Upgrade { app } => (
                self.root.path(&format!("{app}/{STARTOSINSTALL}")),
                STEP_UPGRADE,
                vec![
                    "--agreetolicense",
                    "--nointeraction",
                    "--user",
                    user.as_str(),
                    "--stdinpass",
                ],
            ),
        };
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let runner = self.installers.runner(&program, INSTALL_DEADLINE);
        account
            .run(&*runner, step, &args, &*self.log)
            .map(|_| ())
            .map_err(|error| failed(REASON_INSTALL_FAILED, error.to_string()))
    }

    /// The build the Mac runs, after the reboot of its install.
    ///
    /// # Errors
    /// `macos_look_failed` when `sw_vers` fails or names no build.
    pub fn after_reboot(&self, _params: &MaintainParams) -> Result<String, KindError> {
        let (_, build) = self.running()?;
        Ok(build)
    }

    /// Checks that the Mac runs the plan's build, and then removes a major upgrade's installer
    /// app, as the module says.
    ///
    /// # Errors
    /// The failure of the first check that does not pass, or of the removal.
    pub fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        install_path(params)?;
        let plan = self.plan(params)?;
        let (_, build) = self.running()?;
        if build != plan.build {
            return Err(failed(
                REASON_NOT_TARGET,
                format!(
                    "sw_vers reports the build {build}, not the target build {}",
                    plan.build
                ),
            ));
        }
        match &plan.install {
            Install::Update { .. } => Ok(()),
            Install::Upgrade { app } => self.remove(app),
        }
    }

    /// The plan of the run `params`.
    fn plan(&self, params: &MaintainParams) -> Result<Plan, KindError> {
        self.plans
            .load(params.run_id())
            .map_err(|why| failed(REASON_PLAN_FAILED, why))
    }

    /// The failure of a Mac whose FileVault is not off.
    fn filevault_off(&self) -> Result<(), KindError> {
        let tool = self.programs.tool(Path::new(FDESETUP), LOOK_DEADLINE);
        let (_, text) =
            listing(&*tool, "fdesetup status", &["status"], &[0]).map_err(look_failed)?;
        let state = match parse_fdesetup_status(&text).0 {
            FileVaultState::Off => return Ok(()),
            FileVaultState::On => "on",
            FileVaultState::Encrypting => "encrypting",
            FileVaultState::Decrypting => "decrypting",
            FileVaultState::Unspecified => "in no state fdesetup names",
        };
        Err(failed(
            REASON_FILEVAULT_NOT_OFF,
            format!(
                "FileVault is {state} (fdesetup status): its authenticated restart is not built, \
                 so the update is not installed"
            ),
        ))
    }

    /// The version and the build `sw_vers` reports.
    fn running(&self) -> Result<(String, String), KindError> {
        let tool = self.programs.tool(Path::new(SW_VERS), LOOK_DEADLINE);
        let (_, text) = listing(&*tool, "sw_vers", &[], &[0]).map_err(look_failed)?;
        let (version, build) = parse_sw_vers(&text);
        if build.is_empty() {
            return Err(look_failed("sw_vers named no build".to_owned()));
        }
        Ok((version, build))
    }

    /// What `softwareupdate` lists with `args`.
    fn offered(
        &self,
        args: &[&str],
    ) -> Result<crate::maintenance_softwareupdate::SoftwareUpdateListing, KindError> {
        let tool = self.programs.tool(Path::new(SOFTWAREUPDATE), LIST_DEADLINE);
        let what = format!("softwareupdate {}", args.join(" "));
        let (_, text) = listing(&*tool, &what, args, &[0]).map_err(look_failed)?;
        Ok(parse_softwareupdate_list(&text))
    }

    /// The plan of a Mac of the major version `running`, a major upgrade only when
    /// `major_allowed`, as the module says.
    fn choose(&self, running: u64, major_allowed: bool) -> Result<Plan, KindError> {
        let listed = self.offered(&["--list"])?;
        if major_allowed {
            let full = self.offered(&["--list-full-installers"])?;
            if let Some(installer) = newest_upgrade(running, &full.installers) {
                return Ok(Plan {
                    build: installer.build.clone(),
                    install: Install::Upgrade {
                        app: self.installer_app()?,
                    },
                });
            }
        }
        let update = newest_minor(running, &listed.updates).ok_or_else(|| {
            failed(
                REASON_NOT_LISTED,
                format!("softwareupdate offers no update of macOS {running} the order allows"),
            )
        })?;
        let build = label_build(&update.label).ok_or_else(|| {
            failed(
                REASON_TARGET_UNKNOWN,
                format!(
                    "the label \"{}\" names no build after its last '-'",
                    update.label
                ),
            )
        })?;
        Ok(Plan {
            build: build.to_owned(),
            install: Install::Update {
                label: update.label.clone(),
            },
        })
    }

    /// The one installer app in [`APPLICATIONS`] with its `startosinstall`, by its path on the Mac.
    fn installer_app(&self) -> Result<String, KindError> {
        let not_found = |detail: String| failed(REASON_INSTALLER_NOT_FOUND, detail);
        let mut apps: Vec<String> = match self.root.exists(APPLICATIONS) {
            true => self.root.dirs_in(APPLICATIONS).map_err(not_found)?,
            false => Vec::new(),
        };
        apps.retain(|name| installer_name(name));
        apps.sort();
        let [name] = apps.as_slice() else {
            return Err(not_found(format!(
                "{APPLICATIONS} holds {} apps named Install macOS *{APP_SUFFIX} ({}), not one",
                apps.len(),
                apps.join(", ")
            )));
        };
        let app = format!("{APPLICATIONS}/{name}");
        let program = format!("{app}/{STARTOSINSTALL}");
        match self.root.is_file(&program) {
            true => Ok(app),
            false => Err(not_found(format!("{program} is not a file"))),
        }
    }

    /// Removes the installer app `app`; one already gone is removed.
    fn remove(&self, app: &str) -> Result<(), KindError> {
        match std::fs::remove_dir_all(self.root.path(app)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failed(
                REASON_INSTALLER_NOT_REMOVED,
                format!("{app} could not be removed: {error}"),
            )),
        }
    }
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
