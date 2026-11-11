//! The start's wiring of the Macs' listener.

use std::sync::Arc;

use futures::FutureExt as _;
use tokio::io::DuplexStream;

use super::{attach, serve};
use crate::testutil::Rig;

/// Proves: with no wiring the coordinator has no Macs' listener and no address to bind, and the
/// serve future of a coordinator without a server waits for ever instead of answering.
///
/// Catches: a coordinator without `--mdm-listen` that binds an address, or one that spins.
#[test]
fn no_wiring_is_no_listener_and_a_coordinator_without_a_server_serves_nothing() {
    let rig = Rig::new("config-mdm-none");
    let (coordinator, address) = attach(None, rig.coordinator);
    assert_eq!(address, None);
    assert!(coordinator.mdm_server().is_none());
    let accept = || async { Err::<DuplexStream, _>(std::io::Error::other("none")) };
    assert!(
        serve(accept, Arc::new(coordinator))
            .now_or_never()
            .is_none()
    );
}
