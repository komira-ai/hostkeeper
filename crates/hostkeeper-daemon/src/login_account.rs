//! Which account takes the operator keys (`operator-keys-v1`, [`crate::operator_keys`]). No account
//! name or password is configured or stored anywhere: the daemon finds the account by a rule of
//! its platform, on every call.
//!
//! - Linux, under systemd and under OpenRC: `root`, the line of the passwd file whose name is
//!   `root` and whose uid is 0 ([`LinuxRoot`]): its group and home.
//! - macOS: the one local administrator account ([`MacAdmin`], over `dscl`): a user with a uid of
//!   501 or more whose name does not start with `_`, a member of the group `admin`, and not
//!   hidden. With none, or with more than one, the host has no account the rule picks.
//!
//! A source says why it gives none in one of two kinds that its caller tells apart:
//! [`AccountError::Refused`], the host has no account the rule picks, so trying again changes
//! nothing until the host does, and [`AccountError::Failed`], a tool or a file could not be read,
//! so the next set tries again. The reasons go to the coordinator and hold no account name: a step
//! never puts its arguments in its reason, and a step whose arguments name an account (the `dscl`
//! read of a user record, `dseditgroup`) gives its exit code alone, never what the tool wrote to
//! its standard error, which may name that account. The reasons of the steps that name none keep
//! the first line of the standard error.

use std::fs;
use std::path::PathBuf;

use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput};

/// The passwd file of a Linux host.
pub const PASSWD: &str = "/etc/passwd";

/// The lowest uid of a Mac's own user accounts.
pub const FIRST_USER_UID: u32 = 501;

/// The directory service's node of the local user records.
pub(crate) const USERS: &str = "/Users";

/// The account the operator keys go to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    /// The account's home directory, whose `.ssh` holds the file.
    pub home: PathBuf,
}

/// Why a source gives no account.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    /// The host has no account the rule picks.
    #[error("{0}")]
    Refused(String),
    /// A tool or a file could not be read.
    #[error("{0}")]
    Failed(String),
}

/// Where the login account is found.
pub trait AccountSource: Send + Sync {
    /// The account the keys go to, found now.
    ///
    /// # Errors
    /// [`AccountError`], as its values say.
    fn login_account(&self) -> Result<Account, AccountError>;
}

/// The account of a Linux host: `root`, read from a passwd file.
pub struct LinuxRoot {
    passwd: PathBuf,
}

impl LinuxRoot {
    /// The account of the passwd file `passwd`.
    #[must_use]
    pub fn new(passwd: PathBuf) -> LinuxRoot {
        LinuxRoot { passwd }
    }

    /// The account of the host's own passwd file ([`PASSWD`]).
    #[must_use]
    pub fn host() -> LinuxRoot {
        LinuxRoot::new(PathBuf::from(PASSWD))
    }
}

impl AccountSource for LinuxRoot {
    fn login_account(&self) -> Result<Account, AccountError> {
        let text = fs::read_to_string(&self.passwd).map_err(|error| {
            AccountError::Failed(format!(
                "cannot read the passwd file {}: {error}",
                self.passwd.display()
            ))
        })?;
        let line = text
            .lines()
            .find(|line| line.split(':').next() == Some("root"))
            .ok_or_else(|| {
                AccountError::Refused("the passwd file has no root account".to_owned())
            })?;
        root_of(line).ok_or_else(|| {
            AccountError::Refused(
                "the root line of the passwd file is not name:password:uid:gid:gecos:home:shell \
                 with uid 0 and an absolute home"
                    .to_owned(),
            )
        })
    }
}

/// The account of the passwd line `line` of `root`: its uid must be 0, its home absolute.
fn root_of(line: &str) -> Option<Account> {
    let fields: Vec<&str> = line.split(':').collect();
    let [name, _, uid, gid, _, home, ..] = fields.as_slice() else {
        return None;
    };
    let uid: u32 = uid.parse().ok()?;
    let gid: u32 = gid.parse().ok()?;
    (uid == 0 && home.starts_with('/')).then(|| Account {
        name: (*name).to_owned(),
        uid,
        gid,
        home: PathBuf::from(home),
    })
}

/// What a program's failure says, as a reason that names the step and never the arguments (they
/// may hold an account name): `what`, then why it did not run or did not exit 0.
fn run_failure(what: &str, error: &LaunchctlError) -> String {
    match error {
        LaunchctlError::Spawn { why, .. } => format!("{what} could not be run: {why}"),
        LaunchctlError::TimedOut { .. } => format!("{what} did not finish within its deadline"),
    }
}

/// What a program that did not exit 0 said: `what`, its exit code and, when `with_stderr`, the
/// first line of its standard error; no arguments. `with_stderr` is false for a step whose
/// arguments name an account, since what the tool writes may name it too.
pub(crate) fn exit_failure(what: &str, output: &LaunchctlOutput, with_stderr: bool) -> String {
    let Some(code) = output.code else {
        return format!("{what} was ended by a signal");
    };
    let stderr = output.stderr_text();
    let line = stderr
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| with_stderr && !line.is_empty());
    match line {
        Some(line) => format!("{what} failed with exit code {code}: {line}"),
        None => format!("{what} failed with exit code {code}"),
    }
}

