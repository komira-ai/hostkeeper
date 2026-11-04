//! The paths that run over several steps or restarts: a host that returns after expiry more than
//! once, an approval given while the daemon was down, a reply lost after the coordinator acted on
//! the call, claims and image that change between restarts, and the bytes a poll signs.

use std::path::PathBuf;

use hostkeeper_enroll::{Error, HostId, Outcome, POLL_CONTEXT, RequestId, poll_message};
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{DerSignature, VerifyingKey};
use p256::pkcs8::DecodePublicKey;

use crate::support::{HOST, Op, T0, WEEK, World};

/// Catches a request kept as posted after it was answered: a host that returned once and then
/// expired again in the same process must post again, not poll for the old certificate.
#[test]
fn a_host_returns_after_expiry_twice_in_one_process() {
    let world = World::new("two-returns");
    let mut enroller = world.enrolled();
    for (round, now) in [(2, T0 + WEEK + 1), (3, T0 + 2 * WEEK + 2)] {
        world.set_now(now);
        assert_eq!(
            enroller.run_once().expect("post"),
            Outcome::Pending,
            "{round}"
        );
        world.coordinator().approve(HOST);
        let Outcome::Enrolled(identity) = enroller.run_once().expect("return") else {
            panic!("round {round} did not return");
        };
        assert_eq!(identity.generation, round);
        assert_eq!(identity.not_before, now);
        assert_eq!(world.current(), Some(PathBuf::from(format!("gen-{round}"))));
    }
    let c = world.coordinator();
    assert_eq!(c.enrolls.len(), 3);
    assert_eq!(c.enrolls[2].renewal_of, HostId::new(HOST));
    assert!(c.renews.is_empty());
}

/// Catches a return whose approval, given while the daemon was down, is lost when the daemon
/// starts again and reposts its request.
#[test]
fn an_approval_of_a_return_given_while_the_daemon_was_down_is_kept() {
    let world = World::new("return-approved-while-down");
    let mut enroller = world.enrolled();
    world.set_now(T0 + WEEK + 1);
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    drop(enroller);
    world.coordinator().approve(HOST);
    let mut enroller = world.open();
    let Outcome::Enrolled(identity) = enroller.run_once().expect("return") else {
        panic!("not returned");
    };
    assert_eq!(identity.generation, 2);
    assert_eq!(
        world.coordinator().enrolls.len(),
        3,
        "reposted once on restart"
    );
}

/// Catches a request whose `Enroll` reply was lost, after the coordinator stored it, being
/// posted as something else: the next step posts the same request, and the coordinator holds
/// one row.
#[test]
fn a_lost_enroll_reply_is_posted_again_as_the_same_request() {
    let world = World::new("lost-enroll");
    world.coordinator().lose_reply = Some(Op::Enroll);
    let mut enroller = world.open();
    let failed = enroller.run_once().expect_err("lost");
    assert_eq!(
        failed.to_string(),
        "the coordinator could not be reached: the reply to Enroll was lost"
    );
    assert!(world.coordinator().polls.is_empty());
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    let c = world.coordinator();
    assert_eq!(c.enrolls.len(), 2);
    assert_eq!(c.enrolls[0], c.enrolls[1]);
    assert_eq!(c.rows.len(), 1);
}

/// Catches a poll that reuses a nonce after a lost reply: each step asks for a new nonce, so a
/// nonce the coordinator spent is never signed again, and nothing is stored meanwhile.
#[test]
fn a_lost_poll_reply_gets_a_fresh_challenge() {
    let world = World::new("lost-poll");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().approve(HOST);
    for op in [Op::UnsignedPoll, Op::SignedPoll] {
        world.coordinator().lose_reply = Some(op);
        let failed = enroller.run_once().expect_err("lost");
        assert_eq!(
            failed.to_string(),
            format!("the coordinator could not be reached: the reply to {op:?} was lost")
        );
        assert_eq!(world.current(), None);
    }
    assert!(matches!(enroller.run_once(), Ok(Outcome::Enrolled(_))));
    let c = world.coordinator();
    let signed: Vec<&Vec<u8>> = c
        .polls
        .iter()
        .filter(|poll| !poll.signature.is_empty())
        .map(|poll| &poll.nonce)
        .collect();
    assert_eq!(
        signed.len(),
        3,
        "the first step, the lost reply and the last"
    );
    assert!(
        (signed[0] != signed[1]) & (signed[1] != signed[2]),
        "{signed:?}"
    );
    assert_eq!(c.enrolls.len(), 1, "posted once");
}

