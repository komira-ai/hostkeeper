//! The pusher of the push certificate in use: built when the certificate is first held, built again
//! when a new version of the certificate or its key is stored (`docs/design/coordinator-mdm.md`,
//! sections 4.3 and 4.4), and none while there is none.
//!
//! The sender is a port ([`SenderFactory`]): the production one is the HTTP/2 client of APNs, a
//! test gives a fake. The push policy is kept in the pusher, so a pusher built again starts with no
//! push outstanding and no floor armed: a renewal costs at most one early push.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_mdm_push::apns::{Http2Sender, PushSender};
use hostkeeper_mdm_push::clock::Clock;
use hostkeeper_mdm_push::identity::PushIdentity;
use hostkeeper_mdm_push::pusher::Pusher;

use crate::mdm_secrets::SecretKind;
use crate::mdm_setup::MdmSetup;

/// Makes the sender that presents `identity` to APNs and gives up on a try after the timeout.
pub(crate) type SenderFactory =
    Box<dyn Fn(&PushIdentity, Duration) -> Option<Arc<dyn PushSender>> + Send + Sync>;

/// The sender of the real APNs.
pub(crate) fn production_sender() -> SenderFactory {
    Box::new(|identity, timeout| {
        Http2Sender::production(identity, timeout)
            .ok()
            .map(|sender| Arc::new(sender) as Arc<dyn PushSender>)
    })
}

/// The versions of the certificate and key a pusher was built from.
type Versions = (u32, u32);

/// The pusher for the push certificate in use.
pub(crate) struct SetupPushers {
    setup: Arc<MdmSetup>,
    clock: Arc<dyn Clock>,
    sender: SenderFactory,
    current: Mutex<Option<(Versions, Option<Arc<Pusher>>)>>,
}

impl SetupPushers {
    /// The pushers of `setup`'s push certificate, sending through what `sender` makes, by `clock`.
    pub(crate) fn new(setup: Arc<MdmSetup>, clock: Arc<dyn Clock>, sender: SenderFactory) -> Self {
        Self {
            setup,
            clock,
            sender,
            current: Mutex::new(None),
        }
    }

    /// The newest version of `kind` held.
    fn newest(&self, kind: SecretKind) -> u32 {
        self.setup
            .list_secrets()
            .unwrap_or_default()
            .iter()
            .filter(|info| info.kind == kind)
            .map(|info| info.version)
            .max()
            .unwrap_or(0)
    }

    /// The pusher for the certificate in use; none when no certificate, or none that matches its
    /// key, is held, or the setup cannot be read.
    pub(crate) fn get(&self) -> Option<Arc<Pusher>> {
        let versions = (
            self.newest(SecretKind::PushCert),
            self.newest(SecretKind::PushKey),
        );
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((built, pusher)) = &*current
            && *built == versions
        {
            return pusher.clone();
        }
        let pusher = self.build();
        *current = Some((versions, pusher.clone()));
        pusher
    }

    fn build(&self) -> Option<Arc<Pusher>> {
        let identity = self
            .setup
            .push_identity()
            .map_err(|why| tracing::warn!(%why, "the push identity cannot be read"))
            .ok()??;
        let (_, timeout) = self.setup.push_settings().ok()?;
        let sender = (self.sender)(&identity, timeout)?;
        Some(Arc::new(Pusher::new(
            sender,
            Arc::clone(&self.clock),
            &identity.info.topic,
        )))
    }
}

#[cfg(test)]
#[path = "pushing_tests.rs"]
mod tests;
