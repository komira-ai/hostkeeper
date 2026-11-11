//! The `Users` and `ApiTokens` handlers: what a call makes and refuses, what it audits, what it
//! ends, and what is never in an answer or an entry after the one call that shows it. Time is
//! the number the test sets (`World::at`); no test sleeps.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ApiTokensRefusal as T;
use hostkeeper_proto::hostkeeper::operator::v1::UsersRefusal as U;
use hyper::StatusCode;
use serde_json::json;

use super::{ALPHABET, PASSWORD_CHARS, random_password};
use crate::accounts::AccountStore;
use crate::enrollment::Random;
use crate::initial_admin::InitialAdmin;
use crate::login_support::{
    HOUR, PASSWORD, START, World, cheap, counting, page_post, settings, users,
};
use crate::operator::Caller;
use crate::ops::{OpsState, OpsStore};
use crate::store::{Fake, audit_failure, store_failure};
use crate::testutil::{OPERATOR, Rig};

type Said = (String, String, Option<String>, Option<String>);

pub(super) fn operator(name: &str) -> Caller {
    Caller::Operator(name.parse().expect("a name"))
}

pub(super) fn said(by: &str, action: &str, subject: &str) -> Said {
    (
        by.to_owned(),
        action.to_owned(),
        Some(subject.to_owned()),
        None,
    )
}

fn create(name: &str) -> proto::CreateUserRequest {
    proto::CreateUserRequest {
        name: name.to_owned(),
    }
}

pub(super) fn token(name: &str) -> proto::CreateApiTokenRequest {
    proto::CreateApiTokenRequest {
        name: name.to_owned(),
    }
}

/// The status a login of `name` with `password` is answered with.
pub(super) async fn login_status(world: &World, name: &str, password: &str) -> StatusCode {
    let body = json!({ "username": name, "password": password }).to_string();
    world.send(page_post("/v1/login", None, &body)).await.status
}

/// The status of the session `GET /v1/session` of a signed-in page answers.
async fn session_status(world: &World, session: &crate::login_support::Signed) -> StatusCode {
    world.send(session.get("/v1/session")).await.status
}

/// A coordinator with no sign-in, over the fake store holding the world's users.
fn without_sign_in(name: &str) -> (Rig, crate::service::Coordinator, Fake) {
    let rig = Rig::new(name);
    let fake = Fake::new();
    let coordinator = Rig::start(&rig.dir, &rig.clock)
        .with_ops(OpsStore::over(Arc::new(fake.clone()), OpsState::default()))
        .with_accounts(AccountStore::over(Arc::new(fake.clone()), users()));
    (rig, coordinator, fake)
}

/// Random bytes that are the same at every call.
pub(super) fn fixed(byte: u8) -> Random {
    Box::new(move |buffer| {
        buffer.fill(byte);
        true
    })
}

/// Proves: the temporary password is 24 characters of the alphabet without look-alikes, each
/// character of the alphabet equally likely: a byte at or above 228 is dropped and another drawn
/// (a source of such bytes only is asked again), and a source that gives no bytes gives no
/// password, at the first draw or after some.
#[test]
fn a_temporary_password_is_drawn_without_favouring_a_character() {
    assert_eq!(ALPHABET.len(), 57);
    assert!(
        b"01IOl"
            .iter()
            .all(|look_alike| !ALPHABET.contains(look_alike))
    );
    let ascending = |bytes: &mut [u8]| {
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::try_from(i).unwrap_or(0);
        }
        true
    };
    assert_eq!(
        random_password(ascending).as_deref(),
        Some("ABCDEFGHJKLMNPQRSTUVWXYZ")
    );
    let high_then_zero = {
        let drawn = AtomicBool::new(false);
        move |bytes: &mut [u8]| {
            bytes.fill(if drawn.swap(true, Ordering::SeqCst) {
                0
            } else {
                255
            });
            true
        }
    };
    assert_eq!(
        random_password(high_then_zero),
        Some("A".repeat(PASSWORD_CHARS))
    );
    assert_eq!(random_password(|_| false), None);
    let some_then_none = {
        let drawn = AtomicBool::new(false);
        move |bytes: &mut [u8]| {
            if drawn.swap(true, Ordering::SeqCst) {
                return false;
            }
            bytes.fill(255);
            bytes[..10].fill(0);
            true
        }
    };
    assert_eq!(random_password(some_then_none), None);
}

