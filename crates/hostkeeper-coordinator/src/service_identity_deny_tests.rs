//! `Coordinator.ListDeny`: who may read the deny list, what it holds the moment a host is retired,
//! and the validity of the service certificate that reads it. In process, with the enrollment's
//! fixed clock and the in-memory store.

use std::sync::Arc;

use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::agent::v1::CoordinatorRefusal;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;

use crate::enrollment::RpcError;
use crate::enrollment::tests::support::NOW;
use crate::marks_support::operator as operator_caller;
use crate::service_identity_support::{Fix, Held, SERVICE};

fn refused(refusal: CoordinatorRefusal) -> RpcError {
    RpcError::Refused(refusal)
}

/// The hosts `list_deny` answers to `peer`, or why not.
fn listed(fix: &Fix, peer: &Option<Arc<[u8]>>) -> Result<Vec<String>, RpcError> {
    fix.enrollment()
        .list_deny(peer.as_deref())
        .map(|answer| answer.host_ids)
}

/// A certificate for `SERVICE` of `held`, as the peer a relying party's reader presents.
fn service_peer(fix: &Fix, held: &Held) -> Option<Arc<[u8]>> {
    let der = fix.ask(held, SERVICE).expect("issued").certificate_der;
    Some(Arc::from(der.as_slice()))
}

/// Retires `host` as an operator.
fn retire(fix: &Fix, host: &str) {
    fix.coordinator()
        .retire_host(
            &operator_caller(),
            &operator::RetireRequest {
                node_id: host.to_owned(),
                reason: "board failed".to_owned(),
            },
        )
        .expect("retired");
}

/// Proves: a host retired in the same test, with no movement of any clock, is in the list the
/// next call answers, sorted with the others and none twice; a host never retired is not.
///
/// Catches: a deny list that is read from a cache or a snapshot and lists a retire only after a
/// tick, which leaves a retired host trusted by a relying party meanwhile.
#[tokio::test]
async fn a_retired_host_is_listed_at_once() {
    let fix = Fix::new("deny-at-once").await;
    let reader = fix.operator_host("node-01");
    for host in ["node-03", "node-02"] {
        fix.operator_host(host);
    }
    assert_eq!(listed(&fix, &reader.peer()), Ok(Vec::new()));

    retire(&fix, "node-03");
    assert_eq!(listed(&fix, &reader.peer()), Ok(vec!["node-03".to_owned()]));
    retire(&fix, "node-02");
    retire(&fix, "node-02");
    assert_eq!(
        listed(&fix, &reader.peer()),
        Ok(vec!["node-02".to_owned(), "node-03".to_owned()])
    );
}

/// Proves: a host whose retirement is still under way (a deny row that is retiring, not final) is
/// listed.
///
/// Catches: a list that waits for the final report of a host an operator already turned away.
#[tokio::test]
async fn a_host_that_is_retiring_is_listed() {
    let fix = Fix::new("deny-retiring").await;
    let reader = fix.operator_host("node-01");
    fix.operator_host("node-02");
    let retiring = HostId::parse("node-02").expect("a host id");
    fix.enrollment().store().deny(&retiring).expect("denied");
    assert!(
        fix.enrollment()
            .store()
            .is_retiring(&retiring)
            .expect("read")
    );
    assert_eq!(listed(&fix, &reader.peer()), Ok(vec!["node-02".to_owned()]));
}

/// Proves: the list is answered to the holder of a host's certificate and to the holder of a
/// service certificate of a host that is not denied, the same list to both; and refused `forbidden`
/// to a call with no certificate and to one with a certificate that is neither.
///
/// Catches: a deny list readable by anyone that reaches the listener, and a reader that cannot read
/// it with the certificate it has.
#[tokio::test]
async fn the_list_is_answered_to_a_host_certificate_and_a_service_certificate() {
    let fix = Fix::new("deny-callers").await;
    let held = fix.operator_host("node-01");
    fix.operator_host("node-02");
    retire(&fix, "node-02");
    let by_service = service_peer(&fix, &held);
    let want = Ok(vec!["node-02".to_owned()]);
    assert_eq!(listed(&fix, &held.peer()), want);
    assert_eq!(listed(&fix, &by_service), want);
    for peer in [None, Some(Arc::<[u8]>::from(&b"junk"[..]))] {
        assert_eq!(
            listed(&fix, &peer),
            Err(refused(CoordinatorRefusal::Forbidden))
        );
    }
}

/// Proves: a host that is denied cannot read the list, with its host certificate or with a
/// service certificate it was issued before: both are refused `denied`.
///
/// Catches: a retired host that still learns which hosts are denied, and a service certificate
/// that outlives the retirement of its host for this call.
#[tokio::test]
async fn a_denied_host_is_refused_with_either_certificate() {
    let fix = Fix::new("deny-denied-caller").await;
    let held = fix.operator_host("node-01");
    let by_service = service_peer(&fix, &held);
    retire(&fix, "node-01");
    for peer in [held.peer(), by_service] {
        assert_eq!(
            listed(&fix, &peer),
            Err(refused(CoordinatorRefusal::Denied))
        );
    }
}

/// Proves: a service certificate reads the list from the second it is valid to the last second
/// before its end (28800 seconds on) and not before or after: the call is refused `forbidden`.
///
/// Catches: an expired service certificate that still reads the list, and one that does so before
/// its time.
#[tokio::test]
async fn a_service_certificate_reads_the_list_only_while_it_is_valid() {
    let fix = Fix::new("deny-validity").await;
    let held = fix.operator_host("node-01");
    let by_service = service_peer(&fix, &held);
    let clock = &fix.farm.world.enrollment.clock;
    for (now, answered) in [
        (NOW - 1, false),
        (NOW, true),
        (NOW + 28_799, true),
        (NOW + 28_800, false),
        (NOW + 100_000, false),
    ] {
        clock.set(now);
        let answer = listed(&fix, &by_service);
        if answered {
            assert_eq!(answer, Ok(Vec::new()), "at {now}");
        } else {
            assert_eq!(
                answer,
                Err(refused(CoordinatorRefusal::Forbidden)),
                "at {now}"
            );
        }
    }
}
