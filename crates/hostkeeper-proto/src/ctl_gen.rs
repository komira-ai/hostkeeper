//! The generator of `hostkeeper ctl`'s command table: from a descriptor pool, the Rust source of
//! `COMMANDS` (one entry per RPC), `HAND_WRITTEN_LOCAL` and `RESERVED_FLAGS`, which
//! `hostkeeper_proto::ctl` includes from `OUT_DIR`.
//!
//! `build.rs` runs it on the descriptor set it compiled; the unit tests of this crate run it on
//! fixture protos, so the generator is measured like any other code. It writes data, not code
//! per RPC: a new RPC in a proto becomes a new entry with no hand edit.
//!
//! What it refuses, so that the build fails rather than `ctl` misbehaving:
//!
//! - `(hostkeeper.secret)` on a field that is not `string` or `bytes`, anywhere a request or a
//!   response reaches;
//! - a streaming RPC (`ctl` makes unary calls only);
//! - a request or response that reaches a `google.protobuf.*` message, whose canonical JSON has
//!   special forms the output does not write;
//! - two RPCs with one command path, two inputs of one RPC with one flag, or an input whose flag
//!   is one of [`RESERVED_FLAGS`];
//! - an enum, given as a flag, with a value whose spelling is empty or repeats another's.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};

use prost_reflect::{
    DescriptorPool, EnumDescriptor, ExtensionDescriptor, FieldDescriptor, Kind, MessageDescriptor,
    MethodDescriptor, ServiceDescriptor, Value,
};

/// The flags `hostkeeper ctl` defines itself, which no request field may take.
pub const RESERVED_FLAGS: &[&str] = &[
    "ca-file",
    "client-cert-file",
    "client-key-file",
    "endpoint",
    "help",
    "output",
    "request",
    "timeout-secs",
    "token-file",
    "token-stdin",
    "transport",
];

/// Whether a service is one of a host's admin socket (its package is `hostkeeper.local.v<n>`),
/// whose verbs stay hand-written.
fn is_local(service: &ServiceDescriptor) -> bool {
    service
        .package_name()
        .split('.')
        .take(2)
        .eq(["hostkeeper", "local"])
}

/// Why the descriptors cannot be turned into a command table.
#[derive(Debug, PartialEq, Eq)]
pub enum GenError {
    /// `(hostkeeper.secret)` on a field that is not `string` or `bytes`.
    SecretNotText { field: String },
    /// A client- or server-streaming RPC.
    Streaming { method: String },
    /// A request or response that reaches a well-known type.
    WellKnownType { method: String, message: String },
    /// Two RPCs whose commands are the same words.
    CommandClash { command: String },
    /// Two inputs of an RPC with one flag, or one flag that `ctl` keeps for itself.
    FlagClash { method: String, flag: String },
    /// An enum value whose flag spelling is empty or another value's.
    EnumSpelling { value: String },
}

impl fmt::Display for GenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenError::SecretNotText { field } => write!(
                f,
                "{field}: (hostkeeper.secret) is allowed only on a string or bytes field"
            ),
            GenError::Streaming { method } => {
                write!(f, "{method}: ctl makes unary calls only; this RPC streams")
            }
            GenError::WellKnownType { method, message } => write!(
                f,
                "{method}: reaches {message}, a well-known type ctl does not write"
            ),
            GenError::CommandClash { command } => {
                write!(f, "two RPCs make the command `{command}`")
            }
            GenError::FlagClash { method, flag } => {
                write!(
                    f,
                    "{method}: the flag --{flag} is taken twice or kept by ctl"
                )
            }
            GenError::EnumSpelling { value } => {
                write!(f, "{value}: its flag spelling is empty or another value's")
            }
        }
    }
}

/// `(hostkeeper.secret)` as the pool declares it; `None` when the pool does not import it.
fn secret_extension(pool: &DescriptorPool) -> Option<ExtensionDescriptor> {
    pool.get_extension_by_name("hostkeeper.secret")
}

