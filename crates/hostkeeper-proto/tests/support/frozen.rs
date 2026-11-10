//! The frozen `hostkeeper.local.v1` messages, built dynamically: no Rust is generated from
//! `proto/hostkeeper/local/v1/admin.proto` any more, but the file stays in the descriptor set
//! because an older `hostkeeper admin` on a host still speaks it, so its wire tests stay.

use std::sync::OnceLock;

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, ReflectMessage as _, Value};

/// One pool for every message, so that a message nested in another has the same descriptor.
fn pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
    })
}

/// A `hostkeeper.local.v1.<name>` with `fields` set; any other field is at its default.
pub fn local<'a>(name: &str, fields: impl IntoIterator<Item = (&'a str, Value)>) -> DynamicMessage {
    let full = format!("hostkeeper.local.v1.{name}");
    let descriptor = pool()
        .get_message_by_name(&full)
        .unwrap_or_else(|| panic!("{full} is in the descriptor set"));
    let mut message = DynamicMessage::new(descriptor);
    for (field, value) in fields {
        message.set_field_by_name(field, value);
    }
    message
}

/// The value of enum `hostkeeper.local.v1.<enum_name>` named `value`.
pub fn variant(enum_name: &str, value: &str) -> Value {
    let full = format!("hostkeeper.local.v1.{enum_name}");
    let found = pool()
        .get_enum_by_name(&full)
        .unwrap_or_else(|| panic!("{full} is in the descriptor set"))
        .get_value_by_name(value)
        .unwrap_or_else(|| panic!("{value} is a value of {full}"));
    Value::EnumNumber(found.number())
}

/// The name of the value `message`'s enum field `field` reads as: an unknown number reads as the
/// enum's zero value, as a generated reader's accessor does.
pub fn enum_name(message: &DynamicMessage, field: &str) -> String {
    let descriptor = message
        .descriptor()
        .get_field_by_name(field)
        .expect("a field");
    let kind = descriptor.kind();
    let en = kind.as_enum().expect("an enum field");
    let number = message
        .get_field(&descriptor)
        .as_enum_number()
        .expect("an enum value");
    en.get_value(number)
        .unwrap_or_else(|| en.default_value())
        .name()
        .to_owned()
}

/// `message` written and read again with its own descriptor, after checking it reads back as it
/// was written.
pub fn read_back(message: &DynamicMessage) -> DynamicMessage {
    let read = DynamicMessage::decode(message.descriptor(), message.encode_to_vec().as_slice())
        .expect("decodes");
    assert_eq!(&read, message);
    read
}
