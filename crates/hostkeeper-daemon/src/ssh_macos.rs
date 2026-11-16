//! What a Mac must hold before the daemon sets up its sshd to trust the coordinator's SSH user CA
//! (docs/design/ssh-through-coordinator.md, sections 3, 4 and 6): the macOS side of the host
//! setup, behind the same two ports every Mac adapter uses ([`Launchctl`], whose real side runs
//! one program with a fixed argument list, no shell and an empty environment).
//!
//! [`MacSsh::prepare`] is the whole of it, in this order, and it changes nothing until the last
//! step:
//!
//! 1. **Include.** `/etc/ssh/sshd_config` must have an `Include` of `/etc/ssh/sshd_config.d/*`
//!    that is read for every connection (a line before any `Match`). The daemon never edits a
//!    file Apple owns; a Mac without it is [`NO_INCLUDE`] and stays untrusted.
//! 2. **Login user.** The coordinator's `login_user` when it names one; otherwise the single local
//!    administrator account (user id of 501 or more, member of `admin`, not hidden). None, or
//!    more than one, is [`NO_LOGIN_USER`]. The name becomes part of a file name, so a name that is
//!    not a plain account name is refused.
//! 3. **Access group.** When the group `com.apple.access_ssh` exists, only its members (and the
//!    members of the `admin` group, when the group nests it) may log in over ssh. The login user
//!    must be one of them; the daemon never edits a group, and says [`BLOCKED_BY_ACCESS_GROUP`]
//!    when it is not.
//! 4. **Remote Login**, the macOS switch for sshd, turned on with `systemsetup -setremotelogin on`
//!    and read back with `-getremotelogin`. On macOS 13 and later the tool refuses unless the
//!    calling process has Full Disk Access, which a configuration profile grants (the one in
//!    `docs/deploy/mac/`, from the design's PR); that refusal is
//!    [`NEEDS_FULL_DISK_ACCESS`], and nothing was changed. The daemon only turns Remote Login on:
//!    it never turns it off, so a revert leaves sshd as it found it.
//!
//! The words of each refusal are the `SshReport.state` strings of the wire (`agent.proto`); the
//! detail is a fixed word, never an account's name.
//!
//! Which output macOS gives for each tool is read from the tools' documented behaviour, not
//! measured on a Mac here: the measurement is build item C0 of the design, and the parsing below
//! is the part C0 may change.

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_launchd::{Launchctl, LaunchctlOutput, LaunchctlProcess};

use crate::mac::TOOL_DEADLINE;

/// The tool that turns Remote Login on and reports it.
pub const SYSTEMSETUP: &str = "/usr/sbin/systemsetup";
/// The tool that reads the directory service's accounts and groups.
pub const DSCL: &str = "/usr/bin/dscl";
/// sshd's configuration file, which Apple owns.
pub const SSHD_CONFIG: &str = "/etc/ssh/sshd_config";
/// The directory the daemon's drop-in goes in, as an `Include` names it.
pub const INCLUDE_DIR: &str = "/etc/ssh/sshd_config.d";
/// The group whose members alone may log in over ssh, when it exists.
pub const ACCESS_SSH_GROUP: &str = "com.apple.access_ssh";
/// The group id (GeneratedUID) of the `admin` group, which a group lists in `NestedGroups`.
pub const ADMIN_GROUP_GUID: &str = "ABCDEFAB-CDEF-ABCD-EFAB-CDEF00000050";
/// The first user id of a person's account; lower ones are the system's.
pub const FIRST_USER_ID: u32 = 501;
/// What `systemsetup` says when the caller lacks Full Disk Access.
const FULL_DISK_ACCESS_TEXT: &str = "Full Disk Access";
/// What `dscl` says of a record that is not there.
const NO_RECORD_TEXT: &str = "eDSRecordNotFound";
/// The longest account name taken as a file name part.
const NAME_MAX: usize = 64;

/// The state: Remote Login cannot be turned on without Full Disk Access.
pub const NEEDS_FULL_DISK_ACCESS: &str = "needs_full_disk_access";
/// The state: `sshd_config` has no usable `Include` of the drop-in directory.
pub const NO_INCLUDE: &str = "no_include";
/// The state: no account takes the certificates.
pub const NO_LOGIN_USER: &str = "no_login_user";
/// The state: `com.apple.access_ssh` exists and does not admit the login user.
pub const BLOCKED_BY_ACCESS_GROUP: &str = "blocked_by_access_group";
/// The state: a tool did not give an answer the daemon can read.
pub const UNSUPPORTED: &str = "unsupported";

/// Why the daemon changed nothing: a `SshReport.state` and its `detail`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blocked {
    /// One of the state constants of this module.
    pub state: &'static str,
    /// A fixed word: the file, tool or kind of account at fault. Never an account's name.
    pub detail: &'static str,
}

