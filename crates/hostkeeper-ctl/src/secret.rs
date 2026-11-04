//! `(hostkeeper.secret)` at run time: which fields of a message are secrets, and a copy of a
//! message with every secret taken out, at any depth, for anything `ctl` prints.

use std::collections::BTreeSet;

use prost_reflect::{
    DescriptorPool, DynamicMessage, ExtensionDescriptor, FieldDescriptor, ReflectMessage, Value,
};

/// The option as a pool declares it; `None` for a pool without hostkeeper's options, whose
/// fields are then never secret.
pub fn extension(pool: &DescriptorPool) -> Option<ExtensionDescriptor> {
    pool.get_extension_by_name("hostkeeper.secret")
}

/// Whether the field carries `(hostkeeper.secret) = true`.
pub fn is_secret(field: &FieldDescriptor, ext: Option<&ExtensionDescriptor>) -> bool {
    ext.is_some_and(|ext| field.options().get_extension(ext).as_ref() == &Value::Bool(true))
}

/// Clears every secret field that is set in `message` and in every message it holds, and adds
/// each one's full name to `removed`.
pub fn redact(
    message: &mut DynamicMessage,
    ext: Option<&ExtensionDescriptor>,
    removed: &mut BTreeSet<String>,
) {
    let desc = message.descriptor();
    for field in desc.fields() {
        if is_secret(&field, ext) && message.has_field(&field) {
            message.clear_field(&field);
            removed.insert(field.full_name().to_owned());
        }
    }
    for (_, value) in message.fields_mut() {
        redact_value(value, ext, removed);
    }
}

fn redact_value(
    value: &mut Value,
    ext: Option<&ExtensionDescriptor>,
    removed: &mut BTreeSet<String>,
) {
    match value {
        Value::Message(inner) => redact(inner, ext, removed),
        Value::List(items) => {
            for item in items {
                redact_value(item, ext, removed);
            }
        }
        Value::Map(entries) => {
            for item in entries.values_mut() {
                redact_value(item, ext, removed);
            }
        }
        _ => {}
    }
}

/// A copy of `message` with its secrets cleared, and the full names of those that were set.
pub fn redacted(
    message: &DynamicMessage,
    ext: Option<&ExtensionDescriptor>,
) -> (DynamicMessage, BTreeSet<String>) {
    let mut copy = message.clone();
    let mut removed = BTreeSet::new();
    redact(&mut copy, ext, &mut removed);
    (copy, removed)
}
