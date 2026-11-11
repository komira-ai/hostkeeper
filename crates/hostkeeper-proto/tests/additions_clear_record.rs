//! `Hosts.ClearRecord` (`docs/design/two-programs.md` section 3.2): the RPC with its two
//! messages and the refusal `daemon_too_old` (`HostsRefusal` 9) in `hostkeeper.operator.v1`, and
//! the `clear_record` case (field 10) of `Order`'s `verb` with its message in
//! `hostkeeper.agent.v1`. Each is new against the release before (the baseline,
//! `proto/compat/baseline.binpb`), and each round trips. A reader of the release before reads an
//! order of the new case as one with no verb it knows, keeps its id and try, and writes it back
//! unchanged; the coordinator sends that case only to a daemon that announced
//! `clear-record-v1`, which no daemon of the release before does.

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
fn clear_record_is_an_rpc_two_messages_and_a_refusal_new_since_the_release_before() {
    let older = baseline_pool();
    let current = current_pool();
    let hosts = |pool: &DescriptorPool| {
        pool.get_service_by_name("hostkeeper.operator.v1.Hosts")
            .expect("the Hosts service")
    };
    assert!(
        hosts(&older).methods().all(|m| m.name() != "ClearRecord"),
        "the release before has no ClearRecord"
    );
    let method = hosts(&current)
        .methods()
        .find(|m| m.name() == "ClearRecord")
        .expect("ClearRecord");
    assert_eq!(
        method.input().full_name(),
        "hostkeeper.operator.v1.ClearRecordRequest"
    );
    assert_eq!(
        method.output().full_name(),
        "hostkeeper.operator.v1.ClearRecordResponse"
    );
    assert!(!method.is_client_streaming() && !method.is_server_streaming());

    for name in [
        "hostkeeper.operator.v1.ClearRecordRequest",
        "hostkeeper.operator.v1.ClearRecordResponse",
        "hostkeeper.agent.v1.ClearRecord",
    ] {
        assert!(older.get_message_by_name(name).is_none(), "{name} is new");
        assert!(current.get_message_by_name(name).is_some(), "{name}");
    }

    let request = op::ClearRecordRequest {
        node_id: "db-1".to_owned(),
        service: "web".to_owned(),
    };
    let bytes = request.encode_to_vec();
    assert_eq!(
        op::ClearRecordRequest::decode(bytes.as_slice()).expect("decodes"),
        request
    );
    let response = op::ClearRecordResponse {
        order_id: "clear-record-12".to_owned(),
    };
    let bytes = response.encode_to_vec();
    assert_eq!(
        op::ClearRecordResponse::decode(bytes.as_slice()).expect("decodes"),
        response
    );

    let refusal = older
        .get_enum_by_name("hostkeeper.operator.v1.HostsRefusal")
        .expect("released");
    assert!(refusal.get_value(9).is_none(), "9 is new");
    assert_eq!(op::HostsRefusal::DaemonTooOld as i32, 9);
    assert_eq!(
        op::HostsRefusal::DaemonTooOld.as_str_name(),
        "HOSTS_REFUSAL_DAEMON_TOO_OLD"
    );
    assert_eq!(
        op::HostsRefusal::from_str_name("HOSTS_REFUSAL_DAEMON_TOO_OLD"),
        Some(op::HostsRefusal::DaemonTooOld)
    );
}

/// Catches an order of the new case that does not round trip, that a reader of the release
/// before reads as a verb it knows, or whose id, try or new case that reader drops when it
/// writes the order back.
#[test]
fn an_older_reader_reads_a_clear_record_order_as_no_verb_and_keeps_it() {
    let order = agent::Order {
        order_id: "clear-record-12".to_owned(),
        r#try: 1,
        verb: Some(agent::order::Verb::ClearRecord(agent::ClearRecord {
            service: "web".to_owned(),
        })),
    };
    let bytes = order.encode_to_vec();
    assert_eq!(
        agent::Order::decode(bytes.as_slice()).expect("decodes"),
        order
    );

    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.Order")
        .expect("released");
    assert!(message.get_field(10).is_none(), "field 10 is new");
    let read = DynamicMessage::decode(message.clone(), bytes.as_slice()).expect("decodes");
    let verb = message.oneofs().find(|o| o.name() == "verb").expect("verb");
    assert!(
        verb.fields().all(|field| !read.has_field(&field)),
        "no verb the release before knows"
    );
    assert_eq!(
        read.get_field_by_name("order_id").as_deref(),
        Some(&Value::String("clear-record-12".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("try").as_deref(),
        Some(&Value::U64(1))
    );
    let back = agent::Order::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, order);
}
