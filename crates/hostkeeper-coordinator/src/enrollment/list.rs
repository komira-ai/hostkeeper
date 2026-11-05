//! `Enrollment.List` as a plain function over the store: the requests waiting for an operator,
//! oldest first, a page at a time.
//!
//! The order is stable: the time the coordinator first received a request, then its id. A
//! page token names the last request of the page before (`<first seen>-<request id>`), so a
//! page goes on after it whatever was approved or posted in between, and no request is listed
//! twice or skipped. The store is read without its write lock. An answer carries what the
//! host claimed, never its CSR and never a nonce.

use hostkeeper_enrollment::enroll::{Claims, RequestState, RequestView};
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::EnrollmentRefusal;

use super::Enrollment;

/// The most requests one page lists: a `limit` of 0, or above it, is this.
pub const MAX_PENDING_PAGE: usize = 256;

/// Where a request stands in the order of the list.
type Place = (u64, KeyHash);

fn place_of(view: &RequestView) -> Place {
    (view.posted_unix, view.request_id)
}

/// The page token after `place`.
fn token_of(place: Place) -> String {
    format!("{}-{}", place.0, place.1)
}

/// The place a page token names.
fn place_in(token: &str) -> Option<Place> {
    let (first_seen, id) = token.split_once('-')?;
    let first_seen = first_seen
        .parse::<u64>()
        .ok()
        .filter(|_| first_seen.bytes().all(|b| b.is_ascii_digit()))?;
    Some((first_seen, KeyHash::from_hex(id)?))
}

/// What the host claimed, as the agent's protocol spells it.
fn claims_proto(claims: Claims) -> agent::Claims {
    agent::Claims {
        machine_id: claims.machine_id,
        dmi_serial: claims.dmi_serial,
        product_uuid: claims.dmi_product_uuid,
        addresses: claims.addresses,
        mac_serial: claims.mac_serial,
        platform_uuid: claims.mac_platform_uuid,
    }
}

/// `view` as the list shows it: its key hash is its id, the one an approval names.
fn pending_proto(view: RequestView) -> proto::PendingEnrollment {
    proto::PendingEnrollment {
        request_id: view.request_id.to_hex(),
        key_sha256: view.request_id.to_hex(),
        claims: Some(claims_proto(view.claims)),
        image: view.image,
        first_seen_unix: view.posted_unix,
        renewal_of: view
            .renewal_of
            .map(|host| host.to_string())
            .unwrap_or_default(),
        duplicate_claim: view.state == RequestState::DuplicateClaim,
        mac_nonce_presented: view.mac_nonce_presented,
    }
}

impl Enrollment {
    /// `Enrollment.List`: the pending requests after the page token, oldest first, at most
    /// `limit` ([`MAX_PENDING_PAGE`] for 0 or more), and the token of the next page (empty on
    /// the last). Changes nothing.
    ///
    /// # Errors
    /// `invalid_request` (a page token this coordinator did not give), `store_failed` (a
    /// request file could not be read).
    pub fn list(
        &self,
        request: &proto::ListPendingRequest,
    ) -> Result<proto::ListPendingResponse, EnrollmentRefusal> {
        let after = match request.page_token.as_str() {
            "" => None,
            token => Some(place_in(token).ok_or(EnrollmentRefusal::InvalidRequest)?),
        };
        let limit = match request.limit as usize {
            0 => MAX_PENDING_PAGE,
            limit => limit.min(MAX_PENDING_PAGE),
        };
        let mut pending = self.store.pending().map_err(|why| {
            tracing::error!(%why, "cannot read the pending requests");
            EnrollmentRefusal::StoreFailed
        })?;
        pending.sort_by_key(place_of);
        pending.retain(|view| after.is_none_or(|after| place_of(view) > after));
        let more = pending.len() > limit;
        pending.truncate(limit);
        let next_page_token = match pending.last() {
            Some(last) if more => token_of(place_of(last)),
            _ => String::new(),
        };
        Ok(proto::ListPendingResponse {
            requests: pending.into_iter().map(pending_proto).collect(),
            next_page_token,
        })
    }
}
