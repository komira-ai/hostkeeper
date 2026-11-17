//! The host's operator keys (`operator-keys-v1`): the [`OperatorKeysSink`] of the daemon's heartbeat
//! loop. The coordinator sends the keys of the host's group whole, and this puts them in the managed
//! block of the login account's `authorized_keys` ([`crate::authorized_keys`]); the account is found
//! on every set, never configured ([`crate::login_account`]); on a Mac, Remote Login is made sure of
//! first, and the account's right to use ssh ([`crate::mac_ssh`]), for a set that has keys.
//!
//! **A set** ([`OperatorKeys::apply`]) is judged in this order, and ends one of three ways:
//!
//! 1. its digest must be the digest of its keys, and the keys must keep the rules of a set
//!    (`hostkeeper_wire::operator_keys`); else it is *refused*;
//! 2. the account is found: a host with no account the rule picks *refuses* the set, a tool or
//!    file that could not be read *fails* it;
//! 3. the file is written, and on a Mac the two steps are made sure of: any error *fails* it.
//!
//! *Done*: the set is in force; the report holds its digest. *Refused*: what is in force stays,
//! and the report says which set was refused and why, until another set is sent: retrying cannot
//! help. *Failed*: the report holds no digest and says why, so the coordinator sends the set again
//! in the next heartbeat. Each ending is a line of the daemon's log, none with a key or an account
//! name.
//! A reason of a failed set (the file's, a Mac step's) has the account's home written `~` and its
//! name written `the login account` before it is clipped, logged and reported, so no report holds
//! either; a refusal, and a failure of finding the account, are made before the account is known
//! and are worded by [`crate::login_account`] without one.
//!
//! **The report** ([`OperatorKeys::report`]) holds the digest of the set in force only while the
//! file still holds it: the file is read at every heartbeat, so a block edited or taken away by
//! hand, or a file whose owner or mode changed, reports no digest and the coordinator sends the
//! set again, which puts it back. A daemon that was started and sent no set reports none.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};
use hostkeeper_link::{
    KeysRefusal, KeysReport, Log, MAX_KEYS_REASON, OperatorKeysSink, PushedKeys,
};
use hostkeeper_wire::operator_keys as rules;

use crate::authorized_keys::{self, Put};
use crate::login_account::{Account, AccountError, AccountSource, LinuxRoot, MacAdmin};
use crate::mac_ssh::{DSEDITGROUP, MacSsh};
use crate::remote_login_probe::{DSCL, STEP_DEADLINE};

/// How a set did not end done.
enum Ended {
    Refused(String),
    Failed(String),
}

/// What the daemon keeps between two sets.
#[derive(Default)]
struct State {
    /// The set in force: the account it went to and its digest.
    held: Option<(Account, String)>,
    refused: Option<KeysRefusal>,
    failed: String,
}

/// The daemon's operator keys.
pub struct OperatorKeys {
    accounts: Box<dyn AccountSource>,
    mac: Option<MacSsh>,
    log: Arc<dyn Log>,
    state: Mutex<State>,
}

impl OperatorKeys {
    /// The sink that finds its account through `accounts`, makes sure of Remote Login and ssh
    /// access through `mac` when there is one (a Mac), and says what it did in `log`.
    #[must_use]
    pub fn new(
        accounts: Box<dyn AccountSource>,
        mac: Option<MacSsh>,
        log: Arc<dyn Log>,
    ) -> OperatorKeys {
        OperatorKeys {
            accounts,
            mac,
            log,
            state: Mutex::new(State::default()),
        }
    }