/// What Remote Login was when the daemon was done with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteLogin {
    /// It was on; nothing was run to change it.
    AlreadyOn,
    /// It was off, and the daemon turned it on.
    TurnedOn,
}

/// A Mac that can take the access: the account that takes certificates and the Remote Login
/// result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    /// The account the principals file is written for.
    pub login_user: String,
    /// Whether the daemon had to turn Remote Login on.
    pub remote_login: RemoteLogin,
}

/// A Mac's checks, over `systemsetup` and `dscl` ports and the path of `sshd_config`.
#[derive(Debug)]
pub struct MacSsh<S, D> {
    systemsetup: S,
    dscl: D,
    sshd_config: PathBuf,
}

impl MacSsh<LaunchctlProcess, LaunchctlProcess> {
    /// The host's: [`SYSTEMSETUP`] and [`DSCL`] with [`TOOL_DEADLINE`], and [`SSHD_CONFIG`].
    pub fn system() -> MacSsh<LaunchctlProcess, LaunchctlProcess> {
        MacSsh::new(
            LaunchctlProcess::new(Path::new(SYSTEMSETUP), TOOL_DEADLINE),
            LaunchctlProcess::new(Path::new(DSCL), TOOL_DEADLINE),
            Path::new(SSHD_CONFIG),
        )
    }
}

impl<S: Launchctl, D: Launchctl> MacSsh<S, D> {
    /// Checks that run `systemsetup` through `systemsetup`, `dscl` through `dscl` and read the
    /// sshd configuration at `sshd_config`.
    pub fn new(systemsetup: S, dscl: D, sshd_config: &Path) -> MacSsh<S, D> {
        MacSsh {
            systemsetup,
            dscl,
            sshd_config: sshd_config.to_owned(),
        }
    }

    /// Runs the four checks of the module's header for `requested_login_user` (empty: the
    /// single local administrator), and turns Remote Login on as the last one.
    pub fn prepare(&self, requested_login_user: &str) -> Result<Ready, Blocked> {
        self.check_include()?;
        let login_user = self.login_user(requested_login_user)?;
        self.check_access_group(&login_user)?;
        let remote_login = self.remote_login()?;
        Ok(Ready {
            login_user,
            remote_login,
        })
    }

    fn check_include(&self) -> Result<(), Blocked> {
        let text = fs::read_to_string(&self.sshd_config).map_err(|_| Blocked {
            state: NO_INCLUDE,
            detail: "sshd_config",
        })?;
        if includes_drop_in_dir(&text) {
            Ok(())
        } else {
            Err(Blocked {
                state: NO_INCLUDE,
                detail: "Include",
            })
        }
    }

