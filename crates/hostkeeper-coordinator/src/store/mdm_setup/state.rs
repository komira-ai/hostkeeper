//! The setup tables as memory: the rules of [`SetupStore`](super::SetupStore) written once over
//! a list and a map. [`FileSetup`](super::files::FileSetup) makes each call on a copy and writes
//! the families that changed.

use std::collections::BTreeMap;

use super::{SecretRow, SetupError};

/// The rows of both families.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SetupState {
    /// By kind, then version.
    pub(crate) secrets: Vec<SecretRow>,
    /// By name.
    pub(crate) settings: BTreeMap<String, String>,
}

impl SetupState {
    /// The version a new row of `kind` must have.
    fn next_version(&self, kind: &str) -> u32 {
        self.secrets
            .iter()
            .filter(|row| row.kind == kind)
            .map(|row| row.version)
            .max()
            .map_or(1, |newest| newest.saturating_add(1))
    }

    /// [`SetupStore::put_secret`](super::SetupStore::put_secret).
    pub(crate) fn put_secret(&mut self, row: &SecretRow) -> Result<(), SetupError> {
        let expected = self.next_version(&row.kind);
        if row.version != expected {
            return Err(SetupError::Version {
                kind: row.kind.clone(),
                expected,
            });
        }
        for held in self
            .secrets
            .iter_mut()
            .filter(|held| held.kind == row.kind && held.superseded_unix.is_none())
        {
            held.superseded_unix = Some(row.created_unix);
        }
        self.secrets.push(SecretRow {
            superseded_unix: None,
            ..row.clone()
        });
        self.secrets
            .sort_by(|a, b| (&a.kind, a.version).cmp(&(&b.kind, b.version)));
        Ok(())
    }

    /// [`SetupStore::prune_superseded`](super::SetupStore::prune_superseded).
    pub(crate) fn prune_superseded(&mut self, superseded_unix: i64) -> u64 {
        let before = self.secrets.len();
        self.secrets
            .retain(|row| row.superseded_unix.is_none_or(|at| at > superseded_unix));
        (before - self.secrets.len()) as u64
    }
}
