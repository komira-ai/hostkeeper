//! A Mac's [`SshHost`] (docs/design/ssh-through-coordinator.md, sections 3 and 6): the same
//! [`crate::ssh_setup::SshSetup`] flow and the same three files as Linux, with what differs
//! behind the port.
//!
//! - **Before any write** ([`SshHost::prepare`]), in this order: `/etc/ssh/sshd_config` must have
//!   an `Include` of `/etc/ssh/sshd_config.d/*` that is read for every connection (a line before
//!   any `Match`), else `no_include` (the daemon never edits a file Apple owns); the login user is
//!   the one the record names, which must be an account, or, for an empty one, the single local
//!   administrator ([`MacAdmin`], the rule of the operator keys), else `no_login_user`; when the
//!   group `com.apple.access_ssh` exists, only its members (and the members of `admin`, when the
//!   group nests it) may log in over ssh, and a login user it does not admit is
//!   `blocked_by_access_group`, the group untouched; and `password_auth = off` is `unsupported`,
//!   since a Mac has no shadow file to prove an account has no password path.
//! - **Unit**: Remote Login under launchd ([`SshdUnit::Launchd`]). [`SshHost::start`] is
//!   [`MacSsh::ensure_remote_login`]: `launchctl print`, and only when the job is not loaded
//!   `enable` and `bootstrap`; it never disables or boots out, and it never runs `systemsetup`
//!   (which needs Full Disk Access). launchd starts sshd per connection, so there is no reload:
//!   sshd reads the files at each start and the check is `sshd -t` and the banner.
//! - **The rest** is the Linux host's: `sshd -T -C`, `id -Gn`, `ssh-keygen` and the host keys under
//!   `/etc/ssh` ([`SystemHost`]), and the accounts with keys, read from `dscl`.
//!
//! A reason names a fixed word, never an account.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_link::Log;
use hostkeeper_observe::CommandRunner;

use crate::login_account::{AccountError, AccountSource, MacAdmin, dscl_value, group_members, run};
use crate::mac::TOOL_DEADLINE;
use crate::mac_ssh::{DSEDITGROUP, MacSsh, NO_RECORD};
use crate::remote_login_probe::DSCL;
use crate::ssh_effective::Effective;
use crate::ssh_files::{Refused, SshState, plain};
use crate::ssh_host::{Account, SshHost, SshdUnit, SystemHost, Tools, UnitError};

/// sshd's configuration file, which Apple owns.
pub const SSHD_CONFIG: &str = "/etc/ssh/sshd_config";

/// The directory the daemon's drop-in goes in, as an `Include` names it.
const INCLUDE_DIR: &str = "/etc/ssh/sshd_config.d";

/// The group whose members alone may log in over ssh, when it exists.
const ACCESS_RECORD: &str = "/Groups/com.apple.access_ssh";

/// The group id (GeneratedUID) of the `admin` group, which a group lists in `NestedGroups`.
const ADMIN_GROUP_GUID: &str = "ABCDEFAB-CDEF-ABCD-EFAB-CDEF00000050";

/// The longest login user name, as the record's own check allows.
const NAME_MAX: usize = 32;

/// A Mac as the sshd setup's host.
pub struct MacHost {
    /// The `sshd`, `id`, `ssh-keygen` and host key steps, which are the Linux host's.
    inner: SystemHost,
    remote_login: MacSsh,
    dscl: Box<dyn Launchctl>,
    admin: MacAdmin,
    sshd_config: PathBuf,
    log: Arc<dyn Log>,
}

impl MacHost {
    /// The Mac itself: its programs and `sshd_config`, each program bounded by
    /// [`TOOL_DEADLINE`].
    #[must_use]
    pub fn system(runner: Arc<dyn CommandRunner>, log: Arc<dyn Log>) -> MacHost {
        let tool = |program: &str| -> Box<dyn Launchctl> {
            Box::new(LaunchctlProcess::new(Path::new(program), TOOL_DEADLINE))
        };
        MacHost::new(runner, Tools::macos(), &tool, Path::new(SSHD_CONFIG), log)
    }

    /// A Mac whose `sshd`, `ssh-keygen` and `id` run through `runner` at `tools`, whose
    /// `launchctl`, `dscl` and `dseditgroup` are what `tool` makes of their paths, and whose
    /// `sshd_config` is at `sshd_config`.
    #[must_use]
    pub fn new(
        runner: Arc<dyn CommandRunner>,
        tools: Tools,
        tool: &dyn Fn(&str) -> Box<dyn Launchctl>,
        sshd_config: &Path,
        log: Arc<dyn Log>,
    ) -> MacHost {
        MacHost {
            inner: SystemHost::new(runner, tools),
            remote_login: MacSsh::new(
                tool(hostkeeper_launchd::LAUNCHCTL),
                tool(DSCL),
                tool(DSEDITGROUP),
            ),
            dscl: tool(DSCL),
            admin: MacAdmin::new(tool(DSCL)),
            sshd_config: sshd_config.to_owned(),
            log,
        }
    }

    /// `dscl` could not give an answer the setup can read.
    fn unreadable() -> Refused {
        Refused::new(SshState::Unsupported, "dscl")
    }

    /// `sshd_config` includes the drop-in directory for every connection.
    fn check_include(&self) -> Result<(), Refused> {
        let text = fs::read_to_string(&self.sshd_config)
            .map_err(|_| Refused::new(SshState::NoInclude, "sshd_config"))?;
        if includes_drop_in_dir(&text) {
            Ok(())
        } else {
            Err(Refused::new(SshState::NoInclude, "Include"))
        }
    }

