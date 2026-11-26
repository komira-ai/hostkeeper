use std::collections::BTreeSet;

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::Kind;

/// Every kind, in the order `Alerts.List` answers them.
const ALL: [Kind; 20] = [
    Kind::Halt,
    Kind::WaitingForOperator,
    Kind::Duplicate,
    Kind::DuplicateClaimPending,
    Kind::FailedCanary,
    Kind::HeldOnBadVersion,
    Kind::AbandonedWhileUpdating,
    Kind::SilentHost,
    Kind::CertNearExpiry,
    Kind::StoreUnreachable,
    Kind::TrustedRoot,
    Kind::PrerequisiteFailed,
    Kind::XcodeReleaseWaiting,
    Kind::XcodeVersionTaken,
    Kind::UpdatesDue,
    Kind::FailedUpdate,
    Kind::HostHalt,
    Kind::MaintenanceHalted,
    Kind::MaintenanceNotBack,
    Kind::RepeatedDeferral,
];

/// Where `kind` is in [`ALL`]. The match has no wildcard, so a kind added to [`Kind`] does not
/// build until it is listed here, and so in [`ALL`] and in the table below.
const fn listed(kind: Kind) -> usize {
    match kind {
        Kind::Halt => 0,
        Kind::WaitingForOperator => 1,
        Kind::Duplicate => 2,
        Kind::DuplicateClaimPending => 3,
        Kind::FailedCanary => 4,
        Kind::HeldOnBadVersion => 5,
        Kind::AbandonedWhileUpdating => 6,
        Kind::SilentHost => 7,
        Kind::CertNearExpiry => 8,
        Kind::StoreUnreachable => 9,
        Kind::TrustedRoot => 10,
        Kind::PrerequisiteFailed => 11,
        Kind::XcodeReleaseWaiting => 12,
        Kind::XcodeVersionTaken => 13,
        Kind::UpdatesDue => 14,
        Kind::FailedUpdate => 15,
        Kind::HostHalt => 16,
        Kind::MaintenanceHalted => 17,
        Kind::MaintenanceNotBack => 18,
        Kind::RepeatedDeferral => 19,
    }
}

/// Catches a kind whose log token, severity or wire value changed, and one left out of the list:
/// every kind, in the order `Alerts.List` answers them, has exactly its `alert=` token, its
/// severity and its `operator.proto` value, by number and by name.
#[test]
fn every_kind_keeps_its_token_severity_and_wire_value() {
    use proto::AlertSeverity::{Critical, Warning};
    let table = ALL.map(|kind| {
        let wire = kind.proto();
        (
            kind.log_token(),
            kind.severity(),
            wire as i32,
            wire.as_str_name(),
        )
    });
    assert_eq!(
        table,
        [
            ("halt", Critical, 1, "ALERT_KIND_HALT"),
            (
                "waiting_for_operator",
                Warning,
                2,
                "ALERT_KIND_WAITING_FOR_OPERATOR"
            ),
            ("duplicate", Critical, 3, "ALERT_KIND_DUPLICATE"),
            (
                "duplicate_claim",
                Warning,
                4,
                "ALERT_KIND_DUPLICATE_CLAIM_PENDING"
            ),
            ("canary_failed", Critical, 5, "ALERT_KIND_FAILED_CANARY"),
            (
                "held_on_bad_version",
                Warning,
                6,
                "ALERT_KIND_HELD_ON_BAD_VERSION"
            ),
            (
                "exclude_abandon",
                Warning,
                7,
                "ALERT_KIND_ABANDONED_WHILE_UPDATING"
            ),
            ("silent_host", Warning, 8, "ALERT_KIND_SILENT_HOST"),
            (
                "cert_near_expiry",
                Warning,
                9,
                "ALERT_KIND_CERT_NEAR_EXPIRY"
            ),
            (
                "store_unreachable",
                Critical,
                10,
                "ALERT_KIND_STORE_UNREACHABLE"
            ),
            ("trusted_root", Warning, 11, "ALERT_KIND_TRUSTED_ROOT"),
            (
                "prerequisite_failed",
                Warning,
                16,
                "ALERT_KIND_PREREQUISITE_FAILED"
            ),
            (
                "xcode_release_waiting",
                Warning,
                17,
                "ALERT_KIND_XCODE_RELEASE_WAITING"
            ),
            (
                "xcode_version_taken",
                Warning,
                18,
                "ALERT_KIND_XCODE_VERSION_TAKEN"
            ),
            ("updates_due", Warning, 19, "ALERT_KIND_UPDATES_DUE"),
            ("failed_update", Critical, 20, "ALERT_KIND_FAILED_UPDATE"),
            ("host_halt", Critical, 21, "ALERT_KIND_HOST_HALT"),
            (
                "maintenance_halted",
                Critical,
                22,
                "ALERT_KIND_MAINTENANCE_HALTED"
            ),
            (
                "maintenance_not_back",
                Critical,
                23,
                "ALERT_KIND_MAINTENANCE_NOT_BACK"
            ),
            (
                "repeated_deferral",
                Critical,
                24,
                "ALERT_KIND_REPEATED_DEFERRAL"
            ),
        ]
    );
}

/// Catches a kind missing from the list or listed twice, the list out of the order `Alerts.List`
/// answers in (the order of [`Kind`]), and two kinds sharing a log token or a wire value: each
/// kind is at its own place in the list, the list is strictly ascending, and the 20 tokens and
/// the 20 wire values are distinct, none of them `ALERT_KIND_UNSPECIFIED`.
#[test]
fn the_list_holds_every_kind_once_in_answer_order_with_distinct_tokens_and_values() {
    for (place, kind) in ALL.into_iter().enumerate() {
        assert_eq!(listed(kind), place, "{kind:?}");
    }
    assert!(ALL.windows(2).all(|pair| pair[0] < pair[1]));
    let tokens: BTreeSet<&str> = ALL.into_iter().map(Kind::log_token).collect();
    assert_eq!(tokens.len(), ALL.len());
    let values: BTreeSet<i32> = ALL.into_iter().map(|kind| kind.proto() as i32).collect();
    assert_eq!(values.len(), ALL.len());
    assert!(!values.contains(&(proto::AlertKind::Unspecified as i32)));
}
