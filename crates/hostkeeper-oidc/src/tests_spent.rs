//! The spent `jti`s the caller keeps ([`super::SpentJtis`]): what a verifier made over them
//! refuses, what it drops when it is made, what it does when an id cannot be recorded, and when
//! the record answers that another verifier spent it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::testkit::{
    NOW, STATIC_ISSUER, SpentInMemory, StaticKeys, clock, config_text, good_claims, jwks_of, keys,
    mint,
};
use super::{LEEWAY_SECS, MAX_REMEMBERED_JTIS, OidcConfig, Refusal, Verifier};

/// A verifier for the static issuer over `spent`, at [`NOW`].
fn over(spent: &Arc<SpentInMemory>) -> Verifier {
    let (clock, _) = clock();
    Verifier::new(
        OidcConfig::parse(&config_text(STATIC_ISSUER)).expect("a config"),
        Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
        Duration::from_secs(30),
        clock,
        Arc::clone(spent) as Arc<dyn super::SpentJtis>,
    )
}

/// A token of the static issuer whose `jti` is `jti`.
fn token_with(jti: &str) -> String {
    let mut claims = good_claims(STATIC_ISSUER);
    claims["jti"] = json!(jti);
    mint(&keys()[0], &claims)
}

/// Catches: an id spent only in memory, so a verifier made again (a restart) over the same
/// record accepts the token a second time; and an id recorded with another expiry than the
/// token's `exp` plus the leeway.
#[tokio::test]
async fn a_verifier_made_again_over_the_record_refuses_a_spent_token() {
    let spent = SpentInMemory::holding(Vec::new());
    let token = token_with("spent-once");
    over(&spent).verify(&token).await.expect("the first use");
    assert_eq!(
        spent.entries(),
        [("spent-once".to_owned(), NOW + 300 + LEEWAY_SECS)]
    );
    assert_eq!(over(&spent).verify(&token).await, Err(Refusal::Replayed));
}

/// Catches: an expired id kept when the verifier is made (it would fill the cache for ever),
/// or a live one dropped with it.
#[tokio::test]
async fn an_expired_id_is_dropped_when_the_verifier_is_made() {
    let mut held: Vec<(String, u64)> = (0..MAX_REMEMBERED_JTIS)
        .map(|n| (format!("old-{n}"), NOW))
        .collect();
    held.push(("live".to_owned(), NOW + 1));
    let verifier = over(&SpentInMemory::holding(held));
    assert_eq!(
        verifier.verify(&token_with("new")).await.map(|v| v.jti),
        Ok("new".to_owned())
    );
    assert_eq!(
        verifier.verify(&token_with("live")).await,
        Err(Refusal::Replayed)
    );
}

/// Catches: a full cache that makes room by forgetting a live id, or that records the new id
/// it refuses.
#[tokio::test]
async fn a_full_cache_loaded_from_the_record_refuses_a_new_token_and_forgets_none() {
    let held: Vec<(String, u64)> = (0..MAX_REMEMBERED_JTIS)
        .map(|n| (format!("held-{n}"), NOW + 1_000))
        .collect();
    let spent = SpentInMemory::holding(held.clone());
    let verifier = over(&spent);
    assert_eq!(
        verifier.verify(&token_with("new")).await,
        Err(Refusal::ReplayCacheFull)
    );
    assert_eq!(
        verifier.verify(&token_with("held-0")).await,
        Err(Refusal::Replayed)
    );
    assert_eq!(
        verifier
            .verify(&token_with(&format!("held-{}", MAX_REMEMBERED_JTIS - 1)))
            .await,
        Err(Refusal::Replayed)
    );
    assert_eq!(spent.entries(), held);
}

/// Catches: a token accepted while its id could not be recorded (a restart would accept it
/// again), or an id kept in memory after its record failed (the token could then never be used,
/// though it was refused).
#[tokio::test]
async fn an_id_that_cannot_be_recorded_refuses_the_token_and_spends_nothing() {
    let spent = SpentInMemory::holding(Vec::new());
    let verifier = over(&spent);
    let token = token_with("unrecorded");
    spent.fail(true);
    assert_eq!(
        verifier.verify(&token).await,
        Err(Refusal::ReplayCacheUnavailable)
    );
    assert!(spent.entries().is_empty());
    assert_eq!(
        Refusal::ReplayCacheUnavailable.name(),
        "replay_cache_unavailable"
    );
    spent.fail(false);
    verifier
        .verify(&token)
        .await
        .expect("accepted once recorded");
    assert_eq!(verifier.verify(&token).await, Err(Refusal::Replayed));
}

/// Proves: an id another verifier over the same record spent after this one was made, which this
/// one's memory never held, is refused `replayed` because the record refuses it, not
/// `replay_cache_unavailable`; and this verifier then remembers it, refusing it again without
/// asking the record (here made to fail).
///
/// Catches: a replay accepted because the memory that decides it is one coordinator's while the
/// record is shared, a record's refusal of a spent id answered as the record being down, or a
/// memory that never learns what the record refused.
#[tokio::test]
async fn an_id_spent_through_another_verifier_is_refused_as_replayed_and_remembered() {
    let spent = SpentInMemory::holding(Vec::new());
    let (first, second) = (over(&spent), over(&spent));
    let token = token_with("spent-elsewhere");
    first.verify(&token).await.expect("the first use");
    assert_eq!(second.verify(&token).await, Err(Refusal::Replayed));
    spent.fail(true);
    assert_eq!(second.verify(&token).await, Err(Refusal::Replayed));
    assert_eq!(
        spent.entries(),
        [("spent-elsewhere".to_owned(), NOW + 300 + LEEWAY_SECS)]
    );
}
