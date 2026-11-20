//! The collector `update-account` of a Mac's maintenance look ([`crate::maintenance_collect`]): the
//! state of the update account (docs/design/maintenance-updates.md, section 5.5): whether the
//! account is there, an administrator, holds a Secure Token and is hidden.
//!
//! The account's name comes from the root-only update-account file ([`crate::update_account`]),
//! through the port [`AccountName`]; a Mac without the file has no update account, and the state
//! is all false with no program run. The rest is read with `dscl` (the user record, whose exit
//! 56, `eDSRecordNotFound`, is read as the account being absent **[V]**, and the group `admin`)
//! and with `sysadminctl -secureTokenStatus`, which is reported to write its answer to standard
//! error **[V]**: both streams are read. An answer with neither the enabled nor the disabled line
//! is counted as an unparsed line and reads as no token, never guessed. Nested groups are not
//! followed, as in [`crate::login_account`].
//!
//! **The password.** This collector learns the name alone: the reader's one call gives the
//! account, the name is copied out and the account, with its password, is dropped at once. The
//! password is never read by this collector beyond that call and is never in an argument, a
//! report, a log line or an error. The reason of a step whose arguments name the account (the
//! user record, the Secure Token status) is the step and the exit code alone; the admin group's
//! read, whose arguments name none, adds the first line of its standard error; a file that cannot
//! be read is named by the reader's reason, which holds its path and the problem and no byte of
//! it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use crate::login_account::{USERS, exit_failure, group_members, is_hidden, run, run_ok};
use crate::maintenance_collect::{Collector, Look, Section};
use crate::remote_login_probe::DSCL;
use crate::update_account::{Problem, ROOT_UID, UPDATE_ACCOUNT_FILE, UpdateAccount};

/// The program of the Secure Token status.
pub const SYSADMINCTL: &str = "/usr/sbin/sysadminctl";

/// How long each of the two programs is given.
pub const DEADLINE: Duration = Duration::from_secs(60);

/// What `dscl` exits with for a record that is not there (`eDSRecordNotFound`).
const RECORD_NOT_FOUND: i32 = 56;

/// What `sysadminctl -secureTokenStatus` writes for an account that holds a token.
const TOKEN_ENABLED: &str = "Secure token is ENABLED for user";

/// What it writes for an account that holds none.
const TOKEN_DISABLED: &str = "Secure token is DISABLED for user";

/// Where the update account's name is found.
pub trait AccountName: Send + Sync {
    /// The update account's name, or `None` when the Mac has no update-account file.
    ///
    /// # Errors
    /// Why a file that is there gives no name; the reason never holds a byte of the file.
    fn name(&self) -> Result<Option<String>, String>;
}

/// The name of the account in the update-account file.
pub struct AccountFile {
    path: PathBuf,
    owner: u32,
}

impl AccountFile {
    /// The account in the file at `path`, which must be owned by `owner`.
    #[must_use]
    pub fn new(path: PathBuf, owner: u32) -> AccountFile {
        AccountFile { path, owner }
    }

    /// The account in the host's own file ([`UPDATE_ACCOUNT_FILE`], owned by [`ROOT_UID`]).
    #[must_use]
    pub fn at_host() -> AccountFile {
        AccountFile::new(Path::new(UPDATE_ACCOUNT_FILE).to_owned(), ROOT_UID)
    }
}

impl AccountName for AccountFile {
    fn name(&self) -> Result<Option<String>, String> {
        match UpdateAccount::read(&self.path, self.owner) {
            Ok(account) => Ok(Some(account.name().to_owned())),
            Err(error) if error.problem == Problem::Missing => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// The collector `update-account`: the state of the update account.
pub struct UpdateAccountFacts {
    name: Box<dyn AccountName>,
    dscl: Box<dyn Launchctl>,
    sysadminctl: Box<dyn Launchctl>,
}

impl UpdateAccountFacts {
    /// The collector over the source of the account's name, `dscl` and `sysadminctl`.
    #[must_use]
    pub fn new(
        name: Box<dyn AccountName>,
        dscl: Box<dyn Launchctl>,
        sysadminctl: Box<dyn Launchctl>,
    ) -> UpdateAccountFacts {
        UpdateAccountFacts {
            name,
            dscl,
            sysadminctl,
        }
    }

    /// The collector over the host's own file and programs, each program bounded by
    /// [`DEADLINE`].
    #[must_use]
    pub fn at_host() -> UpdateAccountFacts {
        let tool = |program: &str| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), DEADLINE))
        };
        UpdateAccountFacts::new(
            Box::new(AccountFile::at_host()),
            tool(DSCL),
            tool(SYSADMINCTL),
        )
    }

    /// Whether `name` holds a Secure Token, and how many lines of the answer were none that is
    /// known.
    fn secure_token(&self, name: &str) -> Result<(bool, u32), String> {
        let what = "sysadminctl -secureTokenStatus";
        let output = run(&*self.sysadminctl, what, &["-secureTokenStatus", name])?;
        if output.code != Some(0) {
            return Err(exit_failure(what, &output, false));
        }
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            output.stderr_text()
        );
        let token = text.lines().find_map(|line| {
            if line.contains(TOKEN_ENABLED) {
                Some(true)
            } else if line.contains(TOKEN_DISABLED) {
                Some(false)
            } else {
                None
            }
        });
        Ok(match token {
            Some(token) => (token, 0),
            None => (false, 1),
        })
    }
}

/// The section that carries `state` and `unparsed`.
fn section(state: pb::UpdateAccountState, unparsed: u32) -> Section {
    Section {
        mac: Some(pb::MacUpdateFacts {
            update_account: Some(state),
            ..Default::default()
        }),
        unparsed,
        ..Default::default()
    }
}

impl Collector for UpdateAccountFacts {
    fn name(&self) -> &'static str {
        "update-account"
    }

    fn collect(&self, _look: &Look<'_>) -> Result<Section, String> {
        let Some(name) = self.name.name()? else {
            return Ok(section(pb::UpdateAccountState::default(), 0));
        };
        let what = "dscl read of the update account";
        let user = format!("{USERS}/{name}");
        let record = run(
            &*self.dscl,
            what,
            &[".", "-read", &user, "RecordName", "IsHidden"],
        )?;
        let hidden = match record.code {
            Some(0) => is_hidden(&String::from_utf8_lossy(&record.stdout)),
            Some(RECORD_NOT_FOUND) => return Ok(section(pb::UpdateAccountState::default(), 0)),
            _ => return Err(exit_failure(what, &record, false)),
        };
        let admins = run_ok(
            &*self.dscl,
            "dscl read of the admin group",
            &[".", "-read", "/Groups/admin", "GroupMembership"],
        )?;
        let admin = group_members(&admins).contains(&name);
        let (secure_token, unparsed) = self.secure_token(&name)?;
        let state = pb::UpdateAccountState {
            present: true,
            admin,
            secure_token,
            hidden,
        };
        Ok(section(state, unparsed))
    }
}

#[cfg(test)]
#[path = "maintenance_account_tests.rs"]
mod tests;
