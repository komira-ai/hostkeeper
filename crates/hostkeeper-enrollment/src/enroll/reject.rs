//! The operator's rejection of a pending request (`Enrollment.Reject`): the request leaves the
//! queue, and its key is refused from then on.
//!
//! A pending request has no host id, so no deny row (design 6.1) can name it; the one identity it
//! has is its key, whose SHA-256 is its id. A rejection marks the request [`RequestState::Denied`]
//! and keeps its file: a repost of the key is the same request, still denied
//! ([`EnrollmentStore::post`]), a poll of it is answered denied ([`EnrollmentStore::issue`]),
//! and an approval of it is `not_pending`. A denied request does not count toward
//! [`EnrollmentStore::max_pending`], so a rejection makes room in a full queue.

use super::store::{EnrollmentStore, view};
use super::{RequestState, RequestView, StoreError};
use crate::names::KeyHash;

/// A request [`EnrollmentStore::reject`] answered as rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rejection {
    /// The request, as it stands after the call: denied.
    pub request: RequestView,
    /// This call rejected it; `false` when it was rejected already and nothing was written.
    pub first: bool,
}

/// Why a rejection was refused; nothing is written when one is returned.
#[derive(Debug, thiserror::Error)]
pub enum RejectError {
    #[error("unknown_request: no request has this id")]
    UnknownRequest,
    /// The request was approved, its certificate issued, or its host denied after its approval:
    /// it is not an operator's to reject, but a host's to retire.
    #[error("not_pending: the request is {0:?}")]
    NotPending(RequestState),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl RejectError {
    /// The refusal token; `None` for a failure of the store.
    pub fn token(&self) -> Option<&'static str> {
        match self {
            RejectError::UnknownRequest => Some("unknown_request"),
            RejectError::NotPending(_) => Some("not_pending"),
            RejectError::Store(_) => None,
        }
    }
}

impl EnrollmentStore {
    /// Rejects the request `id` under the store's lock: a pending one (a duplicate claim
    /// included) is marked denied; one denied without having been approved (rejected before) is
    /// answered as it is, with nothing written, so a repeat is told apart from the first call.
    ///
    /// # Errors
    /// [`RejectError::UnknownRequest`], [`RejectError::NotPending`] (approved, issued, or
    /// denied after an approval), [`RejectError::Store`].
    pub fn reject(&self, id: &KeyHash) -> Result<Rejection, RejectError> {
        let _held = self.lock()?;
        let mut row = self.load_request(id)?.ok_or(RejectError::UnknownRequest)?;
        let first = match row.state {
            RequestState::Pending | RequestState::DuplicateClaim => true,
            RequestState::Denied if row.pb.approved.is_none() => false,
            state => return Err(RejectError::NotPending(state)),
        };
        if first {
            row.set_state(RequestState::Denied);
            self.write_request(&row)?;
        }
        Ok(Rejection {
            request: view(&row)?,
            first,
        })
    }
}
