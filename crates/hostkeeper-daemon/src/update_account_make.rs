//! Making the update account at a Mac's start (docs/design/maintenance-updates.md, section 5.5,
//! "Making the account"): the hidden local administrator, holding a Secure Token, whose name and
//! password the bootstrap wrote to the update-account file ([`crate::update_account`]).
//!
//! At every start of a Mac's daemon ([`at_host_start`], called by [`crate::cli::start_with`] once
//! the config is loaded), [`Maker::make`] looks, then changes only what is not so, in this order:
//!
//! 1. The update-account file is read ([`UpdateAccount::read`], root's, mode 0600). A Mac without
//!    it has no update account: nothing is run. A file that is refused is the step's error.
//! 2. The account's user record is read (`dscl . -read <users>/<name> RecordName IsHidden
//!    UserShell`). When `dscl` exits 56 (`eDSRecordNotFound`, as the collector reads it) the
//!    account is made: `sysadminctl -addUser <name> -password - -admin`, the account's password
//!    and a newline on its standard input. The record is read again, and must now be there.
//! 3. The account is hidden before anything else is done, so that the Mac holds two visible
//!    administrators for as short a time as it can (the daemon gives operator keys to the one
//!    visible administrator, [`crate::login_account`]): `dscl . -create <users>/<name> IsHidden 1`
//!    when the record is not hidden, then `dscl . -create <users>/<name> UserShell /usr/bin/false`
//!    when its shell is another ([`NO_SHELL`]).
//! 4. When the group `com.apple.access_ssh` exists and the account is one of its members, it is
//!    taken out: `dseditgroup -o edit -d <name> -t user com.apple.access_ssh`.
//! 5. The Secure Token: `sysadminctl -secureTokenStatus <name>` (read as the collector reads it,
//!    [`crate::maintenance_account`]). When it says DISABLED, the administrator's name and password
//!    are read from the bootstrap's settings ([`AdminSource`], [`BootstrapSettings`]) and
//!    `sysadminctl -secureTokenOn <name> -password - -adminUser <admin> -adminPassword -` runs, the
//!    account's password, a newline, the administrator's password and a newline on its standard
//!    input, in the order of the arguments. The status is read again, and must now say ENABLED.
//!    An answer that says neither is the step's error: no token is granted on a guess.
//!
//! So an account that is there, hidden, with no shell, out of the ssh group and holding a token
//! gets no call that changes it; one without a token gets the token call alone. `sysadminctl` may
//! exit 0 without doing what it was asked, which is why each of its calls is followed by a read.
//!
//! **The passwords** go to one place: the standard input of the two `sysadminctl` calls, through
//! the [`StdinRun`] port ([`StdinProcess`] on a host: a pipe, an empty environment; never an
//! argument). The design leaves open whether `sysadminctl` reads `-` from a pipe or only from a
//! terminal **[V]** (the Mac proof, MP-A2 and MP-A3, has not run): this module gives it a pipe, and a
//! runner that gives it a terminal is another [`StdinRun`]. The administrator's password is read
//! from the settings only when the token call is to run, for that call, and dropped when it
//! returns: nothing keeps it, and the next start reads it again. No log line, error or debug print
//! holds a password or what a program wrote: a call's error names the step and the exit code
//! alone, and the reason of a step whose arguments name the account is its exit code alone.
//!
//! What each change did is said as it is made, through the start's `say`; a step that fails is
//! said once, and the start goes on ([`at_start`]): the next start tries again. Linux makes nothing.

use std::fmt;
use std::path::{Path, PathBuf};

use hostkeeper_bootstrap::settings::{SettingsError, Sources};
use hostkeeper_launchd::stdin_process::run_ok as run_with_input;
use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind, StdinProcess, StdinRun};

use crate::login_account::{USERS, dscl_value, exit_failure, group_members, is_hidden, run};
use crate::mac_ssh::{ACCESS_GROUP, DSEDITGROUP, NO_RECORD};
use crate::maintenance_account::{DEADLINE, RECORD_NOT_FOUND, SYSADMINCTL, secure_token};
use crate::remote_login_probe::{ACCESS_SSH_GROUP, DSCL};
use crate::update_account::{Problem, ROOT_UID, UPDATE_ACCOUNT_FILE, UpdateAccount};

