//! The compatibility rule of hostkeeper's protos, as a comparison of two descriptor sets.
//!
//! A later set may add packages, services, RPCs, messages, fields, enums and enum values. It may
//! not take anything away or change what is there, because a release before it still sends and
//! reads the old shapes. [`breaks`] lists every way `new` does that to `old`:
//!
//! - a package that no file declares any more, or a file that now declares another package;
//! - a message or enum that is gone (renaming one is removing it);
//! - a field number that is gone: renumbered when its name is now at another number, removed
//!   otherwise;
//! - a field number that now has another name, and that name's reuse at another number;
//! - a field whose type (scalar type, or message or enum name) or label (singular, repeated,
//!   explicit presence) changed;
//! - a new field marked required, anywhere;
//! - an enum value that is gone: renumbered when its name is now at another number, removed
//!   otherwise (a number that now has another name is a removed value);
//! - a service or an RPC that is gone (renaming one is removing it), and an RPC whose request
//!   type, response type, or streaming on either side changed.
//!
//! Names are compared fully qualified (`.hostkeeper.agent.v1.ServiceBeat`), so a message that
//! moves to another file of the same package is no break. Comments, options and source
//! positions are not compared.
//!
//! The test that holds the protos to the rule compares this crate's descriptor set with
//! `proto/compat/baseline.binpb`, the set the last release committed. Writing a new baseline is
//! the release's step, never a pull request's.

use std::collections::{BTreeMap, BTreeSet};

use prost_types::field_descriptor_proto::Label;
use prost_types::{
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorSet,
    ServiceDescriptorProto,
};

/// One way a descriptor set breaks an earlier one. Names are fully qualified and numbers are
/// field or enum-value numbers.
#[derive(Debug, PartialEq, Eq)]
pub enum Break {
    /// No file declares the package any more.
    PackageRemoved {
        package: String,
    },
    /// The file declares another package.
    PackageChanged {
        file: String,
        from: String,
        to: String,
    },
    MessageRemoved {
        message: String,
    },
    FieldRemoved {
        message: String,
        number: i32,
        name: String,
    },
    /// The field's name is at another number, and its number holds nothing.
    FieldRenumbered {
        message: String,
        name: String,
        from: i32,
        to: i32,
    },
    /// The field's number holds another name.
    FieldRenamed {
        message: String,
        number: i32,
        from: String,
        to: String,
    },
    /// The name a number held is now at another number, and the number holds another name.
    NameReused {
        message: String,
        name: String,
        from: i32,
        to: i32,
    },
    /// The scalar type, or the message or enum, of the field changed.
    FieldTypeChanged {
        message: String,
        name: String,
        from: String,
        to: String,
    },
    /// Singular, repeated, required or explicit presence changed.
    FieldLabelChanged {
        message: String,
        name: String,
        from: String,
        to: String,
    },
    /// A field that is not in the earlier set is required.
    RequiredAdded {
        message: String,
        name: String,
    },
    EnumRemoved {
        name: String,
    },
    EnumValueRemoved {
        name: String,
        number: i32,
        value: String,
    },
    /// The value's name is at another number.
    EnumValueRenumbered {
        name: String,
        value: String,
        from: i32,
        to: i32,
    },
    ServiceRemoved {
        service: String,
    },
    RpcRemoved {
        service: String,
        rpc: String,
    },
    RequestTypeChanged {
        service: String,
        rpc: String,
        from: String,
        to: String,
    },
    ResponseTypeChanged {
        service: String,
        rpc: String,
        from: String,
        to: String,
    },
    /// The request or the response became, or stopped being, a stream.
    StreamingChanged {
        service: String,
        rpc: String,
    },
}

