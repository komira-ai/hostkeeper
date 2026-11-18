//! The account that owns a service's identity files (`identityAccount` of its manifest,
//! docs/design/service-identity.md, section 4): [`ServiceAccounts`] resolves its name to the user
//! and group numbers the files are given.
//!
//! - Linux, under systemd and under OpenRC: the line of the passwd file whose name it is
//!   ([`PasswdAccounts`]): its uid and gid.
//! - macOS: the directory service, `dscl . -read /Users/<name> UniqueID PrimaryGroupID`, through
//!   the same program port as the operator keys' account ([`DsclAccounts`]).
//!
//! An account the host does not have, a name that is not an account name, and a file or a tool
//! that cannot be read each give a reason, which is the service's reported error: nothing is
//! written for it.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use hostkeeper_enroll::Owner;
use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};

use crate::login_account::{PASSWD, dscl_value, run_ok_named};
use crate::remote_login_probe::{DSCL, STEP_DEADLINE};

/// The account of a service whose manifest names none.
pub const DEFAULT_ACCOUNT: &str = "root";

/// The directory service's node of the local user records.
const USERS: &str = "/Users";

/// The longest account name that is looked up.
const MAX_ACCOUNT_NAME: usize = 255;

/// Where a service's account is found.
pub trait ServiceAccounts: Send + Sync {
    /// The user and group of the account `name`, found now.
    ///
    /// # Errors
    /// Why the host has none, as the report of the service says it.
    fn lookup(&self, name: &str) -> Result<Owner, String>;
}

/// A name that is looked up: 1 to 255 bytes of A-Z, a-z, 0-9, `.`, `_` and `-`, not beginning with
/// `-`.
fn check_name(name: &str) -> Result<(), String> {
    let chars = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'-'));
    if (1..=MAX_ACCOUNT_NAME).contains(&name.len()) & chars & !name.starts_with('-') {
        Ok(())
    } else {
        Err(format!("{name:?} is not an account name"))
    }
}

/// The accounts of a Linux host: the lines of a passwd file.
pub struct PasswdAccounts {
    passwd: PathBuf,
}

impl PasswdAccounts {
    /// The accounts of the passwd file `passwd`.
    #[must_use]
    pub fn new(passwd: PathBuf) -> PasswdAccounts {
        PasswdAccounts { passwd }
    }
}

impl ServiceAccounts for PasswdAccounts {
    fn lookup(&self, name: &str) -> Result<Owner, String> {
        check_name(name)?;
        let text = fs::read_to_string(&self.passwd)
            .map_err(|e| format!("cannot read the passwd file {}: {e}", self.passwd.display()))?;
        let line = text
            .lines()
            .find(|line| line.split(':').next() == Some(name))
            .ok_or_else(|| format!("the host has no account named {name:?}"))?;
        let fields: Vec<&str> = line.split(':').collect();
        let ids = match fields.as_slice() {
            [_, _, uid, gid, ..] => uid.parse().ok().zip(gid.parse().ok()),
            _ => None,
        };
        ids.map(|(uid, gid)| Owner { uid, gid }).ok_or_else(|| {
            format!("the passwd line of {name:?} is not name:password:uid:gid:gecos:home:shell")
        })
    }
}

/// The accounts of a Mac: the directory service, read through `dscl`.
pub struct DsclAccounts {
    dscl: Box<dyn Launchctl>,
}

impl DsclAccounts {
    /// The accounts found through `dscl`, the port of `/usr/bin/dscl`.
    #[must_use]
    pub fn new(dscl: Box<dyn Launchctl>) -> DsclAccounts {
        DsclAccounts { dscl }
    }
}

impl ServiceAccounts for DsclAccounts {
    fn lookup(&self, name: &str) -> Result<Owner, String> {
        check_name(name)?;
        let user = format!("{USERS}/{name}");
        let record = run_ok_named(
            &*self.dscl,
            "dscl read of a service account",
            &[".", "-read", &user, "UniqueID", "PrimaryGroupID"],
        )
        .map_err(|why| format!("the account {name:?} cannot be read: {why}"))?;
        let number = |key: &str| dscl_value(&record, key).and_then(|v| v.parse::<u32>().ok());
        number("UniqueID")
            .zip(number("PrimaryGroupID"))
            .map(|(uid, gid)| Owner { uid, gid })
            .ok_or_else(|| {
                format!("dscl did not give a user id and a primary group for the account {name:?}")
            })
    }
}

/// The accounts of a host of `kind`.
#[must_use]
pub fn at_host(kind: PlatformKind) -> Arc<dyn ServiceAccounts> {
    match kind {
        PlatformKind::Linux | PlatformKind::Openrc => {
            Arc::new(PasswdAccounts::new(PathBuf::from(PASSWD)))
        }
        PlatformKind::Macos => Arc::new(DsclAccounts::new(Box::new(LaunchctlProcess::new(
            std::path::Path::new(DSCL),
            STEP_DEADLINE,
        )))),
    }
}

#[cfg(test)]
#[path = "service_account_tests.rs"]
mod tests;
