//! What the operator login adds to the wire, new since the committed baseline
//! (`proto/compat/baseline.binpb`, the release before): the `Users` and `ApiTokens` services of the
//! operator API with their messages, `UsersRefusal`, `ApiTokensRefusal` and `ApiTokenSource`
//! (`proto/hostkeeper/operator/v1/accounts.proto`).
//!
//! Nothing existing changed: the release before knows none of them, a reader of it that meets one
//! of the new messages keeps every byte, and this build reads what that release wrote (a message of
//! no field) as the default of each. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// `message` encoded, after checking that the generated type reads it back as it was.
fn round_trip<M: prost::Message + Default + PartialEq + std::fmt::Debug>(message: &M) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    assert_eq!(&M::decode(bytes.as_slice()).expect("decodes"), message);
    bytes
}

/// A user with every field set.
fn user() -> op::UserInfo {
    op::UserInfo {
        name: "carol".to_owned(),
        enabled: true,
        must_change: true,
        created_by: "alice".to_owned(),
        created_unix: 1_000,
        last_login_unix: Some(1_500),
        password_valid_until_unix: Some(4_600),
    }
}

/// A token with every field set.
fn api_token() -> op::ApiTokenInfo {
    op::ApiTokenInfo {
        name: "robot".to_owned(),
        source: op::ApiTokenSource::Store.into(),
        created_by: "alice".to_owned(),
        created_unix: 1_000,
    }
}

/// One message of each type the login added, every field set, by full name.
fn new_messages() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("hostkeeper.operator.v1.UserInfo", round_trip(&user())),
        (
            "hostkeeper.operator.v1.ApiTokenInfo",
            round_trip(&api_token()),
        ),
        (
            "hostkeeper.operator.v1.ListUsersResponse",
            round_trip(&op::ListUsersResponse {
                users: vec![user()],
            }),
        ),
        (
            "hostkeeper.operator.v1.CreateUserRequest",
            round_trip(&op::CreateUserRequest {
                name: "carol".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.CreateUserResponse",
            round_trip(&op::CreateUserResponse {
                user: Some(user()),
                password: "Abcdefghjkmnpqrstuvwxyz2".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.ResetPasswordResponse",
            round_trip(&op::ResetPasswordResponse {
                user: Some(user()),
                password: "Abcdefghjkmnpqrstuvwxyz2".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.DisableUserResponse",
            round_trip(&op::DisableUserResponse { user: Some(user()) }),
        ),
        (
            "hostkeeper.operator.v1.EnableUserResponse",
            round_trip(&op::EnableUserResponse { user: Some(user()) }),
        ),
        (
            "hostkeeper.operator.v1.ListApiTokensResponse",
            round_trip(&op::ListApiTokensResponse {
                tokens: vec![api_token()],
            }),
        ),
        (
            "hostkeeper.operator.v1.CreateApiTokenResponse",
            round_trip(&op::CreateApiTokenResponse {
                token: Some(api_token()),
                secret: "hkt_secret".to_owned(),
            }),
        ),
        (
            "hostkeeper.operator.v1.RevokeApiTokenRequest",
            round_trip(&op::RevokeApiTokenRequest {
                name: "robot".to_owned(),
            }),
        ),
    ]
}

/// Catches: a message, enum or service of the login that the release before already knows, or that
/// does not read back as written, or that a reader of that release drops: it knows none of them,
/// and a message it reads with no field keeps every byte of the new ones.
#[test]
fn the_account_messages_are_new_and_an_older_reader_keeps_their_bytes() {
    let older = baseline_pool();
    let blind = older
        .get_message_by_name("hostkeeper.operator.v1.ListHostsRequest")
        .expect("a message of the release before");
    assert_eq!(blind.fields().count(), 0);
    let messages = new_messages();
    assert_eq!(messages.len(), 11);
    for (name, bytes) in messages {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        let read = DynamicMessage::decode(blind.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(read.encode_to_vec(), bytes, "{name}");
    }
    for name in ["UsersRefusal", "ApiTokensRefusal", "ApiTokenSource"] {
        assert!(
            older
                .get_enum_by_name(&format!("hostkeeper.operator.v1.{name}"))
                .is_none(),
            "{name} is new"
        );
    }
    for name in ["Users", "ApiTokens"] {
        assert!(
            older
                .get_service_by_name(&format!("hostkeeper.operator.v1.{name}"))
                .is_none(),
            "{name} is new"
        );
    }
}

/// Catches: a user that tells a user who never signed in from one who signed in at second 0, and a
/// user with no temporary password from one whose password is good until second 0: the two
/// optional times are absent when unset and present when set, and a message of the release before
/// (no field at all) reads as a user with none of them.
#[test]
fn a_user_tells_an_unset_time_from_a_time() {
    let unset = op::UserInfo {
        name: "carol".to_owned(),
        ..op::UserInfo::default()
    };
    let set = op::UserInfo {
        last_login_unix: Some(0),
        password_valid_until_unix: Some(0),
        ..unset.clone()
    };
    let (unset_bytes, set_bytes) = (round_trip(&unset), round_trip(&set));
    assert!(set_bytes.len() > unset_bytes.len());
    let read = op::UserInfo::decode(unset_bytes.as_slice()).expect("decodes");
    assert_eq!(
        (read.last_login_unix, read.password_valid_until_unix),
        (None, None)
    );
    let read = op::UserInfo::decode(set_bytes.as_slice()).expect("decodes");
    assert_eq!(
        (read.last_login_unix, read.password_valid_until_unix),
        (Some(0), Some(0))
    );
    let empty = op::UserInfo::decode(&[][..]).expect("decodes");
    assert_eq!(empty, op::UserInfo::default());
}

/// Catches: a token source that a reader of this build cannot tell from no source: the zero is
/// `UNSPECIFIED`, the two sources are 1 and 2, and an unknown number is read back as the number.
#[test]
fn a_token_source_has_an_unspecified_zero_and_two_sources() {
    assert_eq!(op::ApiTokenSource::Unspecified as i32, 0);
    assert_eq!(op::ApiTokenSource::Store as i32, 1);
    assert_eq!(op::ApiTokenSource::TokensFile as i32, 2);
    let future = op::ApiTokenInfo {
        source: 9,
        ..op::ApiTokenInfo::default()
    };
    let read = op::ApiTokenInfo::decode(round_trip(&future).as_slice()).expect("decodes");
    assert_eq!(read.source, 9);
    assert_eq!(read.source(), op::ApiTokenSource::Unspecified);
}