/// Every way `new` breaks `old`: packages first, then messages, enums and services, each in name
/// order. Empty when `new` only adds.
pub fn breaks(old: &FileDescriptorSet, new: &FileDescriptorSet) -> Vec<Break> {
    let mut out = Vec::new();
    packages(old, new, &mut out);

    let (old_messages, new_messages) = (messages(old), messages(new));
    for (name, before) in &old_messages {
        match new_messages.get(name) {
            Some(after) => fields(name, before, after, &mut out),
            None => out.push(Break::MessageRemoved {
                message: name.clone(),
            }),
        }
    }
    for (name, after) in &new_messages {
        let before = old_messages.get(name).map_or(&[][..], |m| &m.field[..]);
        for field in &after.field {
            let added = !before.iter().any(|b| b.number() == field.number());
            if added && field.label() == Label::Required {
                out.push(Break::RequiredAdded {
                    message: name.clone(),
                    name: field.name().to_owned(),
                });
            }
        }
    }

    let new_enums = enums(new);
    for (name, before) in enums(old) {
        match new_enums.get(&name) {
            Some(after) => values(&name, before, after, &mut out),
            None => out.push(Break::EnumRemoved { name }),
        }
    }

    let new_services = services(new);
    for (name, before) in services(old) {
        match new_services.get(&name) {
            Some(after) => rpcs(&name, before, after, &mut out),
            None => out.push(Break::ServiceRemoved { service: name }),
        }
    }
    out
}

fn packages(old: &FileDescriptorSet, new: &FileDescriptorSet, out: &mut Vec<Break>) {
    let declared: BTreeSet<&str> = new.file.iter().map(|f| f.package()).collect();
    let before: BTreeSet<&str> = old.file.iter().map(|f| f.package()).collect();
    for package in before.difference(&declared) {
        out.push(Break::PackageRemoved {
            package: (*package).to_owned(),
        });
    }
    for file in &old.file {
        let moved = new
            .file
            .iter()
            .filter(|f| f.name() == file.name() && f.package() != file.package());
        for now in moved {
            out.push(Break::PackageChanged {
                file: file.name().to_owned(),
                from: file.package().to_owned(),
                to: now.package().to_owned(),
            });
        }
    }
}

/// The scope a file's top-level names live in: `.package`.
fn scope(package: &str) -> String {
    format!(".{package}")
}

/// Every message of the set, nested ones included, by fully qualified name.
fn messages(set: &FileDescriptorSet) -> BTreeMap<String, &DescriptorProto> {
    let mut out = BTreeMap::new();
    for file in &set.file {
        collect_messages(&scope(file.package()), &file.message_type, &mut out);
    }
    out
}

fn collect_messages<'a>(
    scope: &str,
    list: &'a [DescriptorProto],
    out: &mut BTreeMap<String, &'a DescriptorProto>,
) {
    for message in list {
        let name = format!("{scope}.{}", message.name());
        collect_messages(&name, &message.nested_type, out);
        out.insert(name, message);
    }
}

/// Every enum of the set, nested ones included, by fully qualified name.
fn enums(set: &FileDescriptorSet) -> BTreeMap<String, &EnumDescriptorProto> {
    let mut out = BTreeMap::new();
    for file in &set.file {
        let scope = scope(file.package());
        for e in &file.enum_type {
            out.insert(format!("{scope}.{}", e.name()), e);
        }
    }
    for (name, message) in messages(set) {
        for e in &message.enum_type {
            out.insert(format!("{name}.{}", e.name()), e);
        }
    }
    out
}

/// Every service of the set, by fully qualified name.
fn services(set: &FileDescriptorSet) -> BTreeMap<String, &ServiceDescriptorProto> {
    let mut out = BTreeMap::new();
    for file in &set.file {
        let scope = scope(file.package());
        for service in &file.service {
            out.insert(format!("{scope}.{}", service.name()), service);
        }
    }
    out
}

/// What a field holds: its message or enum when it names one, its scalar type otherwise.
fn shape(field: &FieldDescriptorProto) -> String {
    match field.type_name() {
        "" => field.r#type().as_str_name().to_owned(),
        name => name.to_owned(),
    }
}

