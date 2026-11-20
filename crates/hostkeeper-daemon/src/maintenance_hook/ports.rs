//! The ports of the maintenance hook: the run record it keeps each kind's progress in
//! ([`RunRecord`]) and the reboot it asks for ([`Rebooter`]). [`Unwired`] stands in for the run
//! record where the hook has no directory to keep it in (parameters on standard input) and for the
//! reboot until each platform's reboot is written.

use hostkeeper_steps::BootId;

use super::params::Kind;

/// Where one kind of a run stands, as the run record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindState {
    /// Not applied, and no reboot of it under way.
    Pending,
    /// Installed; the reboot it needs was asked for in the boot `boot`.
    RebootUnderWay { boot: BootId },
    /// Applied.
    Applied,
}

/// Why the run record could not be read or written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The record failed; the text says where and how, never a parameter's value.
    #[error("the run record failed: {0}")]
    Failed(String),
    /// This host has no run record yet.
    #[error("this host keeps no run record yet")]
    NotWired,
}

/// The run record's part the hook keeps: each kind's progress, durable before the call returns,
/// so that a reboot or a crash after it finds it.
pub trait RunRecord {
    /// Where `kind` of the run `run_id` stands.
    fn state(&self, run_id: &str, kind: Kind) -> Result<KindState, RecordError>;
    /// Records that the reboot `kind` needs is under way, asked for in the boot `boot`.
    fn reboot_under_way(&self, run_id: &str, kind: Kind, boot: &BootId) -> Result<(), RecordError>;
    /// Records `kind` applied, the host running `version` of it.
    fn applied(&self, run_id: &str, kind: Kind, version: &str) -> Result<(), RecordError>;
}

/// Why the reboot could not be asked for.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RebootError {
    /// The platform's reboot failed; the text names the program and its exit.
    #[error("the reboot failed: {0}")]
    Failed(String),
    /// This host has no reboot wired yet.
    #[error("this host has no reboot wired yet")]
    NotWired,
}

/// The host's reboot: `systemctl reboot` or OpenRC's `reboot` on Linux, the Mac's own. It returns
/// once the reboot is asked for; the hook then ends, and the next `cutover` runs in the next boot.
pub trait Rebooter {
    /// Asks the host to reboot.
    fn reboot(&self) -> Result<(), RebootError>;
}

/// The run record of a hook with no directory to keep it in and the reboot of a host that has none
/// yet: the record holds nothing (every
/// kind [`KindState::Pending`]) and refuses every write, and the reboot is refused, each
/// [`RecordError::NotWired`] or [`RebootError::NotWired`]. With every kind a stub, no step of the
/// hook gets as far as a write or a reboot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unwired;

impl RunRecord for Unwired {
    fn state(&self, _run_id: &str, _kind: Kind) -> Result<KindState, RecordError> {
        Ok(KindState::Pending)
    }

    fn reboot_under_way(
        &self,
        _run_id: &str,
        _kind: Kind,
        _boot: &BootId,
    ) -> Result<(), RecordError> {
        Err(RecordError::NotWired)
    }

    fn applied(&self, _run_id: &str, _kind: Kind, _version: &str) -> Result<(), RecordError> {
        Err(RecordError::NotWired)
    }
}

impl Rebooter for Unwired {
    fn reboot(&self) -> Result<(), RebootError> {
        Err(RebootError::NotWired)
    }
}
