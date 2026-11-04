//! The typed words of an order and of a verdict, besides service names and versions (those are
//! `hostkeeper_config::names`, with the grammars of design 11.8): the coordinator's order id, a
//! reason token and a package digest. Each is refused at construction when it leaves its
//! grammar, and none of the grammars holds a `/`, a `:` or white space, so no word of an order
//! can carry a command, a path or a URL.

use hostkeeper_hook::{MAX_REASON_LEN, is_reason_token};

/// The most bytes of an order id.
pub const MAX_ORDER_ID: usize = 64;

/// The bytes of a package digest: SHA-256 as lower-case hex.
pub const DIGEST_LEN: usize = 64;

/// Why a word was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WordError {
    #[error(
        "{0:?} is not an order id: 1 to {MAX_ORDER_ID} bytes of A-Z, a-z, 0-9, `.`, `_` and `-`, not beginning with `.`"
    )]
    OrderId(String),
    #[error("{0:?} is not a reason: 1 to {MAX_REASON_LEN} bytes of a-z, 0-9 and `_`")]
    Reason(String),
    #[error("{0:?} is not a package digest: {DIGEST_LEN} lower-case hex digits")]
    Digest(String),
}

/// The coordinator's id for an order (`Order.order_id` of `hostkeeper.agent.v1`): 1 to
/// [`MAX_ORDER_ID`] bytes of `A-Z`, `a-z`, `0-9`, `.`, `_` and `-`, not beginning with `.`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderId(String);

impl OrderId {
    pub fn new(id: &str) -> Result<OrderId, WordError> {
        let fits = (1..=MAX_ORDER_ID).contains(&id.len())
            & !id.starts_with('.')
            & id.bytes()
                .all(|b| b.is_ascii_alphanumeric() | b"._-".contains(&b));
        fits.then(|| OrderId(id.to_owned()))
            .ok_or_else(|| WordError::OrderId(id.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A reason: a token of `a-z`, `0-9` and `_`, 1 to `MAX_REASON_LEN` bytes, the grammar of the
/// hook contract (`drain_bound`, `busy`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason(String);

impl Reason {
    pub fn new(reason: &str) -> Result<Reason, WordError> {
        is_reason_token(reason)
            .then(|| Reason(reason.to_owned()))
            .ok_or_else(|| WordError::Reason(reason.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `busy`: an order that arrived while another ran, and was not started.
    pub(crate) fn busy() -> Reason {
        Reason("busy".to_owned())
    }

    /// `deferred`: the reason a deferral is kept under when its verdict gave none.
    pub(crate) fn deferred() -> Reason {
        Reason("deferred".to_owned())
    }
}

/// The digest a package must have: SHA-256 as [`DIGEST_LEN`] lower-case hex digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageDigest(String);

impl PackageDigest {
    pub fn new(digest: &str) -> Result<PackageDigest, WordError> {
        let fits = (digest.len() == DIGEST_LEN)
            & digest
                .bytes()
                .all(|b| b.is_ascii_digit() | (b'a'..=b'f').contains(&b));
        fits.then(|| PackageDigest(digest.to_owned()))
            .ok_or_else(|| WordError::Digest(digest.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
