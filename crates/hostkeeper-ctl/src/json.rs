//! Proto3 canonical JSON of a dynamic message, written here rather than through serde so that the
//! output is the same at every run: fields in the order the message declares them, map keys
//! sorted, and nothing that is absent written.
//!
//! The canonical rules it keeps: a field is named by its JSON name (`nodeId`); a field without
//! presence is left out when it holds its default, and a field with presence when it is unset;
//! an empty list or map is left out; 64-bit integers are strings; an enum is its value's name (its
//! number when the reader does not know the value); bytes are standard base64 with padding; a
//! float that is not finite is `"NaN"`, `"Infinity"` or `"-Infinity"`. The generator refuses
//! every RPC that reaches a well-known type, whose special forms are not written here.

use std::fmt::Write as _;

use prost_reflect::{
    DynamicMessage, FieldDescriptor, Kind, MapKey, MessageDescriptor, ReflectMessage, Value,
};

/// The fields of a message in the order it declares them.
pub fn declared_fields(desc: &MessageDescriptor) -> Vec<FieldDescriptor> {
    desc.descriptor_proto()
        .field
        .iter()
        .filter_map(|proto| desc.get_field_by_name(proto.name()))
        .collect()
}

/// The message as one line of JSON.
pub fn compact(message: &DynamicMessage) -> String {
    let mut out = String::new();
    write_message(&mut out, message, None);
    out
}

/// The message as JSON indented by two spaces a level, its first line at `depth` levels.
pub fn pretty(message: &DynamicMessage, depth: usize) -> String {
    let mut out = String::new();
    write_message(&mut out, message, Some(depth));
    out
}

/// One field's value as indented JSON, at `depth` levels.
pub fn pretty_field(message: &DynamicMessage, field: &FieldDescriptor, depth: usize) -> String {
    let mut out = String::new();
    write_field(&mut out, &message.get_field(field), field, Some(depth));
    out
}

/// One singular scalar or enum value as text: its JSON, without the quotes of a string (escapes
/// stay, so no control character reaches a terminal).
pub fn scalar_text(value: &Value, kind: &Kind) -> String {
    let mut out = String::new();
    write_single(&mut out, value, kind, None);
    match out.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(inner) => inner.to_owned(),
        None => out,
    }
}

/// A line break and the indent of `depth` levels, when pretty.
fn newline(out: &mut String, depth: Option<usize>) {
    if let Some(depth) = depth {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    }
}

fn write_message(out: &mut String, message: &DynamicMessage, depth: Option<usize>) {
    let inner = depth.map(|d| d + 1);
    out.push('{');
    let mut first = true;
    for field in declared_fields(&message.descriptor()) {
        if !message.has_field(&field) {
            continue;
        }
        if !first {
            out.push(',');
        }
        first = false;
        newline(out, inner);
        write_string(out, field.json_name());
        out.push_str(if depth.is_some() { ": " } else { ":" });
        write_field(out, &message.get_field(&field), &field, inner);
    }
    if !first {
        newline(out, depth);
    }
    out.push('}');
}

fn write_field(out: &mut String, value: &Value, field: &FieldDescriptor, depth: Option<usize>) {
    let kind = field.kind();
    match value {
        Value::List(items) => {
            let inner = depth.map(|d| d + 1);
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, inner);
                write_single(out, item, &kind, inner);
            }
            if !items.is_empty() {
                newline(out, depth);
            }
            out.push(']');
        }
        Value::Map(map) => {
            let value_kind = kind
                .as_message()
                .map_or(Kind::String, |entry| entry.map_entry_value_field().kind());
            let inner = depth.map(|d| d + 1);
            let mut entries: Vec<(&MapKey, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, item)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, inner);
                write_string(out, &map_key(key));
                out.push_str(if depth.is_some() { ": " } else { ":" });
                write_single(out, item, &value_kind, inner);
            }
            if !entries.is_empty() {
                newline(out, depth);
            }
            out.push('}');
        }
        single => write_single(out, single, &kind, depth),
    }
}

/// A map key as the string JSON names it with.
fn map_key(key: &MapKey) -> String {
    match key {
        MapKey::Bool(b) => b.to_string(),
        MapKey::I32(n) => n.to_string(),
        MapKey::I64(n) => n.to_string(),
        MapKey::U32(n) => n.to_string(),
        MapKey::U64(n) => n.to_string(),
        MapKey::String(s) => s.clone(),
    }
}

/// The string a float that is not finite is written as.
fn non_finite(x: f64) -> &'static str {
    if x.is_nan() {
        "\"NaN\""
    } else if x > 0.0 {
        "\"Infinity\""
    } else {
        "\"-Infinity\""
    }
}

fn write_single(out: &mut String, value: &Value, kind: &Kind, depth: Option<usize>) {
    match value {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::I32(n) => out.push_str(&n.to_string()),
        Value::U32(n) => out.push_str(&n.to_string()),
        Value::I64(n) => write_string(out, &n.to_string()),
        Value::U64(n) => write_string(out, &n.to_string()),
        Value::F32(x) if x.is_finite() => out.push_str(&x.to_string()),
        Value::F64(x) if x.is_finite() => out.push_str(&x.to_string()),
        Value::F32(x) => out.push_str(non_finite(f64::from(*x))),
        Value::F64(x) => out.push_str(non_finite(*x)),
        Value::String(s) => write_string(out, s),
        Value::Bytes(b) => write_string(out, &base64(b)),
        Value::EnumNumber(n) => match kind.as_enum().and_then(|e| e.get_value(*n)) {
            Some(v) => write_string(out, v.name()),
            None => out.push_str(&n.to_string()),
        },
        Value::Message(m) => write_message(out, m, depth),
        // A list or a map is never an element of another; `write_field` writes both.
        Value::List(_) | Value::Map(_) => out.push_str("null"),
    }
}

/// A JSON string: quoted, with `"`, `\` and every control character escaped (C1 and DEL too,
/// which JSON allows raw but a terminal may act on).
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Standard base64 with padding, as canonical JSON writes bytes.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