/// The login shell the update account has: none that logs in.
pub(crate) const NO_SHELL: &str = "/usr/bin/false";

/// The administrator that gives the update account its Secure Token: a holder of one, made at
/// Setup Assistant. Its `Debug` prints the name alone.
pub(crate) struct Admin {
    name: String,
    password: Vec<u8>,
}

impl Admin {
    /// The administrator `name`, whose password is `password`.
    pub(crate) fn new(name: String, password: Vec<u8>) -> Admin {
        Admin { name, password }
    }
}

impl fmt::Debug for Admin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Admin")
            .field("name", &self.name)
            .field("password", &format_args!("withheld"))
            .finish()
    }
}

/// Where the administrator's name and password are read, each time a token is to be granted.
pub(crate) trait AdminSource: Send + Sync {
    /// The administrator, or `None` when the source names none.
    ///
    /// # Errors
    /// Why a source that is there gives none; the reason holds no value of it.
    fn admin(&self) -> Result<Option<Admin>, String>;
}

/// The administrator of the bootstrap's settings: the keys `admin_name` and `admin_password` of
/// the first of its two sources that is there, read under its trust rule
/// ([`hostkeeper_bootstrap::settings::Sources::load`], docs/design/mac-bootstrap.md, section 4).
/// Neither source there, or one that names no update account, gives none.
pub(crate) struct BootstrapSettings {
    sources: Sources,
}

impl BootstrapSettings {
    /// The administrator of `sources`.
    pub(crate) fn new(sources: Sources) -> BootstrapSettings {
        BootstrapSettings { sources }
    }
}

impl AdminSource for BootstrapSettings {
    fn admin(&self) -> Result<Option<Admin>, String> {
        match self.sources.load() {
            Ok(settings) => Ok(settings.update_account.map(|keys| {
                Admin::new(
                    keys.admin.name,
                    keys.admin.password.expose().as_bytes().to_vec(),
                )
            })),
            Err(SettingsError::NoSource { .. }) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// One change [`Maker::make`] made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// The account was made, an administrator.
    Created,
    /// Its record was hidden.
    Hidden,
    /// Its login shell was set to [`NO_SHELL`].
    NoShell,
    /// It was taken out of the ssh access group.
    LeftSsh,
    /// It was given a Secure Token.
    TokenGranted,
}

impl Change {
    /// The line said when the change is made; it names no account.
    pub(crate) fn line(self) -> String {
        let what = match self {
            Change::Created => {
                "the update account was made, an administrator (sysadminctl -addUser)".to_owned()
            }
            Change::Hidden => "the update account was hidden (IsHidden 1)".to_owned(),
            Change::NoShell => format!("the update account's login shell was set to {NO_SHELL}"),
            Change::LeftSsh => format!("the update account was taken out of {ACCESS_GROUP}"),
            Change::TokenGranted => {
                "the update account was given a Secure Token (sysadminctl -secureTokenOn)"
                    .to_owned()
            }
        };
        format!("level=info update_account: {what}")
    }
}

/// The steps of the module's doc, over the update-account file, the administrator's source and a
/// port for each program.
pub(crate) struct Maker {
    file: PathBuf,
    owner: u32,
    admin: Box<dyn AdminSource>,
    dscl: Box<dyn Launchctl>,
    dseditgroup: Box<dyn Launchctl>,
    sysadminctl: Box<dyn Launchctl>,
    sysadminctl_input: Box<dyn StdinRun>,
}

/// The programs [`Maker`] runs: `dscl`, `dseditgroup`, `sysadminctl` for its status, and
/// `sysadminctl` with the passwords on its standard input.
pub(crate) struct Programs {
    pub(crate) dscl: Box<dyn Launchctl>,
    pub(crate) dseditgroup: Box<dyn Launchctl>,
    pub(crate) sysadminctl: Box<dyn Launchctl>,
    pub(crate) sysadminctl_input: Box<dyn StdinRun>,
}

impl Programs {
    /// The host's own programs, each bounded by [`DEADLINE`].
    pub(crate) fn host() -> Programs {
        let tool = |program: &str| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), DEADLINE))
        };
        Programs {
            dscl: tool(DSCL),
            dseditgroup: tool(DSEDITGROUP),
            sysadminctl: tool(SYSADMINCTL),
            sysadminctl_input: Box::new(StdinProcess::new(Path::new(SYSADMINCTL), DEADLINE)),
        }
    }
}

