//! The kinds of alert ([`Kind`]) and what each kind is outside its derivation: the `alert=` token
//! of its log line ([`Kind::log_token`]), its severity ([`Kind::severity`]) and its value in
//! `hostkeeper.operator.v1` ([`Kind::proto`]). The derivations are in [`crate::alerts`], one per
//! kind; [`crate::alerts::Kind`] is this type.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

/// What an alert is about, in the order `Alerts.List` answers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A service is halted.
    Halt,
    /// A host waits for an operator to approve its enrollment.
    WaitingForOperator,
    /// Two live daemons report one host's identity.
    Duplicate,
    /// A pending request claims an identity another host or request has.
    DuplicateClaimPending,
    /// A canary failed a release.
    FailedCanary,
    /// A held host runs a version marked bad.
    HeldOnBadVersion,
    /// An operator abandoned a host that may be mid-update.
    AbandonedWhileUpdating,
    /// A host has not heartbeated for the silent-host threshold.
    SilentHost,
    /// The coordinator's certificate for its hosts expires soon.
    CertNearExpiry,
    /// The store's link to its database is lost.
    StoreUnreachable,
    /// A host's trusted root is stale, missing or unparseable.
    TrustedRoot,
    /// A host's daemon failed to make sure of a host prerequisite.
    PrerequisiteFailed,
    /// The list of Xcode releases has a release newer than any the Xcode store holds.
    XcodeReleaseWaiting,
    /// An upload of a second build under a version the Xcode store holds was refused.
    XcodeVersionTaken,
    /// Hosts of a host group have updates due.
    UpdatesDue,
    /// A host failed its update or install of a service's desired version.
    FailedUpdate,
    /// A host holds a halt of a service of its own.
    HostHalt,
    /// A host's turn of a maintenance run halted.
    MaintenanceHalted,
    /// A host rebooting in its turn of a maintenance run is silent past its group's
    /// `return_secs`.
    MaintenanceNotBack,
    /// A host declined a service's desired version several times in a row for one reason.
    RepeatedDeferral,
    /// A host whose last word of a service is an update running has been silent long enough that
    /// the word holds no update slot and no rollout place ([`crate::silent`]).
    SilentWhileUpdating,
}

impl Kind {
    /// The `alert=` field of its log line. The four kinds the coordinator logged before
    /// `Alerts.List` keep the tokens they had (`canary_failed`, `exclude_abandon`,
    /// `duplicate_claim`, `halt`), so a log search keeps finding them.
    #[must_use]
    pub const fn log_token(self) -> &'static str {
        match self {
            Self::Halt => "halt",
            Self::WaitingForOperator => "waiting_for_operator",
            Self::Duplicate => "duplicate",
            Self::DuplicateClaimPending => "duplicate_claim",
            Self::FailedCanary => "canary_failed",
            Self::HeldOnBadVersion => "held_on_bad_version",
            Self::AbandonedWhileUpdating => "exclude_abandon",
            Self::SilentHost => "silent_host",
            Self::CertNearExpiry => "cert_near_expiry",
            Self::StoreUnreachable => "store_unreachable",
            Self::TrustedRoot => "trusted_root",
            Self::PrerequisiteFailed => "prerequisite_failed",
            Self::XcodeReleaseWaiting => "xcode_release_waiting",
            Self::XcodeVersionTaken => "xcode_version_taken",
            Self::UpdatesDue => "updates_due",
            Self::FailedUpdate => "failed_update",
            Self::HostHalt => "host_halt",
            Self::MaintenanceHalted => "maintenance_halted",
            Self::MaintenanceNotBack => "maintenance_not_back",
            Self::RepeatedDeferral => "repeated_deferral",
            Self::SilentWhileUpdating => "silent_while_updating",
        }
    }

    /// How soon it needs a person: a stop, or a risk now, is critical.
    #[must_use]
    pub const fn severity(self) -> proto::AlertSeverity {
        match self {
            Self::Halt
            | Self::Duplicate
            | Self::FailedCanary
            | Self::StoreUnreachable
            | Self::FailedUpdate
            | Self::HostHalt
            | Self::MaintenanceHalted
            | Self::MaintenanceNotBack
            | Self::RepeatedDeferral
            | Self::SilentWhileUpdating => proto::AlertSeverity::Critical,
            Self::WaitingForOperator
            | Self::DuplicateClaimPending
            | Self::HeldOnBadVersion
            | Self::AbandonedWhileUpdating
            | Self::SilentHost
            | Self::CertNearExpiry
            | Self::TrustedRoot
            | Self::PrerequisiteFailed
            | Self::XcodeReleaseWaiting
            | Self::XcodeVersionTaken
            | Self::UpdatesDue => proto::AlertSeverity::Warning,
        }
    }

    /// Its value in `hostkeeper.operator.v1`.
    #[must_use]
    pub const fn proto(self) -> proto::AlertKind {
        match self {
            Self::Halt => proto::AlertKind::Halt,
            Self::WaitingForOperator => proto::AlertKind::WaitingForOperator,
            Self::Duplicate => proto::AlertKind::Duplicate,
            Self::DuplicateClaimPending => proto::AlertKind::DuplicateClaimPending,
            Self::FailedCanary => proto::AlertKind::FailedCanary,
            Self::HeldOnBadVersion => proto::AlertKind::HeldOnBadVersion,
            Self::AbandonedWhileUpdating => proto::AlertKind::AbandonedWhileUpdating,
            Self::SilentHost => proto::AlertKind::SilentHost,
            Self::CertNearExpiry => proto::AlertKind::CertNearExpiry,
            Self::StoreUnreachable => proto::AlertKind::StoreUnreachable,
            Self::TrustedRoot => proto::AlertKind::TrustedRoot,
            Self::PrerequisiteFailed => proto::AlertKind::PrerequisiteFailed,
            Self::XcodeReleaseWaiting => proto::AlertKind::XcodeReleaseWaiting,
            Self::XcodeVersionTaken => proto::AlertKind::XcodeVersionTaken,
            Self::UpdatesDue => proto::AlertKind::UpdatesDue,
            Self::FailedUpdate => proto::AlertKind::FailedUpdate,
            Self::HostHalt => proto::AlertKind::HostHalt,
            Self::MaintenanceHalted => proto::AlertKind::MaintenanceHalted,
            Self::MaintenanceNotBack => proto::AlertKind::MaintenanceNotBack,
            Self::RepeatedDeferral => proto::AlertKind::RepeatedDeferral,
            Self::SilentWhileUpdating => proto::AlertKind::SilentWhileUpdating,
        }
    }
}

#[cfg(test)]
#[path = "alerts_kind_tests.rs"]
mod tests;
