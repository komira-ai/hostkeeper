//! The coordinator's hold of the MDM setup (`docs/design/coordinator-mdm.md`, section 4.9): the
//! secrets sealed under the master key, and the settings, over a [`SetupStore`].
//!
//! Plaintext enters here as a [`Secret`] and leaves, sealed, to the store; it comes back only
//! through [`MdmSetup::open`], to the code that uses it. Nothing here prints a value, and what a
//! caller may show of a secret is its [`SecretInfo`]: kind, version, fingerprint, expiry and times.
//!
//! One writer at a time makes a secret: the version a row takes is the one after the newest, which
//! the store keeps, and the seal binds that version, so the next version is read, sealed for and
//! stored under one lock.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};

use crate::enrollment::Random;
use crate::mdm_acme::status::TlsStatus;
use crate::mdm_secrets::{MasterKey, SealError, Secret, SecretKind};
use crate::mdm_settings;
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{Sealed, SecretRow, SetupError, SetupStore};

/// Why a setup call failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SetupFailure {
    /// No master key is held: `--mdm-secrets-key-file` was not given.
    #[error("no master key is held: --mdm-secrets-key-file is not given")]
    NoKey,
    /// The store failed.
    #[error(transparent)]
    Store(#[from] SetupStoreError),
    /// A value could not be sealed, or a sealed one did not open.
    #[error(transparent)]
    Seal(#[from] SealError),
    /// The system gave no random bytes for a nonce.
    #[error("the system gave no random bytes for a nonce")]
    Random,
}

/// The error of the setup store, as the public API says it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SetupStoreError(String);

impl From<SetupError> for SetupFailure {
    fn from(why: SetupError) -> Self {
        Self::Store(SetupStoreError(why.to_string()))
    }
}

/// What is shown of a stored secret: nothing of its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretInfo {
    /// The kind.
    pub kind: SecretKind,
    /// The version.
    pub version: u32,
    /// The SHA-256 of the value in hex.
    pub fingerprint: String,
    /// When the value stops being good, when it says so.
    pub not_after_unix: Option<i64>,
    /// When this version was stored.
    pub created_unix: i64,
    /// When a newer version took its place; none for the one in use.
    pub superseded_unix: Option<i64>,
}

/// The MDM setup: a store, and the master key when one is held.
pub struct MdmSetup {
    store: Arc<dyn SetupStore>,
    key: Option<MasterKey>,
    random: Random,
    writing: Mutex<()>,
    tls: Arc<TlsStatus>,
}

impl MdmSetup {
    /// The setup over `store`, sealing under `key` (none: no secret is stored or opened), with
    /// nonces from `random`.
    pub(crate) fn new(store: Arc<dyn SetupStore>, key: Option<MasterKey>, random: Random) -> Self {
        Self {
            store,
            key,
            random,
            writing: Mutex::new(()),
            tls: Arc::default(),
        }
    }

    /// What the renewal of the certificate for the MDM name reports to the alerts.
    #[must_use]
    pub fn tls_status(&self) -> &Arc<TlsStatus> {
        &self.tls
    }

    /// A setup in memory alone and without a key: for a coordinator built without a store.
    #[must_use]
    pub fn volatile() -> Self {
        Self::new(
            Arc::new(FileSetup::volatile()),
            None,
            crate::enrollment::os_random(),
        )
    }

    /// The same setup holding `key`.
    #[must_use]
    pub fn with_key(mut self, key: MasterKey) -> Self {
        self.key = Some(key);
        self
    }

    /// The same setup with `random` as its source of nonces.
    #[must_use]
    pub fn with_random(mut self, random: Random) -> Self {
        self.random = random;
        self
    }

    /// Whether a master key is held, which storing a secret needs.
    #[must_use]
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// Whether the store holds any sealed secret.
    ///
    /// # Errors
    /// [`SetupFailure::Store`]: the rows cannot be read.
    pub(crate) fn holds_secrets(store: &dyn SetupStore) -> Result<bool, SetupFailure> {
        Ok(!store.secrets()?.is_empty())
    }

    /// Seals `material` as the next version of `kind` and stores it, at `now_unix`; the version it
    /// replaces is kept for `secret_grace_days`, and versions replaced longer ago are deleted.
    ///
    /// # Errors
    /// [`SetupFailure::NoKey`]: no master key is held. [`SetupFailure::Random`], `Seal` and
    /// `Store`: nothing is stored.
    pub fn put_secret(
        &self,
        kind: SecretKind,
        material: &Secret,
        not_after_unix: Option<i64>,
        now_unix: i64,
    ) -> Result<SecretInfo, SetupFailure> {
        let key = self.key.as_ref().ok_or(SetupFailure::NoKey)?;
        let _one = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        let version = self
            .store
            .secrets()?
            .iter()
            .filter(|row| row.kind == kind.as_str())
            .map(|row| row.version)
            .max()
            .map_or(1, |newest| newest + 1);
        let mut nonce = [0_u8; 12];
        if !(self.random)(&mut nonce) {
            return Err(SetupFailure::Random);
        }
        let row = SecretRow {
            kind: kind.as_str().to_owned(),
            version,
            sealed: Sealed(key.seal(kind.as_str(), version, material, nonce)?),
            fingerprint: fingerprint(material),
            not_after_unix,
            created_unix: now_unix,
            superseded_unix: None,
        };
        self.store.put_secret(&row)?;
        // The new version is stored and in use: from here on the upload is done, and a failure
        // only leaves replaced versions for the next upload to prune.
        let days = match self.store.settings() {
            Ok(set) => mdm_settings::days_of(&set, "secret_grace_days", 7),
            Err(why) => {
                tracing::error!(%why, "the settings could not be read: the default grace is used");
                7
            }
        };
        let grace = i64::from(days).saturating_mul(86_400);
        if let Err(why) = self.store.prune_superseded(now_unix.saturating_sub(grace)) {
            tracing::error!(%why, "replaced secrets could not be pruned: the next upload prunes");
        }
        Ok(info_of(kind, &row))
    }