    /// The account that takes the certificates.
    fn login_user(&self, requested: &str) -> Result<String, Refused> {
        if requested.is_empty() {
            return match self.admin.login_account() {
                Ok(account) => Ok(account.name),
                Err(AccountError::Refused(_)) => {
                    Err(Refused::new(SshState::NoLoginUser, "administrator"))
                }
                Err(AccountError::Failed(_)) => Err(MacHost::unreadable()),
            };
        }
        if !plain(requested, NAME_MAX) {
            return Err(Refused::new(SshState::ConfigInvalid, "login_user"));
        }
        let record = format!("/Users/{requested}");
        let output = run(
            &*self.dscl,
            "dscl read of a user record",
            &[".", "-read", &record, "UniqueID"],
        )
        .map_err(|_| MacHost::unreadable())?;
        if output.code == Some(0) {
            Ok(requested.to_owned())
        } else {
            Err(Refused::new(SshState::NoLoginUser, "not_an_account"))
        }
    }

    /// The members of the `admin` group.
    fn admin_members(&self) -> Result<Vec<String>, Refused> {
        let output = run(
            &*self.dscl,
            "dscl read of the admin group",
            &[".", "-read", "/Groups/admin", "GroupMembership"],
        )
        .map_err(|_| MacHost::unreadable())?;
        Ok(group_members(&String::from_utf8_lossy(&output.stdout)))
    }

    /// `com.apple.access_ssh`, when it exists, admits `user`. Nothing is edited.
    fn check_access_group(&self, user: &str) -> Result<(), Refused> {
        let output = run(
            &*self.dscl,
            "dscl read of the ssh access group",
            &[
                ".",
                "-read",
                ACCESS_RECORD,
                "GroupMembership",
                "NestedGroups",
            ],
        )
        .map_err(|_| MacHost::unreadable())?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.code != Some(0) {
            let said = format!("{text}{}", output.stderr_text());
            return if NO_RECORD.iter().any(|mark| said.contains(mark)) {
                Ok(())
            } else {
                Err(MacHost::unreadable())
            };
        }
        let nests_admin = dscl_value(&text, "NestedGroups").is_some_and(|value| {
            value
                .split_whitespace()
                .any(|guid| guid.eq_ignore_ascii_case(ADMIN_GROUP_GUID))
        });
        let admitted = group_members(&text).iter().any(|member| member == user)
            || (nests_admin && self.admin_members()?.iter().any(|member| member == user));
        if admitted {
            Ok(())
        } else {
            Err(Refused::new(
                SshState::BlockedByAccessGroup,
                "access_ssh_group",
            ))
        }
    }
}

impl SshHost for MacHost {
    fn prepare(&self, requested: &str, passwords_off: bool) -> Result<String, Refused> {
        self.check_include()?;
        let user = self.login_user(requested)?;
        self.check_access_group(&user)?;
        if passwords_off {
            return Err(Refused::new(SshState::Unsupported, "password_auth"));
        }
        Ok(user)
    }

    fn unit(&self) -> Result<SshdUnit, UnitError> {
        Ok(SshdUnit::Launchd)
    }

    fn start(&self, _unit: &SshdUnit) -> Result<(), String> {
        self.remote_login
            .ensure_remote_login(&*self.log)
            .map_err(|error| error.to_string())
    }

    fn reload(&self, unit: &SshdUnit) -> Result<(), String> {
        self.inner.reload(unit)
    }

    fn check_config(&self, candidate: &Path) -> Result<(), String> {
        self.inner.check_config(candidate)
    }

    fn effective(&self, user: &str) -> Result<Effective, String> {
        self.inner.effective(user)
    }

    fn groups(&self, user: &str) -> Vec<String> {
        self.inner.groups(user)
    }

    fn banner(&self, address: &(String, u16)) -> Result<(), String> {
        self.inner.banner(address)
    }

    /// The accounts with a non-empty `authorized_keys` in their home, from the directory
    /// service; none when it cannot be read. No account is marked as having a password.
    fn accounts(&self) -> Vec<Account> {
        let Ok(output) = run(
            &*self.dscl,
            "dscl list of homes",
            &[".", "-list", "/Users", "NFSHomeDirectory"],
        ) else {
            return Vec::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let (name, home) = line.split_once(char::is_whitespace)?;
                let keys = Path::new(home.trim()).join(".ssh/authorized_keys");
                fs::metadata(keys)
                    .is_ok_and(|meta| meta.len() > 0)
                    .then(|| Account {
                        name: name.to_owned(),
                        keyed: true,
                        password: false,
                    })
            })
            .collect()
    }

    fn host_key_fingerprints(&self) -> Vec<String> {
        self.inner.host_key_fingerprints()
    }

    fn revocations(&self, ca_keys: &[String], serials: &[u64]) -> Result<Vec<u8>, String> {
        self.inner.revocations(ca_keys, serials)
    }
}

/// Whether `sshd_config` reads the drop-in directory for every connection: an `Include` line
/// naming `/etc/ssh/sshd_config.d/*` or `*.conf` (or the same relative to `/etc/ssh`), before the
/// first `Match` line, since an `Include` inside a `Match` block applies to some connections
/// only. Keywords are not case sensitive; a line starting with `#` is a comment.
#[must_use]
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
#[path = "ssh_host_mac_tests.rs"]
mod tests;
