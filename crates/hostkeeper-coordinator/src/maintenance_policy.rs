//! A host group's maintenance settings (`docs/operations/hostgroups.md`, Maintenance): the
//! `maintenance` table of its rollout policy ([`crate::host_groups::Policy`]), kept with the policy
//! in the state file and in the database's `host_groups.policy`, checked by `HostGroups.Put` and
//! answered with every key named.
//!
//! Each key at its default is not written, and a table that holds only defaults is not written at
//! all, so a group that names no maintenance is written byte for byte as the release before wrote
//! it, and a group written by the release before reads as one with every key at its default.
//!
//! The maintenance runs read it through [`crate::maintenance_effective`]
//! ([`crate::decide_maintenance`]).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::desired::MAX_COUNT;

/// The most Xcode releases a group's Macs keep.
pub(crate) const MAX_XCODE_KEEP: u32 = 4;

/// The fewest seconds `return_secs` names, when it names any.
pub(crate) const MIN_RETURN_SECS: u64 = 60;

/// The most seconds `return_secs` names.
pub(crate) const MAX_RETURN_SECS: u64 = 86_400;

/// The most numbers of an Xcode version.
const MAX_VERSION_PARTS: usize = 3;

/// The most digits of one number of an Xcode version.
const MAX_PART_DIGITS: usize = 4;

/// What `xcode_default` says for the newest Xcode the group keeps.
const LATEST: &str = "latest";

/// The Xcode `xcode-select` points at on a group's Macs: the newest one, or a version. Read from
/// text only through [`FromStr`], which serde uses too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) enum XcodeDefault {
    /// The newest Xcode the group keeps (`latest`, the default).
    #[default]
    Latest,
    /// One version: 1 to 3 numbers of 1 to 4 digits joined by `.` (`26.1.1`).
    Version(String),
}

/// Text that is neither `latest` nor an Xcode version.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "xcode_default {0:?} is neither latest nor a version of 1 to 3 numbers of 1 to 4 digits joined by ."
)]
pub(crate) struct BadXcodeDefault(pub(crate) String);

impl FromStr for XcodeDefault {
    type Err = BadXcodeDefault;

    fn from_str(text: &str) -> Result<Self, BadXcodeDefault> {
        if text == LATEST {
            return Ok(Self::Latest);
        }
        let parts: Vec<&str> = text.split('.').collect();
        let numbers = parts.iter().all(|part| {
            (1..=MAX_PART_DIGITS).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit())
        });
        if parts.len() > MAX_VERSION_PARTS || !numbers {
            return Err(BadXcodeDefault(text.to_owned()));
        }
        Ok(Self::Version(text.to_owned()))
    }
}

impl TryFrom<String> for XcodeDefault {
    type Error = BadXcodeDefault;

    fn try_from(text: String) -> Result<Self, BadXcodeDefault> {
        text.parse()
    }
}

impl From<XcodeDefault> for String {
    fn from(default: XcodeDefault) -> Self {
        default.to_string()
    }
}

impl fmt::Display for XcodeDefault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Latest => f.write_str(LATEST),
            Self::Version(version) => f.write_str(version),
        }
    }
}

/// Which Linux updates a group's maintenance counts as due.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinuxScope {
    /// Security updates and the kernel (the default).
    #[default]
    Security,
    /// Every update the package manager offers.
    All,
}

/// A group's maintenance settings. Each key at its default is not written, so a
/// table of defaults is written `{}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct Maintenance {
    /// How many of the newest Xcode releases the group's Macs keep, 0 to [`MAX_XCODE_KEEP`]; 0
    /// leaves Xcode alone.
    #[serde(skip_serializing_if = "is_zero")]
    pub(crate) xcode_keep: u32,
    /// The Xcode `xcode-select` points at.
    #[serde(skip_serializing_if = "is_latest")]
    pub(crate) xcode_default: XcodeDefault,
    /// Whether a new major macOS counts as due.
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) macos_major: bool,
    /// Which Linux updates count as due.
    #[serde(skip_serializing_if = "is_security")]
    pub(crate) linux_scope: LinuxScope,
    /// Hosts of the group in maintenance at once, 1 to [`MAX_COUNT`].
    #[serde(skip_serializing_if = "is_one")]
    pub(crate) batch: u32,
    /// Hosts of the group that must stay serving, 0 to [`MAX_COUNT`].
    #[serde(skip_serializing_if = "is_one")]
    pub(crate) min_serving: u32,
    /// How long a drain may wait, in seconds; 0 is no deadline.
    #[serde(skip_serializing_if = "is_zero_secs")]
    pub(crate) drain_deadline_secs: u64,
    /// How long a rebooting host may be silent, in seconds, [`MIN_RETURN_SECS`] to
    /// [`MAX_RETURN_SECS`]; 0 is the platform's default.
    #[serde(skip_serializing_if = "is_zero_secs")]
    pub(crate) return_secs: u64,
}

impl Default for Maintenance {
    fn default() -> Self {
        Self {
            xcode_keep: 0,
            xcode_default: XcodeDefault::Latest,
            macos_major: false,
            linux_scope: LinuxScope::Security,
            batch: 1,
            min_serving: 1,
            drain_deadline_secs: 0,
            return_secs: 0,
        }
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_one(n: &u32) -> bool {
    *n == 1
}

fn is_zero_secs(n: &u64) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_latest(default: &XcodeDefault) -> bool {
    *default == XcodeDefault::Latest
}

fn is_security(scope: &LinuxScope) -> bool {
    *scope == LinuxScope::Security
}

impl Maintenance {
    /// Whether every key is in its range, and `xcode_default` names `latest` or a
    /// version the group keeps an Xcode for.
    ///
    /// # Errors
    /// Why not, naming the first key, in the order of the table, that is not.
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.xcode_keep > MAX_XCODE_KEEP {
            return Err(format!("xcode_keep is over {MAX_XCODE_KEEP}"));
        }
        if let XcodeDefault::Version(version) = &self.xcode_default
            && self.xcode_keep == 0
        {
            return Err(format!(
                "xcode_default names {version}, but an xcode_keep of 0 keeps no Xcode"
            ));
        }
        if !(1..=MAX_COUNT).contains(&self.batch) {
            return Err(format!("batch is not 1 to {MAX_COUNT}"));
        }
        if self.min_serving > MAX_COUNT {
            return Err(format!("min_serving is over {MAX_COUNT}"));
        }
        if self.return_secs != 0 && !(MIN_RETURN_SECS..=MAX_RETURN_SECS).contains(&self.return_secs)
        {
            return Err(format!(
                "return_secs is not 0 or {MIN_RETURN_SECS} to {MAX_RETURN_SECS}"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "maintenance_policy_tests.rs"]
mod tests;
