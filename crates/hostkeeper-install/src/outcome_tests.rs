//! Tests of the reasons, their classes and outcomes, and the failure's text.

use super::*;

/// Every reason with the token, class and outcome the failure table of section 4.7 gives it, and
/// `package_scripts` as packages-native.md section 10.2 gives it.
const TABLE: [(Reason, &str, Class, Outcome); 9] = [
    (
        Reason::InstallerError,
        "installer_error",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::IoError,
        "io_error",
        Class::Environment,
        Outcome::Failed,
    ),
    (
        Reason::ManifestInvalid,
        "manifest_invalid",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::OrderInvalid,
        "order_invalid",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::PackageConflict,
        "package_conflict",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::PackageManagerBusy,
        "package_manager_busy",
        Class::Environment,
        Outcome::Deferred,
    ),
    (
        Reason::PackageScripts,
        "package_scripts",
        Class::Release,
        Outcome::Failed,
    ),
    (
        Reason::Signature,
        "signature",
        Class::Environment,
        Outcome::Failed,
    ),
    (
        Reason::UnsafeDirectory,
        "unsafe_directory",
        Class::Environment,
        Outcome::Failed,
    ),
];

#[test]
fn every_reason_has_its_token_class_and_outcome() {
    for (reason, token, class, outcome) in TABLE {
        assert_eq!(reason.token(), token);
        assert_eq!(reason.class(), class, "{token}");
        assert_eq!(reason.outcome(), outcome, "{token}");
    }
}

/// Catches: a reason added to the enum but not to `ALL`, or `ALL` out of token order.
#[test]
fn all_lists_every_reason_once_sorted_by_token() {
    let tokens: Vec<&str> = Reason::ALL.iter().map(|r| r.token()).collect();
    let mut sorted = tokens.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(tokens, sorted);
    let table: Vec<Reason> = TABLE.iter().map(|row| row.0).collect();
    assert_eq!(Reason::ALL.to_vec(), table);
}

#[test]
fn only_a_busy_package_manager_is_deferred() {
    let deferred: Vec<Reason> = Reason::ALL
        .into_iter()
        .filter(|r| r.outcome() == Outcome::Deferred)
        .collect();
    assert_eq!(deferred, vec![Reason::PackageManagerBusy]);
}

#[test]
fn a_failure_displays_its_token_then_its_detail() {
    let failure = Failure::new(Reason::Signature, "no signer names it");
    assert_eq!(failure.to_string(), "signature: no signer names it");
    assert_eq!(failure.reason(), Reason::Signature);
    assert_eq!(failure.detail(), "no signer names it");
}

#[test]
fn drift_repaired_is_the_reported_token() {
    assert_eq!(DRIFT_REPAIRED, "drift_repaired");
}
