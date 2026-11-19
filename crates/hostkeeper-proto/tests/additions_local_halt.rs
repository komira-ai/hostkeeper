//! `ServiceBeat.local_halt` (field 15), the halt a host holds of a service of its own, and the
//! two gate reasons `GATE_REASON_HALTED_HERE` (37) and `GATE_REASON_STALE_HALT_HERE` (38) that a
//! coordinator names it by. A reader of the release before (the baseline,
//! `proto/compat/baseline.binpb`) knows neither the field nor the message nor the values, reads
//! every field it knows from an entry that carries them, and writes the unknown field and the
//! unknown enum number back unchanged; an entry that release wrote reads with no local halt.

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

/// Proves: a reader of the release before, which has no `LocalHalt` and no field 15, reads every
/// field it knows from a service entry that carries a local halt and writes the halt back byte
/// for byte as an unknown field 15, so a coordinator of that release in between loses nothing.
#[test]
fn an_older_reader_keeps_a_services_local_halt() {
    let older = baseline_pool();
    assert!(
        older
            .get_message_by_name("hostkeeper.agent.v1.LocalHalt")
            .is_none(),
        "the message is new"
    );
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.ServiceBeat")
        .expect("released");
    assert!(message.get_field(15).is_none());
    assert!(message.get_field_by_name("local_halt").is_none());
    let entry = agent::ServiceBeat {
        name: "hostkeeper".to_owned(),
        version: "0.1.111".to_owned(),
        health: agent::Health::Healthy.into(),
        local_halt: Some(agent::LocalHalt {
            reason: Some("update_failed".to_owned()),
            generation: 7,
            age_secs: Some(90),
        }),
        ..agent::ServiceBeat::default()
    };
    let bytes = entry.encode_to_vec();
    assert_eq!(
        agent::ServiceBeat::decode(bytes.as_slice()).expect("decodes"),
        entry
    );
    let read = DynamicMessage::decode(message, bytes.as_slice()).expect("decodes");
    assert_eq!(
        read.get_field_by_name("name").as_deref(),
        Some(&Value::String("hostkeeper".to_owned()))
    );
    assert_eq!(
        read.get_field_by_name("version").as_deref(),
        Some(&Value::String("0.1.111".to_owned()))
    );
    let kept: Vec<u32> = read.unknown_fields().map(|f| f.number()).collect();
    assert_eq!(kept, [15]);
    let written = read.encode_to_vec();
    assert_eq!(
        written, bytes,
        "the unknown field is written back as it came"
    );
    let back = agent::ServiceBeat::decode(written.as_slice()).expect("decodes");
    assert_eq!(back, entry);
}

/// Proves: an entry the release before wrote, which has no field 15, reads in this build with no
/// local halt (not an empty one), and an empty `LocalHalt` is written and read back as present,
/// so a halt held at generation 0 with no reason is told from none.
#[test]
fn this_build_reads_a_service_of_the_release_before_with_no_local_halt() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.agent.v1.ServiceBeat")
        .expect("released");
    let mut written = DynamicMessage::new(message);
    written.set_field_by_name("name", Value::String("hostkeeper".to_owned()));
    written.set_field_by_name("version", Value::String("0.1.111".to_owned()));
    let read = agent::ServiceBeat::decode(written.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(read.name, "hostkeeper");
    assert_eq!(read.local_halt, None);

    let empty = agent::ServiceBeat {
        local_halt: Some(agent::LocalHalt::default()),
        ..agent::ServiceBeat::default()
    };
    let bytes = empty.encode_to_vec();
    assert_eq!(bytes, [0x7a, 0], "field 15, length 0");
    let back = agent::ServiceBeat::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(back.local_halt, Some(agent::LocalHalt::default()));
}

