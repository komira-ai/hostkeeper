//! The timer of the ABM sync as the coordinator runs it: idle without a client, and under the
//! store's lease.

use std::sync::Arc;

use super::tests::{start, token_json};
use crate::mdm_secrets::{Secret, SecretKind};

/// Proves: the coordinator's timer does nothing and never ends without an ABM client, and with one
/// runs a pass (here with no token: nothing is asked of Apple) and then waits.
///
/// Catches: a timer that ends and takes the serve with it, or one that asks Apple with no token.
#[tokio::test(start_paused = true)]
async fn the_coordinators_timer_idles_without_a_client_and_runs_with_one() {
    for abm in [false, true] {
        let rig = start("abm-timer-coordinator", true, abm);
        let timer = crate::operator_abm::abm_sync_every(Arc::clone(&rig.world.coordinator));
        tokio::select! {
            never = timer => match never {},
            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        }
        assert!(rig.fake.seen().is_empty());
    }
}

/// Proves: the coordinator's timer syncs while the coordinator leads its store, asks Apple nothing
/// while the store's session is lost, and goes on when it is back.
///
/// Catches: a coordinator that keeps syncing ABM after another one took over the database, or one
/// that never syncs again after a lost session was revived.
#[tokio::test(start_paused = true)]
async fn the_coordinators_timer_syncs_only_while_its_store_is_led() {
    let rig = start("abm-timer-lease", true, true);
    rig.world
        .coordinator
        .mdm_setup
        .put_secret(SecretKind::AbmToken, &Secret::new(token_json()), None, 1)
        .expect("the token");
    let run = || async {
        let timer = crate::operator_abm::abm_sync_every(Arc::clone(&rig.world.coordinator));
        tokio::select! {
            never = timer => match never {},
            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        }
    };
    rig.world.fake.set_health(crate::store::Health::Lost);
    run().await;
    assert!(
        rig.fake.seen().is_empty(),
        "nothing is asked while it is lost"
    );
    rig.world.fake.set_health(crate::store::Health::Up);
    run().await;
    assert!(
        !rig.fake.seen().is_empty(),
        "it syncs once the store is back"
    );
}