/// Proves: a created user is told its temporary password once, in the answer; the store holds the
/// hash alone; it signs in with it up to the second before its hour is out and not after; and the
/// audit holds exactly the creation, with no password, hash or token in it.
#[tokio::test]
async fn a_created_user_has_a_temporary_password_good_for_an_hour() {
    let world = World::new("accounts-create");
    world.at(START + 10);
    let answer = world
        .coordinator
        .users_create(&operator("ops"), &create("carol"))
        .expect("created");
    assert_eq!(answer.password.len(), PASSWORD_CHARS);
    assert!(answer.password.bytes().all(|byte| ALPHABET.contains(&byte)));
    assert_eq!(
        answer.user,
        Some(proto::UserInfo {
            name: "carol".to_owned(),
            enabled: true,
            must_change: true,
            created_by: "ops".to_owned(),
            created_unix: START + 10,
            last_login_unix: None,
            password_valid_until_unix: Some(START + 10 + HOUR),
        })
    );
    let stored = world
        .coordinator
        .accounts()
        .snapshot()
        .user("carol")
        .cloned()
        .expect("stored");
    assert!(stored.hash.starts_with("$argon2id$"));
    assert!(!stored.hash.contains(&answer.password));
    assert_eq!(world.said(), [said("ops", "user_create", "carol")]);
    assert_eq!(world.fake.accounts().user("carol"), Some(&stored));

    world.at(START + 10 + HOUR);
    assert_eq!(
        login_status(&world, "carol", &answer.password).await,
        StatusCode::UNAUTHORIZED,
        "the second the hour is out"
    );
    world.at(START + 10 + HOUR - 1);
    let session = world.sign_in("carol", &answer.password).await;
    let told = world.send(session.get("/v1/session")).await.json();
    assert_eq!(told["must_change"], json!(true));
}

/// Proves: a user whose name is not a name, is one the audit uses for something else, or is
/// already a user's, a stored token's or one of the tokens file's, is refused and nothing is
/// written: not the accounts and not the audit beyond the token that made one name taken.
#[tokio::test]
async fn a_user_cannot_be_made_under_a_name_that_is_not_free() {
    let world = World::new("accounts-create-names");
    world
        .coordinator
        .api_tokens_create(&operator("ops"), &token("robot"))
        .expect("a token");
    let before = world.coordinator.accounts().snapshot();
    let long = "a".repeat(33);
    for (name, why) in [
        ("", U::InvalidRequest),
        ("Bad Name", U::InvalidRequest),
        (long.as_str(), U::InvalidRequest),
        ("coordinator", U::InvalidRequest),
        ("ci", U::InvalidRequest),
        ("alice", U::NameTaken),
        ("robot", U::NameTaken),
        ("operator", U::NameTaken),
    ] {
        assert_eq!(
            world
                .coordinator
                .users_create(&operator("ops"), &create(name))
                .expect_err("refused"),
            why,
            "{name:?}"
        );
    }
    assert!(world.coordinator.accounts().snapshot() == before);
    assert_eq!(world.said(), [said("ops", "api_token_create", "robot")]);
}

