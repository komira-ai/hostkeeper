//! `Enrollment.List`: what each pending request shows, the order and the pages, the cap on
//! `limit`, page tokens, and the pending cap seen through `Enroll`.

use std::sync::Arc;

use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_proto::hostkeeper::agent::v1::{
    Claims, CoordinatorRefusal, EnrollRequest, EnrollState,
};
use hostkeeper_proto::hostkeeper::operator::v1::{
    EnrollmentRefusal, InventoryMatch, ListPendingRequest, ListPendingResponse, PendingEnrollment,
};
use prost::Message;
use tonic::Code;

use super::support::{
    HostKey, NOW, Rig, SERIAL, approval, coordinator_over, enroll_request, enrolled, mac_request,
};
use crate::enrollment::{MAX_PENDING_PAGE, RpcError, os_random};
use crate::operator::Caller;
use crate::testutil::OPERATOR;

/// An enrollment letting `max` requests wait, a coordinator over it, and the operator.
async fn rig(name: &str, max: usize) -> (Rig, crate::testutil::Rig, Caller) {
    let enrollment = Rig::configured(name, Arc::new(RealFs), os_random(), |enrollment| {
        enrollment.with_max_pending(max)
    });
    let rig = coordinator_over(&format!("{name}-coordinator"), &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    (enrollment, rig, operator)
}

/// Every page of the list, `limit` at a time.
fn pages(rig: &crate::testutil::Rig, operator: &Caller, limit: u32) -> Vec<ListPendingResponse> {
    let mut out = Vec::new();
    let mut page_token = String::new();
    loop {
        let page = rig
            .coordinator
            .enrollment_list(
                operator,
                &ListPendingRequest {
                    limit,
                    page_token: page_token.clone(),
                },
            )
            .expect("a page");
        page_token.clone_from(&page.next_page_token);
        out.push(page);
        if page_token.is_empty() {
            return out;
        }
    }
}

/// Catches a page that repeats or skips a request, an order that is not first seen then
/// request id, a flag or claim left out, and a CSR or nonce in an answer.
#[tokio::test]
async fn list_reports_claims_key_hash_and_flags_without_secrets() {
    let (enrollment, rig, operator) = rig("list-shows", 300).await;
    let e = &enrollment.enrollment;
    let renewing = HostKey::new();
    enrolled(e, &renewing, "machine-9", "s9");
    let (plain, clone, mac) = (HostKey::new(), HostKey::new(), HostKey::new());
    let nonce = b"a-nonce-of-thirty-two-bytes-long".to_vec();
    let at = |offset: u64| enrollment.clock.set(NOW + offset);
    at(3);
    let plain_request = EnrollRequest {
        claims: Some(Claims {
            machine_id: "machine-1".to_owned(),
            dmi_serial: "DMI-1".to_owned(),
            product_uuid: "uuid-1".to_owned(),
            addresses: vec!["192.0.2.10".to_owned(), "2001:db8::10".to_owned()],
            ..Claims::default()
        }),
        ..enroll_request(&plain, "machine-1")
    };
    e.enroll(plain_request.clone()).expect("enrolled");
    at(1);
    e.enroll(enroll_request(&clone, "machine-1"))
        .expect("a duplicate claim");
    at(2);
    let mac_enroll = mac_request(&mac, "machine-mac", SERIAL, &nonce);
    e.enroll(mac_enroll.clone()).expect("pending: no gate");
    at(3);
    e.enroll(EnrollRequest {
        renewal_of: "s9".to_owned(),
        ..enroll_request(&renewing, "machine-9")
    })
    .expect("a renewal");

    let all = pages(&rig, &operator, 0);
    assert_eq!(all.len(), 1);
    let listed = &all[0].requests;
    let entry = |key: &HostKey, claims: Option<Claims>, first_seen: u64| PendingEnrollment {
        request_id: key.key_hash().to_hex(),
        key_sha256: key.key_hash().to_hex(),
        claims,
        image: "hostkeeper-image-1".to_owned(),
        first_seen_unix: NOW + first_seen,
        renewal_of: String::new(),
        duplicate_claim: false,
        mac_nonce_presented: false,
        // No inventory record is held: every request reads unmatched.
        inventory_match: InventoryMatch::Unmatched.into(),
        ..PendingEnrollment::default()
    };
    let mut third = [
        entry(&plain, plain_request.claims.clone(), 3),
        PendingEnrollment {
            renewal_of: "s9".to_owned(),
            ..entry(&renewing, enroll_request(&renewing, "machine-9").claims, 3)
        },
    ];
    third.sort_by(|a, b| a.request_id.cmp(&b.request_id));
    let expected = [
        PendingEnrollment {
            duplicate_claim: true,
            ..entry(&clone, enroll_request(&clone, "machine-1").claims, 1)
        },
        PendingEnrollment {
            mac_nonce_presented: true,
            ..entry(&mac, mac_enroll.claims.clone(), 2)
        },
        third[0].clone(),
        third[1].clone(),
    ];
    assert_eq!(listed.as_slice(), expected.as_slice());

    // A page at a time, each request once, in the same order, whatever the page size.
    for limit in [1, 2, 3] {
        let paged: Vec<PendingEnrollment> = pages(&rig, &operator, limit)
            .into_iter()
            .flat_map(|page| page.requests)
            .collect();
        assert_eq!(paged.as_slice(), expected.as_slice(), "limit {limit}");
    }
    // The answer holds neither a CSR nor the nonce.
    let bytes = all[0].encode_to_vec();
    for secret in [mac_enroll.csr_der.as_slice(), nonce.as_slice()] {
        assert!(!bytes.windows(secret.len()).any(|w| w == secret));
    }
}

/// Catches a `limit` over the cap answered whole, or 0 not meaning the cap.
#[tokio::test]
async fn a_limit_over_the_cap_is_clamped() {
    let (enrollment, rig, operator) = rig("list-clamp", 300).await;
    for n in 0..=MAX_PENDING_PAGE {
        enrollment
            .enrollment
            .enroll(enroll_request(&HostKey::new(), &format!("machine-{n}")))
            .expect("enrolled");
    }
    for limit in [0, 257, 100_000] {
        let page = rig
            .coordinator
            .enrollment_list(
                &operator,
                &ListPendingRequest {
                    limit,
                    page_token: String::new(),
                },
            )
            .expect("a page");
        assert_eq!(page.requests.len(), MAX_PENDING_PAGE, "limit {limit}");
        assert!(!page.next_page_token.is_empty());
    }
    let sizes: Vec<usize> = pages(&rig, &operator, 0)
        .iter()
        .map(|page| page.requests.len())
        .collect();
    assert_eq!(sizes, [MAX_PENDING_PAGE, 1]);
}

/// Catches a page token the coordinator did not give being read as a place in the list, and a
/// page going on wrongly after a request on an earlier page was approved.
#[tokio::test]
async fn a_page_token_this_coordinator_did_not_give_is_invalid_request() {
    let (enrollment, rig, operator) = rig("list-token", 300).await;
    let keys: Vec<HostKey> = (0..3).map(|_| HostKey::new()).collect();
    for (n, key) in keys.iter().enumerate() {
        enrollment.clock.set(NOW + n as u64);
        enrollment
            .enrollment
            .enroll(enroll_request(key, &format!("machine-{n}")))
            .expect("enrolled");
    }
    let hex = keys[0].key_hash().to_hex();
    for token in [
        "x".to_owned(),
        "5".to_owned(),
        "5-zz".to_owned(),
        format!("+5-{hex}"),
        format!("-{hex}"),
        format!("5-{}", hex.to_ascii_uppercase()),
    ] {
        let refused = rig.coordinator.enrollment_list(
            &operator,
            &ListPendingRequest {
                limit: 1,
                page_token: token.clone(),
            },
        );
        assert_eq!(refused, Err(EnrollmentRefusal::InvalidRequest), "{token}");
    }
    let first = rig
        .coordinator
        .enrollment_list(
            &operator,
            &ListPendingRequest {
                limit: 1,
                page_token: String::new(),
            },
        )
        .expect("a page");
    assert_eq!(first.requests[0].request_id, hex);
    // The first request is approved; the next page still starts at the second.
    rig.coordinator
        .enrollment_approve(&operator, approval(&keys[0], "s1"))
        .expect("approved");
    let next = rig
        .coordinator
        .enrollment_list(
            &operator,
            &ListPendingRequest {
                limit: 1,
                page_token: first.next_page_token,
            },
        )
        .expect("a page");
    assert_eq!(next.requests[0].request_id, keys[1].key_hash().to_hex());
}

/// Catches the newest request evicting an older one, a refusal other than `pending_full`
/// with `RESOURCE_EXHAUSTED`, the cap counting approved requests, and a repost of a waiting
/// request refused as a new one.
#[tokio::test]
async fn a_full_pending_queue_keeps_existing_requests() {
    let (enrollment, rig, operator) = rig("list-full", 3).await;
    let e = &enrollment.enrollment;
    let keys: Vec<HostKey> = (0..5).map(|_| HostKey::new()).collect();
    for (n, key) in keys.iter().take(3).enumerate() {
        e.enroll(enroll_request(key, &format!("machine-{n}")))
            .expect("enrolled");
    }
    let list = || {
        rig.coordinator
            .enrollment_list(&operator, &ListPendingRequest::default())
            .expect("listed")
    };
    let before = list();
    let refused = e
        .enroll(enroll_request(&keys[3], "machine-3"))
        .expect_err("full");
    assert_eq!(refused, RpcError::Refused(CoordinatorRefusal::PendingFull));
    let status = refused.status();
    assert_eq!(
        (status.code(), status.message()),
        (Code::ResourceExhausted, "pending_full")
    );
    assert_eq!(list(), before, "the waiting requests are as they were");

    // The first request posted again is still the same request.
    let again = e
        .enroll(enroll_request(&keys[0], "machine-0"))
        .expect("a repost is accepted");
    assert_eq!(
        (again.request_id, again.state),
        (keys[0].key_hash().to_hex(), EnrollState::Pending as i32)
    );
    // One of them is still approved, and then there is room.
    rig.coordinator
        .enrollment_approve(&operator, approval(&keys[1], "s1"))
        .expect("approved");
    e.enroll(enroll_request(&keys[3], "machine-3"))
        .expect("room after an approval");
    assert_eq!(
        e.enroll(enroll_request(&keys[4], "machine-4")),
        Err(RpcError::Refused(CoordinatorRefusal::PendingFull))
    );
}

/// Catches a store that cannot be read answered as an empty list: it is `store_failed`.
#[tokio::test]
async fn a_list_the_store_cannot_read_is_store_failed() {
    let fs = Arc::new(hostkeeper_enrollment::fs::FaultFs::new());
    let enrollment = Rig::over("list-failing", fs.clone(), os_random());
    let rig = coordinator_over("list-failing-coordinator", &enrollment).await;
    let operator = rig.coordinator.caller(Some(OPERATOR)).await;
    enrollment
        .enrollment
        .enroll(enroll_request(&HostKey::new(), "machine-1"))
        .expect("enrolled");
    // The list of the requests' directory is the first operation.
    fs.fail_at(fs.count() + 1);
    assert_eq!(
        rig.coordinator
            .enrollment_list(&operator, &ListPendingRequest::default()),
        Err(EnrollmentRefusal::StoreFailed)
    );
}
