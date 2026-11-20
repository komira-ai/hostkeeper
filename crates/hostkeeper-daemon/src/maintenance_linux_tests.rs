//! Tests of the helpers `maintenance_linux` shares between its upgraders.

use super::*;

/// Proves: `without_arch` takes off an architecture suffix and leaves a name without one, and
/// cuts at the first colon.
#[test]
fn a_package_name_loses_its_architecture_suffix() {
    assert_eq!(without_arch("libc6:amd64"), "libc6");
    assert_eq!(without_arch("libc6"), "libc6");
    assert_eq!(without_arch("libc6:i386:extra"), "libc6");
    assert_eq!(without_arch(""), "");
}

/// Proves: `install_failed` is a failure with the reason `install_failed` and the detail given,
/// unchanged.
#[test]
fn an_install_failure_carries_its_reason_and_detail() {
    assert_eq!(
        install_failed("apt-get update failed with exit code 100".to_owned()),
        KindError::Failed {
            reason: "install_failed",
            detail: "apt-get update failed with exit code 100".to_owned(),
        }
    );
    assert_eq!(REASON_INSTALL_FAILED, "install_failed");
}