    fn dscl(&self, args: &[&str]) -> Result<LaunchctlOutput, Blocked> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        self.dscl.run(&args).map_err(|_| Blocked {
            state: UNSUPPORTED,
            detail: "dscl",
        })
    }

    fn login_user(&self, requested: &str) -> Result<String, Blocked> {
        if requested.is_empty() {
            return self.single_administrator();
        }
        let no_account = Blocked {
            state: NO_LOGIN_USER,
            detail: "not_an_account",
        };
        if !is_account_name(requested) {
            return Err(no_account);
        }
        let record = format!("/Users/{requested}");
        let output = self.dscl(&[".", "-read", &record, "UniqueID"])?;
        if output.code == Some(0) && !values_of(&text_of(&output), "UniqueID").is_empty() {
            Ok(requested.to_owned())
        } else {
            Err(no_account)
        }
    }

    fn single_administrator(&self) -> Result<String, Blocked> {
        let admins = self.admin_members()?;
        let listing = self.dscl(&[".", "-list", "/Users", "UniqueID"])?;
        let mut found: Vec<String> = Vec::new();
        for (name, id) in parse_ids(&text_of(&listing)) {
            if id >= FIRST_USER_ID
                && is_account_name(&name)
                && admins.contains(&name)
                && !self.is_hidden(&name)?
            {
                found.push(name);
            }
        }
        match <[String; 1]>::try_from(found) {
            Ok([only]) => Ok(only),
            Err(found) => Err(Blocked {
                state: NO_LOGIN_USER,
                detail: if found.is_empty() {
                    "no_administrator"
                } else {
                    "several_administrators"
                },
            }),
        }
    }

    fn admin_members(&self) -> Result<Vec<String>, Blocked> {
        let output = self.dscl(&[".", "-read", "/Groups/admin", "GroupMembership"])?;
        Ok(values_of(&text_of(&output), "GroupMembership"))
    }

    fn is_hidden(&self, name: &str) -> Result<bool, Blocked> {
        let record = format!("/Users/{name}");
        let output = self.dscl(&[".", "-read", &record, "IsHidden"])?;
        Ok(values_of(&text_of(&output), "IsHidden").first().map(String::as_str) == Some("1"))
    }

    fn check_access_group(&self, login_user: &str) -> Result<(), Blocked> {
        let record = format!("/Groups/{ACCESS_SSH_GROUP}");
        let output = self.dscl(&[".", "-read", &record, "GroupMembership", "NestedGroups"])?;
        let text = text_of(&output);
        if output.code != Some(0) {
            return if text.contains(NO_RECORD_TEXT) {
                Ok(())
            } else {
                Err(Blocked {
                    state: UNSUPPORTED,
                    detail: "dscl",
                })
            };
        }
        let admitted = values_of(&text, "GroupMembership")
            .iter()
            .any(|member| member == login_user)
            || (values_of(&text, "NestedGroups")
                .iter()
                .any(|guid| guid.eq_ignore_ascii_case(ADMIN_GROUP_GUID))
                && self.admin_members()?.iter().any(|member| member == login_user));
        if admitted {
            Ok(())
        } else {
            Err(Blocked {
                state: BLOCKED_BY_ACCESS_GROUP,
                detail: ACCESS_SSH_GROUP,
            })
        }
    }

    fn systemsetup(&self, args: &[&str]) -> Result<String, Blocked> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let output = self.systemsetup.run(&args).map_err(|_| Blocked {
            state: UNSUPPORTED,
            detail: "systemsetup",
        })?;
        let text = text_of(&output);
        if text.contains(FULL_DISK_ACCESS_TEXT) {
            return Err(Blocked {
                state: NEEDS_FULL_DISK_ACCESS,
                detail: "systemsetup",
            });
        }
        Ok(text)
    }

    fn remote_login(&self) -> Result<RemoteLogin, Blocked> {
        if self.remote_login_is_on()? {
            return Ok(RemoteLogin::AlreadyOn);
        }
        self.systemsetup(&["-setremotelogin", "on"])?;
        if self.remote_login_is_on()? {
            Ok(RemoteLogin::TurnedOn)
        } else {
            Err(Blocked {
                state: UNSUPPORTED,
                detail: "systemsetup",
            })
        }
    }

    /// Whether `systemsetup -getremotelogin` says `Remote Login: On`; a line that says neither On
    /// nor Off is a tool the daemon cannot read.
    fn remote_login_is_on(&self) -> Result<bool, Blocked> {
        let text = self.systemsetup(&["-getremotelogin"])?.to_ascii_lowercase();
        if text.contains("remote login: on") {
            Ok(true)
        } else if text.contains("remote login: off") {
            Ok(false)
        } else {
            Err(Blocked {
                state: UNSUPPORTED,
                detail: "systemsetup",
            })
        }
    }
}

/// The standard output and standard error of a tool, as one text.
fn text_of(output: &LaunchctlOutput) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// Whether `name` is a plain account name: 1 to [`NAME_MAX`] bytes of ASCII letters, digits, `.`,
/// `_` and `-`, not starting with `-` or `.`. It becomes part of a file name.
pub fn is_account_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_MAX
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The accounts of `dscl . -list /Users UniqueID`: a name and a number on each line. A line that
/// is not that is skipped.
pub fn parse_ids(text: &str) -> Vec<(String, u32)> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let name = words.next()?;
            let id = words.next()?.parse().ok()?;
            words.next().is_none().then(|| (name.to_owned(), id))
        })
        .collect()
}

/// The values of the attribute `key` in `dscl . -read` output: the words after `key:` on its line
/// and on the lines after it that start with a space (a long value `dscl` wraps).
pub fn values_of(text: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key}:");
    let mut values = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            inside = true;
            values.extend(rest.split_whitespace().map(str::to_owned));
        } else if inside && line.starts_with(' ') {
            values.extend(line.split_whitespace().map(str::to_owned));
        } else {
            inside = false;
        }
    }
    values
}

/// Whether `sshd_config` reads the drop-in directory for every connection: an `Include` line
/// naming `/etc/ssh/sshd_config.d/*` or `*.conf` (or the same relative to `/etc/ssh`), before the
/// first `Match` line, since an `Include` inside a `Match` block applies to some connections
/// only. Keywords are not case sensitive; a line starting with `#` is a comment.
pub fn includes_drop_in_dir(text: &str) -> bool {
    let wanted = ["*", "*.conf"].map(|glob| format!("{INCLUDE_DIR}/{glob}"));
    let relative = ["*", "*.conf"].map(|glob| format!("sshd_config.d/{glob}"));
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        if keyword.eq_ignore_ascii_case("match") {
            return false;
        }
        if keyword.eq_ignore_ascii_case("include")
            && words.any(|path| wanted.iter().chain(&relative).any(|known| known == path))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[path = "ssh_macos_tests.rs"]
mod tests;
