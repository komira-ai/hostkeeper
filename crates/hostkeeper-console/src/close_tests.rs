//! The close codes and reason tokens of a console stream.

use super::Close;
use crate::bmc::{BmcError, Step};

/// Proves: every cause has its own code, 4001 to 4013 in order, and its own token, as the design's
/// section 12 lists them (4013 `bmc_failed` beside them); each BMC error closes with its cause.
///
/// Catches: a code renumbered or two causes sharing one, which a page would show as the wrong
/// reason, and a BMC error closing with another's code.
#[test]
fn each_cause_has_its_code_and_token() {
    let pairs: Vec<(u16, &str)> = Close::ALL
        .iter()
        .map(|close| (close.code(), close.token()))
        .collect();
    assert_eq!(
        pairs,
        [
            (4001, "device_unreachable"),
            (4002, "bmc_sol_busy"),
            (4003, "bmc_busy"),
            (4004, "bmc_auth_failed"),
            (4005, "idle"),
            (4006, "total_limit"),
            (4007, "closed_by_operator"),
            (4008, "token_revoked"),
            (4009, "address_mismatch"),
            (4010, "coordinator_stopping"),
            (4011, "mac_screen_sharing_off"),
            (4012, "mac_unreachable_or_asleep"),
            (4013, "bmc_failed"),
        ]
    );
    let mapped = [
        BmcError::Unreachable,
        BmcError::SolBusy,
        BmcError::Busy,
        BmcError::AuthFailed,
        BmcError::AddressMismatch,
        BmcError::Failed(Step::PayloadStatus),
    ]
    .map(Close::from);
    assert_eq!(
        mapped,
        [
            Close::DeviceUnreachable,
            Close::BmcSolBusy,
            Close::BmcBusy,
            Close::BmcAuthFailed,
            Close::AddressMismatch,
            Close::BmcFailed,
        ]
    );
    for error in [BmcError::SolBusy, BmcError::Failed(Step::PayloadStatus)] {
        let close = Close::from(error);
        assert!(error.to_string().starts_with(close.token()), "{error}");
    }
}
