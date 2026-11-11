//! The `ApiTokens` handlers: what a token is made of, what is kept of it, whose bearer it is,
//! and the names it cannot take. Time is the number the test sets (`World::at`).

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ApiTokensRefusal as T;
use hyper::StatusCode;

use super::TOKEN_PREFIX;
use super::tests::{fixed, login_status, operator, said, token};
use crate::login_support::{PASSWORD, START, World, settings};
use crate::ops_file::digest;
use crate::testutil::{OPERATOR, ci_token};

/// Proves: a token is made as `hkt_` and 43 base64url characters, shown once; the store keeps its
/// SHA-256 and the creator and time, never the token; the list shows its name, source, creator
/// and time and nothing that could be used; the audit holds exactly the creation.
#[tokio::test]
async fn a_created_token_is_shown_once_and_only_its_digest_is_kept() {
    let world = World::new("accounts-token-create");
    world.at(START + 7);
    let made = world
        .coordinator
        .api_tokens_create(&operator("ops"), &token("robot"))
        .expect("created");
    assert_eq!(made.secret.len(), TOKEN_PREFIX.len() + 43);
    let body = made.secret.strip_prefix(TOKEN_PREFIX).expect("a prefix");
    assert_eq!(URL_SAFE_NO_PAD.decode(body).expect("base64url").len(), 32);
    let stored = world
        .coordinator
        .accounts()
        .snapshot()
        .token("robot")
        .cloned()
        .expect("stored");
    assert_eq!(stored.sha256, digest(made.secret.as_bytes()));
    assert_eq!(
        (stored.created_by.as_str(), stored.created_at),
        ("ops", START + 7)
    );
    assert_eq!(
        made.token,
        Some(proto::ApiTokenInfo {
            name: "robot".to_owned(),
            source: proto::ApiTokenSource::Store.into(),
            created_by: "ops".to_owned(),
            created_unix: START + 7,
        })
    );
    let listed = world
        .coordinator
        .api_tokens_list(&operator("ops"), &proto::ListApiTokensRequest {})
        .expect("listed")
        .tokens;
    assert_eq!(
        listed,
        [
            proto::ApiTokenInfo {
                name: "operator".to_owned(),
                source: proto::ApiTokenSource::TokensFile.into(),
                created_by: String::new(),
                created_unix: 0,
            },
            made.token.expect("a token"),
        ]
    );
    assert_eq!(world.said(), [said("ops", "api_token_create", "robot")]);
}

/// Proves: a stored token is an operator's bearer under its name, as a token of the tokens file
/// is, from the call that made it to the call that revokes it, and not after; revoking leaves the
/// tokens file's; a user's password is no bearer.
#[tokio::test]
async fn a_stored_token_authenticates_under_its_name_until_it_is_revoked() {
    let world = World::new("accounts-token-bearer");
    let c = &world.coordinator;
    let made = c
        .api_tokens_create(&operator("ops"), &token("robot"))
        .expect("created");
    assert_eq!(
        c.operator(Some(&made.secret)).expect("a bearer").as_str(),
        "robot"
    );
    assert_eq!(
        c.operator(Some(OPERATOR)).expect("a bearer").as_str(),
        "operator"
    );
    assert!(c.operator(Some(PASSWORD)).is_err());
    assert!(c.operator(Some(&ci_token())).is_err());
    assert!(c.operator(None).is_err());
    c.api_tokens_revoke(
        &operator("ops"),
        &proto::RevokeApiTokenRequest {
            name: "robot".to_owned(),
        },
    )
    .expect("revoked");
    assert!(c.operator(Some(&made.secret)).is_err());
    assert_eq!(
        c.operator(Some(OPERATOR)).expect("a bearer").as_str(),
        "operator"
    );
    assert_eq!(
        world.said(),
        [
            said("ops", "api_token_create", "robot"),
            said("ops", "api_token_revoke", "robot"),
        ]
    );
}

/// Proves: a token cannot sign in: the login route reads passwords only, so the token of a
/// stored token used as the password of a user of its name, or of any, is refused as any wrong
/// password is.
#[tokio::test]
async fn a_token_cannot_sign_in() {
    let world = World::new("accounts-token-login");
    let made = world
        .coordinator
        .api_tokens_create(&operator("ops"), &token("robot"))
        .expect("created");
    for name in ["robot", "alice"] {
        assert_eq!(
            login_status(&world, name, &made.secret).await,
            StatusCode::UNAUTHORIZED,
            "{name}"
        );
    }
}

/// Proves: a token name that is not a name, one a user has, one the tokens file has and one a
/// stored token has are each refused, and nothing is written.
#[tokio::test]
async fn a_token_cannot_be_made_under_a_name_that_is_not_free() {
    let world = World::new("accounts-token-names");
    world
        .coordinator
        .api_tokens_create(&operator("ops"), &token("robot"))
        .expect("created");
    let before = world.coordinator.accounts().snapshot();
    for (name, why) in [
        ("", T::InvalidRequest),
        ("Bad Name", T::InvalidRequest),
        ("coordinator", T::InvalidRequest),
        ("alice", T::NameTaken),
        ("operator", T::NameTaken),
        ("robot", T::NameTaken),
    ] {
        assert_eq!(
            world
                .coordinator
                .api_tokens_create(&operator("ops"), &token(name))
                .expect_err("refused"),
            why,
            "{name:?}"
        );
    }
    assert!(world.coordinator.accounts().snapshot() == before);
    assert_eq!(world.said(), [said("ops", "api_token_create", "robot")]);
}

/// Proves: a token whose digest is one the tokens file holds is refused and not made (the
/// coordinator's random bytes are not random), so one secret never answers to two names.
#[tokio::test]
async fn a_token_that_is_one_of_the_tokens_files_is_not_made() {
    let secret = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode([7u8; 32]));
    let world = World::build(
        "accounts-token-same",
        settings(),
        fixed(7),
        |coordinator, _| {
            let mut coordinator = coordinator;
            coordinator.tokens = secret.clone().into();
            coordinator
        },
    );
    assert_eq!(
        world
            .coordinator
            .api_tokens_create(&operator("ops"), &token("robot"))
            .expect_err("refused"),
        T::StoreFailed
    );
    assert!(
        world
            .coordinator
            .accounts()
            .snapshot()
            .token("robot")
            .is_none()
    );
    assert!(world.said().is_empty());
}
