//! The group `hostkeeper-identity`, whose members may read the host's identity copy
//! ([`crate::host_identity`], docs/design/host-identity.md, section 2): found by name each time it
//! is asked for, and made when the host has none.
//!
//! - Linux, under systemd and under OpenRC: the group file is read (`/etc/group`, a path the
//!   caller gives): the line whose name is the group's gives its gid, at every step.
//! - macOS: `dscl . -list /Groups PrimaryGroupID` is read for the group's line. The gid found is
//!   kept and used for [`LOOKUP_EVERY`] steps before `dscl` is run again, or until
//!   [`IdentityGroup::forget`] says a write with it failed.
//!
//! When the host has no such group it is made by the platform's own tool, run once: `groupadd
//! --system` under systemd, `addgroup -S` (busybox) under OpenRC, `dseditgroup -o create` on a
//! Mac. The group is then looked up again; a tool that exited 0 and made nothing is an error too.
//! After a failure to make the group the tool is run again at the first step [`LOOKUP_EVERY`]
//! steps after the try (steps are counted by calls, no clock is read); the steps between look the
//! group up as always and give the failure of the try again. Every tool runs through a
//! [`Launchctl`] port, so that the tests run none.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};

use crate::login_account::run_ok;

/// The name of the group.
pub const IDENTITY_GROUP: &str = "hostkeeper-identity";

/// The group file of a Linux host.
const GROUP_FILE: &str = "/etc/group";

/// The shadow-utils tool that makes a group under systemd.
const GROUPADD: &str = "/usr/sbin/groupadd";

/// The busybox tool that makes a group on Alpine.
const ADDGROUP: &str = "/usr/sbin/addgroup";

/// How long a tool of this module may run.
const TOOL_DEADLINE: Duration = Duration::from_secs(30);

/// How many steps pass between two tries to make the group that failed, and between two runs of
/// `dscl` while the gid found is kept.
pub(crate) const LOOKUP_EVERY: u32 = 12;

/// The directory service's node of the group records of a Mac.
const GROUPS: &str = "/Groups";

/// Why the group has no gid.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GroupError {
    /// The group file could not be read.
    #[error("cannot read the group file {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The group's line does not give a gid.
    #[error("the line of the group {group} in {source_name} does not give a gid")]
    Malformed { group: String, source_name: String },
    /// A tool did not run or did not exit 0 (what the tool runner says of it).
    #[error("{0}")]
    Tool(String),
    /// The group was made and is still not found.
    #[error("the group {0} is not there after it was made")]
    StillMissing(String),
    /// The failure of the last try to make the group, given again at a step that does not try.
    #[error("{0}")]
    Earlier(String),
}

/// Where the group is looked up.
enum Directory {
    /// The group file of a Linux host.
    Etc(PathBuf),
    /// The directory service of a Mac, through `dscl`.
    Dscl(Box<dyn Launchctl>),
}

/// A try to make the group that failed.
struct Failed {
    /// What the failure said.
    message: String,
    /// The steps still to pass before the tool is run again.
    wait: u32,
}

/// A group of the host that is found by name and made when it is missing.
pub struct IdentityGroup {
    name: String,
    directory: Directory,
    maker: Box<dyn Launchctl>,
    make_args: Vec<String>,
    /// The gid `dscl` gave and the steps it has been used for.
    kept: Option<(u32, u32)>,
    failed: Option<Failed>,
}

/// The tool that makes the group `name` on a host of `kind`, and its arguments.
fn make_command(kind: PlatformKind, name: &str) -> (&'static str, Vec<String>) {
    let (tool, args): (&str, &[&str]) = match kind {
        PlatformKind::Linux => (GROUPADD, &["--system"]),
        PlatformKind::Openrc => (ADDGROUP, &["-S"]),
        PlatformKind::Macos => (crate::mac_ssh::DSEDITGROUP, &["-o", "create"]),
    };
    let mut args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    args.push(name.to_owned());
    (tool, args)
}

impl IdentityGroup {
    fn over(
        name: &str,
        directory: Directory,
        maker: Box<dyn Launchctl>,
        kind: PlatformKind,
    ) -> Self {
        IdentityGroup {
            name: name.to_owned(),
            directory,
            maker,
            make_args: make_command(kind, name).1,
            kept: None,
            failed: None,
        }
    }

    /// The group `name` looked up in the group file `file`, made by `maker`, which runs the tool
    /// of `kind` (a Linux platform) with the arguments that make `name`.
    #[must_use]
    pub fn in_group_file(
        name: &str,
        file: PathBuf,
        maker: Box<dyn Launchctl>,
        kind: PlatformKind,
    ) -> IdentityGroup {
        IdentityGroup::over(name, Directory::Etc(file), maker, kind)
    }

