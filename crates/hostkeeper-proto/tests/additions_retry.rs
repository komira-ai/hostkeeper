//! `Hosts.Retry` and the order a host is told now: the RPC with its two messages, the refusal
//! `nothing_to_retry` (`HostsRefusal` 10), the message `CurrentOrder` and the field
//! `HostService.order` (12) in `hostkeeper.operator.v1`. Each is new against the release before
//! (the baseline, `proto/compat/baseline.binpb`), and each round trips. A reader of the release
//! before reads a `HostService` that carries an order as the same entry with a field it does not
//! know, keeps every field it knows, and writes the order back unchanged.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set of this build.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes")
}

/// Catches the RPC, a message or the refusal missing, renamed, or present in the release before
/// (so not an addition), and a message that does not round trip.
#[test]
fn retry_is_an_rpc_two_messages_and_a_refusal_new_since_the_release_before() {
    let older = baseline_pool();
    let current = current_pool();
    let hosts = |pool: &DescriptorPool| {
        pool.get_service_by_name("hostkeeper.operator.v1.Hosts")
            .expect("the Hosts service")
    };
    assert!(
        hosts(&older).methods().all(|m| m.name() != "Retry"),
        "the release before has no Retry"
    );
    let method = hosts(&current)
        .methods()
        .find(|m| m.name() == "Retry")
        .expect("Retry");
    assert_eq!(
        method.input().full_name(),
        "hostkeeper.operator.v1.RetryRequest"
    );
    assert_eq!(
        method.output().full_name(),
        "hostkeeper.operator.v1.RetryResponse"
    );
    assert!(!method.is_client_streaming() && !method.is_server_streaming());

    for name in [
        "hostkeeper.operator.v1.RetryRequest",
        "hostkeeper.operator.v1.RetryResponse",
        "hostkeeper.operator.v1.CurrentOrder",
    ] {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        assert!(current.get_message_by_name(name).is_some(), "{name}");
    }

    let request = op::RetryRequest {
        node_id: "db-1".to_owned(),
        service: "web".to_owned(),
    };
    let bytes = request.encode_to_vec();
    assert_eq!(
        op::RetryRequest::decode(bytes.as_slice()).expect("decodes"),
        request
    );
    let response = op::RetryResponse {
        set_aside: Some(agent::UpdateResult {
            target: "2.0".to_owned(),
            outcome: agent::Outcome::Failed.into(),
            order_try: Some(1),
            order_id: Some("install-21957d839273aac366285ab4".to_owned()),
            ..agent::UpdateResult::default()
        }),
        entry: Some(op::AuditEntry {
            seq: 253,
            action: "retry".to_owned(),
            ..op::AuditEntry::default()
        }),
    };
    let bytes = response.encode_to_vec();
    assert_eq!(
        op::RetryResponse::decode(bytes.as_slice()).expect("decodes"),
        response
    );

    let refusal = older
        .get_enum_by_name("hostkeeper.operator.v1.HostsRefusal")
        .expect("released");
    assert!(refusal.get_value(10).is_none(), "10 is new");
    assert_eq!(op::HostsRefusal::NothingToRetry as i32, 10);
    assert_eq!(
        op::HostsRefusal::NothingToRetry.as_str_name(),
        "HOSTS_REFUSAL_NOTHING_TO_RETRY"
    );
    assert_eq!(
        op::HostsRefusal::from_str_name("HOSTS_REFUSAL_NOTHING_TO_RETRY"),
        Some(op::HostsRefusal::NothingToRetry)
    );
}

/// Catches a `HostService` with an order that does not round trip, a field 12 the release before
/// already had, and a reader of the release before that loses a field it knows, or the order,
/// when it writes the entry back.
#[test]
fn an_older_reader_keeps_a_host_service_that_carries_its_current_order() {
    let view = op::HostService {
        service: "web".to_owned(),
        holds_slot: true,
        gate_reason: op::GateReason::Updating.into(),
        order: Some(op::CurrentOrder {
            order_id: Some("update-0123456789abcdef0123456789abcdef".to_owned()),
            order_try: 2,
            version: "2.0".to_owned(),
            uninstall: false,
        }),
        ..op::HostService::default()
    };
    let bytes = view.encode_to_vec();
    assert_eq!(
        op::HostService::decode(bytes.as_slice()).expect("decodes"),
        view
    );

    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.HostService")
        .expect("released");
    assert!(message.get_field(12).is_none(), "field 12 is new");
    let read = DynamicMessage::decode(message, bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("service").as_deref(),
        Some(&Value::String("web".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("holds_slot").as_deref(),
        Some(&Value::Bool(true))
    );
    let back = op::HostService::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, view);
}
