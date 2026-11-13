//! [`Verifier::verify_open`]: the token's run is judged by the caller, before the `jti` is spent,
//! and every rule that does not name the run still holds.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use super::testkit::{
    STATIC_ISSUER, StaticKeys, WORKFLOW, WORKFLOW_REF, clock, config_text, good_claims, jwks_of,
    keys, mint, spent,
};
use super::{AllowList, Refusal, Verified, Verifier};

fn verifier(text: &str) -> Verifier {
    let (clock, _) = clock();
    Verifier::new(
        AllowList::parse(text).expect("an allow-list"),
        Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
        Duration::from_secs(30),
        clock,
        spent(),
    )
}

fn token(mutate: impl FnOnce(&mut Value)) -> String {
    let mut claims = good_claims(STATIC_ISSUER);
    mutate(&mut claims);
    mint(&keys()[0], &claims)
}

/// A token of a repository, workflow, ref and environment the allow-list file does not name is
/// checked all the same, and says its run: the file's identity rules are the caller's to judge.
#[tokio::test]
async fn a_run_the_file_does_not_name_is_judged_by_the_caller() {
    let verifier = verifier(&config_text(STATIC_ISSUER));
    let stranger = token(|c| {
        c["repository_id"] = json!("999");
        c["repository"] = json!("example-org/other");
        c["job_workflow_ref"] = json!("example-org/other/.github/workflows/x.yml@refs/heads/dev");
        c["workflow_ref"] = json!("example-org/other/.github/workflows/y.yml@refs/heads/dev");
        c["ref"] = json!("refs/heads/dev");
        c["environment"] = json!("prod");
    });
    assert_eq!(
        verifier.verify(&stranger).await.expect_err("file mode"),
        Refusal::Repository
    );
    let seen = AtomicUsize::new(0);
    let who = verifier
        .verify_open(&stranger, &|who: &Verified| {
            seen.fetch_add(1, Ordering::SeqCst);
            assert_eq!(who.repository.as_deref(), Some("example-org/other"));
            Ok(())
        })
        .await
        .expect("admitted by the caller");
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert_eq!(who.repository.as_deref(), Some("example-org/other"));
    assert_eq!(who.repository_id, "999");
    assert_eq!(
        who.workflow_ref.as_deref(),
        Some("example-org/other/.github/workflows/y.yml@refs/heads/dev")
    );
    assert_eq!(
        who.job_workflow_ref,
        "example-org/other/.github/workflows/x.yml@refs/heads/dev"
    );
    assert_eq!(who.git_ref.as_deref(), Some("refs/heads/dev"));
    assert_eq!(who.environment.as_deref(), Some("prod"));
    assert_eq!((who.services, who.host_groups), (None, None));
}

/// A token the caller refuses is refused as the caller says and its `jti` is not spent: the same token
/// is admitted when the caller admits it, and refused `replayed` the time after.
#[tokio::test]
async fn a_token_the_caller_refuses_is_not_spent() {
    let verifier = verifier(&config_text(STATIC_ISSUER));
    let t = token(|_| {});
    let refused = verifier
        .verify_open(&t, &|_: &Verified| Err(Refusal::Workflow))
        .await;
    assert_eq!(refused.expect_err("refused"), Refusal::Workflow);
    verifier
        .verify_open(&t, &|_: &Verified| Ok(()))
        .await
        .expect("not spent by the refusal");
    let again = verifier.verify_open(&t, &|_: &Verified| Ok(())).await;
    assert_eq!(again.expect_err("spent"), Refusal::Replayed);
}

/// The rules that do not name the run hold in the open check: audience, event, runner and the
/// signed times, and a token with no `job_workflow_ref` is malformed.
#[tokio::test]
async fn the_other_rules_still_hold() {
    let verifier = verifier(&config_text(STATIC_ISSUER));
    let cases: [(&str, Value, Refusal); 4] = [
        ("aud", json!("elsewhere"), Refusal::Audience),
        ("event_name", json!("pull_request"), Refusal::Event),
        ("runner_environment", json!("self-hosted"), Refusal::Runner),
        ("exp", json!(1), Refusal::Expired),
    ];
    for (claim, value, want) in cases {
        let t = token(|c| c[claim] = value);
        let got = verifier.verify_open(&t, &|_: &Verified| Ok(())).await;
        assert_eq!(got.expect_err(claim), want, "{claim}");
    }
    let t = token(|c| {
        c.as_object_mut()
            .expect("object")
            .remove("job_workflow_ref");
    });
    let got = verifier.verify_open(&t, &|_: &Verified| Ok(())).await;
    assert_eq!(
        got.expect_err("no workflow"),
        Refusal::MissingClaim("job_workflow_ref")
    );
}

/// A token missing the claims only a caller reads still verifies, with those left empty.
#[tokio::test]
async fn the_claims_of_the_run_may_be_absent() {
    let verifier = verifier(&config_text(STATIC_ISSUER));
    let t = token(|c| {
        let o = c.as_object_mut().expect("object");
        for name in ["repository", "workflow_ref", "environment", "ref"] {
            o.remove(name);
        }
    });
    let who = verifier
        .verify_open(&t, &|_: &Verified| Ok(()))
        .await
        .expect("admitted");
    assert_eq!(
        (
            who.repository,
            who.workflow_ref,
            who.environment,
            who.git_ref
        ),
        (None, None, None, None)
    );
    assert_eq!(who.job_workflow_ref, WORKFLOW);
}

/// The file mode names the run too: a token it admits carries the repository claims.
#[tokio::test]
async fn the_file_mode_names_the_run_as_well() {
    let who = verifier(&config_text(STATIC_ISSUER))
        .verify(&token(|_| {}))
        .await
        .expect("admitted");
    assert_eq!(who.repository.as_deref(), Some("example-org/example-repo"));
    assert_eq!(who.repository_id, "4242");
    assert_eq!(who.workflow_ref.as_deref(), Some(WORKFLOW_REF));
    assert_eq!(who.environment.as_deref(), Some("canary"));
}