    /// What is shown of every version of every secret held, by kind and version. A row of a kind
    /// this release does not know is not listed.
    ///
    /// # Errors
    /// [`SetupFailure::Store`]: the rows cannot be read.
    pub fn list_secrets(&self) -> Result<Vec<SecretInfo>, SetupFailure> {
        Ok(self
            .store
            .secrets()?
            .iter()
            .filter_map(|row| SecretKind::parse(&row.kind).map(|kind| info_of(kind, row)))
            .collect())
    }

    /// The secret of `kind` in use, opened.
    ///
    /// # Errors
    /// [`SetupFailure::NoKey`]; `Seal` ([`SealError::Open`]) when the row does not open under the
    /// key for its kind and version; `Store`.
    pub fn open(&self, kind: SecretKind) -> Result<Option<Secret>, SetupFailure> {
        let key = self.key.as_ref().ok_or(SetupFailure::NoKey)?;
        let rows = self.store.secrets()?;
        let current = rows
            .iter()
            .filter(|row| row.kind == kind.as_str())
            .max_by_key(|row| row.version);
        current
            .map(|row| {
                key.open(&row.kind, row.version, &row.sealed.0)
                    .map_err(SetupFailure::from)
            })
            .transpose()
    }

    /// Every version of `kind` still held, newest first, each opened. Replaced versions are held
    /// for `secret_grace_days`.
    ///
    /// # Errors
    /// As [`Self::open`], for any version.
    pub fn open_held(&self, kind: SecretKind) -> Result<Vec<Secret>, SetupFailure> {
        let key = self.key.as_ref().ok_or(SetupFailure::NoKey)?;
        let mut rows: Vec<_> = self
            .store
            .secrets()?
            .into_iter()
            .filter(|row| row.kind == kind.as_str())
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.version));
        rows.iter()
            .map(|row| {
                key.open(&row.kind, row.version, &row.sealed.0)
                    .map_err(SetupFailure::from)
            })
            .collect()
    }

    /// The kinds whose secret in use the key does not open: the check of a start that holds a key
    /// and sealed rows, which refuses to start on a key of another store.
    ///
    /// # Errors
    /// [`SetupFailure::NoKey`]; `Store`.
    pub fn unopened(&self) -> Result<Vec<SecretKind>, SetupFailure> {
        let mut bad = Vec::new();
        for kind in SecretKind::ALL {
            match self.open(kind) {
                Err(SetupFailure::Seal(_)) => bad.push(kind),
                Err(other) => return Err(other),
                Ok(_) => {}
            }
        }
        Ok(bad)
    }

    /// The settings: every one this release knows, with its value and whether that is its
    /// default, and a name the store holds that this release does not know is not listed.
    ///
    /// # Errors
    /// [`SetupFailure::Store`]: the rows cannot be read.
    pub fn list_settings(&self) -> Result<Vec<(&'static str, String, bool)>, SetupFailure> {
        let set = self.store.settings()?;
        Ok(mdm_settings::SETTINGS
            .iter()
            .map(|setting| {
                (
                    setting.name,
                    mdm_settings::value_of(&set, setting).to_owned(),
                    !set.contains_key(setting.name),
                )
            })
            .collect())
    }

    /// Sets `name` to `value`, which the caller has judged with [`mdm_settings::Setting::check`].
    ///
    /// # Errors
    /// [`SetupFailure::Store`]: it is not durable.
    pub fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupFailure> {
        Ok(self.store.set_setting(name, value)?)
    }

    /// The settings that were set, by name.
    ///
    /// # Errors
    /// [`SetupFailure::Store`]: the rows cannot be read.
    pub fn settings(&self) -> Result<BTreeMap<String, String>, SetupFailure> {
        Ok(self.store.settings()?)
    }
}

/// The SHA-256 of `material` in hex.
pub(crate) fn fingerprint(material: &Secret) -> String {
    hex::encode(Sha256::digest(material.expose()))
}

fn info_of(kind: SecretKind, row: &SecretRow) -> SecretInfo {
    SecretInfo {
        kind,
        version: row.version,
        fingerprint: row.fingerprint.clone(),
        not_after_unix: row.not_after_unix,
        created_unix: row.created_unix,
        superseded_unix: row.superseded_unix,
    }
}

#[cfg(test)]
#[path = "mdm_setup_tests.rs"]
mod tests;