impl Maker {
    /// The steps over the update-account file at `file`, which must be owned by `owner`, the
    /// administrator of `admin` and `programs`.
    pub(crate) fn new(
        file: PathBuf,
        owner: u32,
        admin: Box<dyn AdminSource>,
        programs: Programs,
    ) -> Maker {
        Maker {
            file,
            owner,
            admin,
            dscl: programs.dscl,
            dseditgroup: programs.dseditgroup,
            sysadminctl: programs.sysadminctl,
            sysadminctl_input: programs.sysadminctl_input,
        }
    }

    /// The steps over the host's own file ([`UPDATE_ACCOUNT_FILE`], owned by [`ROOT_UID`]), the
    /// bootstrap's own settings ([`Sources::host`]) and the host's programs.
    pub(crate) fn at_host() -> Maker {
        Maker::new(
            PathBuf::from(UPDATE_ACCOUNT_FILE),
            ROOT_UID,
            Box::new(BootstrapSettings::new(Sources::host())),
            Programs::host(),
        )
    }

    /// The steps of the module's doc, each change said through `say` as it is made; the changes
    /// made, none when the Mac has no update-account file or the account is as it should be.
    ///
    /// # Errors
    /// The first step that failed, in words that hold no password and no account name.
    pub(crate) fn make(&self, say: &dyn Fn(&str)) -> Result<Vec<Change>, String> {
        let account = match UpdateAccount::read(&self.file, self.owner) {
            Ok(account) => account,
            Err(error) if error.problem == Problem::Missing => return Ok(Vec::new()),
            Err(error) => return Err(error.to_string()),
        };
        let mut changes = Vec::new();
        let mut made = |change: Change| {
            say(&change.line());
            changes.push(change);
        };
        let name = account.name();
        let user = format!("{USERS}/{name}");
        let record = match self.record(&user)? {
            Some(record) => record,
            None => {
                self.create(&account)?;
                made(Change::Created);
                self.record(&user)?.ok_or_else(|| {
                    "sysadminctl -addUser exited 0, but dscl does not find the update account"
                        .to_owned()
                })?
            }
        };
        if !is_hidden(&record) {
            self.write(
                &user,
                "IsHidden",
                "1",
                "dscl write of the update account's IsHidden",
            )?;
            made(Change::Hidden);
        }
        if dscl_value(&record, "UserShell").as_deref() != Some(NO_SHELL) {
            self.write(
                &user,
                "UserShell",
                NO_SHELL,
                "dscl write of the update account's UserShell",
            )?;
            made(Change::NoShell);
        }
        if self.in_ssh_group(name)? {
            let what = "dseditgroup removal from the ssh access group";
            let args = ["-o", "edit", "-d", name, "-t", "user", ACCESS_GROUP];
            let output = run(&*self.dseditgroup, what, &args)?;
            if output.code != Some(0) {
                return Err(exit_failure(what, &output, false));
            }
            made(Change::LeftSsh);
        }
        match secure_token(&*self.sysadminctl, name)? {
            (true, _) => {}
            (false, 0) => {
                self.grant(&account)?;
                if secure_token(&*self.sysadminctl, name)? != (true, 0) {
                    return Err(
                        "sysadminctl -secureTokenOn exited 0, but the update account holds no \
                         Secure Token"
                            .to_owned(),
                    );
                }
                made(Change::TokenGranted);
            }
            (false, _) => {
                return Err(
                    "sysadminctl -secureTokenStatus said neither ENABLED nor DISABLED; no Secure \
                     Token is granted"
                        .to_owned(),
                );
            }
        }
        Ok(changes)
    }

