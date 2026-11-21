//! The work of one kind of maintenance ([`MaintenanceKind`]), what its steps answer, and the stub
//! ([`Unsupported`]) a kind is until its platform's work is written.

use super::params::{Kind, MaintainParams};

/// The reason token of a kind this host has no work for, or whose work is not written yet.
pub const REASON_UNSUPPORTED_KIND: &str = "unsupported_kind";

/// What a kind's `cutover` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cutover {
    /// The kind is applied, and the host now runs `version` of it (the macOS build, the Xcode
    /// version, the kernel).
    Applied { version: String },
    /// The kind takes effect only after a reboot: the hook records that the kind's reboot is under
    /// way, then reboots, by the kind's own [`MaintenanceKind::restart`] when it has one (a Mac's
    /// update, whose install restarts the Mac: the kind is then ready to install, not installed),
    /// else by the host's [`super::Rebooter`] (the kind is installed). The next `cutover`, in the
    /// next boot, asks the kind for its version ([`MaintenanceKind::after_reboot`]).
    NeedsReboot,
}

/// Why a kind's step did not succeed: deferred (nothing was touched; exit `75`) or failed. The
/// reason is a token of the code (`[a-z0-9_]`, at most 64 bytes); the detail is a short text the
/// hook bounds, which never holds a password or a parameter's value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KindError {
    /// The step touched nothing and may be tried again later.
    #[error("deferred, {reason}: {detail}")]
    Deferred {
        reason: &'static str,
        detail: String,
    },
    /// The step failed.
    #[error("failed, {reason}: {detail}")]
    Failed {
        reason: &'static str,
        detail: String,
    },
}

impl KindError {
    /// The failure of a kind this host has no work for.
    pub fn unsupported(kind: Kind) -> KindError {
        KindError::Failed {
            reason: REASON_UNSUPPORTED_KIND,
            detail: format!("{kind} is not supported on this host"),
        }
    }
}

/// The work of one kind of maintenance on one platform (the design's section 5.2, step 1, 3 and
/// 4). Every method gets the run's parameters; none is given a password, which a kind that needs
/// one reads itself and passes only on a program's standard input.
pub trait MaintenanceKind {
    /// Stage, while the host serves: check the free space and download what can be downloaded.
    /// Touches nothing the host runs; a shortfall is [`KindError::Deferred`].
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError>;
    /// Apply, on the drained host. Run once per run: the hook does not call it again once the
    /// kind is recorded applied or its reboot under way.
    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError>;
    /// The reboot a [`Cutover::NeedsReboot`] asked for, when the kind makes it itself: run by the
    /// hook only after it recorded the kind's reboot under way, and again in place of a reboot
    /// asked for in the boot the host still runs. A Mac's update restarts the Mac as it installs
    /// (the design's section 5.5), so its install runs here. `Ok` once the restart is under way.
    /// `None`, the default: the kind has no restart of its own, and the host's
    /// [`super::Rebooter`] makes it.
    fn restart(&self, _params: &MaintainParams) -> Option<Result<(), KindError>> {
        None
    }
    /// After the reboot a [`Cutover::NeedsReboot`] asked for: the version the host now runs, or
    /// why the kind did not take.
    fn after_reboot(&self, params: &MaintainParams) -> Result<String, KindError>;
    /// Check, before the host is put back: what was due of the kind is no longer due.
    fn verify(&self, params: &MaintainParams) -> Result<(), KindError>;
}

/// A kind whose work on this platform is not written yet: every step fails
/// [`REASON_UNSUPPORTED_KIND`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unsupported(pub Kind);

impl MaintenanceKind for Unsupported {
    fn prepare(&self, _params: &MaintainParams) -> Result<(), KindError> {
        Err(KindError::unsupported(self.0))
    }

    fn cutover(&self, _params: &MaintainParams) -> Result<Cutover, KindError> {
        Err(KindError::unsupported(self.0))
    }

    fn after_reboot(&self, _params: &MaintainParams) -> Result<String, KindError> {
        Err(KindError::unsupported(self.0))
    }

    fn verify(&self, _params: &MaintainParams) -> Result<(), KindError> {
        Err(KindError::unsupported(self.0))
    }
}
