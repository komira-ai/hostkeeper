//! FileVault in the `macos` kind's apply (docs/design/maintenance-updates.md, section 5.5,
//! "FileVault"): with FileVault on, a restart stops at the pre-boot login unless it is an
//! authenticated restart, so on such a Mac the update is installed without `--restart` and the
//! Mac is then restarted by `fdesetup authrestart -inputplist`, the update account's name and
//! password in a property list on that program's standard input.
//!
//! **Readiness** ([`restart_of`]), which the `cutover` checks before it lists or keeps anything,
//! in order, each look through the apply's [`Programs`] and its exit 0 alone accepted:
//!
//! 1. `fdesetup status`. Off: the install restarts the Mac itself ([`Restart::ByInstall`]), and
//!    nothing below is asked. Being turned on or off, or in no state it names: `filevault_not_ready`.
//! 2. On: `fdesetup supportsauthrestart` must say `true`, else `filevault_not_ready`.
//! 3. `fdesetup list` must hold a line `<name>,<uuid>` whose name is the update account's,
//!    exactly, else `filevault_not_ready`: only a FileVault user can authorise the restart.
//! 4. The account's password must go in a property list as it is: UTF-8, with no control
//!    character but a tab and neither U+FFFE nor U+FFFF (XML would refuse one, or read a carriage
//!    return as a line feed), else
//!    `filevault_not_ready`. Then [`Restart::Authenticated`].
//!
//! **The property list** ([`RestartPlist`]) is Apple's XML layout, one tab per level, a
//! dictionary of `Username` and `Password`, the name and the password escaped (`&`, `<`, `>`,
//! `"`, `'`). Its `Debug` prints none of it; it goes to one place, the standard input of
//! [`authrestart`], whose arguments are `authrestart -inputplist` alone and whose environment is
//! empty ([`super::HostInstallers`]). A run that does not start, is killed at
//! [`AUTHRESTART_DEADLINE`] or exits non-zero fails `filevault_authrestart_failed`, whose detail,
//! like the one log line of the call, names the step and the exit code alone.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use hostkeeper_launchd::stdin_process::run_ok;
use hostkeeper_link::Log;
use hostkeeper_proto::hostkeeper::agent::v1::FileVaultState;

use super::{Installers, failed, look_failed};
use crate::maintenance_collect::listing;
use crate::maintenance_hook::kind::KindError;
use crate::maintenance_hook::macos::stage::Programs;
use crate::maintenance_mac::{FDESETUP, parse_fdesetup_status, parse_supportsauthrestart};
use crate::update_account::UpdateAccount;
use crate::xcode_clt::LOOK_DEADLINE;

/// The reason of a Mac whose FileVault does not let the update be installed and restarted.
pub const REASON_FILEVAULT_NOT_READY: &str = "filevault_not_ready";
/// The reason of an authenticated restart that did not run or did not succeed.
pub const REASON_AUTHRESTART_FAILED: &str = "filevault_authrestart_failed";

/// The step of the authenticated restart, as its error and log line name it.
pub const STEP_AUTHRESTART: &str = "fdesetup authrestart";

/// How long `fdesetup authrestart` may run before the Mac restarts: it stores the unlock key and
/// asks for the restart. One stopped at its deadline fails.
pub const AUTHRESTART_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// The arguments of the authenticated restart.
const AUTHRESTART_ARGS: [&str; 2] = ["authrestart", "-inputplist"];

/// How the Mac restarts after its install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Restart {
    /// The install program restarts it (`--restart`, `startosinstall`): FileVault is off.
    ByInstall,
    /// `fdesetup authrestart`, after an install without `--restart`: FileVault is on.
    Authenticated,
}

/// The failure of a Mac whose FileVault is not ready, for `why`.
fn not_ready(why: &str) -> KindError {
    failed(
        REASON_FILEVAULT_NOT_READY,
        format!("{why}, so the update is not installed"),
    )
}

/// What `fdesetup` writes for `args`, `what` naming the look.
fn fdesetup(programs: &dyn Programs, what: &str, args: &[&str]) -> Result<String, KindError> {
    let tool = programs.tool(Path::new(FDESETUP), LOOK_DEADLINE);
    let (_, text) = listing(&*tool, what, args, &[0]).map_err(look_failed)?;
    Ok(text)
}

