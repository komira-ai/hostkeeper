//! What tokens with owners add to the wire: `ApiTokenInfo.owner` (field 5),
//! `CreateApiTokenRequest.owner` (field 2), `AuditEntry.api_token` (field 10) and
//! `ApiTokensRefusal.UNKNOWN_OWNER` (8).
//!
//! Nothing existing changed. `AuditEntry` is in the committed baseline (`proto/compat/baseline.binpb`):
//! a reader of it knows no `api_token`, reads every field it knows from an entry that carries one
//! and writes the entry back with it kept. The two `ApiTokens` messages are not in the baseline
//! yet (the release step writes it), so the release before's reader of each is written here, its
//! fields as that release has them: it reads a message with an owner, and this build reads what
//! it writes with the owner absent. Those are the tests of rule B8 in both directions.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// `ApiTokenInfo` as the release before has it: no `owner`.
#[derive(Clone, PartialEq, prost::Message)]
struct OlderApiTokenInfo {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(enumeration = "op::ApiTokenSource", tag = "2")]
    source: i32,
    #[prost(string, tag = "3")]
    created_by: String,
    #[prost(uint64, tag = "4")]
    created_unix: u64,
}

/// `CreateApiTokenRequest` as the release before has it: no `owner`.
#[derive(Clone, PartialEq, prost::Message)]
struct OlderCreateApiTokenRequest {
    #[prost(string, tag = "1")]
    name: String,
}

/// Catches: an audit entry's API token that a reader of the release before refuses or drops (an
/// older client of `Audit.List`, a coordinator of the release before during an update), or that
/// the entry's other fields lose.
#[test]
fn a_reader_of_the_release_before_keeps_an_audit_entrys_api_token() {
    let older = baseline_pool();
    let descriptor = older
        .get_message_by_name("hostkeeper.operator.v1.AuditEntry")
        .expect("released");
    assert!(descriptor.get_field_by_name("api_token").is_none());
    let entry = op::AuditEntry {
        seq: 41,
        by: op::Actor::Operator.into(),
        action: "exclude".to_owned(),
        by_name: Some("erin".to_owned()),
        api_token: Some("devbox".to_owned()),
        ..op::AuditEntry::default()
    };
    let read = DynamicMessage::decode(descriptor.clone(), entry.encode_to_vec().as_slice())
        .expect("the release before reads it");
    assert_eq!(
        read.get_field_by_name("action").as_deref(),
        Some(&Value::String("exclude".to_owned()))
    );
    let back = op::AuditEntry::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, entry);

    let mut written = DynamicMessage::new(descriptor);
    written.set_field_by_name("action", Value::String("exclude".to_owned()));
    let read = op::AuditEntry::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.api_token, None);
}

/// Catches: a token's owner or a creation's owner that the release before's reader refuses or
/// that makes it lose a field it knows (an older `hk` or page against a newer coordinator), and
/// a message of the release before read with an owner it never named (a creation read as naming
/// one, a token of the tokens file listed with one); an owner named empty is written apart from
/// one not named.
#[test]
fn the_owners_of_the_api_tokens_messages_are_read_past_and_absent_in_older_ones() {
    let info = op::ApiTokenInfo {
        name: "devbox".to_owned(),
        source: op::ApiTokenSource::Store.into(),
        created_by: "erin".to_owned(),
        created_unix: 1_000,
        owner: "erin".to_owned(),
    };
    let older = OlderApiTokenInfo::decode(info.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        older,
        OlderApiTokenInfo {
            name: "devbox".to_owned(),
            source: op::ApiTokenSource::Store.into(),
            created_by: "erin".to_owned(),
            created_unix: 1_000,
        }
    );
    let read = op::ApiTokenInfo::decode(older.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(
        read,
        op::ApiTokenInfo {
            owner: String::new(),
            ..info
        }
    );

    let create = op::CreateApiTokenRequest {
        name: "laptop".to_owned(),
        owner: Some("erin".to_owned()),
    };
    let older =
        OlderCreateApiTokenRequest::decode(create.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(older.name, "laptop");
    let read =
        op::CreateApiTokenRequest::decode(older.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.owner, None);
    let named_empty = op::CreateApiTokenRequest {
        owner: Some(String::new()),
        ..op::CreateApiTokenRequest::default()
    };
    assert!(
        !named_empty.encode_to_vec().is_empty(),
        "an owner named empty is written"
    );
}

/// Catches: a refusal value renumbered or reused, which a client of the release before would read
/// as another refusal.
#[test]
fn unknown_owner_is_a_new_value_after_the_last() {
    assert_eq!(op::ApiTokensRefusal::AuditUnavailable as i32, 7);
    assert_eq!(op::ApiTokensRefusal::UnknownOwner as i32, 8);
    assert_eq!(
        op::ApiTokensRefusal::UnknownOwner.as_str_name(),
        "API_TOKENS_REFUSAL_UNKNOWN_OWNER"
    );
    assert_eq!(
        op::ApiTokensRefusal::from_str_name("API_TOKENS_REFUSAL_UNKNOWN_OWNER"),
        Some(op::ApiTokensRefusal::UnknownOwner)
    );
}
