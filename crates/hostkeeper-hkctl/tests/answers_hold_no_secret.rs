//! A script prints the answer as the gateway sends it, and cannot take a field out of it. So no
//! method hkctl has for the real protos may answer a message that reaches a field marked
//! `(hostkeeper.secret)`; the fixture's `Login`, which does, shows the check finds one.

use std::collections::BTreeSet;

use prost_reflect::{DescriptorPool, Kind, MessageDescriptor, Value};

/// The fixture's command table and descriptors, which `build.rs` generated from tests/fixtures.
#[expect(
    dead_code,
    reason = "the generated file also holds the local-service and reserved-flag lists"
)]
mod fixture {
    use hostkeeper_proto::ctl::{Command, EnumValue, Field, FieldKind, Input, Label};

    include!(concat!(env!("OUT_DIR"), "/fixture_commands.rs"));
}

const FIXTURE_SET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fixture.binpb"));

/// The full names of the secret fields `message` reaches.
fn secrets(
    pool: &DescriptorPool,
    message: &MessageDescriptor,
    seen: &mut BTreeSet<String>,
) -> Vec<String> {
    if !seen.insert(message.full_name().to_owned()) {
        return Vec::new();
    }
    let ext = pool.get_extension_by_name("hostkeeper.secret");
    let mut found = Vec::new();
    for field in message.fields() {
        if ext
            .as_ref()
            .is_some_and(|e| field.options().get_extension(e).as_ref() == &Value::Bool(true))
        {
            found.push(field.full_name().to_owned());
        }
        if let Kind::Message(inner) = field.kind() {
            found.extend(secrets(pool, &inner, seen));
        }
    }
    found
}

/// The secret fields the answers of `commands` reach, in `pool`.
fn answered(pool: &DescriptorPool, commands: &[hostkeeper_proto::ctl::Command]) -> Vec<String> {
    commands
        .iter()
        .filter(|c| hostkeeper_hkctl::is_served(c))
        .flat_map(|c| {
            let response = pool
                .get_message_by_name(c.response)
                .expect("the response message");
            secrets(pool, &response, &mut BTreeSet::new())
        })
        .collect()
}

/// Catches a method of the real protos whose answer reaches a secret, which hkctl would print.
#[test]
fn no_method_hkctl_has_answers_a_secret() {
    let real = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("descriptors");
    assert_eq!(
        answered(&real, hostkeeper_proto::ctl::COMMANDS),
        Vec::<String>::new()
    );
    let fixture = DescriptorPool::decode(FIXTURE_SET).expect("the fixture's descriptors");
    assert_eq!(
        answered(&fixture, fixture::COMMANDS),
        ["hostkeeper.fixture.v1.LoginResponse.session"]
    );
}