    /// The sink of a host of `kind`: on Linux, under systemd or OpenRC, `root` of the host's passwd
    /// file; on a Mac, its one local administrator, with the Mac's steps, over the host's programs
    /// each bounded by [`STEP_DEADLINE`].
    #[must_use]
    pub fn at_host(kind: PlatformKind, log: Arc<dyn Log>) -> OperatorKeys {
        match kind {
            PlatformKind::Linux | PlatformKind::Openrc => {
                OperatorKeys::new(Box::new(LinuxRoot::host()), None, log)
            }
            PlatformKind::Macos => {
                let tool = |program: &str| -> Box<dyn Launchctl> {
                    Box::new(LaunchctlProcess::new(Path::new(program), STEP_DEADLINE))
                };
                OperatorKeys::new(
                    Box::new(MacAdmin::new(tool(DSCL))),
                    Some(MacSsh::new(
                        tool(hostkeeper_launchd::LAUNCHCTL),
                        tool(DSCL),
                        tool(DSEDITGROUP),
                    )),
                    log,
                )
            }
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Puts `pushed` in force, as the module's steps say: the account it went to, and whether the
    /// file was written or held the block already.
    fn put(&self, pushed: &PushedKeys) -> Result<(Account, Put), Ended> {
        if pushed.digest != rules::digest(&pushed.keys) {
            return Err(Ended::Refused(
                "the coordinator's digest is not the digest of its keys".to_owned(),
            ));
        }
        rules::check_set(&pushed.keys).map_err(|error| Ended::Refused(error.to_string()))?;
        let account = self.accounts.login_account().map_err(|error| match error {
            AccountError::Refused(reason) => Ended::Refused(reason),
            AccountError::Failed(reason) => Ended::Failed(reason),
        })?;
        let put = authorized_keys::put(&account, &pushed.keys)
            .map_err(|error| Ended::Failed(redact(&error.to_string(), &account)))?;
        if let (Some(mac), false) = (&self.mac, pushed.keys.is_empty()) {
            mac.ensure(&account, &*self.log)
                .map_err(|error| Ended::Failed(redact(&error.to_string(), &account)))?;
        }
        Ok((account, put))
    }
}

/// `reason` with the account's home written `~`: the reason goes to the coordinator, and a Mac's
/// home names its account, which no report carries.
fn home_as_tilde(reason: &str, account: &Account) -> String {
    let home = account.home.to_string_lossy();
    match home.as_ref() {
        "" | "/" => reason.to_owned(),
        home => reason.replace(home, "~"),
    }
}

/// `reason` with the account's home written `~` and then its name written `the login account`
/// (an empty name is left, which would match everywhere): what a failed set reports.
fn redact(reason: &str, account: &Account) -> String {
    let reason = home_as_tilde(reason, account);
    if account.name.is_empty() {
        reason
    } else {
        reason.replace(&account.name, "the login account")
    }
}

/// `reason` cut to [`MAX_KEYS_REASON`] bytes at a character boundary.
fn clip(reason: &str) -> String {
    let mut end = reason.len().min(MAX_KEYS_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

impl OperatorKeysSink for OperatorKeys {
    fn report(&self) -> KeysReport {
        let state = self.state();
        let digest = state
            .held
            .as_ref()
            .filter(|(account, digest)| authorized_keys::held(account, digest))
            .map(|(_, digest)| digest.clone())
            .unwrap_or_default();
        KeysReport {
            digest,
            refused: state.refused.clone(),
            failed: state.failed.clone(),
        }
    }

    fn apply(&self, pushed: &PushedKeys) {
        let short: String = pushed.digest.chars().take(12).collect();
        let result = self.put(pushed);
        let mut state = self.state();
        match result {
            Ok((account, put)) => {
                let count = pushed.keys.len();
                let keys = if count == 1 { "key" } else { "keys" };
                let how = match put {
                    Put::Written => "in force",
                    Put::Unchanged => "already in force",
                };
                self.log.line(&format!(
                    "operator keys {short}: {count} {keys} {how} in the login account's authorized_keys"
                ));
                *state = State {
                    held: Some((account, pushed.digest.clone())),
                    refused: None,
                    failed: String::new(),
                };
            }
            Err(Ended::Refused(reason)) => {
                let reason = clip(&reason);
                self.log.line(&format!(
                    "error: operator keys {short} are refused: {reason}"
                ));
                state.refused = Some(KeysRefusal {
                    digest: pushed.digest.clone(),
                    reason,
                });
                state.failed = String::new();
            }
            Err(Ended::Failed(reason)) => {
                let reason = clip(&reason);
                self.log.line(&format!(
                    "error: operator keys {short} are not in force: {reason}"
                ));
                state.held = None;
                state.refused = None;
                state.failed = reason;
            }
        }
    }
}

#[cfg(test)]
#[path = "operator_keys_tests.rs"]
mod tests;
