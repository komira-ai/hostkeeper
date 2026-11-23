//! The health value `HEALTH_SILENT` (4) of `Health`, which a coordinator answers in the view of a
//! host it has not heard from for 45 seconds or more (`HostService.health`). A reader of the
//! release before (the baseline, `proto/compat/baseline.binpb`) does not know the value, reads
//! every field it knows from an answer that carries it, and writes the unknown enum number back
//! unchanged; this build reads what that release wrote as it was.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set this build serves.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

/// A service view of a host that is not live, as a coordinator now answers it.
fn silent_view() -> op::HostService {
    op::HostService {
        service: "hostkeeper".to_owned(),
        installed_version: Some("0.1.300".to_owned()),
        health: agent::Health::Silent.into(),
        ..op::HostService::default()
    }
}

/// Proves: `HEALTH_SILENT` is a new value of `Health` at 4 under its token, this build serves it
/// by that name and number and its generated type agrees, and every value the release before has
/// keeps its name and number.
#[test]
fn silent_is_a_new_health_value_at_4() {
    let name = "hostkeeper.agent.v1.Health";
    let (older, now) = (baseline_pool(), current_pool());
    let before = older.get_enum_by_name(name).expect("released");
    let current = now.get_enum_by_name(name).expect("served");
    assert_eq!(agent::Health::Silent as i32, 4);
    assert_eq!(agent::Health::Silent.as_str_name(), "HEALTH_SILENT");
    assert_eq!(
        current.get_value(4).expect("the value").name(),
        "HEALTH_SILENT"
    );
    assert_eq!(
        current
            .get_value_by_name("HEALTH_SILENT")
            .map(|v| v.number()),
        Some(4)
    );
    assert!(before.get_value(4).is_none());
    assert!(before.get_value_by_name("HEALTH_SILENT").is_none());
    for kept in before.values() {
        let same = current.get_value(kept.number()).expect("kept");
        assert_eq!(same.name(), kept.name());
    }
}

/// Proves: a reader of the release before keeps a health it does not know: a host service
/// carrying 4 reads there with the number as it is and every other field as it was, and writes
/// the answer back byte for byte, and this build reads the number it wrote as the new value.
///
/// Catches: a coordinator or client of the release before that loses or refuses an answer from a
/// coordinator that says `HEALTH_SILENT`.
#[test]
fn an_older_reader_keeps_a_health_it_does_not_know() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.HostService")
        .expect("released");
    let entry = silent_view();
    let bytes = entry.encode_to_vec();
    let read = DynamicMessage::decode(message, bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("health").as_deref(),
        Some(&Value::EnumNumber(4))
    );
    assert_eq!(
        read.get_field_by_name("service").as_deref(),
        Some(&Value::String("hostkeeper".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("installed_version").as_deref(),
        Some(&Value::String("0.1.300".to_owned()))
    );
    assert_eq!(read.unknown_fields().count(), 0);
    assert_eq!(read.encode_to_vec(), bytes, "the number is written back");
    let back = op::HostService::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, entry);
    assert_eq!(back.health(), agent::Health::Silent);
}

/// Proves: a service view the release before wrote, with each health it knows, reads in this
/// build as that health, so the new value changes nothing for old data.
#[test]
fn this_build_reads_the_healths_the_release_before_wrote() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.HostService")
        .expect("released");
    for (number, health) in [
        (0, agent::Health::Unspecified),
        (1, agent::Health::Healthy),
        (2, agent::Health::Unhealthy),
        (3, agent::Health::Absent),
    ] {
        let mut written = DynamicMessage::new(message.clone());
        written.set_field_by_name("service", Value::String("hostkeeper".to_owned()));
        written.set_field_by_name("health", Value::EnumNumber(number));
        let read = op::HostService::decode(written.encode_to_vec().as_slice()).expect("decodes");
        assert_eq!(read.service, "hostkeeper");
        assert_eq!(read.health(), health);
    }
}