/// Whether `fdesetup list` wrote a line `<name>,<uuid>` of the account `name`.
fn listed(text: &str, name: &str) -> bool {
    text.lines()
        .filter_map(|line| line.trim().split_once(','))
        .any(|(user, _)| user == name)
}

/// How the Mac of `programs` restarts after an install authorised by `account`, or why FileVault
/// does not let it, as the module says.
///
/// # Errors
/// `filevault_not_ready` naming what is not ready, or `macos_look_failed` for a look that failed.
pub(super) fn restart_of(
    programs: &dyn Programs,
    account: &UpdateAccount,
) -> Result<Restart, KindError> {
    let status = fdesetup(programs, "fdesetup status", &["status"])?;
    let state = match parse_fdesetup_status(&status).0 {
        FileVaultState::Off => return Ok(Restart::ByInstall),
        FileVaultState::On => None,
        FileVaultState::Encrypting => Some("encrypting"),
        FileVaultState::Decrypting => Some("decrypting"),
        FileVaultState::Unspecified => Some("in no state fdesetup names"),
    };
    if let Some(state) = state {
        return Err(not_ready(&format!(
            "FileVault is {state} (fdesetup status), neither on nor off"
        )));
    }
    let answer = fdesetup(
        programs,
        "fdesetup supportsauthrestart",
        &["supportsauthrestart"],
    )?;
    if !parse_supportsauthrestart(&answer).0 {
        return Err(not_ready(
            "FileVault is on and fdesetup supportsauthrestart does not say true",
        ));
    }
    let users = fdesetup(programs, "fdesetup list", &["list"])?;
    if !listed(&users, account.name()) {
        return Err(not_ready(&format!(
            "FileVault is on and the update account {} is not a FileVault user (fdesetup list)",
            account.name()
        )));
    }
    RestartPlist::of(account).map(|_| Restart::Authenticated)
}

/// The property list of an authenticated restart. Its `Debug` prints none of it.
pub(super) struct RestartPlist(Vec<u8>);

impl fmt::Debug for RestartPlist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RestartPlist(withheld)")
    }
}

/// `text` escaped for an XML string, or `None` when it holds a control character but a tab, or
/// U+FFFE or U+FFFF, which XML 1.0 refuses too.
fn escaped(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' => out.push(c),
            '\u{FFFE}' | '\u{FFFF}' => return None,
            c if c.is_control() => return None,
            c => out.push(c),
        }
    }
    Some(out)
}

impl RestartPlist {
    /// The property list of `account`, as the module says.
    ///
    /// # Errors
    /// `filevault_not_ready` when the password cannot go in it; the error holds none of it.
    pub(super) fn of(account: &UpdateAccount) -> Result<RestartPlist, KindError> {
        let password = std::str::from_utf8(account.password())
            .ok()
            .and_then(escaped)
            .ok_or_else(|| {
                not_ready(
                    "FileVault is on and the update account's password is not UTF-8 or holds a \
                     control character, which the property list of fdesetup authrestart cannot \
                     carry",
                )
            })?;
        // The name is ASCII letters, digits, `_`, `.` and `-` (the account file's rule): nothing
        // to escape, but escaped all the same.
        let name = escaped(account.name()).unwrap_or_default();
        let text = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \t<key>Username</key>\n\
             \t<string>{name}</string>\n\
             \t<key>Password</key>\n\
             \t<string>{password}</string>\n\
             </dict>\n\
             </plist>\n"
        );
        Ok(RestartPlist(text.into_bytes()))
    }
}

/// The authenticated restart: `fdesetup authrestart -inputplist` through `installers`, `plist` on
/// its standard input, and one line logged, which names the step and the exit code alone.
///
/// # Errors
/// `filevault_authrestart_failed`, naming the step and the exit code alone.
pub(super) fn authrestart(
    installers: &dyn Installers,
    plist: &RestartPlist,
    log: &dyn Log,
) -> Result<(), KindError> {
    let runner = installers.runner(Path::new(FDESETUP), AUTHRESTART_DEADLINE);
    let args: Vec<String> = AUTHRESTART_ARGS
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
    match run_ok(&*runner, STEP_AUTHRESTART, &args, &plist.0) {
        Ok(_) => {
            log.line(&format!("{STEP_AUTHRESTART}: exited with code 0"));
            Ok(())
        }
        Err(error) => {
            let detail = error.to_string();
            log.line(&detail);
            Err(failed(REASON_AUTHRESTART_FAILED, detail))
        }
    }
}