/// Catches a lost `Renew` reply changing the current generation, or the renewal not being
/// retried.
#[test]
fn a_lost_renew_reply_leaves_the_current_generation() {
    let world = World::new("lost-renew");
    let mut enroller = world.enrolled();
    world.set_now(T0 + WEEK / 2);
    world.coordinator().lose_reply = Some(Op::Renew);
    assert!(matches!(enroller.run_once(), Err(Error::Transport(_))));
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));
    assert!(matches!(enroller.run_once(), Ok(Outcome::Renewed(_))));
    assert_eq!(world.coordinator().renews.len(), 2);
}

/// Catches a request whose identity depends on its claims or image: a host whose interfaces or
/// image changed between two starts posts the same request (the request is its key), with the
/// claims and image it has now, and the approval of the first post stands.
#[test]
fn a_restart_with_changed_claims_and_image_is_the_same_request() {
    let world = World::new("changed-claims");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    drop(enroller);
    world.coordinator().approve(HOST);
    world
        .claims
        .lock()
        .expect("claims")
        .network_addresses
        .push("02:00:00:00:00:09".into());
    world.claims.lock().expect("claims").product_uuid = None;
    *world.image.lock().expect("image") = "image-2".into();

    let mut enroller = world.open();
    assert!(matches!(enroller.run_once(), Ok(Outcome::Enrolled(_))));
    let c = world.coordinator();
    let (first, second) = (&c.enrolls[0], &c.enrolls[1]);
    assert_eq!(first.csr_der, second.csr_der);
    assert_eq!(
        second.claims.network_addresses,
        ["02:00:00:00:00:01", "02:00:00:00:00:09"]
    );
    assert_eq!(second.claims.product_uuid, None);
    assert_eq!(
        (first.image.as_str(), second.image.as_str()),
        ("image-1", "image-2")
    );
    assert_eq!(c.rows.len(), 1);
}

/// Catches a change to the bytes a poll signs, checked here against the contract's literal
/// bytes rather than against the crate's own function: the context, a zero byte, the 64 hex
/// digits of the request id, a zero byte, the nonce.
#[test]
fn the_poll_signature_covers_the_contract_bytes() {
    let world = World::new("poll-bytes");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    let c = world.coordinator();
    let signed = &c.polls[1];
    let id = signed.request_id.as_str();
    assert_eq!(RequestId::parse(id), Some(signed.request_id.clone()));
    let mut literal = b"hostkeeper-enroll-poll-v1".to_vec();
    literal.push(0);
    literal.extend_from_slice(id.as_bytes());
    literal.push(0);
    literal.extend_from_slice(&signed.nonce);
    assert_eq!(POLL_CONTEXT, b"hostkeeper-enroll-poll-v1");
    assert_eq!(poll_message(&signed.request_id, &signed.nonce), literal);
    let spki = &c.rows[&signed.request_id].spki_der;
    let key = VerifyingKey::from_public_key_der(spki).expect("the request key");
    let signature = DerSignature::from_bytes(&signed.signature).expect("a DER signature");
    assert!(key.verify(&literal, &signature).is_ok());
    // The signature covers the request id: under another id it does not verify.
    let other = RequestId::parse(&"0".repeat(64)).expect("an id");
    let moved = poll_message(&other, &signed.nonce);
    assert!(key.verify(&moved, &signature).is_err());
}