    /// The account's user record as `dscl` wrote it, or `None` when it is not there.
    fn record(&self, user: &str) -> Result<Option<String>, String> {
        let what = "dscl read of the update account";
        let args = [".", "-read", user, "RecordName", "IsHidden", "UserShell"];
        let output = run(&*self.dscl, what, &args)?;
        match output.code {
            Some(0) => Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned())),
            Some(RECORD_NOT_FOUND) => Ok(None),
            _ => Err(exit_failure(what, &output, false)),
        }
    }

    /// `key` of the account's record set to `value`.
    fn write(&self, user: &str, key: &str, value: &str, what: &str) -> Result<(), String> {
        let output = run(&*self.dscl, what, &[".", "-create", user, key, value])?;
        match output.code {
            Some(0) => Ok(()),
            _ => Err(exit_failure(what, &output, false)),
        }
    }

    /// Whether the ssh access group exists and holds `name`.
    fn in_ssh_group(&self, name: &str) -> Result<bool, String> {
        let what = "dscl read of the ssh access group";
        let args = [".", "-read", ACCESS_SSH_GROUP, "GroupMembership"];
        let output = run(&*self.dscl, what, &args)?;
        if output.code == Some(0) {
            let members = group_members(&String::from_utf8_lossy(&output.stdout));
            return Ok(members.iter().any(|member| member == name));
        }
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            output.stderr_text()
        );
        if NO_RECORD.iter().any(|mark| said.contains(mark)) {
            return Ok(false);
        }
        Err(exit_failure(what, &output, true))
    }

    /// `sysadminctl -addUser`, the account's password and a newline on its standard input.
    fn create(&self, account: &UpdateAccount) -> Result<(), String> {
        let args = ["-addUser", account.name(), "-password", "-", "-admin"].map(str::to_owned);
        let mut input = account.password().to_vec();
        input.push(b'\n');
        run_with_input(
            &*self.sysadminctl_input,
            "sysadminctl -addUser",
            &args,
            &input,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    /// `sysadminctl -secureTokenOn` by the administrator of the source, read now, both passwords
    /// on its standard input; the administrator is dropped when the call returns.
    fn grant(&self, account: &UpdateAccount) -> Result<(), String> {
        let admin = self
            .admin
            .admin()
            .map_err(|why| format!("the administrator's keys could not be read: {why}"))?
            .ok_or_else(|| {
                "the update account holds no Secure Token, and the bootstrap's settings name no \
                 administrator to give it one"
                    .to_owned()
            })?;
        let args = [
            "-secureTokenOn",
            account.name(),
            "-password",
            "-",
            "-adminUser",
            admin.name.as_str(),
            "-adminPassword",
            "-",
        ]
        .map(str::to_owned);
        let mut input = account.password().to_vec();
        input.push(b'\n');
        input.extend_from_slice(&admin.password);
        input.push(b'\n');
        run_with_input(
            &*self.sysadminctl_input,
            "sysadminctl -secureTokenOn",
            &args,
            &input,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    }
}

/// The start's step: on a Mac, [`Maker::make`] by `maker`, saying through `say` each change and,
/// once, a step that failed; on any other host, nothing. The start goes on either way.
pub(crate) fn at_start(
    kind: PlatformKind,
    maker: &Maker,
    say: &dyn Fn(&str),
) -> Result<Vec<Change>, String> {
    if kind != PlatformKind::Macos {
        return Ok(Vec::new());
    }
    let made = maker.make(say);
    if let Err(error) = &made {
        say(&format!(
            "level=error update_account: {error}; the start goes on, and the next start tries again"
        ));
    }
    made
}

/// [`at_start`] over the host's own file, settings and programs ([`Maker::at_host`]), saying
/// through the daemon's standard error.
pub(crate) fn at_host_start(kind: PlatformKind) {
    let maker = Maker::at_host();
    let _made = at_start(kind, &maker, &to_stderr);
}

/// Says `line` on the daemon's standard error, as the start's other steps do.
pub(crate) fn to_stderr(line: &str) {
    eprintln!("hostkeeper daemon: {line}");
}

#[cfg(test)]
#[path = "update_account_make_fake.rs"]
mod fake;

#[cfg(test)]
#[path = "update_account_make_tests.rs"]
mod tests;