/// Proves: the numbers and presence of `LocalHalt` are as the schema gives them in the served
/// set: reason 1 and age 3 with explicit presence (a zero age is written, absent is not),
/// generation 2 without it, and the field of `ServiceBeat` at 15 typed by the message.
#[test]
fn the_local_halt_fields_have_their_numbers_and_presence() {
    let now = current_pool();
    let halt = now
        .get_message_by_name("hostkeeper.agent.v1.LocalHalt")
        .expect("served");
    let field = |number: u32| halt.get_field(number).expect("the field");
    assert_eq!(field(1).name(), "reason");
    assert_eq!(field(2).name(), "generation");
    assert_eq!(field(3).name(), "age_secs");
    assert!(field(1).supports_presence());
    assert!(!field(2).supports_presence());
    assert!(field(3).supports_presence());
    assert_eq!(halt.fields().count(), 3);
    let beat = now
        .get_message_by_name("hostkeeper.agent.v1.ServiceBeat")
        .expect("served");
    let local = beat.get_field(15).expect("the field");
    assert_eq!(local.name(), "local_halt");
    assert_eq!(
        local.kind().as_message().map(|m| m.full_name().to_owned()),
        Some("hostkeeper.agent.v1.LocalHalt".to_owned())
    );

    let zero_age = agent::LocalHalt {
        reason: None,
        generation: 0,
        age_secs: Some(0),
    };
    assert_eq!(zero_age.encode_to_vec(), [0x18, 0]);
    let read = agent::LocalHalt::decode([0x18, 0].as_slice()).expect("decodes");
    assert_eq!(read, zero_age);
    assert_eq!(
        agent::LocalHalt::decode(&[][..]).expect("decodes").age_secs,
        None
    );
}

/// Proves: the two gate reasons are new to the release before at 37 and 38 under their tokens,
/// this build serves them by those names and numbers and its generated type agrees, and every
/// value the release before has keeps its name and number.
#[test]
fn the_halted_here_gate_reasons_are_new_values_at_37_and_38() {
    let cases = [
        (
            37,
            "GATE_REASON_HALTED_HERE",
            op::GateReason::HaltedHere as i32,
            op::GateReason::HaltedHere.as_str_name(),
        ),
        (
            38,
            "GATE_REASON_STALE_HALT_HERE",
            op::GateReason::StaleHaltHere as i32,
            op::GateReason::StaleHaltHere.as_str_name(),
        ),
    ];
    let name = "hostkeeper.operator.v1.GateReason";
    let (older, now) = (baseline_pool(), current_pool());
    let before = older.get_enum_by_name(name).expect("released");
    let current = now.get_enum_by_name(name).expect("served");
    for (number, token, generated_number, generated_token) in cases {
        assert_eq!(generated_number, number, "{token}");
        assert_eq!(generated_token, token);
        let value = current.get_value(number).expect("the new value");
        assert_eq!(value.name(), token);
        assert_eq!(
            current.get_value_by_name(token).map(|v| v.number()),
            Some(number)
        );
        assert!(before.get_value(number).is_none(), "{token}");
        assert!(before.get_value_by_name(token).is_none(), "{token}");
    }
    for kept in before.values() {
        let same = current.get_value(kept.number()).expect("kept");
        assert_eq!(same.name(), kept.name());
    }
}

/// Proves: a reader of the release before keeps a gate reason it does not know: a host service
/// carrying 37 (or 38) reads there with the number as it is, and writes it back unchanged, and
/// this build reads the number it wrote as the new value.
#[test]
fn an_older_reader_keeps_a_gate_reason_it_does_not_know() {
    let older = baseline_pool();
    let message = older
        .get_message_by_name("hostkeeper.operator.v1.HostService")
        .expect("released");
    for (reason, number) in [
        (op::GateReason::HaltedHere, 37),
        (op::GateReason::StaleHaltHere, 38),
    ] {
        let entry = op::HostService {
            service: "hostkeeper".to_owned(),
            gate_reason: reason.into(),
            ..op::HostService::default()
        };
        let bytes = entry.encode_to_vec();
        let read = DynamicMessage::decode(message.clone(), bytes.as_slice()).expect("decodes");
        assert_eq!(
            read.get_field_by_name("gate_reason").as_deref(),
            Some(&Value::EnumNumber(number))
        );
        assert_eq!(read.encode_to_vec(), bytes, "the number is written back");
        let back = op::HostService::decode(read.encode_to_vec().as_slice()).expect("decodes");
        assert_eq!(back.gate_reason(), reason);
    }
}
