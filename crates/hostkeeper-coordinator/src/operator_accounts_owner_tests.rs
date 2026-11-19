//! Who an API token belongs to, through the `ApiTokens` and `Users` handlers: the owner a creation
//! names or the caller, the refusal of an owner who is no user, a token that stops at once when
//! its owner is disabled or deleted, and the audit of a call made with a token, by the owner with
//! the token named. Time is the number the test sets (`World::at`); no test sleeps.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ApiTokensRefusal as T;

use super::tests::{operator, said, token};
use crate::login_support::{START, World};
use crate::operator::Caller;
use crate::service::Unauthorized;
use crate::testutil::OPERATOR;
use crate::tokens::{Operator, TokenName};

/// `text` as a name.
fn name(text: &str) -> TokenName {
    text.parse().expect("a name")
}

/// A request for the token `name` owned by `owner`.
fn owned(name: &str, owner: &str) -> proto::CreateApiTokenRequest {
    proto::CreateApiTokenRequest {
        name: name.to_owned(),
        owner: Some(owner.to_owned()),
    }
}

/// An audit entry's `(by, action, subject, api_token, reason)`.
type Entry = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// The audit's entries, in order.
fn entries(world: &World) -> Vec<Entry> {
    world
        .audit()
        .into_iter()
        .map(|entry| {
            (
                entry.by,
                entry.action,
                entry.subject,
                entry.api_token,
                entry.reason,
            )
        })
        .collect()
}

fn some(text: &str) -> Option<String> {
    Some(text.to_owned())
}

/// Proves: a token is owned by the user the request names, whoever calls (a token of the tokens
/// file, or a user for another user, a disabled one too), or by the caller when none is named;
/// a caller that is a token of the tokens file and names no owner, and an owner who is no user
/// (nor a token's name, nor a tokens file's), are refused `unknown_owner` and nothing is written
/// or audited; the creation is audited by the caller with the owner as its reason.
///
/// Catches: a token owned by a tokens file name (which would act as no one, or as that name), an
/// owner taken from the request without a check, and a creation by one person for another that
/// the audit does not tell apart.
#[tokio::test]
async fn a_token_is_owned_by_the_user_named_or_else_by_the_caller() {
    let world = World::new("accounts-token-owner");
    world.at(START + 3);
    let c = &world.coordinator;
    let file = operator("operator");
    for (caller, request) in [
        (&file, token("robot")),
        (&file, owned("robot", "nobody")),
        (&file, owned("robot", "operator")),
        (&operator("alice"), owned("robot", "nobody")),
    ] {
        assert_eq!(
            c.api_tokens_create(caller, &request)
                .expect_err("no such owner"),
            T::UnknownOwner,
            "{request:?}"
        );
    }
    assert!(world.audit().is_empty());
    assert_eq!(c.accounts().snapshot().tokens().count(), 0);

    let made = [
        c.api_tokens_create(&file, &owned("for-bob", "bob")),
        c.api_tokens_create(&operator("alice"), &owned("for-dora", "dora")),
        c.api_tokens_create(&operator("alice"), &token("mine")),
    ]
    .map(|made| made.expect("made").token.expect("a token"));
    let kept: Vec<(&str, &str, &str)> = made
        .iter()
        .map(|t| (t.name.as_str(), t.owner.as_str(), t.created_by.as_str()))
        .collect();
    assert_eq!(
        kept,
        [
            ("for-bob", "bob", "operator"),
            ("for-dora", "dora", "alice"),
            ("mine", "alice", "alice"),
        ]
    );
    let stored = c.accounts().snapshot();
    assert_eq!(
        stored.token("for-bob").map(|t| t.owner.as_str()),
        Some("bob")
    );
    assert_eq!(
        entries(&world),
        [
            (
                "operator".to_owned(),
                "api_token_create".to_owned(),
                some("for-bob"),
                None,
                some("owner bob")
            ),
            (
                "alice".to_owned(),
                "api_token_create".to_owned(),
                some("for-dora"),
                None,
                some("owner dora")
            ),
            (
                "alice".to_owned(),
                "api_token_create".to_owned(),
                some("mine"),
                None,
                some("owner alice")
            ),
        ]
    );
}

