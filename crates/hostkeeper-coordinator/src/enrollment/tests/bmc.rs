//! What the heartbeat path keeps of a host's BMC with enrollment: a store that cannot be written
//! is logged and the heartbeat path goes on, and the next report writes it.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::{HostBmc, HostRunning};
use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_enrollment::names::{HostId, MacAddress};

use super::support::{HostKey, Rig, enrolled};
use crate::enrollment::os_random;
use crate::testutil::Log;

/// Proves: a BMC report whose host record cannot be written is logged as not recorded, with the
/// host's id, leaves no BMC kept and does not stop the caller; the next report writes it.
///
/// Catches: a failed write that panics in the heartbeat path, is not logged, or is remembered as
/// written (its next report never written).
#[test]
fn a_bmc_that_cannot_be_written_is_logged_and_written_by_the_next_report() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = Rig::over("bmc-failing", fs.clone(), os_random());
    enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let host: hostkeeper_wire::HostId = "s1".parse().expect("a host id");
    let stored = HostId::parse("s1").expect("a host id");
    enrollment
        .enrollment
        .note_running(&host, &HostRunning::default());
    let bmc = HostBmc {
        ip: "192.0.2.12".parse().expect("an address"),
        mac: MacAddress::parse("0c:c4:7a:00:00:01").expect("a MAC"),
    };
    let kept = || {
        let enrolled = enrollment.enrollment.store().enrolled_host(&stored);
        enrolled
            .expect("read")
            .expect("enrolled")
            .bmc
            .map(|kept| kept.bmc)
    };

    let log = Log::default();
    let guard = log.install();
    // The store's lock (1), the read of the host file (2), then its write (3): that fails.
    fs.fail_at(fs.count() + 3);
    enrollment.enrollment.note_bmc(&host, bmc);
    drop(guard);
    assert!(
        log.text().contains("a host's BMC was not recorded"),
        "{}",
        log.text()
    );
    assert!(log.text().contains("s1"), "{}", log.text());
    assert_eq!(kept(), None);

    enrollment.enrollment.note_bmc(&host, bmc);
    assert_eq!(kept(), Some(bmc));
}
