//! What a Mac needs besides the file before an operator can log in with the keys of
//! [`crate::operator_keys`]: Remote Login on, and the login account allowed to use ssh. Run only on
//! a Mac, and only when the set has keys (docs/design/ssh-through-coordinator.md, section 3, has
//! the measurements). Each step looks first and changes only what is not so; every change is
//! logged.
//!
//! - Remote Login: `launchctl print system/com.openssh.sshd` exits 0 when sshd's job is loaded,
//!   and nothing is done. Otherwise `launchctl enable system/com.openssh.sshd` and `launchctl
//!   bootstrap system /System/Library/LaunchDaemons/ssh.plist` run, each of which must exit 0, and
//!   `launchctl print` must then exit 0. This is the way that needs no Full Disk Access;
//!   `systemsetup` is never run, and nothing is disabled or booted out.
//! - The access group: when the group `com.apple.access_ssh` does not exist, every account may use
//!   ssh and nothing is done. When it does, and the login account is not among its members,
//!   `dseditgroup -o edit -a <account> -t user com.apple.access_ssh` adds that one account; no
//!   other account and no other group is touched.
//!
//! An error names the step and its exit code, never an account: the `launchctl` steps and the read
//! of the access group name none, so their reason also holds the first line of the tool's standard
//! error; the `dseditgroup` step names the account in its arguments, so its reason is the step and
//! the exit code alone.

use hostkeeper_launchd::{Launchctl, LaunchctlOutput};
use hostkeeper_link::Log;

use crate::login_account::{Account, exit_failure, group_members, run, run_ok_named};
use crate::remote_login_probe::{ACCESS_SSH_GROUP, SSHD_PLIST, SSHD_SERVICE};

/// The tool that edits a directory service group.
pub const DSEDITGROUP: &str = "/usr/sbin/dseditgroup";

/// The group whose members, when it exists, are the accounts that may log in by ssh.
pub const ACCESS_GROUP: &str = "com.apple.access_ssh";

/// What `dscl` says for a group record that does not exist: the error's name, and its number.
const NO_RECORD: [&str; 2] = ["eDSRecordNotFound", "-14136"];

/// Why a Mac could not be made ready for ssh.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum MacSshError {
    /// Remote Login is not on and could not be turned on.
    #[error("Remote Login is not on: {0}")]
    RemoteLogin(String),
    /// The login account could not be allowed to use ssh.
    #[error("the login account is not allowed to use ssh: {0}")]
    AccessGroup(String),
}

/// The Mac's steps, over a port for each program: `launchctl`, `dscl` and `dseditgroup`.
pub struct MacSsh {
    launchctl: Box<dyn Launchctl>,
    dscl: Box<dyn Launchctl>,
    dseditgroup: Box<dyn Launchctl>,
}

impl MacSsh {
    /// The steps over the three ports.
    #[must_use]
    pub fn new(
        launchctl: Box<dyn Launchctl>,
        dscl: Box<dyn Launchctl>,
        dseditgroup: Box<dyn Launchctl>,
    ) -> MacSsh {
        MacSsh {
            launchctl,
            dscl,
            dseditgroup,
        }
    }

    /// Remote Login on, then `account` allowed to use ssh.
    ///
    /// # Errors
    /// [`MacSshError`] for the first step that fails.
    pub fn ensure(&self, account: &Account, log: &dyn Log) -> Result<(), MacSshError> {
        self.ensure_remote_login(log)?;
        self.ensure_access(account, log)
    }

    /// Remote Login on, as the module says.
    ///
    /// # Errors
    /// [`MacSshError::RemoteLogin`], naming the step and its exit code.
    pub fn ensure_remote_login(&self, log: &dyn Log) -> Result<(), MacSshError> {
        let launchctl = |what: &str, args: &[&str]| -> Result<LaunchctlOutput, MacSshError> {
            run(&*self.launchctl, what, args).map_err(MacSshError::RemoteLogin)
        };
        let print = || launchctl("launchctl print of sshd", &["print", SSHD_SERVICE]);
        if print()?.code == Some(0) {
            return Ok(());
        }
        let steps = [
            ("launchctl enable of sshd", vec!["enable", SSHD_SERVICE]),
            (
                "launchctl bootstrap of sshd",
                vec!["bootstrap", "system", SSHD_PLIST],
            ),
        ];
        for (what, args) in &steps {
            let output = launchctl(what, args)?;
            if output.code != Some(0) {
                return Err(MacSshError::RemoteLogin(exit_failure(what, &output, true)));
            }
        }
        let output = print()?;
        if output.code != Some(0) {
            return Err(MacSshError::RemoteLogin(exit_failure(
                "launchctl print of sshd after its enable and bootstrap",
                &output,
                true,
            )));
        }
        log.line(&format!(
            "operator keys: Remote Login turned on (launchctl enable and bootstrap of {SSHD_SERVICE})"
        ));
        Ok(())
    }

    /// `account` among the members of the access group, as the module says.
    ///
    /// # Errors
    /// [`MacSshError::AccessGroup`], naming the step and its exit code.
    pub fn ensure_access(&self, account: &Account, log: &dyn Log) -> Result<(), MacSshError> {
        let what = "dscl read of the ssh access group";
        let output = run(
            &*self.dscl,
            what,
            &[".", "-read", ACCESS_SSH_GROUP, "GroupMembership"],
        )
        .map_err(MacSshError::AccessGroup)?;
        if output.code != Some(0) {
            let said = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                output.stderr_text()
            );
            if NO_RECORD.iter().any(|mark| said.contains(mark)) {
                return Ok(());
            }
            return Err(MacSshError::AccessGroup(exit_failure(what, &output, true)));
        }
        let members = group_members(&String::from_utf8_lossy(&output.stdout));
        if members.contains(&account.name) {
            return Ok(());
        }
        run_ok_named(
            &*self.dseditgroup,
            "dseditgroup add to the ssh access group",
            &[
                "-o",
                "edit",
                "-a",
                &account.name,
                "-t",
                "user",
                ACCESS_GROUP,
            ],
        )
        .map_err(MacSshError::AccessGroup)?;
        log.line(&format!(
            "operator keys: the login account added to {ACCESS_GROUP}"
        ));
        Ok(())
    }
}

#[cfg(test)]
#[path = "mac_ssh_tests.rs"]
mod tests;
