//! The audit of a request's life: the request, the operator's approval, the certificate's first
//! issue, a renewal and a second request, each one entry in order, read back with `Audit.List`
//! at the time the enrollment's clock says, and none carrying a CSR, a key, a nonce or a
//! signature.

use std::sync::Arc;

use hostkeeper_enrollment::enroll::poll_message;
use hostkeeper_enrollment::validity::UnixClock as _;
use hostkeeper_proto::hostkeeper::agent::v1::{
    EnrollRequest, PollRequest, PollState, RenewRequest,
};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use super::support::{
    HostKey, NOW, ONE_STORAGE, Rig, approval, audited, enroll_request, requested, signed_poll,
};
use crate::marks_support::operator;
use crate::testutil::ops_of;

/// Every entry `Audit.List` answers, oldest first.
fn listed(rig: &crate::testutil::Rig) -> Vec<proto::AuditEntry> {
    rig.coordinator
        .audit_list(&operator(), proto::ListAuditRequest::default())
        .expect("listed")
        .entries
}

/// Hex of `bytes`, as a secret would read if it were written into a reason.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Proves: a request, its approval, the first poll that takes the certificate, a renewal and a
/// second request are audited `enroll_requested`, `enroll_approve` (with the `assign` it makes),
/// `enroll_issued`, `cert_renewed`, `enroll_requested`, in that order, under `coordinator` (the approval under the
/// operator's token), naming the host once there is one, each at the time of the enrollment's
/// clock; a repost of a request, a later poll of an issued request, and a poll still pending
/// write nothing; and no reason holds the CSR, the public key, a poll nonce or signature, or a
/// certificate.
///
/// Catches: a state change left out of the audit, an entry for every repost or poll, an entry
/// named after the host's claim instead of who made the change, and a secret in a reason.
#[tokio::test]
async fn enrollment_lifecycle_is_audited() {
    let enrollment = Rig::new("audit-lifecycle");
    let mut rig = crate::testutil::Rig::new("audit-lifecycle-coordinator");
    let clock = Arc::clone(&enrollment.clock);
    rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&enrollment.enrollment))
        .with_ops(ops_of(&rig.dir).with_unix(Arc::new(move || clock.now_unix())));
    rig.at(2_000);
    rig.set(ONE_STORAGE).await;
    let e = &enrollment.enrollment;
    let before = listed(&rig).len();

    let key = HostKey::new();
    let request = enroll_request(&key, "machine-1");
    enrollment.clock.set(NOW + 10);
    e.enroll(request.clone()).expect("requested");
    e.enroll(request.clone()).expect("reposted");
    assert_eq!(
        signed_poll(e, &key, &key).expect("polled").state,
        PollState::Pending as i32
    );

    enrollment.clock.set(NOW + 20);
    rig.coordinator
        .enrollment_approve(&operator(), approval(&key, "s1"))
        .expect("approved");

    enrollment.clock.set(NOW + 30);
    let challenge = e
        .poll(PollRequest {
            request_id: key.key_hash().to_hex(),
            ..PollRequest::default()
        })
        .expect("a nonce");
    let signature = key.sign(&poll_message(&key.key_hash(), &challenge.nonce));
    let issued = e
        .poll(PollRequest {
            request_id: key.key_hash().to_hex(),
            nonce: challenge.nonce.clone(),
            signature: signature.clone(),
        })
        .expect("issued");
    assert_eq!(issued.state, PollState::Issued as i32);
    let again = signed_poll(e, &key, &key).expect("polled again");
    assert_eq!(again.certificate_der, issued.certificate_der);

    enrollment.clock.set(NOW + 40);
    let renewed = e
        .renew(
            Some(issued.certificate_der.as_slice()),
            RenewRequest { csr_der: key.csr() },
        )
        .expect("renewed");

    enrollment.clock.set(NOW + 50);
    let other = HostKey::new();
    e.enroll(EnrollRequest {
        image: "hostkeeper-image-2".to_owned(),
        ..enroll_request(&other, "machine-2")
    })
    .expect("a second request");

    let entries = listed(&rig).split_off(before);
    let got: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                entry.by_name.clone().unwrap_or_default(),
                entry.action.clone(),
                entry.node_id.clone(),
                entry.reason.clone(),
            )
        })
        .collect();
    let id = key.key_hash();
    assert_eq!(
        got,
        [
            requested(&key, None, "pending"),
            audited(
                "operator",
                "enroll_approve",
                Some("s1"),
                &format!(
                    "request {id}; role storage; services kbf; claims machine_id=machine-1; \
                     new rack"
                )
            ),
            audited("operator", "assign", Some("s1"), "host s1"),
            audited(
                "coordinator",
                "enroll_issued",
                Some("s1"),
                &format!("request {id}; round 0; serial 3")
            ),
            audited("coordinator", "cert_renewed", Some("s1"), "serial 4"),
            requested(&other, None, "pending"),
        ]
    );
    let times: Vec<Option<u64>> = entries.iter().map(|entry| entry.at_unix).collect();
    assert_eq!(
        times,
        [10, 20, 20, 30, 40, 50]
            .map(|offset| Some(NOW + offset))
            .to_vec()
    );
    let actors: Vec<i32> = entries.iter().map(|entry| entry.by).collect();
    let (coordinator, operator) = (
        i32::from(proto::Actor::Coordinator),
        i32::from(proto::Actor::Operator),
    );
    assert_eq!(
        actors,
        [
            coordinator,
            operator,
            operator,
            coordinator,
            coordinator,
            coordinator
        ]
    );

    let secrets = [
        hex(&request.csr_der),
        hex(&key.csr()),
        hex(&challenge.nonce),
        hex(&signature),
        hex(&issued.certificate_der),
        hex(&renewed.certificate_der),
    ];
    for entry in &entries {
        let reason = entry.reason.as_deref().unwrap_or_default();
        for secret in &secrets {
            assert!(!reason.contains(&secret[..32]), "{reason}");
        }
    }
}

/// Proves: a renewal request of an enrolled host (`renewal_of`) is audited naming the host, in
/// the next round; a request whose machine id another request holds is audited as
/// `duplicate_claim`; a refused request writes nothing.
///
/// Catches: a renewal audited without its host, a duplicate claim audited as pending, and an
/// entry for a request the store refused.
#[tokio::test]
async fn a_renewal_request_names_its_host_and_a_duplicate_claim_says_so() {
    let enrollment = Rig::new("audit-renewal-request");
    let rig = super::support::coordinator_over("audit-renewal-request-c", &enrollment).await;
    let e = &enrollment.enrollment;
    let key = HostKey::new();
    super::support::enrolled(e, &key, "machine-1", "s1");
    let before = listed(&rig).len();

    e.enroll(EnrollRequest {
        renewal_of: "s1".to_owned(),
        ..enroll_request(&key, "machine-1")
    })
    .expect("a renewal request");
    let twin = HostKey::new();
    e.enroll(enroll_request(&twin, "machine-1"))
        .expect("a duplicate claim");
    assert!(
        e.enroll(EnrollRequest {
            csr_der: b"not a CSR".to_vec(),
            ..enroll_request(&HostKey::new(), "machine-3")
        })
        .is_err()
    );

    let got: Vec<_> = listed(&rig)
        .split_off(before)
        .into_iter()
        .map(|entry| {
            (
                entry.by_name.unwrap_or_default(),
                entry.action,
                entry.node_id,
                entry.reason,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            requested(&key, Some("s1"), "pending"),
            requested(&twin, None, "duplicate_claim"),
        ]
    );
}
