//! The request of a call: the request file (`--request`), then each flag and each secret laid
//! over it, as a dynamic message of the RPC's request type.
//!
//! A field named both by the request file and by a flag is a usage error, never a silent
//! override. Nothing read for a secret field is ever put in an error message, and the request's
//! `Debug` shows it with its secrets cleared.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::io::Read;
use std::path::PathBuf;

use bytes::Bytes;
use clap::ArgMatches;
use hostkeeper_proto::ctl::{Command, Field, FieldKind, Input, Label};
use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DynamicMessage, ExtensionDescriptor, FieldDescriptor, MapKey,
    MessageDescriptor, ReflectMessage, Value,
};

use crate::{Failure, json, secret};

/// A request, ready to send.
pub struct BuiltRequest {
    message: DynamicMessage,
    ext: Option<ExtensionDescriptor>,
}

impl BuiltRequest {
    /// The request in protobuf's wire format.
    pub fn encode(&self) -> Bytes {
        Bytes::from(self.message.encode_to_vec())
    }

    /// The message itself, secrets included.
    #[cfg(test)]
    pub fn message(&self) -> &DynamicMessage {
        &self.message
    }
}

/// The request as canonical JSON with every secret cleared, and the names of those cleared.
impl fmt::Debug for BuiltRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (shown, removed) = secret::redacted(&self.message, self.ext.as_ref());
        f.debug_struct("BuiltRequest")
            .field("type", &self.message.descriptor().full_name())
            .field("json", &json::compact(&shown))
            .field("redacted", &removed)
            .finish()
    }
}

/// The bytes of a file, or of stdin when `path` is `None`, with one trailing newline dropped.
/// `what` names the input in an error; the contents never appear in one.
pub(crate) fn read_input(
    path: Option<&PathBuf>,
    stdin: &mut dyn Read,
    what: &str,
) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    let read = match path {
        Some(path) => std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut bytes)),
        None => stdin.read_to_end(&mut bytes),
    };
    if let Err(e) = read {
        let from = path.map_or_else(|| "stdin".to_owned(), |p| p.display().to_string());
        return Err(Failure::Usage(format!(
            "cannot read {what} from {from}: {e}"
        )));
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    Ok(bytes)
}

/// The request file, parsed, and the names of the fields it sets at its top level.
fn request_file(
    desc: &MessageDescriptor,
    leaf: &ArgMatches,
    stdin: &mut dyn Read,
) -> Result<(DynamicMessage, BTreeSet<String>), Failure> {
    let Some(source) = leaf.get_one::<String>("request") else {
        return Ok((DynamicMessage::new(desc.clone()), BTreeSet::new()));
    };
    let path = (source != "-").then(|| PathBuf::from(source));
    let bytes = read_input(path.as_ref(), stdin, "the request")?;
    // Neither error says what the file holds: it may hold a secret.
    let where_ = |e: &serde_json::Error| format!("line {}, column {}", e.line(), e.column());
    let keys: BTreeSet<String> = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(serde_json::Value::Object(map)) => map.keys().cloned().collect(),
        Ok(_) => {
            return Err(Failure::Usage(
                "the request file is not a JSON object".to_owned(),
            ));
        }
        Err(e) => {
            return Err(Failure::Usage(format!(
                "the request file is not JSON ({})",
                where_(&e)
            )));
        }
    };
    let mut de = serde_json::Deserializer::from_slice(&bytes);
    let message = DynamicMessage::deserialize(desc.clone(), &mut de).map_err(|e| {
        Failure::Usage(format!(
            "the request file is not canonical JSON of {} ({})",
            desc.full_name(),
            where_(&e)
        ))
    })?;
    Ok((message, keys))
}