/// Proves: a caller who is no operator is refused every call of both services before the request
/// is looked at: no credential is `unauthorized` and a CI token `forbidden`, and nothing is
/// changed or audited.
#[tokio::test]
async fn no_call_is_served_to_a_caller_who_is_not_an_operator() {
    let world = World::new("accounts-callers");
    let before = world.coordinator.accounts().snapshot();
    let c = &world.coordinator;
    for (caller, users, tokens) in [
        (Caller::Anonymous, U::Unauthorized, T::Unauthorized),
        (Caller::Ci, U::Forbidden, T::Forbidden),
    ] {
        let name = "alice".to_owned();
        assert_eq!(
            c.users_list(&caller, &proto::ListUsersRequest {})
                .expect_err("refused"),
            users
        );
        assert_eq!(
            c.users_create(&caller, &create("carol"))
                .expect_err("refused"),
            users
        );
        assert_eq!(
            c.users_disable(&caller, &proto::DisableUserRequest { name: name.clone() })
                .expect_err("refused"),
            users
        );
        assert_eq!(
            c.users_enable(&caller, &proto::EnableUserRequest { name: name.clone() })
                .expect_err("refused"),
            users
        );
        assert_eq!(
            c.users_reset_password(&caller, &proto::ResetPasswordRequest { name: name.clone() })
                .expect_err("refused"),
            users
        );
        assert_eq!(
            c.users_delete(&caller, &proto::DeleteUserRequest { name })
                .expect_err("refused"),
            users
        );
        assert_eq!(
            c.api_tokens_list(&caller, &proto::ListApiTokensRequest {})
                .expect_err("refused"),
            tokens
        );
        assert_eq!(
            c.api_tokens_create(&caller, &token("robot"))
                .expect_err("refused"),
            tokens
        );
        assert_eq!(
            c.api_tokens_revoke(
                &caller,
                &proto::RevokeApiTokenRequest {
                    name: "robot".to_owned()
                }
            )
            .expect_err("refused"),
            tokens
        );
    }
    assert!(c.accounts().snapshot() == before);
    assert!(world.said().is_empty());
}

/// Proves: `List` tells each user's name, whether it is enabled and must change its password,
/// who made it and when, when it last signed in, and until when a temporary password works, in
/// name order, and nothing else of the record.
#[tokio::test]
async fn the_list_of_users_says_what_the_page_shows_and_no_hash() {
    let world = World::new("accounts-list");
    world.at(START + 5);
    world.sign_in("alice", PASSWORD).await;
    let listed = world
        .coordinator
        .users_list(&operator("ops"), &proto::ListUsersRequest {})
        .expect("listed")
        .users;
    let user = |name: &str, enabled: bool, must_change: bool, valid: Option<u64>| proto::UserInfo {
        name: name.to_owned(),
        enabled,
        must_change,
        created_by: "coordinator".to_owned(),
        created_unix: 1,
        last_login_unix: (name == "alice").then_some(START + 5),
        password_valid_until_unix: valid,
    };
    assert_eq!(
        listed,
        [
            user("alice", true, false, None),
            user("bob", true, false, None),
            user("dora", false, false, None),
            user("late", true, true, Some(START - 1)),
            user("newbie", true, true, Some(START + HOUR)),
        ]
    );
}