/// Runs `tool` with `args`; how it ended, or why it did not run ([`run_failure`]).
pub(crate) fn run(
    tool: &dyn Launchctl,
    what: &str,
    args: &[&str],
) -> Result<LaunchctlOutput, String> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    tool.run(&args).map_err(|error| run_failure(what, &error))
}

/// [`run`], and the program's standard output when it exited 0, else why not ([`exit_failure`],
/// with the first line of its standard error): for a step whose arguments name no account.
pub(crate) fn run_ok(tool: &dyn Launchctl, what: &str, args: &[&str]) -> Result<String, String> {
    run_checked(tool, what, args, true)
}

/// [`run_ok`] for a step whose arguments name an account: its reason is the step and the exit
/// code alone.
pub(crate) fn run_ok_named(
    tool: &dyn Launchctl,
    what: &str,
    args: &[&str],
) -> Result<String, String> {
    run_checked(tool, what, args, false)
}

fn run_checked(
    tool: &dyn Launchctl,
    what: &str,
    args: &[&str],
    with_stderr: bool,
) -> Result<String, String> {
    let output = run(tool, what, args)?;
    if output.code == Some(0) {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(exit_failure(what, &output, with_stderr))
    }
}

/// The value of `key` in what `dscl -read` wrote: the rest of its `Key:` line and the lines after
/// it that start with white space, joined by a space; `None` when there is no such line (`dscl`
/// writes `No such key: Key` for an absent key).
pub(crate) fn dscl_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let mut lines = text.lines();
    let first = lines.find_map(|line| line.strip_prefix(prefix.as_str()))?;
    let mut value = first.trim().to_owned();
    for line in lines.take_while(|line| line.starts_with([' ', '\t'])) {
        if !value.is_empty() {
            value.push(' ');
        }
        value.push_str(line.trim());
    }
    Some(value)
}

/// The members of a group record `dscl -read` wrote: the words of its `GroupMembership`.
pub(crate) fn group_members(text: &str) -> Vec<String> {
    dscl_value(text, "GroupMembership")
        .map(|value| value.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The account of a Mac: its one local administrator, found through `dscl`.
pub struct MacAdmin {
    dscl: Box<dyn Launchctl>,
}

impl MacAdmin {
    /// The account found through `dscl`, the port of `/usr/bin/dscl` ([`crate::remote_login_probe::DSCL`]).
    #[must_use]
    pub fn new(dscl: Box<dyn Launchctl>) -> MacAdmin {
        MacAdmin { dscl }
    }

    /// The users, with their uids, that may be the account: a uid of [`FIRST_USER_UID`] or more, a name that does
    /// not start with `_`, and a member of `admin`.
    fn candidates(&self) -> Result<Vec<(String, u32)>, AccountError> {
        let list = run_ok(
            &*self.dscl,
            "dscl list of users",
            &[".", "-list", USERS, "UniqueID"],
        )
        .map_err(AccountError::Failed)?;
        let admins = run_ok(
            &*self.dscl,
            "dscl read of the admin group",
            &[".", "-read", "/Groups/admin", "GroupMembership"],
        )
        .map_err(AccountError::Failed)?;
        let admins = group_members(&admins);
        Ok(list
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let (name, uid) = (words.next()?, words.next()?);
                let uid: u32 = uid.parse().ok()?;
                (uid >= FIRST_USER_UID
                    && !name.starts_with('_')
                    && admins.iter().any(|a| a == name))
                .then(|| (name.to_owned(), uid))
            })
            .collect())
    }

    /// The account of the candidate `name`, or `None` when it is hidden.
    fn account(&self, name: &str, uid: u32) -> Result<Option<Account>, AccountError> {
        let user = format!("{USERS}/{name}");
        let record = run_ok_named(
            &*self.dscl,
            "dscl read of a user record",
            &[
                ".",
                "-read",
                &user,
                "NFSHomeDirectory",
                "PrimaryGroupID",
                "IsHidden",
            ],
        )
        .map_err(AccountError::Failed)?;
        let hidden = dscl_value(&record, "IsHidden").is_some_and(|value| {
            matches!(value.to_ascii_lowercase().as_str(), "1" | "yes" | "true")
        });
        if hidden {
            return Ok(None);
        }
        let home = dscl_value(&record, "NFSHomeDirectory").filter(|home| home.starts_with('/'));
        let gid = dscl_value(&record, "PrimaryGroupID").and_then(|gid| gid.parse::<u32>().ok());
        match (home, gid) {
            (Some(home), Some(gid)) => Ok(Some(Account {
                name: name.to_owned(),
                uid,
                gid,
                home: PathBuf::from(home),
            })),
            _ => Err(AccountError::Failed(
                "dscl did not give the home directory and the primary group of an \
                 administrator account"
                    .to_owned(),
            )),
        }
    }
}

impl AccountSource for MacAdmin {
    fn login_account(&self) -> Result<Account, AccountError> {
        let mut found = Vec::new();
        for (name, uid) in self.candidates()? {
            if let Some(account) = self.account(&name, uid)? {
                found.push(account);
            }
        }
        match found.len() {
            0 => Err(AccountError::Refused(
                "no local administrator account: uid 501 or more, in admin, not hidden".to_owned(),
            )),
            1 => Ok(found.remove(0)),
            n => Err(AccountError::Refused(format!(
                "{n} local administrator accounts: the managed block takes exactly one"
            ))),
        }
    }
}

#[cfg(test)]
#[path = "login_account_tests.rs"]
mod tests;
