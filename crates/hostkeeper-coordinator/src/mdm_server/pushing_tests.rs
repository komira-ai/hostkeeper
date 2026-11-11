//! The pusher of the push certificate in use (`docs/design/coordinator-mdm.md`, section 4.4).

use std::sync::Arc;

use hostkeeper_mdm_push::apns::PushSender;
use hostkeeper_mdm_push::testkit::{FakeApns, ManualClock};

use super::{SetupPushers, production_sender};
use crate::mdm_server::support::Rig;
use crate::mdm_setup::MdmSetup;

fn sender() -> super::SenderFactory {
    let apns = Arc::new(FakeApns::new());
    Box::new(move |_, _| Some(Arc::clone(&apns) as Arc<dyn PushSender>))
}

/// Proves: there is no pusher until a push certificate and its key are held, the same pusher is
/// handed out while nothing changed, and a new version of the certificate gives a new one.
///
/// Catches: a pusher built from a certificate that was since replaced, or one built for each push.
#[test]
fn the_pusher_follows_the_push_certificate_in_use() {
    let rig = Rig::bare("pushing-follows");
    let pushers = SetupPushers::new(
        Arc::clone(&rig.setup),
        Arc::new(ManualClock::at(1)),
        sender(),
    );
    assert!(pushers.get().is_none());
    rig.store_push();
    let first = pushers.get().expect("a pusher");
    assert!(Arc::ptr_eq(&first, &pushers.get().expect("the same")));
    rig.store_push();
    assert!(!Arc::ptr_eq(&first, &pushers.get().expect("a new one")));
}

/// Proves: a sender that cannot be made, and a setup that cannot be read, leave no pusher.
///
/// Catches: a failing sender taking down the queue instead of leaving the Macs to connect on
/// their own.
#[test]
fn a_sender_that_cannot_be_made_or_a_setup_that_cannot_be_read_leaves_no_pusher() {
    let rig = Rig::new("pushing-fails");
    let none: super::SenderFactory = Box::new(|_, _| None);
    let pushers = SetupPushers::new(Arc::clone(&rig.setup), Arc::new(ManualClock::at(1)), none);
    assert!(pushers.get().is_none());
    let unkeyed = Arc::new(MdmSetup::volatile());
    let pushers = SetupPushers::new(unkeyed, Arc::new(ManualClock::at(1)), sender());
    assert!(pushers.get().is_none());
}

/// Proves: the production sender is made from a push identity, with no connection made.
///
/// Catches: the real client failing to build from the identity the setup holds.
#[test]
fn the_production_sender_is_made_from_the_push_identity() {
    let rig = Rig::new("pushing-production");
    let identity = rig.setup.push_identity().expect("read").expect("a pair");
    let made = production_sender()(&identity, std::time::Duration::from_secs(30));
    assert!(made.is_some());
}