/// A flag's text as a value of the field's kind.
fn scalar(field: &Field, text: &str) -> Result<Value, Failure> {
    let bad = |what: &str| Failure::Usage(format!("--{} takes {what}, not {text:?}", field.flag));
    Ok(match field.kind {
        FieldKind::Double => Value::F64(text.parse().map_err(|_| bad("a number"))?),
        FieldKind::Float => Value::F32(text.parse().map_err(|_| bad("a number"))?),
        FieldKind::Int32 | FieldKind::Sint32 | FieldKind::Sfixed32 => {
            Value::I32(text.parse().map_err(|_| bad("a 32-bit signed integer"))?)
        }
        FieldKind::Int64 | FieldKind::Sint64 | FieldKind::Sfixed64 => {
            Value::I64(text.parse().map_err(|_| bad("a 64-bit signed integer"))?)
        }
        FieldKind::Uint32 | FieldKind::Fixed32 => {
            Value::U32(text.parse().map_err(|_| bad("a 32-bit unsigned integer"))?)
        }
        FieldKind::Uint64 | FieldKind::Fixed64 => {
            Value::U64(text.parse().map_err(|_| bad("a 64-bit unsigned integer"))?)
        }
        // clap took only `true` or `false`.
        FieldKind::Bool => Value::Bool(text == "true"),
        // clap took only the spellings of the table; the generator listed each with its number.
        FieldKind::Enum => Value::EnumNumber(
            field
                .enum_values
                .iter()
                .find(|v| v.flag == text)
                .map_or(0, |v| v.number),
        ),
        // A string; the generator makes no flag of bytes or a message.
        FieldKind::String | FieldKind::Bytes | FieldKind::Message => Value::String(text.to_owned()),
    })
}

/// The value of a flag field, from its occurrences.
fn flag_value(field: &Field, texts: &[&String]) -> Result<Value, Failure> {
    match field.label {
        Label::Map => {
            let mut map = HashMap::new();
            for text in texts {
                let Some((key, value)) = text.split_once('=') else {
                    return Err(Failure::Usage(format!(
                        "--{} takes key=value, not {text:?}",
                        field.flag
                    )));
                };
                let entry = (
                    MapKey::String(key.to_owned()),
                    Value::String(value.to_owned()),
                );
                if map.insert(entry.0, entry.1).is_some() {
                    return Err(Failure::Usage(format!(
                        "--{} names the key {key:?} twice",
                        field.flag
                    )));
                }
            }
            Ok(Value::Map(map))
        }
        Label::Repeated => Ok(Value::List(
            texts
                .iter()
                .map(|t| scalar(field, t))
                .collect::<Result<_, _>>()?,
        )),
        // One value: clap refused a second.
        Label::Singular | Label::Optional | Label::Oneof => scalar(field, texts[0]),
    }
}

/// Refuses a field the request file also sets.
fn not_in_file(
    field: &FieldDescriptor,
    keys: &BTreeSet<String>,
    flag: &str,
) -> Result<(), Failure> {
    if keys.contains(field.name()) || keys.contains(field.json_name()) {
        return Err(Failure::Usage(format!(
            "{flag} and the request file both set `{}`",
            field.name()
        )));
    }
    Ok(())
}

/// The request of `command`, from the request file, its flags and its secrets.
pub(crate) fn build(
    command: &Command,
    leaf: &ArgMatches,
    pool: &DescriptorPool,
    stdin: &mut dyn Read,
) -> Result<BuiltRequest, Failure> {
    let desc = pool.get_message_by_name(command.request).ok_or_else(|| {
        Failure::Local(format!(
            "the descriptors have no message {}",
            command.request
        ))
    })?;
    let (mut message, keys) = request_file(&desc, leaf, stdin)?;
    for field in command.fields {
        let Some(proto) = desc.get_field_by_name(field.name) else {
            return Err(Failure::Local(format!(
                "the descriptors' {} has no field {}",
                command.request, field.name
            )));
        };
        match field.input {
            Input::Flag => {
                let Some(texts) = leaf.get_many::<String>(field.flag) else {
                    continue;
                };
                let texts: Vec<&String> = texts.collect();
                not_in_file(&proto, &keys, &format!("--{}", field.flag))?;
                message.set_field(&proto, flag_value(field, &texts)?);
            }
            Input::SecretFile => {
                let file_id = format!("{}-file", field.flag);
                let path = leaf.get_one::<PathBuf>(&file_id);
                if path.is_none() && !leaf.get_flag(&format!("{}-stdin", field.flag)) {
                    continue;
                }
                not_in_file(&proto, &keys, &format!("--{file_id}"))?;
                let bytes = read_input(path, stdin, &format!("`{}`", field.name))?;
                let value = match field.kind {
                    FieldKind::Bytes => Value::Bytes(Bytes::from(bytes)),
                    _ => Value::String(String::from_utf8(bytes).map_err(|_| {
                        Failure::Usage(format!("`{}` is not UTF-8 text", field.name))
                    })?),
                };
                message.set_field(&proto, value);
            }
            Input::RequestFile => {}
        }
    }
    Ok(BuiltRequest {
        message,
        ext: secret::extension(pool),
    })
}

#[cfg(test)]
mod tests;