/// Proves: disabling a user ends their sessions at once and keeps them out until they are
/// enabled again, with the password they had; another user's session is untouched; the audit
/// holds each call under the caller's name about the user; and disabling a disabled user, or
/// enabling an enabled one, changes nothing.
#[tokio::test]
async fn disabling_a_user_ends_their_sessions_and_enabling_lets_them_back() {
    let world = World::new("accounts-disable");
    let alice = world.sign_in("alice", PASSWORD).await;
    let bob = world.sign_in("bob", PASSWORD).await;
    let off = world
        .coordinator
        .users_disable(
            &operator("bob"),
            &proto::DisableUserRequest {
                name: "alice".to_owned(),
            },
        )
        .expect("disabled")
        .user
        .expect("a user");
    assert!(!off.enabled);
    assert_eq!(
        session_status(&world, &alice).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(session_status(&world, &bob).await, StatusCode::OK);
    assert_eq!(
        login_status(&world, "alice", PASSWORD).await,
        StatusCode::UNAUTHORIZED
    );
    let on = world
        .coordinator
        .users_enable(
            &operator("bob"),
            &proto::EnableUserRequest {
                name: "alice".to_owned(),
            },
        )
        .expect("enabled")
        .user
        .expect("a user");
    assert!(on.enabled);
    assert_eq!(
        login_status(&world, "alice", PASSWORD).await,
        StatusCode::OK
    );
    let commits = world.fake.commits();
    let again = world
        .coordinator
        .users_enable(
            &operator("bob"),
            &proto::EnableUserRequest {
                name: "alice".to_owned(),
            },
        )
        .expect("enabled again");
    assert!(again.user.expect("a user").enabled);
    let dora = world
        .coordinator
        .users_disable(
            &operator("bob"),
            &proto::DisableUserRequest {
                name: "dora".to_owned(),
            },
        )
        .expect("a disabled user disabled");
    assert!(!dora.user.expect("a user").enabled);
    assert_eq!(
        world.fake.commits(),
        commits + 2,
        "the two audit lines, and no change of the accounts"
    );
    let calls: Vec<Said> = world
        .said()
        .into_iter()
        .filter(|(_, action, _, _)| action.starts_with("user_"))
        .collect();
    assert_eq!(
        calls,
        [
            said("bob", "user_disable", "alice"),
            said("bob", "user_enable", "alice"),
            said("bob", "user_enable", "alice"),
            said("bob", "user_disable", "dora"),
        ]
    );
}

/// Proves: the last enabled user can be neither disabled nor deleted (a farm is not locked out by
/// its own admin), the refusal is `last_enabled` and writes nothing, and a disabled user is
/// deleted without that rule.
#[tokio::test]
async fn the_last_enabled_user_can_be_neither_disabled_nor_deleted() {
    let world = World::new("accounts-last");
    let c = &world.coordinator;
    for name in ["bob", "newbie", "late"] {
        c.users_disable(
            &operator("alice"),
            &proto::DisableUserRequest {
                name: name.to_owned(),
            },
        )
        .expect("disabled");
    }
    let written = world.said();
    assert_eq!(
        c.users_disable(
            &operator("ops"),
            &proto::DisableUserRequest {
                name: "alice".to_owned()
            }
        )
        .expect_err("refused"),
        U::LastEnabled
    );
    assert_eq!(
        c.users_delete(
            &operator("ops"),
            &proto::DeleteUserRequest {
                name: "alice".to_owned()
            }
        )
        .expect_err("refused"),
        U::LastEnabled
    );
    assert!(
        c.accounts()
            .snapshot()
            .user("alice")
            .expect("alice")
            .enabled
    );
    assert_eq!(world.said(), written, "a refused call is not audited");
    c.users_delete(
        &operator("ops"),
        &proto::DeleteUserRequest {
            name: "dora".to_owned(),
        },
    )
    .expect("a disabled user is deleted");
}

/// Proves: a name that is no user's is `unknown_user` whatever it looks like, for each call that
/// takes a user, and a name that is no stored token's, a token of the tokens file included, is
/// `unknown_token`; nothing is written, audited or removed.
#[tokio::test]
async fn an_unknown_name_is_refused_and_nothing_is_written() {
    let world = World::new("accounts-unknown");
    let before = world.coordinator.accounts().snapshot();
    let c = &world.coordinator;
    for name in ["ghost", "Not A Name", ""] {
        let name = name.to_owned();
        assert_eq!(
            c.users_disable(
                &operator("ops"),
                &proto::DisableUserRequest { name: name.clone() }
            )
            .expect_err("refused"),
            U::UnknownUser
        );
        assert_eq!(
            c.users_enable(
                &operator("ops"),
                &proto::EnableUserRequest { name: name.clone() }
            )
            .expect_err("refused"),
            U::UnknownUser
        );
        assert_eq!(
            c.users_reset_password(
                &operator("ops"),
                &proto::ResetPasswordRequest { name: name.clone() }
            )
            .expect_err("refused"),
            U::UnknownUser
        );
        assert_eq!(
            c.users_delete(
                &operator("ops"),
                &proto::DeleteUserRequest { name: name.clone() }
            )
            .expect_err("refused"),
            U::UnknownUser
        );
        assert_eq!(
            c.api_tokens_revoke(&operator("ops"), &proto::RevokeApiTokenRequest { name })
                .expect_err("refused"),
            T::UnknownToken
        );
    }
    assert_eq!(
        c.api_tokens_revoke(
            &operator("ops"),
            &proto::RevokeApiTokenRequest {
                name: "operator".to_owned()
            }
        )
        .expect_err("refused"),
        T::UnknownToken,
        "a token of the tokens file is removed by editing the file"
    );
    assert_eq!(
        c.operator(Some(OPERATOR)).expect("still one").as_str(),
        "operator"
    );
    assert!(c.accounts().snapshot() == before);
    assert!(world.said().is_empty());
}

/// Proves: a reset gives the user a new temporary password, shown once, that works until the hour
/// is out (up to the second before) and makes them choose another; the old password stops
/// working; the user's sessions end; a user whose credential was voided can sign in again with
/// the new one; and the audit holds the reset under the admin's name about the user.
#[tokio::test]
async fn a_reset_gives_a_temporary_password_and_ends_the_sessions() {
    let world = World::new("accounts-reset");
    let alice = world.sign_in("alice", PASSWORD).await;
    world
        .coordinator
        .accounts()
        .change(|held| held.with_user_edited("alice", |user| user.initial_void = true))
        .expect("voided");
    world.at(START + 100);
    let answer = world
        .coordinator
        .users_reset_password(
            &operator("bob"),
            &proto::ResetPasswordRequest {
                name: "alice".to_owned(),
            },
        )
        .expect("reset");
    assert_eq!(answer.password.len(), PASSWORD_CHARS);
    let user = answer.user.expect("a user");
    assert!(user.must_change);
    assert_eq!(user.password_valid_until_unix, Some(START + 100 + HOUR));
    assert_eq!(
        session_status(&world, &alice).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login_status(&world, "alice", PASSWORD).await,
        StatusCode::UNAUTHORIZED
    );
    world.at(START + 100 + HOUR);
    assert_eq!(
        login_status(&world, "alice", &answer.password).await,
        StatusCode::UNAUTHORIZED,
        "the second the hour is out"
    );
    world.at(START + 100 + HOUR - 1);
    let session = world.sign_in("alice", &answer.password).await;
    let told = world.send(session.get("/v1/session")).await.json();
    assert_eq!(told["must_change"], json!(true));
    assert_eq!(
        world
            .said()
            .into_iter()
            .find(|(_, action, _, _)| action == "password_reset"),
        Some(said("bob", "password_reset", "alice"))
    );
}

/// Proves: a caller cannot reset their own password: it is `own_password`, nothing changes and
/// nothing is audited; a person changes their own on the page.
#[tokio::test]
async fn a_reset_of_the_callers_own_password_is_refused() {
    let world = World::new("accounts-reset-own");
    let before = world.coordinator.accounts().snapshot();
    assert_eq!(
        world
            .coordinator
            .users_reset_password(
                &operator("alice"),
                &proto::ResetPasswordRequest {
                    name: "alice".to_owned()
                }
            )
            .expect_err("refused"),
        U::OwnPassword
    );
    assert!(world.coordinator.accounts().snapshot() == before);
    assert!(world.said().is_empty());
}

/// Proves: a deleted user is gone: their sessions end, they cannot sign in (the refused login is
/// the first failure of a name that is no longer one), and the audit rows that name them stay,
/// with the deletion added.
#[tokio::test]
async fn a_deleted_user_is_gone_and_the_audit_that_names_them_stays() {
    let world = World::new("accounts-delete");
    let alice = world.sign_in("alice", PASSWORD).await;
    world
        .coordinator
        .users_delete(
            &operator("bob"),
            &proto::DeleteUserRequest {
                name: "alice".to_owned(),
            },
        )
        .expect("deleted");
    assert!(
        world
            .coordinator
            .accounts()
            .snapshot()
            .user("alice")
            .is_none()
    );
    assert_eq!(
        session_status(&world, &alice).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login_status(&world, "alice", PASSWORD).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        world.said(),
        [
            ("alice".to_owned(), "login".to_owned(), None, None),
            said("bob", "user_delete", "alice"),
            said("coordinator", "login_failed", "alice"),
        ]
    );
}

/// A world whose coordinator looks at an initial-admin file in its directory.
fn with_file(name: &str) -> World {
    World::build(name, settings(), counting(), |coordinator, rig| {
        let clock = Arc::clone(&rig.clock);
        let admin = InitialAdmin::new(
            rig.dir.path().join("initial-admin.toml"),
            HOUR,
            cheap(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
        .expect("an initial admin");
        coordinator.with_initial_admin(admin)
    })
}

fn put_file(world: &World, id: &str, mode: u32) {
    let path = world.rig.dir.path().join("initial-admin.toml");
    let _ = fs::remove_file(&path);
    fs::write(
        &path,
        format!(
            "id = \"{id}\"\nuser = \"admin\"\npassword = \"{PASSWORD}\"\nvalid_until = {}\n",
            START + HOUR
        ),
    )
    .expect("written");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("mode");
}

fn file_exists(world: &World) -> bool {
    world.rig.dir.path().join("initial-admin.toml").exists()
}

fn reset_admin(world: &World) -> Result<proto::ResetPasswordResponse, U> {
    world.coordinator.users_reset_password(
        &operator("ops"),
        &proto::ResetPasswordRequest {
            name: "admin".to_owned(),
        },
    )
}

/// Proves: resetting the password of a user whose password came from the initial-admin file
/// deletes that file, so its expiry cannot void the new password, and the file is not adopted
/// again: the reset password signs in after the second the file's hour is out.
#[tokio::test]
async fn a_reset_deletes_the_file_that_made_the_old_password() {
    let world = with_file("accounts-reset-file");
    put_file(&world, "first", 0o400);
    world.coordinator.look_at_initial_admin().expect("a look");
    assert!(file_exists(&world));
    world.at(START + 30);
    let answer = reset_admin(&world).expect("reset");
    assert!(!file_exists(&world), "the file is deleted by the reset");
    world.at(START + HOUR + 5);
    world.coordinator.look_at_initial_admin().expect("a look");
    world.sign_in("admin", &answer.password).await;
}

/// Proves: a file the reset did not come from (another id) stays, and so does one that does not
/// read; the reset is made all the same.
#[tokio::test]
async fn a_reset_leaves_a_file_it_did_not_come_from_and_one_that_does_not_read() {
    let world = with_file("accounts-reset-other-file");
    put_file(&world, "first", 0o400);
    world.coordinator.look_at_initial_admin().expect("a look");
    put_file(&world, "second", 0o400);
    reset_admin(&world).expect("reset");
    assert!(file_exists(&world), "another id's file stays");

    let world = with_file("accounts-reset-bad-file");
    put_file(&world, "first", 0o400);
    world.coordinator.look_at_initial_admin().expect("a look");
    put_file(&world, "first", 0o644);
    reset_admin(&world).expect("reset");
    assert!(file_exists(&world), "a file that does not read stays");
}

/// Proves: a store that cannot write the accounts refuses each change `store_failed` and leaves
/// them as they were; an audit that cannot be written refuses it `audit_unavailable` before the
/// accounts are touched; and once the store takes writes again the same call is made.
#[tokio::test]
async fn a_store_or_an_audit_that_cannot_write_refuses_the_change() {
    let world = World::new("accounts-store-down");
    let before = world.coordinator.accounts().snapshot();
    let c = &world.coordinator;
    world
        .fake
        .refuse(|commit| commit.accounts.is_some().then(store_failure));
    assert_eq!(
        c.users_create(&operator("ops"), &create("carol"))
            .expect_err("refused"),
        U::StoreFailed
    );
    assert_eq!(
        c.users_reset_password(
            &operator("ops"),
            &proto::ResetPasswordRequest {
                name: "alice".to_owned()
            }
        )
        .expect_err("refused"),
        U::StoreFailed
    );
    assert_eq!(
        c.api_tokens_create(&operator("ops"), &token("robot"))
            .expect_err("refused"),
        T::StoreFailed
    );
    assert!(c.accounts().snapshot() == before);
    world.fake.refuse(|_| Some(audit_failure()));
    assert_eq!(
        c.users_create(&operator("ops"), &create("carol"))
            .expect_err("refused"),
        U::AuditUnavailable
    );
    assert_eq!(
        c.api_tokens_create(&operator("ops"), &token("robot"))
            .expect_err("refused"),
        T::AuditUnavailable
    );
    assert!(c.accounts().snapshot() == before);
    world.fake.heal();
    c.users_create(&operator("ops"), &create("carol"))
        .expect("made once the store writes");
}

/// Proves: a coordinator with no sign-in, which has no random bytes and no password hashing,
/// makes no password and no token (`store_failed`, nothing changed), and still disables, enables
/// and deletes users, whose sessions it has none of.
#[tokio::test]
async fn a_coordinator_without_a_sign_in_makes_no_password_or_token() {
    let (_rig, c, _fake) = without_sign_in("accounts-no-login");
    let before = c.accounts().snapshot();
    assert_eq!(
        c.users_create(&operator("ops"), &create("carol"))
            .expect_err("refused"),
        U::StoreFailed
    );
    assert_eq!(
        c.users_reset_password(
            &operator("ops"),
            &proto::ResetPasswordRequest {
                name: "alice".to_owned()
            }
        )
        .expect_err("refused"),
        U::StoreFailed
    );
    assert_eq!(
        c.api_tokens_create(&operator("ops"), &token("robot"))
            .expect_err("refused"),
        T::StoreFailed
    );
    assert!(c.accounts().snapshot() == before);
    c.users_disable(
        &operator("ops"),
        &proto::DisableUserRequest {
            name: "bob".to_owned(),
        },
    )
    .expect("disabled");
    c.users_delete(
        &operator("ops"),
        &proto::DeleteUserRequest {
            name: "bob".to_owned(),
        },
    )
    .expect("deleted");
}

/// Proves: random bytes that cannot be had make no password and no token, nothing is changed,
/// and nothing is audited.
#[tokio::test]
async fn no_random_bytes_make_no_password_and_no_token() {
    let world = World::with("accounts-no-random", settings(), Box::new(|_| false));
    let before = world.coordinator.accounts().snapshot();
    let c = &world.coordinator;
    assert_eq!(
        c.users_create(&operator("ops"), &create("carol"))
            .expect_err("refused"),
        U::StoreFailed
    );
    assert_eq!(
        c.users_reset_password(
            &operator("ops"),
            &proto::ResetPasswordRequest {
                name: "alice".to_owned()
            }
        )
        .expect_err("refused"),
        U::StoreFailed
    );
    assert_eq!(
        c.api_tokens_create(&operator("ops"), &token("robot"))
            .expect_err("refused"),
        T::StoreFailed
    );
    assert!(c.accounts().snapshot() == before);
    assert!(world.said().is_empty());
}

/// Proves: a user whose password came from an initial-admin file is reset all the same by a
/// coordinator that is not looking at one (started without the flag since): there is no file to
/// delete and the reset is made.
#[tokio::test]
async fn a_reset_is_made_without_a_file_to_look_at() {
    let world = World::new("accounts-reset-no-flag");
    world
        .coordinator
        .accounts()
        .change(|held| {
            held.with_user_edited("alice", |user| user.initial_id = Some("first".to_owned()))
        })
        .expect("adopted once");
    let answer = world
        .coordinator
        .users_reset_password(
            &operator("bob"),
            &proto::ResetPasswordRequest {
                name: "alice".to_owned(),
            },
        )
        .expect("reset");
    world.sign_in("alice", &answer.password).await;
}
