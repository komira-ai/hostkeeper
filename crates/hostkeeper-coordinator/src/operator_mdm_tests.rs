//! The baseline columns of `Mdm.ListDevices`: each state of the gate's [`BaselineStatus`] in
//! the row of its device, and none for a device the gate says nothing of.

use hostkeeper_mdm_gate::{BaselineState, BaselineStatus, Phase};
use hostkeeper_proto::hostkeeper::operator::v1::MdmBaselineState;

use super::device_proto;
use crate::mdm::tests::record;

/// Catches: a baseline state shown as another (pending for failed), a failure's reason lost or
/// shown for a state that has none, or a state shown for a device the gate has no baseline for.
#[test]
fn a_row_shows_the_state_and_the_reason_of_the_baseline() {
    let device = record(
        "AAAA-ROW",
        Some("C02XK1ABJG5J"),
        Phase::Enrolled,
        Some(true),
    );
    let row = |status| device_proto(&device, status);
    let status = |state, reason: &str| {
        Some(BaselineStatus {
            state,
            reason: reason.to_owned(),
        })
    };

    let none = row(None);
    assert_eq!(
        none.baseline_state,
        i32::from(MdmBaselineState::Unspecified)
    );
    assert_eq!(none.baseline_reason, "");
    for (state, shown) in [
        (BaselineState::Pending, MdmBaselineState::Pending),
        (BaselineState::Applied, MdmBaselineState::Applied),
    ] {
        let row = row(status(state, ""));
        assert_eq!(row.baseline_state, i32::from(shown));
        assert_eq!(row.baseline_reason, "");
    }
    let failed = row(status(
        BaselineState::Failed,
        "profile com.example.fskit: the device answered an error",
    ));
    assert_eq!(failed.baseline_state, i32::from(MdmBaselineState::Failed));
    assert_eq!(
        failed.baseline_reason,
        "profile com.example.fskit: the device answered an error"
    );
}
