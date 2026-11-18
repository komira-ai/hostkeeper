//! The group `hostkeeper-identity`, whose members may read the host's identity copy
//! ([`crate::host_identity`], docs/design/host-identity.md, section 2): found by name each time it
//! is asked for, and made when the host has none.
//!
//! - Linux, under systemd and under OpenRC: the group file is read (`/etc/group`, a path the
//!   caller gives): the line whose name is the group's gives its gid.
//! - macOS: `dscl . -list /Groups PrimaryGroupID` is read for the group's line.
//!
//! When the host has no such group it is made by the platform's own tool, run once: `groupadd
//! --system` under systemd, `addgroup -S` (busybox) under OpenRC, `dseditgroup -o create` on a
//! Mac. The group is then looked up again; a tool that exited 0 and made nothing is an error too.
//! Every tool runs through a [`Launchctl`] port, so that the tests run none.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};

use crate::login_account::run_ok;

/// The name of the group.
pub const IDENTITY_GROUP: &str = "hostkeeper-identity";

/// The group file of a Linux host.
pub const GROUP_FILE: &str = "/etc/group";

/// The shadow-utils tool that makes a group under systemd.
pub const GROUPADD: &str = "/usr/sbin/groupadd";

/// The busybox tool that makes a group on Alpine.
pub const ADDGROUP: &str = "/usr/sbin/addgroup";

/// How long a tool of this module may run.
pub const TOOL_DEADLINE: Duration = Duration::from_secs(30);

/// The directory service's node of the group records of a Mac.
const GROUPS: &str = "/Groups";

/// Why the group has no gid.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GroupError {
    /// The group file could not be read.
    #[error("cannot read the group file {}: {why}", path.display())]
    Read { path: PathBuf, why: String },
    /// The group's line does not give a gid.
    #[error("the line of the group {group} in {source_name} does not give a gid")]
    Malformed { group: String, source_name: String },
    /// A tool did not run or did not exit 0.
    #[error("{0}")]
    Tool(String),
    /// The group was made and is still not found.
    #[error("the group {0} is not there after it was made")]
    StillMissing(String),
}

/// Where the group is looked up.
enum Directory {
    /// The group file of a Linux host.
    Etc(PathBuf),
    /// The directory service of a Mac, through `dscl`.
    Dscl(Box<dyn Launchctl>),
}

/// A group of the host that is found by name and made when it is missing.
pub struct IdentityGroup {
    name: String,
    directory: Directory,
    maker: Box<dyn Launchctl>,
    make_args: Vec<String>,
}

/// The tool that makes the group `name` on a host of `kind`, and its arguments.
#[must_use]
pub fn make_command(kind: PlatformKind, name: &str) -> (&'static str, Vec<String>) {
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
    /// The group `name` looked up in the group file `file`, made by `maker` run with `make_args`.
    #[must_use]
    pub fn in_group_file(
        name: &str,
        file: PathBuf,
        maker: Box<dyn Launchctl>,
        make_args: Vec<String>,
    ) -> IdentityGroup {
        IdentityGroup {
            name: name.to_owned(),
            directory: Directory::Etc(file),
            maker,
            make_args,
        }
    }

    /// The group `name` looked up through `dscl`, made by `maker` run with `make_args`.
    #[must_use]
    pub fn in_directory_service(
        name: &str,
        dscl: Box<dyn Launchctl>,
        maker: Box<dyn Launchctl>,
        make_args: Vec<String>,
    ) -> IdentityGroup {
        IdentityGroup {
            name: name.to_owned(),
            directory: Directory::Dscl(dscl),
            maker,
            make_args,
        }
    }

    /// The group [`IDENTITY_GROUP`] of this host, looked up and made as `kind`'s platform does.
    #[must_use]
    pub fn for_host(kind: PlatformKind) -> IdentityGroup {
        let (tool, args) = make_command(kind, IDENTITY_GROUP);
        let maker = Box::new(LaunchctlProcess::new(Path::new(tool), TOOL_DEADLINE));
        match kind {
            PlatformKind::Macos => IdentityGroup::in_directory_service(
                IDENTITY_GROUP,
                Box::new(LaunchctlProcess::new(
                    Path::new(crate::remote_login_probe::DSCL),
                    TOOL_DEADLINE,
                )),
                maker,
                args,
            ),
            PlatformKind::Linux | PlatformKind::Openrc => {
                IdentityGroup::in_group_file(IDENTITY_GROUP, PathBuf::from(GROUP_FILE), maker, args)
            }
        }
    }

    /// The group's gid, found now, after making the group when the host has none.
    ///
    /// # Errors
    /// [`GroupError`]: the lookup failed, the tool failed, or the group is not there after it ran.
    pub fn ensure(&self) -> Result<u32, GroupError> {
        if let Some(gid) = self.find()? {
            return Ok(gid);
        }
        run_ok(&*self.maker, "making the identity group", &self.args())
            .map_err(GroupError::Tool)?;
        self.find()?
            .ok_or_else(|| GroupError::StillMissing(self.name.clone()))
    }

    fn args(&self) -> Vec<&str> {
        self.make_args.iter().map(String::as_str).collect()
    }

    /// The group's gid, or `None` when the host has no such group.
    fn find(&self) -> Result<Option<u32>, GroupError> {
        match &self.directory {
            Directory::Etc(file) => {
                let text = fs::read_to_string(file).map_err(|error| GroupError::Read {
                    path: file.clone(),
                    why: error.to_string(),
                })?;
                let line = text
                    .lines()
                    .find(|line| line.split(':').next() == Some(self.name.as_str()));
                line.map(|line| self.gid(line.split(':').nth(2), &file.display().to_string()))
                    .transpose()
            }
            Directory::Dscl(dscl) => {
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
                    .transpose()
            }
        }
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