    /// The group `name` looked up through `dscl`, made by `maker`, which runs `dseditgroup`.
    #[must_use]
    pub(crate) fn in_directory_service(
        name: &str,
        dscl: Box<dyn Launchctl>,
        maker: Box<dyn Launchctl>,
    ) -> IdentityGroup {
        IdentityGroup::over(name, Directory::Dscl(dscl), maker, PlatformKind::Macos)
    }

    /// The group [`IDENTITY_GROUP`] of this host, looked up and made as `kind`'s platform does.
    #[must_use]
    pub(crate) fn for_host(kind: PlatformKind) -> IdentityGroup {
        let (tool, _) = make_command(kind, IDENTITY_GROUP);
        let maker = Box::new(LaunchctlProcess::new(Path::new(tool), TOOL_DEADLINE));
        match kind {
            PlatformKind::Macos => IdentityGroup::in_directory_service(
                IDENTITY_GROUP,
                Box::new(LaunchctlProcess::new(
                    Path::new(crate::remote_login_probe::DSCL),
                    TOOL_DEADLINE,
                )),
                maker,
            ),
            PlatformKind::Linux | PlatformKind::Openrc => {
                IdentityGroup::in_group_file(IDENTITY_GROUP, PathBuf::from(GROUP_FILE), maker, kind)
            }
        }
    }

    /// The group's gid for this step, after making the group when the host has none and the
    /// last try to make it was not within [`LOOKUP_EVERY`] steps. Each call is one step.
    ///
    /// # Errors
    /// [`GroupError`]: the lookup failed, the tool failed, or the group is not there after it ran.
    pub(crate) fn ensure(&mut self) -> Result<u32, GroupError> {
        if let Some(gid) = self.find()? {
            self.failed = None;
            return Ok(gid);
        }
        if let Some(failed) = self.failed.as_mut()
            && failed.wait > 0
        {
            failed.wait -= 1;
            return Err(GroupError::Earlier(failed.message.clone()));
        }
        let made = self.make();
        self.failed = made.as_ref().err().map(|error| Failed {
            message: error.to_string(),
            wait: LOOKUP_EVERY - 1,
        });
        made
    }

    /// Drops the gid kept from `dscl`, so that the next step looks the group up again.
    pub(crate) fn forget(&mut self) {
        self.kept = None;
    }

    fn make(&mut self) -> Result<u32, GroupError> {
        let args: Vec<&str> = self.make_args.iter().map(String::as_str).collect();
        run_ok(&*self.maker, "making the identity group", &args).map_err(GroupError::Tool)?;
        self.kept = None;
        self.find()?
            .ok_or_else(|| GroupError::StillMissing(self.name.clone()))
    }

    /// The group's gid, or `None` when the host has no such group.
    fn find(&mut self) -> Result<Option<u32>, GroupError> {
        let found = match &self.directory {
            Directory::Etc(file) => {
                let text = fs::read_to_string(file).map_err(|source| GroupError::Read {
                    path: file.clone(),
                    source,
                })?;
                let line = text
                    .lines()
                    .find(|line| line.split(':').next() == Some(self.name.as_str()));
                return line
                    .map(|line| self.gid(line.split(':').nth(2), &file.display().to_string()))
                    .transpose();
            }
            Directory::Dscl(dscl) => {
                if let Some((gid, used)) = self.kept.as_mut()
                    && *used < LOOKUP_EVERY
                {
                    *used += 1;
                    return Ok(Some(*gid));
                }
                let text = run_ok(
                    &**dscl,
                    "dscl list of groups",
                    &[".", "-list", GROUPS, "PrimaryGroupID"],
                )
                .map_err(GroupError::Tool)?;
                let line = text
                    .lines()
                    .find(|line| line.split_whitespace().next() == Some(self.name.as_str()));
                line.map(|line| self.gid(line.split_whitespace().nth(1), "the directory service"))
                    .transpose()?
            }
        };
        self.kept = found.map(|gid| (gid, 1));
        Ok(found)
    }

    /// `field` as a gid, or why the group's line in `source` does not give one.
    fn gid(&self, field: Option<&str>, source: &str) -> Result<u32, GroupError> {
        field
            .and_then(|gid| gid.parse::<u32>().ok())
            .ok_or_else(|| GroupError::Malformed {
                group: self.name.clone(),
                source_name: source.to_owned(),
            })
    }
}

#[cfg(test)]
#[path = "identity_group_tests.rs"]
pub(crate) mod tests;
