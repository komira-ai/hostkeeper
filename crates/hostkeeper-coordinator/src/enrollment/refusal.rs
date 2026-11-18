//! The one place an error of the enrollment store or the CA becomes what a caller is told: a
//! [`CoordinatorRefusal`] whose token the error names (`token()`), or [`RpcError::Internal`]
//! for a failure that is no refusal (a store file, signing), which is logged here and told to
//! no one.
//!
//! A refusal is a gRPC status whose message is exactly the token: the value's name without
//! `COORDINATOR_REFUSAL_`, in lower case, with the status code the proto's comment on the value
//! names ([`RpcError::status`]).

use std::fmt::Display;

use hostkeeper_enrollment::ca::{ProfileError, SignError};
use hostkeeper_enrollment::enroll::{IssueError, PollError, PostError, StoreError};
use hostkeeper_proto::hostkeeper::agent::v1::CoordinatorRefusal;
use tonic::{Code, Status};

/// The prefix of every value's name.
const PREFIX: &str = "COORDINATOR_REFUSAL_";

/// Why a call of `Enroll`, `Poll` or `Renew` was not answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcError {
    /// Refused: the caller is told the token.
    Refused(CoordinatorRefusal),
    /// The request is not a message of the method: larger than the limit, not all there in
    /// time, or not protobuf. `INVALID_ARGUMENT`, with no token.
    Malformed,
    /// The coordinator failed (a store file, signing, random bytes); the cause was logged.
    /// `INTERNAL`, with no token.
    Internal,
}

/// The refusal whose token is `token`, if a value of [`CoordinatorRefusal`] has it.
#[must_use]
pub fn refusal_of(token: &str) -> Option<CoordinatorRefusal> {
    CoordinatorRefusal::from_str_name(&format!("{PREFIX}{}", token.to_ascii_uppercase()))
        .filter(|refusal| *refusal != CoordinatorRefusal::Unspecified)
}

impl RpcError {
    /// The token a caller is told: the refusal's name without its prefix, in lower case; empty
    /// for an error that is no refusal.
    #[must_use]
    pub fn token(&self) -> String {
        match self {
            RpcError::Refused(refusal) => refusal
                .as_str_name()
                .trim_start_matches(PREFIX)
                .to_ascii_lowercase(),
            RpcError::Malformed | RpcError::Internal => String::new(),
        }
    }

    /// The gRPC status code: the one the proto's comment on the refusal names.
    #[must_use]
    pub fn code(&self) -> Code {
        match self {
            RpcError::Refused(CoordinatorRefusal::Forbidden | CoordinatorRefusal::Denied) => {
                Code::PermissionDenied
            }
            RpcError::Refused(
                CoordinatorRefusal::InvalidHeartbeat
                | CoordinatorRefusal::KeyNotP256
                | CoordinatorRefusal::BadCsr,
            )
            | RpcError::Malformed => Code::InvalidArgument,
            RpcError::Refused(CoordinatorRefusal::PendingFull) => Code::ResourceExhausted,
            RpcError::Refused(CoordinatorRefusal::UnknownRequest) => Code::NotFound,
            RpcError::Refused(CoordinatorRefusal::BadSignature) => Code::Unauthenticated,
            RpcError::Refused(CoordinatorRefusal::RenewalNotAllowed) => Code::FailedPrecondition,
            RpcError::Refused(CoordinatorRefusal::Unspecified) | RpcError::Internal => {
                Code::Internal
            }
        }
    }

    /// The gRPC status a transport answers with: [`Self::code`] and [`Self::token`].
    #[must_use]
    pub fn status(&self) -> Status {
        Status::new(self.code(), self.token())
    }
}

/// An error of the store or the CA that may name a refusal.
pub(crate) trait Token: Display {
    /// The refusal token, if the error is a refusal.
    fn token(&self) -> Option<&'static str>;
}

impl Token for PostError {
    fn token(&self) -> Option<&'static str> {
        PostError::token(self)
    }
}

impl Token for IssueError {
    fn token(&self) -> Option<&'static str> {
        IssueError::token(self)
    }
}

impl Token for PollError {
    fn token(&self) -> Option<&'static str> {
        PollError::token(self)
    }
}

impl Token for ProfileError {
    fn token(&self) -> Option<&'static str> {
        Some(ProfileError::token(self))
    }
}

impl Token for SignError {
    fn token(&self) -> Option<&'static str> {
        SignError::token(self)
    }
}

impl Token for StoreError {
    fn token(&self) -> Option<&'static str> {
        None
    }
}

/// What a caller is told of `error`: the refusal it names, or, for one that names none (or a
/// token that is no refusal of the coordinator's), [`RpcError::Internal`], logged.
pub(crate) fn rpc_error(error: impl Token) -> RpcError {
    match error.token().and_then(refusal_of) {
        Some(refusal) => RpcError::Refused(refusal),
        None => {
            tracing::error!(%error, "an enrollment call failed");
            RpcError::Internal
        }
    }
}
