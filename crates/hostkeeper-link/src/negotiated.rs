//! [`NegotiatedLink`]: the [`CoordinatorLink`] that speaks gRPC to a coordinator that says it
//! serves it, and the JSON route to any other.
//!
//! The daemon announces `grpc-v1` in every heartbeat, on either route. The first heartbeat goes
//! to the JSON route, because nothing is known of the coordinator yet. A reply that advertises
//! [`Feature::GrpcV1`] sends the next heartbeat over gRPC, and so does every reply of gRPC that
//! advertises it; a reply that does not, from either route, sends the next one over JSON. A gRPC
//! heartbeat that fails with [`LinkError::Transport`] (the coordinator closed the connection, did
//! not answer, or does not serve gRPC after all) is sent again at once over JSON, so no heartbeat
//! is lost to a coordinator that advertises gRPC it cannot carry; the JSON answer then decides
//! the next route as any reply does. A refusal or a bad reply is the answer: the coordinator
//! answered.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::beat::{Feature, Heartbeat};
use crate::order::Reply;
use crate::ports::{ClientIdentity, CoordinatorLink, LinkError};

/// The link that follows the features the coordinator's last reply advertised.
pub struct NegotiatedLink {
    json: Box<dyn CoordinatorLink>,
    grpc: Box<dyn CoordinatorLink>,
    /// Whether the next heartbeat goes over gRPC.
    use_grpc: AtomicBool,
}

impl NegotiatedLink {
    /// A link over `json` (the route every coordinator serves) and `grpc`. It starts on `json`.
    pub fn new(
        json: impl CoordinatorLink + 'static,
        grpc: impl CoordinatorLink + 'static,
    ) -> NegotiatedLink {
        NegotiatedLink {
            json: Box::new(json),
            grpc: Box::new(grpc),
            use_grpc: AtomicBool::new(false),
        }
    }

    /// Whether the next heartbeat goes over gRPC.
    pub fn uses_grpc(&self) -> bool {
        self.use_grpc.load(Ordering::Acquire)
    }
}

impl CoordinatorLink for NegotiatedLink {
    fn heartbeat(&self, identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        let answer = if self.uses_grpc() {
            match self.grpc.heartbeat(identity, beat) {
                Err(LinkError::Transport(_)) => self.json.heartbeat(identity, beat),
                answered => answered,
            }
        } else {
            self.json.heartbeat(identity, beat)
        };
        match &answer {
            Ok(reply) => self
                .use_grpc
                .store(reply.features.contains(&Feature::GrpcV1), Ordering::Release),
            Err(LinkError::Transport(_)) => self.use_grpc.store(false, Ordering::Release),
            Err(_) => {}
        }
        answer
    }
}