/// How many a field holds, and whether its presence is explicit (`optional` in proto3).
fn label(field: &FieldDescriptorProto) -> String {
    let label = field.label().as_str_name();
    if field.proto3_optional() {
        format!("{label} (explicit presence)")
    } else {
        label.to_owned()
    }
}

fn fields(message: &str, before: &DescriptorProto, after: &DescriptorProto, out: &mut Vec<Break>) {
    for old in &before.field {
        let by_name = after.field.iter().find(|f| f.name() == old.name());
        let Some(now) = after.field.iter().find(|f| f.number() == old.number()) else {
            out.push(match by_name {
                Some(moved) => Break::FieldRenumbered {
                    message: message.to_owned(),
                    name: old.name().to_owned(),
                    from: old.number(),
                    to: moved.number(),
                },
                None => Break::FieldRemoved {
                    message: message.to_owned(),
                    number: old.number(),
                    name: old.name().to_owned(),
                },
            });
            continue;
        };
        if now.name() != old.name() {
            out.push(Break::FieldRenamed {
                message: message.to_owned(),
                number: old.number(),
                from: old.name().to_owned(),
                to: now.name().to_owned(),
            });
            // A field of the old name elsewhere is at another number: the number it had is `now`.
            out.extend(by_name.map(|moved| Break::NameReused {
                message: message.to_owned(),
                name: old.name().to_owned(),
                from: old.number(),
                to: moved.number(),
            }));
        }
        let (was, is) = (shape(old), shape(now));
        if was != is {
            out.push(Break::FieldTypeChanged {
                message: message.to_owned(),
                name: old.name().to_owned(),
                from: was,
                to: is,
            });
        }
        let (was, is) = (label(old), label(now));
        if was != is {
            out.push(Break::FieldLabelChanged {
                message: message.to_owned(),
                name: old.name().to_owned(),
                from: was,
                to: is,
            });
        }
    }
}

fn values(
    name: &str,
    before: &EnumDescriptorProto,
    after: &EnumDescriptorProto,
    out: &mut Vec<Break>,
) {
    for old in &before.value {
        let kept = after
            .value
            .iter()
            .any(|v| v.number() == old.number() && v.name() == old.name());
        if kept {
            continue;
        }
        out.push(match after.value.iter().find(|v| v.name() == old.name()) {
            Some(moved) => Break::EnumValueRenumbered {
                name: name.to_owned(),
                value: old.name().to_owned(),
                from: old.number(),
                to: moved.number(),
            },
            None => Break::EnumValueRemoved {
                name: name.to_owned(),
                number: old.number(),
                value: old.name().to_owned(),
            },
        });
    }
}

fn rpcs(
    service: &str,
    before: &ServiceDescriptorProto,
    after: &ServiceDescriptorProto,
    out: &mut Vec<Break>,
) {
    for old in &before.method {
        let Some(now) = after.method.iter().find(|m| m.name() == old.name()) else {
            out.push(Break::RpcRemoved {
                service: service.to_owned(),
                rpc: old.name().to_owned(),
            });
            continue;
        };
        if now.input_type() != old.input_type() {
            out.push(Break::RequestTypeChanged {
                service: service.to_owned(),
                rpc: old.name().to_owned(),
                from: old.input_type().to_owned(),
                to: now.input_type().to_owned(),
            });
        }
        if now.output_type() != old.output_type() {
            out.push(Break::ResponseTypeChanged {
                service: service.to_owned(),
                rpc: old.name().to_owned(),
                from: old.output_type().to_owned(),
                to: now.output_type().to_owned(),
            });
        }
        let streams =
            |m: &prost_types::MethodDescriptorProto| (m.client_streaming(), m.server_streaming());
        if streams(now) != streams(old) {
            out.push(Break::StreamingChanged {
                service: service.to_owned(),
                rpc: old.name().to_owned(),
            });
        }
    }
}

#[cfg(test)]
mod tests;
