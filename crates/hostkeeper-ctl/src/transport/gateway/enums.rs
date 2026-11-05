//! The check that keeps a newer server's enum value from being dropped. A reply may carry fields
//! this build does not know, and they are skipped; but the JSON reader leaves out an enum
//! name it does not know in a field it does know, which would hand the caller a reply with that
//! value missing. [`check`] walks the reply against the response type first and names the first
//! such value.

use prost_reflect::{Kind, MessageDescriptor};
use serde_json::Value;

/// Nothing when every enum name in `value`, a reply of type `message`, is one of its enum's
/// values; else the refusal, naming the field and the value. Numbers for enums are not names and
/// pass; keys the type does not have are skipped.
pub(crate) fn check(value: &Value, message: &MessageDescriptor, path: &str) -> Result<(), String> {
    let Value::Object(object) = value else {
        return Ok(());
    };
    for (key, inner) in object {
        let Some(field) = message
            .get_field_by_json_name(key)
            .or_else(|| message.get_field_by_name(key))
        else {
            continue;
        };
        let here = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if field.is_map() {
            if let (Kind::Message(entry), Value::Object(entries)) = (field.kind(), inner) {
                let kind = entry.map_entry_value_field().kind();
                for (entry_key, entry_value) in entries {
                    single(entry_value, &kind, &format!("{here}[{entry_key:?}]"))?;
                }
            }
        } else if field.is_list() {
            if let Value::Array(items) = inner {
                let kind = field.kind();
                for (index, item) in items.iter().enumerate() {
                    single(item, &kind, &format!("{here}[{index}]"))?;
                }
            }
        } else {
            single(inner, &field.kind(), &here)?;
        }
    }
    Ok(())
}

/// One value of `kind`: an enum name must be known; a message is walked.
fn single(value: &Value, kind: &Kind, path: &str) -> Result<(), String> {
    match (kind, value) {
        (Kind::Enum(values), Value::String(name)) if values.get_value_by_name(name).is_none() => {
            Err(format!(
                "the reply has {name:?} at {path}, a value of {} that this build of hostkeeper \
                 does not know; a newer hostkeeper, or --transport grpc, reads it",
                values.full_name()
            ))
        }
        (Kind::Message(message), _) => check(value, message, path),
        _ => Ok(()),
    }
}
