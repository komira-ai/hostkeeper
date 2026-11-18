//! The console operators file (`--console-operators-file`, `docs/design/console-broker.md`
//! decision D9): which operators, by the name of their token, may use the console.
//!
//! ```toml
//! [[operator]]
//! name = "alice"
//!
//! [[operator]]
//! name = "bob"
//! power = false
//! ```
//!
//! Each `[[operator]]` names a token (1 to 32 bytes of `a-z`, `0-9` and `-`, the characters of
//! the tokens file's names) once. A name no token can have (`coordinator`, `ci` and the other
//! names the audit keeps for itself) is taken and grants nothing, as any name no token has. `power` is true unless the entry says false: an operator whose `power` is false
//! may open, list and close consoles but not call `Power` or `SetBootDevice`. A name that is not
//! in the file has no console access, and no file at all ([`Operators::nobody`]) means nobody
//! has. A file with no entry is read as that too. The tokens file is not changed: a key added
//! there would be refused by the release before (rule B8).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::file::{ReadError, read_text};

/// The flag that names the file, which error texts name instead of its path.
pub const FLAG: &str = "--console-operators-file";

/// The longest file read.
pub const MAX_FILE_BYTES: u64 = 1 << 16;

/// The longest operator name, as a token's name is.
const MAX_NAME_BYTES: usize = 32;

/// Why the file is refused. The text names the flag, never the path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OperatorsError {
    /// The file could not be read.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// The text is not TOML of `[[operator]]` entries with `name` and `power`.
    #[error("--console-operators-file: {0}")]
    Parse(String),
    /// An entry's name is no token name.
    #[error(
        "--console-operators-file: operator {index}: the name {name:?} is not 1 to 32 bytes of a-z, 0-9 and -"
    )]
    Name {
        /// The entry's place in the file, from 1.
        index: usize,
        /// The name as written.
        name: String,
    },
    /// Two entries name one operator.
    #[error("--console-operators-file: the operator {0} is named twice")]
    Repeated(String),
}

/// What a console call needs of the operator who makes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// To open, list or close a console.
    Console,
    /// To change a host's power or boot device (`Power`, `SetBootDevice`), which also needs
    /// [`Need::Console`].
    Power,
}

/// Why an operator may not make a console call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// The name is not in the file, or there is no file (`no_console_access`).
    NoConsoleAccess,
    /// The name is in the file with `power = false`, and the call needs power
    /// (`power_not_permitted`).
    PowerNotPermitted,
}

/// The operators of the file, by name: whether each may change power.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Operators {
    power: BTreeMap<String, bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    operator: Vec<RawOperator>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOperator {
    name: String,
    #[serde(default = "allowed")]
    power: bool,
}

/// `power`'s default: an operator named in the file may change power unless it says otherwise.
fn allowed() -> bool {
    true
}

impl Operators {
    /// No operator: what the coordinator holds with no file.
    #[must_use]
    pub fn nobody() -> Self {
        Self::default()
    }

    /// The operators of the file's text `text`.
    ///
    /// # Errors
    /// [`OperatorsError`]: the text is not the file's TOML, names an operator that is no token
    /// name, or names one twice.
    pub fn parse(text: &str) -> Result<Self, OperatorsError> {
        let file: RawFile =
            toml::from_str(text).map_err(|e| OperatorsError::Parse(e.message().to_owned()))?;
        let mut power = BTreeMap::new();
        for (index, entry) in file.operator.into_iter().enumerate() {
            if !is_name(&entry.name) {
                return Err(OperatorsError::Name {
                    index: index + 1,
                    name: entry.name,
                });
            }
            if power.contains_key(&entry.name) {
                return Err(OperatorsError::Repeated(entry.name));
            }
            power.insert(entry.name, entry.power);
        }
        Ok(Self { power })
    }

    /// The operators of the file at `path`.
    ///
    /// # Errors
    /// [`OperatorsError`]: the file cannot be read, is over [`MAX_FILE_BYTES`] or not UTF-8, or
    /// its text is refused as [`Self::parse`] refuses it.
    pub fn read(path: &Path) -> Result<Self, OperatorsError> {
        Self::parse(&read_text(path, FLAG, MAX_FILE_BYTES)?)
    }

    /// Whether the operator `name` may make a call that needs `need`.
    ///
    /// # Errors
    /// [`Denied`]: the name is not in the file, or the call needs power and its entry does not
    /// allow it.
    pub fn check(&self, name: &str, need: Need) -> Result<(), Denied> {
        match (self.power.get(name), need) {
            (None, _) => Err(Denied::NoConsoleAccess),
            (Some(false), Need::Power) => Err(Denied::PowerNotPermitted),
            (Some(_), _) => Ok(()),
        }
    }
}

/// Whether `text` is a token's name: 1 to 32 bytes of `a-z`, `0-9` and `-`.
fn is_name(text: &str) -> bool {
    (1..=MAX_NAME_BYTES).contains(&text.len())
        && text
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[cfg(test)]
#[path = "operators_tests.rs"]
mod tests;