/// Whether the field carries `(hostkeeper.secret) = true`.
fn is_secret(field: &FieldDescriptor, ext: Option<&ExtensionDescriptor>) -> bool {
    ext.is_some_and(|ext| field.options().get_extension(ext).as_ref() == &Value::Bool(true))
}

/// The words of a CamelCase name, lower case: `SetDesired` is `set`, `desired`; an acronym is
/// one word (`GetTLSConfig` is `get`, `tls`, `config`).
fn words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let boundary = i > 0
            && c.is_ascii_uppercase()
            && (!chars[i - 1].is_ascii_uppercase()
                || chars.get(i + 1).is_some_and(char::is_ascii_lowercase));
        if (boundary || c == '_') && !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        if c != '_' {
            word.push(c.to_ascii_lowercase());
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// A CamelCase or snake_case name as a command or a flag: `SetDesired` is `set-desired`,
/// `node_id` is `node-id`.
pub fn kebab(name: &str) -> String {
    words(name).join("-")
}

/// An enum's name as its values' prefix: `ConvergenceState` is `CONVERGENCE_STATE_`.
fn value_prefix(enum_name: &str) -> String {
    format!("{}_", words(enum_name).join("_").to_ascii_uppercase())
}

/// The comment written before the element at `path` in its file, one leading space taken off
/// each line, trimmed; empty when there is none.
fn doc(file: &prost_reflect::FileDescriptor, path: &[i32]) -> String {
    let comment = file
        .file_descriptor_proto()
        .source_code_info
        .as_ref()
        .and_then(|info| info.location.iter().find(|l| l.path == path))
        .and_then(|l| l.leading_comments.as_deref())
        .unwrap_or("");
    comment
        .lines()
        .map(|line| line.strip_prefix(' ').unwrap_or(line).trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// How a request field reaches the request, and how it is described in the table.
struct Input {
    label: &'static str,
    input: &'static str,
}

/// The table's `Label` and `Input` of a field.
fn input_of(field: &FieldDescriptor, secret: bool) -> Input {
    let oneof = field.containing_oneof().is_some_and(|o| !o.is_synthetic());
    let label = if field.is_map() {
        "Map"
    } else if field.is_list() {
        "Repeated"
    } else if oneof {
        "Oneof"
    } else if field.supports_presence() && field.kind().as_message().is_none() {
        "Optional"
    } else {
        "Singular"
    };
    // A secret is text: `check_reach` refused any other before a field is written.
    let input = match label {
        "Map" => {
            let entry = field.kind();
            let entry = entry.as_message().map(|m| {
                (
                    m.map_entry_key_field().kind(),
                    m.map_entry_value_field().kind(),
                )
            });
            if matches!(entry, Some((Kind::String, Kind::String))) {
                "Flag"
            } else {
                "RequestFile"
            }
        }
        "Oneof" => "RequestFile",
        "Repeated" if secret => "RequestFile",
        _ if secret => "SecretFile",
        _ if matches!(field.kind(), Kind::Message(_) | Kind::Bytes) => "RequestFile",
        _ => "Flag",
    };
    Input { label, input }
}

/// The table's `FieldKind` of a field, and the full name of its enum or message (empty for a
/// scalar).
fn kind_of(field: &FieldDescriptor) -> (&'static str, String) {
    match field.kind() {
        Kind::Double => ("Double", String::new()),
        Kind::Float => ("Float", String::new()),
        Kind::Int32 => ("Int32", String::new()),
        Kind::Int64 => ("Int64", String::new()),
        Kind::Uint32 => ("Uint32", String::new()),
        Kind::Uint64 => ("Uint64", String::new()),
        Kind::Sint32 => ("Sint32", String::new()),
        Kind::Sint64 => ("Sint64", String::new()),
        Kind::Fixed32 => ("Fixed32", String::new()),
        Kind::Fixed64 => ("Fixed64", String::new()),
        Kind::Sfixed32 => ("Sfixed32", String::new()),
        Kind::Sfixed64 => ("Sfixed64", String::new()),
        Kind::Bool => ("Bool", String::new()),
        Kind::String => ("String", String::new()),
        Kind::Bytes => ("Bytes", String::new()),
        Kind::Enum(e) => ("Enum", e.full_name().to_owned()),
        Kind::Message(m) => ("Message", m.full_name().to_owned()),
    }
}

/// The values of an enum a flag takes: every value but the `*_UNSPECIFIED` ones, spelled
/// without the enum's prefix, in lower case, as `(spelling, name, number)`.
fn enum_values(e: &EnumDescriptor) -> Result<Vec<(String, String, i32)>, GenError> {
    let prefix = value_prefix(e.name());
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in e.values() {
        let name = value.name();
        if name.ends_with("_UNSPECIFIED") {
            continue;
        }
        let spelling = name
            .strip_prefix(&prefix)
            .unwrap_or(name)
            .to_ascii_lowercase();
        if spelling.is_empty() || !seen.insert(spelling.clone()) {
            return Err(GenError::EnumSpelling {
                value: value.full_name().to_owned(),
            });
        }
        out.push((spelling, name.to_owned(), value.number()));
    }
    Ok(out)
}

/// Checks every message `message` reaches: no well-known type, and a secret only on text.
fn check_reach(
    method: &str,
    message: &MessageDescriptor,
    ext: Option<&ExtensionDescriptor>,
    seen: &mut BTreeSet<String>,
) -> Result<(), GenError> {
    if !seen.insert(message.full_name().to_owned()) {
        return Ok(());
    }
    if message.full_name().starts_with("google.protobuf.") {
        return Err(GenError::WellKnownType {
            method: method.to_owned(),
            message: message.full_name().to_owned(),
        });
    }
    for field in message.fields() {
        if is_secret(&field, ext) && !matches!(field.kind(), Kind::String | Kind::Bytes) {
            return Err(GenError::SecretNotText {
                field: field.full_name().to_owned(),
            });
        }
        if let Kind::Message(inner) = field.kind() {
            check_reach(method, &inner, ext, seen)?;
        }
    }
    Ok(())
}

/// Appends a Rust string literal.
fn lit(out: &mut String, s: &str) {
    let _ = write!(out, "{s:?}");
}

/// The table entry of one field, appended to `out`; its flags are added to `flags`.
fn field_entry(
    out: &mut String,
    method: &str,
    field: &FieldDescriptor,
    ext: Option<&ExtensionDescriptor>,
    flags: &mut BTreeSet<String>,
) -> Result<(), GenError> {
    let secret = is_secret(field, ext);
    let Input { label, input } = input_of(field, secret);
    let flag = kebab(field.name());
    let taken = match input {
        "Flag" => vec![flag.clone()],
        "SecretFile" => vec![format!("{flag}-file"), format!("{flag}-stdin")],
        _ => Vec::new(),
    };
    for name in taken {
        if RESERVED_FLAGS.contains(&name.as_str()) || !flags.insert(name.clone()) {
            return Err(GenError::FlagClash {
                method: method.to_owned(),
                flag: name,
            });
        }
    }
    let values = match (input, field.kind()) {
        ("Flag", Kind::Enum(e)) => enum_values(&e)?,
        _ => Vec::new(),
    };
    let (kind, type_name) = kind_of(field);
    out.push_str("            Field { name: ");
    lit(out, field.name());
    out.push_str(", flag: ");
    lit(out, &flag);
    let _ = write!(out, ", kind: FieldKind::{kind}, type_name: ");
    lit(out, &type_name);
    let _ = write!(
        out,
        ", label: Label::{label}, input: Input::{input}, secret: {secret}, enum_values: &["
    );
    for (spelling, name, number) in values {
        out.push_str("EnumValue { flag: ");
        lit(out, &spelling);
        out.push_str(", name: ");
        lit(out, &name);
        let _ = write!(out, ", number: {number} }}, ");
    }
    out.push_str("], doc: ");
    lit(out, &doc(&field.parent_file(), field.path()));
    out.push_str(" },\n");
    Ok(())
}

/// The table entry of one RPC, appended to `out`.
fn command_entry(
    out: &mut String,
    service: &ServiceDescriptor,
    method: &MethodDescriptor,
    ext: Option<&ExtensionDescriptor>,
) -> Result<(), GenError> {
    let full = method.full_name();
    if method.is_client_streaming() || method.is_server_streaming() {
        return Err(GenError::Streaming {
            method: full.to_owned(),
        });
    }
    let mut seen = BTreeSet::new();
    check_reach(full, &method.input(), ext, &mut seen)?;
    check_reach(full, &method.output(), ext, &mut seen)?;
    let file = service.parent_file();
    out.push_str("    Command {\n        package: ");
    lit(out, service.package_name());
    out.push_str(",\n        service: ");
    lit(out, service.name());
    out.push_str(",\n        method: ");
    lit(out, method.name());
    out.push_str(",\n        path: ");
    lit(out, &format!("/{}/{}", service.full_name(), method.name()));
    out.push_str(",\n        service_command: ");
    lit(out, &kebab(service.name()));
    out.push_str(",\n        method_command: ");
    lit(out, &kebab(method.name()));
    out.push_str(",\n        service_doc: ");
    lit(out, &doc(&file, service.path()));
    out.push_str(",\n        doc: ");
    lit(out, &doc(&file, method.path()));
    out.push_str(",\n        request: ");
    lit(out, method.input().full_name());
    out.push_str(",\n        response: ");
    lit(out, method.output().full_name());
    out.push_str(",\n        fields: &[\n");
    let mut flags = BTreeSet::new();
    let request = method.input();
    // As the message declares them, which is not always the order of their numbers.
    let fields: Vec<FieldDescriptor> = request
        .descriptor_proto()
        .field
        .iter()
        .filter_map(|proto| request.get_field_by_name(proto.name()))
        .collect();
    for field in &fields {
        field_entry(out, full, field, ext, &mut flags)?;
    }
    out.push_str("        ],\n    },\n");
    Ok(())
}

/// The Rust source of the command table of every non-local hostkeeper service in `pool`.
///
/// Services are ordered by command name and RPCs as their service declares them.
pub fn generate(pool: &DescriptorPool) -> Result<String, GenError> {
    let ext = secret_extension(pool);
    let mut services: Vec<ServiceDescriptor> = pool
        .services()
        .filter(|s| s.package_name().split('.').next() == Some("hostkeeper"))
        .collect();
    services.sort_by_key(|s| (kebab(s.name()), s.full_name().to_owned()));

    let mut out = String::from(
        "/// Every RPC of hostkeeper's non-local services, as `hostkeeper ctl` calls it.\n\
         pub static COMMANDS: &[Command] = &[\n",
    );
    let mut local = Vec::new();
    let mut commands = BTreeSet::new();
    for service in &services {
        if is_local(service) {
            local.push(service.full_name().to_owned());
            continue;
        }
        for method in service.methods() {
            let command = format!("{} {}", kebab(service.name()), kebab(method.name()));
            if !commands.insert(command.clone()) {
                return Err(GenError::CommandClash { command });
            }
            command_entry(&mut out, service, &method, ext.as_ref())?;
        }
    }
    out.push_str("];\n\n");
    out.push_str(
        "/// The services of a host's admin socket (packages `hostkeeper.local.v<n>`): their verbs \
         stay hand-written, and `ctl` has no command for them.\n\
         pub static HAND_WRITTEN_LOCAL: &[&str] = &[",
    );
    for name in &local {
        lit(&mut out, name);
        out.push_str(", ");
    }
    out.push_str("];\n\n");
    out.push_str(
        "/// The flags `hostkeeper ctl` defines itself; no request field takes one of them.\n\
         pub static RESERVED_FLAGS: &[&str] = &[",
    );
    for flag in RESERVED_FLAGS {
        lit(&mut out, flag);
        out.push_str(", ");
    }
    out.push_str("];\n");
    Ok(out)
}

// The path is spelled out because this file is also a module of two build scripts.
#[cfg(test)]
#[path = "ctl_gen/tests.rs"]
mod tests;
