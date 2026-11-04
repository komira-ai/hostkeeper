//! `(hostkeeper.secret)` at run time: which fields of a message are secrets, and a copy of a
//! message with every secret taken out, at any depth, for anything `ctl` prints. Also the
//! credentials a diagnostic must not repeat: the secret values of a request, the bearer token,
//! and the values among the arguments that may be either.

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

/// What takes the place of a credential in a diagnostic.
pub const REDACTED: &str = "<redacted>";

/// The text of every non-empty secret value set in `message` and in every message it holds. A
/// `bytes` secret counts when it is UTF-8 text.
pub fn values(message: &DynamicMessage, ext: Option<&ExtensionDescriptor>) -> Vec<String> {
    let mut found = Vec::new();
    collect(message, ext, &mut found);
    found
}

fn collect(message: &DynamicMessage, ext: Option<&ExtensionDescriptor>, found: &mut Vec<String>) {
    for (field, value) in message.fields() {
        if is_secret(&field, ext) {
            texts(value, found);
        } else {
            collect_value(value, ext, found);
        }
    }
}

fn collect_value(value: &Value, ext: Option<&ExtensionDescriptor>, found: &mut Vec<String>) {
    match value {
        Value::Message(inner) => collect(inner, ext, found),
        Value::List(items) => {
            for item in items {
                collect_value(item, ext, found);
            }
        }
        Value::Map(entries) => {
            for item in entries.values() {
                collect_value(item, ext, found);
            }
        }
        _ => {}
    }
}

/// The non-empty text of a secret field's value, or of each element of a list of them.
fn texts(value: &Value, found: &mut Vec<String>) {
    if let Some(items) = value.as_list() {
        for item in items {
            texts(item, found);
        }
        return;
    }
    let text = value
        .as_str()
        .or_else(|| value.as_bytes().and_then(|b| std::str::from_utf8(b).ok()));
    found.extend(text.filter(|t| !t.is_empty()).map(str::to_owned));
}

/// `text` with every occurrence of every credential replaced by [`REDACTED`]. At each place the
/// longest credential that starts there is the one replaced, and the replacement is never
/// searched again. An empty credential matches nothing.
pub fn scrub(text: &str, credentials: &[String]) -> String {
    let mut ordered: Vec<&str> = credentials
        .iter()
        .map(String::as_str)
        .filter(|c| !c.is_empty())
        .collect();
    ordered.sort_by_key(|c| std::cmp::Reverse(c.len()));
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        if let Some(credential) = ordered.iter().copied().find(|c| rest.starts_with(*c)) {
            out.push_str(REDACTED);
            rest = &rest[credential.len()..];
        } else {
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

/// Every long flag of a command and of the commands under it, and `help`.
pub fn known_flags(cli: &clap::Command) -> BTreeSet<String> {
    let mut flags = BTreeSet::from(["help".to_owned()]);
    add_flags(cli, &mut flags);
    flags
}

fn add_flags(cli: &clap::Command, flags: &mut BTreeSet<String>) {
    flags.extend(
        cli.get_arguments()
            .filter_map(|a| a.get_long().map(str::to_owned)),
    );
    for sub in cli.get_subcommands() {
        add_flags(sub, flags);
    }
}

/// The values among `args` that may be credentials, which clap could repeat in a usage error:
/// the value attached to a flag that ends in `-stdin` (`--token-stdin=value`), the argument that
/// follows such a flag when it does not start with `-`, and the value attached to any flag
/// that is not in `known` (`--password=value`: a secret's name typed as a flag).
pub fn argument_credentials(args: &[String], known: &BTreeSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    let mut after_stdin = false;
    for arg in args {
        if after_stdin && !arg.starts_with('-') {
            found.push(arg.clone());
        }
        after_stdin = false;
        let Some(long) = arg.strip_prefix("--") else {
            continue;
        };
        match long.split_once('=') {
            Some((name, value)) => {
                if name.ends_with("-stdin") || !known.contains(name) {
                    found.push(value.to_owned());
                }
            }
            None => after_stdin = long.ends_with("-stdin"),
        }
    }
    found
}

#[cfg(test)]
mod tests;