/// Proves: a token whose owner is disabled is refused as no bearer from that moment, and works
/// again once the owner is enabled; deleting the owner removes their tokens (and no other user's),
/// audits `user_delete` then an `api_token_revoke` of each with the reason `owner deleted`, and a
/// user made again under the name does not bring the token back.
///
/// Catches: a disabled or deleted person's script that keeps working, a deletion that leaves a
/// token for the next user of the name, and a token removed with no audit line.
#[tokio::test]
async fn a_token_ends_at_once_when_its_owner_is_disabled_or_deleted() {
    let world = World::new("accounts-token-owner-gone");
    let c = &world.coordinator;
    let bobs = c
        .api_tokens_create(&operator("alice"), &owned("bob-bot", "bob"))
        .expect("made")
        .secret;
    let alices = c
        .api_tokens_create(&operator("alice"), &token("alice-bot"))
        .expect("made")
        .secret;
    let as_bob = Ok(Operator::by_token(name("bob"), name("bob-bot")));
    assert_eq!(c.operator(Some(&bobs)), as_bob);

    c.users_disable(
        &operator("alice"),
        &proto::DisableUserRequest {
            name: "bob".to_owned(),
        },
    )
    .expect("disabled");
    assert_eq!(c.operator(Some(&bobs)), Err(Unauthorized));
    c.users_enable(
        &operator("alice"),
        &proto::EnableUserRequest {
            name: "bob".to_owned(),
        },
    )
    .expect("enabled");
    assert_eq!(c.operator(Some(&bobs)), as_bob);

    c.users_delete(
        &operator("alice"),
        &proto::DeleteUserRequest {
            name: "bob".to_owned(),
        },
    )
    .expect("deleted");
    assert_eq!(c.operator(Some(&bobs)), Err(Unauthorized));
    assert!(c.accounts().snapshot().token("bob-bot").is_none());
    assert_eq!(
        c.operator(Some(&alices)),
        Ok(Operator::by_token(name("alice"), name("alice-bot")))
    );
    c.users_create(
        &operator("alice"),
        &proto::CreateUserRequest {
            name: "bob".to_owned(),
        },
    )
    .expect("a new bob");
    assert_eq!(c.operator(Some(&bobs)), Err(Unauthorized));
    let audit = entries(&world);
    assert_eq!(
        audit[4..6],
        [
            (
                "alice".to_owned(),
                "user_delete".to_owned(),
                some("bob"),
                None,
                None
            ),
            (
                "alice".to_owned(),
                "api_token_revoke".to_owned(),
                some("bob-bot"),
                None,
                some("owner deleted")
            ),
        ]
    );
    assert_eq!(audit.len(), 7, "{audit:?}");
}

/// Proves: a call made with an API token is audited by the token's owner, with the token as the
/// entry's `api_token`, and is checked as the owner (resetting the owner's own password is
/// `own_password`, as it is for the owner signed in); a call made with a token of the tokens file
/// or as a signed-in user carries no `api_token`.
///
/// Catches: an audit that names the token as the actor (so "who did that" has two answers for one
/// person), one that drops the token, and a check that reads the token's name in place of the
/// owner's.
#[tokio::test]
async fn a_call_made_with_a_token_is_audited_by_its_owner_with_the_token_named() {
    let world = World::new("accounts-token-owner-audit");
    let c = &world.coordinator;
    let secret = c
        .api_tokens_create(&operator("alice"), &token("alice-bot"))
        .expect("made")
        .secret;
    let with_token = Caller::Operator(c.operator(Some(&secret)).expect("a bearer"));
    c.users_create(
        &with_token,
        &proto::CreateUserRequest {
            name: "carol".to_owned(),
        },
    )
    .expect("made");
    assert_eq!(
        c.users_reset_password(
            &with_token,
            &proto::ResetPasswordRequest {
                name: "alice".to_owned(),
            },
        )
        .expect_err("alice's own"),
        proto::UsersRefusal::OwnPassword
    );
    let file = Caller::Operator(c.operator(Some(OPERATOR)).expect("a bearer"));
    c.users_disable(
        &file,
        &proto::DisableUserRequest {
            name: "carol".to_owned(),
        },
    )
    .expect("disabled");
    assert_eq!(
        world.said()[1..],
        [
            said("alice", "user_create", "carol"),
            said("operator", "user_disable", "carol"),
        ]
    );
    let tokens: Vec<Option<String>> = world
        .audit()
        .into_iter()
        .map(|entry| entry.api_token)
        .collect();
    assert_eq!(tokens, [None, some("alice-bot"), None]);
}
