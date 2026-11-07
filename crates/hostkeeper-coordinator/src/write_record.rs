//! How the store records a service's writes (`docs/design/coordinator-storage.md`, section 3.2):
//! the generation, one more for every accepted write of the service's desired state, a write of
//! the same version included; the write id of the last write; and the version and minimums the
//! last change of version replaced. `Releases.SetDesired` answers it (`SetDesiredResponse.record`)
//! and `Releases.Converge` shows it (`Convergence.record`), so a writer that lost an answer can
//! tell whether its write landed, and a writer that names `if_generation` writes only over the
//! state it saw.
//!
//! Only a backend that keeps them records them ([`crate::store::Backend::keeps_write_records`]):
//! the Postgres store does, in the columns migration `0002` adds to `desired`. The desired-state
//! file keeps neither, so over the files a set that names `if_generation` or a write id is refused
//! `invalid_request` and an identical set still writes nothing.

use hostkeeper_wire::version::Version;

use crate::desired::{Desired, DesiredError, RoleLimits, StoreError};

/// The most characters a write id has.
pub(crate) const MAX_WRITE_ID: usize = 128;

/// A writer's own id for one write: 1 to [`MAX_WRITE_ID`] visible ASCII characters (`!` to `~`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WriteId(String);

/// A write id that breaks its rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a write id is 1 to 128 visible ASCII characters")]
pub(crate) struct BadWriteId;

impl WriteId {
    /// `text` as a write id.
    ///
    /// # Errors
    /// [`BadWriteId`]: it is empty, longer than [`MAX_WRITE_ID`], or holds a character that is
    /// not visible ASCII (a space among them).
    pub(crate) fn parse(text: &str) -> Result<WriteId, BadWriteId> {
        let visible = text.bytes().all(|b| b.is_ascii_graphic());
        if text.is_empty() || text.len() > MAX_WRITE_ID || !visible {
            return Err(BadWriteId);
        }
        Ok(WriteId(text.to_owned()))
    }

    /// The id as the writer sent it.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The version and minimums a change of version replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Replaced {
    /// The version before the change.
    pub(crate) version: Version,
    /// Its minimums, as they were held.
    pub(crate) min_up: RoleLimits,
}

/// How the store records a service's writes. The default (generation 0, nothing else) is the
/// record of a service never written, and is not stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WriteRecord {
    /// One more for every accepted write; 0 before the first.
    pub(crate) generation: u64,
    /// The version and minimums the last change of version replaced; none until a second
    /// version is set.
    pub(crate) replaced: Option<Replaced>,
    /// The write id of the last write; none when it had none.
    pub(crate) write_id: Option<WriteId>,
}

impl WriteRecord {
    /// The record after one more accepted write of `to`, over `from` (the state before; none
    /// before the first), whose id is `id`. A change of version replaces `from`'s version and
    /// minimums; a write of the same version keeps what the last change of version replaced.
    #[must_use]
    pub(crate) fn after(
        &self,
        from: Option<&Desired>,
        to: &Desired,
        id: Option<WriteId>,
    ) -> WriteRecord {
        let replaced = match from {
            Some(old) if old.version != to.version => Some(Replaced {
                version: old.version.clone(),
                min_up: old.min_up.clone(),
            }),
            _ => self.replaced.clone(),
        };
        WriteRecord {
            generation: self.generation.saturating_add(1),
            replaced,
            write_id: id,
        }
    }
}

/// What a write asks of the store beside its state: a precondition on the generation, and the
/// writer's id for it. The default asks nothing, as every write but `Releases.SetDesired`'s does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WriteAsk {
    /// Write only if the service's generation is this one now.
    pub(crate) if_generation: Option<u64>,
    /// The writer's id for this write.
    pub(crate) write_id: Option<WriteId>,
}

impl WriteAsk {
    /// Whether it asks for anything only a store that keeps write records can honour.
    #[must_use]
    pub(crate) fn needs_records(&self) -> bool {
        self.if_generation.is_some() || self.write_id.is_some()
    }

    /// Whether a service whose generation is `now` may be written: always, unless the write
    /// names another generation.
    ///
    /// # Errors
    /// [`WriteError::Conflict`]: it names a generation that is not `now`.
    pub(crate) fn admits(&self, now: u64) -> Result<(), WriteError> {
        match self.if_generation {
            Some(asked) if asked != now => Err(WriteError::Conflict { asked, now }),
            _ => Ok(()),
        }
    }
}

/// Why a write that asked for something was not made.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WriteError {
    /// `if_generation` is not the service's generation now: nothing changed.
    #[error("generation_conflict: the service's generation is {now}, not {asked}")]
    Conflict {
        /// The generation the write named.
        asked: u64,
        /// The generation now.
        now: u64,
    },
    /// The write names `if_generation` or a write id, and the store keeps no write records (the
    /// desired-state file): nothing changed.
    #[error("the desired-state store keeps no generation or write id")]
    Unrecorded,
    /// The state could not be stored.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<WriteError> for crate::service::SetError {
    fn from(why: WriteError) -> Self {
        match why {
            WriteError::Conflict { asked, now } => {
                Self::Invalid(DesiredError::GenerationConflict { asked, now })
            }
            WriteError::Unrecorded => Self::Invalid(DesiredError::Unrecorded),
            WriteError::Store(e) => Self::Store(e),
        }
    }
}

#[cfg(test)]
#[path = "write_record_tests.rs"]
mod tests;
