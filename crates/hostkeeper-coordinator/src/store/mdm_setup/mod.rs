//! The tables of the MDM setup (`docs/design/coordinator-mdm.md`, section 4.9), behind one port,
//! [`SetupStore`]: `mdm_secrets`, the sealed values the operator uploads or the coordinator makes,
//! and `mdm_settings`, the settings of the setup page.
//!
//! The store keeps what it is given and judges none of it: a secret arrives sealed
//! ([`crate::mdm_secrets`]), so neither store ever holds or sees plaintext, and a setting is a
//! name and a text its caller ([`crate::mdm_settings`]) has already checked. What a store does
//! keep by itself is the numbering of versions: the newest version of a kind is the one in use,
//! and a row added must be the next one, so two writers cannot both think they made version 3.
//!
//! Two stores implement it and one suite ([`conformance_tests`]) runs against both:
//! [`files::FileSetup`], one file per family in a directory, each replaced whole, and
//! [`crate::store::postgres::mdm_setup::PgSetup`], the tables of migration `0012`. A volatile
//! store, [`files::FileSetup::volatile`], is the same memory with no file, for a coordinator built
//! without a store.

use std::collections::BTreeMap;

pub(crate) mod files;
pub(crate) mod state;

#[cfg(test)]
#[path = "conformance_tests.rs"]
pub(crate) mod conformance_tests;
#[cfg(test)]
#[path = "files_tests.rs"]
mod files_tests;

/// Why a call of the setup store failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SetupError {
    /// The files or the database could not be read or written; the change is not made.
    #[error("the MDM setup store failed: {0}")]
    Store(String),
    /// The version of the row added is not the one after the newest of its kind.
    #[error("the next version of {kind} is {expected}")]
    Version {
        /// The kind of the row.
        kind: String,
        /// The version a row of this kind must have now.
        expected: u32,
    },
}

/// One version of one kind of secret, as the store holds it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SecretRow {
    /// The kind: lower-case letters, digits and `_`, 1 to 40 of them.
    pub(crate) kind: String,
    /// 1 for the first of a kind, and one more for each replacement.
    pub(crate) version: u32,
    /// The nonce followed by the ciphertext and its tag: no plaintext reaches a store. The `Debug`
    /// of the row names the length only.
    #[serde(with = "hex_bytes")]
    pub(crate) sealed: Sealed,
    /// The SHA-256 of the plaintext in hex: what the status shows in place of the value.
    pub(crate) fingerprint: String,
    /// When the value stops being good, when it says so.
    pub(crate) not_after_unix: Option<i64>,
    /// When this version was stored.
    pub(crate) created_unix: i64,
    /// When a newer version took its place; `None` for the one in use.
    pub(crate) superseded_unix: Option<i64>,
}

/// Sealed bytes. Its `Debug` names the length only, so that a row in a log line or a failed
/// assertion shows no bytes.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Sealed(pub(crate) Vec<u8>);

impl std::fmt::Debug for Sealed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sealed({} bytes)", self.0.len())
    }
}

/// The text of sealed bytes in a file: lower-case hex.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    use super::Sealed;

    pub(super) fn serialize<S: Serializer>(sealed: &Sealed, to: S) -> Result<S::Ok, S::Error> {
        to.serialize_str(&hex::encode(&sealed.0))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Sealed, D::Error> {
        let text = String::deserialize(from)?;
        hex::decode(text)
            .map(Sealed)
            .map_err(serde::de::Error::custom)
    }
}

/// The MDM setup tables.
pub(crate) trait SetupStore: Send + Sync {
    /// Stores `row`, the version after the newest of its kind, and marks that newest one
    /// superseded at `row.created_unix`; the row is stored with no `superseded_unix`.
    ///
    /// # Errors
    /// [`SetupError::Version`]: `row.version` is not the next one: nothing changes.
    /// [`SetupError::Store`]: the row is not durable.
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError>;

    /// Every version of every kind, by kind and then version.
    ///
    /// # Errors
    /// [`SetupError::Store`]: the rows cannot be read.
    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError>;

    /// Deletes the versions superseded at or before `superseded_unix`: the grace of an upload
    /// that was replaced is over. A version in use is never deleted. Answers how many went.
    ///
    /// # Errors
    /// [`SetupError::Store`]: the change is not durable.
    fn prune_superseded(&self, superseded_unix: i64) -> Result<u64, SetupError>;

    /// The settings that were set, by name.
    ///
    /// # Errors
    /// [`SetupError::Store`]: the rows cannot be read.
    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError>;

    /// Sets the setting `name` to `value`, replacing what it was.
    ///
    /// # Errors
    /// [`SetupError::Store`]: the change is not durable.
    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError>;
}
