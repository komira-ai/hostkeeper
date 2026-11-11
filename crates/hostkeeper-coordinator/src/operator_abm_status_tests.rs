//! What `Mdm.Status` says of the ABM sync: the last good pass and the last failure.

use super::tests::{alice, json_request, start, token_json};
use crate::abm::fake::NOW;

/// Proves: `Mdm.Status` carries the time of the last ABM pass that ended well and the words of the
/// last one that failed: none of either before a pass, the time and no error after a good one, the
/// same time and the words Apple's refusal of the clock gives after a failed one, and none of
/// either from a coordinator with no ABM client, which says so; a token Apple accepts clears the
/// failure at once.
///
/// Catches: a status page that shows a sync as healthy after it failed, or never shows it.
#[tokio::test]
async fn the_status_names_the_last_good_abm_pass_and_the_last_failure() {
    let rig = start("abm-status-fields", true, true);
    let status = |rig: &super::tests::Rig| {
        let status = rig.world.coordinator.mdm_status(&alice()).expect("status");
        (status.abm_last_sync_unix, status.abm_sync_error)
    };
    assert_eq!(status(&rig), (None, String::new()));

    rig.world
        .coordinator
        .mdm_put_abm_token(&alice(), json_request(&token_json()))
        .await
        .expect("kept");
    let abm = rig.world.coordinator.abm.as_ref().expect("a client");
    abm.sync_noted(&rig.world.coordinator.mdm_setup).await;
    let at = i64::try_from(NOW).expect("a time");
    assert_eq!(status(&rig), (Some(at), String::new()));

    rig.sim.state().session_fault = Some((403, "timestamp_refused".to_owned()));
    rig.sim.state().session = None;
    abm.sync_noted(&rig.world.coordinator.mdm_setup).await;
    assert_eq!(
        status(&rig),
        (
            Some(at),
            "ABM refused the timestamp (timestamp_refused): this machine's clock is off by an hour or more"
                .to_owned()
        )
    );

    rig.sim.state().session_fault = None;
    rig.world
        .coordinator
        .mdm_put_abm_token(&alice(), json_request(&token_json()))
        .await
        .expect("kept again");
    assert_eq!(
        status(&rig),
        (Some(at), String::new()),
        "a token Apple has accepted answers the failure at once"
    );

    let none = start("abm-status-fields-none", true, false);
    assert_eq!(status(&none), (None, String::new()));
    assert!(
        none.world
            .coordinator
            .mdm_status(&alice())
            .expect("status")
            .abm_unavailable
    );
    assert!(
        !rig.world
            .coordinator
            .mdm_status(&alice())
            .expect("status")
            .abm_unavailable
    );
}
