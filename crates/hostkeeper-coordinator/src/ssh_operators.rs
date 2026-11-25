//! The operators file of `--ssh-operators-file` (docs/design/ssh-through-coordinator.md, section
//! 5): who may have a certificate for, and later a tunnel to, which host. Default deny: an
//! operator the file does not list, or a host outside the lists of the operator's entry, is
//! refused, and so is every operator when the file lists none.
//!
//! ```toml
//! [[operator]]
//! name = "alice"                 # a user (and each API token they own) or a tokens file token
//! classes = ["fleet", "mac-build"]  # host classes (`fleet`, `control-plane`) or host groups
//! hosts = ["node-07"]            # host ids
//! ```
//!
//! The file holds no secret. It is read at start, which refuses a file that does not parse, and
//! again at every call that signs, so removing an entry takes effect with the next call.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::tokens::TokenName;

/// The most bytes the file may have.
const MAX_BYTES: u64 = 256 * 1024;

/// The class of a host (section 5): the control plane's hosts trust a CA of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// Every host the owner did not name `--ssh-control-plane-host`.
    Fleet,
    /// A host named by `--ssh-control-plane-host`.
    ControlPlane,
}

impl Class {
    /// The class as the file spells it.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Class::Fleet => "fleet",
            Class::ControlPlane => "control-plane",
        }
    }
}

/// The host an operator asks for, as the entries are matched against it.
pub(crate) struct Target<'a> {
    pub(crate) id: &'a str,
    /// The host group of the host; empty for a host in none.
    pub(crate) group: &'a str,
    pub(crate) class: Class,
}

/// Why the file is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OperatorsError {
    /// The file could not be read, or is longer than 256 KiB.
    #[error("{}: {why}", .path.display())]
    Read { path: PathBuf, why: String },
    /// The text is not the file's TOML: a key it does not know, or a value of the wrong type.
    #[error("the operators file does not parse: {0}")]
    Parse(String),
    /// A name that is not 1 to 32 bytes of a-z, 0-9 and -.
    #[error("the operators file names an operator {0:?} that is not a token name")]
    BadName(String),
    /// A name listed twice.
    #[error("the operators file lists the operator {0:?} twice")]
    Duplicate(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    operator: Vec<Entry>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    #[serde(default)]
    classes: BTreeSet<String>,
    #[serde(default)]
    hosts: BTreeSet<String>,
}

/// The entries of the file.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Operators {
    entries: Vec<Entry>,
}

impl Operators {
    /// The entries of `text`.
    ///
    /// # Errors
    /// [`OperatorsError`] for text that is not the file.
    pub(crate) fn parse(text: &str) -> Result<Operators, OperatorsError> {
        let file: File = toml::from_str(text).map_err(|e| OperatorsError::Parse(e.to_string()))?;
        let mut seen = BTreeSet::new();
        for entry in &file.operator {
            if entry.name.parse::<TokenName>().is_err() {
                return Err(OperatorsError::BadName(entry.name.clone()));
            }
            if !seen.insert(entry.name.as_str()) {
                return Err(OperatorsError::Duplicate(entry.name.clone()));
            }
        }
        Ok(Operators {
            entries: file.operator,
        })
    }

    /// The entries of the file at `path`.
    ///
    /// # Errors
    /// [`OperatorsError`] when it cannot be read or is not the file.
    pub(crate) fn read(path: &Path) -> Result<Operators, OperatorsError> {
        let read = |why: String| OperatorsError::Read {
            path: path.to_owned(),
            why,
        };
        let meta = std::fs::metadata(path).map_err(|e| read(e.to_string()))?;
        if meta.len() > MAX_BYTES {
            return Err(read(format!("longer than {MAX_BYTES} bytes")));
        }
        let text = std::fs::read_to_string(path).map_err(|e| read(e.to_string()))?;
        Operators::parse(&text)
    }

    /// Whether the file has an entry for the operator `name`.
    pub(crate) fn lists(&self, name: &str) -> bool {
        self.entries.iter().any(|entry| entry.name == name)
    }

    /// Whether the operator `name` may reach `target`: the entry of that name lists the host's
    /// id in `hosts`, or its class or its host group in `classes`.
    pub(crate) fn permits(&self, name: &str, target: &Target<'_>) -> bool {
        self.entries.iter().any(|entry| {
            entry.name == name
                && (entry.hosts.contains(target.id)
                    || entry.classes.contains(target.class.as_str())
                    || (!target.group.is_empty() && entry.classes.contains(target.group)))
        })
    }
}

#[cfg(test)]
#[path = "ssh_operators_tests.rs"]
mod tests;
