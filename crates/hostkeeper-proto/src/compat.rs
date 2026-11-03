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
//! - a field whose type (its descriptor type, and the message or enum it names) or label
//!   (optional, repeated, required) changed;
//! - a singular field whose presence changed (implicit, as a proto3 scalar has it, or explicit, as
//!   a proto2 field, a message, a `optional` proto3 field or a oneof member has it), as moving it
//!   into a oneof or its file to another syntax does;
//! - a field that joined, left or moved to another oneof (named, so a oneof renumbered among its
//!   message's oneofs is no break; the synthetic oneof of a proto3 `optional` field is not one);
//! - a field whose default value (proto2) or JSON name changed;
//! - a message that became, or stopped being, a map entry;
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

use prost_types::field_descriptor_proto::{Label, Type};
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
    /// The label (optional, repeated, required) changed.
    FieldLabelChanged {
        message: String,
        name: String,
        from: String,
        to: String,
    },
    /// The field's presence, oneof, default value or JSON name changed.
    FieldAttributeChanged {
        message: String,
        name: String,
        attribute: Attribute,
        from: String,
        to: String,
    },
    /// The message became, or stopped being, the entry of a map field.
    MapEntryChanged {
        message: String,
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

/// An attribute of a field other than its name, number, type and label.
#[derive(Debug, PartialEq, Eq)]
pub enum Attribute {
    /// `implicit` or `explicit`, for a singular field.
    Presence,
    /// The oneof's name, or `none`.
    Oneof,
    /// The proto2 default value; empty when there is none.
    Default,
    /// The name in JSON.
    JsonName,
}

/// Every way `new` breaks `old`: packages first, then messages, enums and services, each in name
/// order. Empty when `new` only adds.
pub fn breaks(old: &FileDescriptorSet, new: &FileDescriptorSet) -> Vec<Break> {
    let mut out = Vec::new();
    packages(old, new, &mut out);

    let (old_messages, new_messages) = (messages(old), messages(new));
    for (name, before) in &old_messages {
        match new_messages.get(name) {
            Some(after) => {
                if is_map_entry(before.proto) != is_map_entry(after.proto) {
                    out.push(Break::MapEntryChanged {
                        message: name.clone(),
                    });
                }
                fields(name, *before, *after, &mut out);
            }
            None => out.push(Break::MessageRemoved {
                message: name.clone(),
            }),
        }
    }
    for (name, after) in &new_messages {
        let before = old_messages
            .get(name)
            .map_or(&[][..], |m| &m.proto.field[..]);
        for field in &after.proto.field {
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

/// A message, and whether its file gives every singular field explicit presence (proto2).
#[derive(Clone, Copy)]
struct Message<'a> {
    proto: &'a DescriptorProto,
    proto2: bool,
}

/// Every message of the set, nested ones included, by fully qualified name.
fn messages(set: &FileDescriptorSet) -> BTreeMap<String, Message<'_>> {
    let mut out = BTreeMap::new();
    for file in &set.file {
        let proto2 = file.syntax() != "proto3";
        collect_messages(&scope(file.package()), &file.message_type, proto2, &mut out);
    }
    out
}

fn collect_messages<'a>(
    scope: &str,
    list: &'a [DescriptorProto],
    proto2: bool,
    out: &mut BTreeMap<String, Message<'a>>,
) {
    for proto in list {
        let name = format!("{scope}.{}", proto.name());
        collect_messages(&name, &proto.nested_type, proto2, out);
        out.insert(name, Message { proto, proto2 });
    }
}

fn is_map_entry(message: &DescriptorProto) -> bool {
    message.options.as_ref().is_some_and(|o| o.map_entry())
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
        for e in &message.proto.enum_type {
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

/// What a field holds: its descriptor type, and the message or enum it names, if it names one.
/// A group and a message field of one message differ, as their encodings do.
fn shape(field: &FieldDescriptorProto) -> String {
    let kind = field.r#type().as_str_name();
    match field.type_name() {
        "" => kind.to_owned(),
        name => format!("{kind} {name}"),
    }
}

/// Whether a singular field's presence is explicit or implicit.
fn presence(field: &FieldDescriptorProto, message: Message<'_>) -> &'static str {
    let explicit = matches!(field.r#type(), Type::Message | Type::Group)
        || message.proto2
        || field.proto3_optional()
        || field.oneof_index.is_some();
    if explicit { "explicit" } else { "implicit" }
}

/// The name of the oneof a field belongs to, or `none`. A proto3 `optional` field's synthetic
/// oneof is not a oneof here: its presence says it.
fn oneof(field: &FieldDescriptorProto, message: Message<'_>) -> String {
    let declared = field
        .oneof_index
        .filter(|_| !field.proto3_optional())
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| message.proto.oneof_decl.get(i));
    declared.map_or_else(|| "none".to_owned(), |o| o.name().to_owned())
}

fn fields(message: &str, before: Message<'_>, after: Message<'_>, out: &mut Vec<Break>) {
    for old in &before.proto.field {
        let by_name = after.proto.field.iter().find(|f| f.name() == old.name());
        let Some(now) = after
            .proto
            .field
            .iter()
            .find(|f| f.number() == old.number())
        else {
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
        let (was, is) = (old.label(), now.label());
        if was != is {
            out.push(Break::FieldLabelChanged {
                message: message.to_owned(),
                name: old.name().to_owned(),
                from: was.as_str_name().to_owned(),
                to: is.as_str_name().to_owned(),
            });
        } else if was != Label::Repeated {
            let (was, is) = (presence(old, before), presence(now, after));
            if was != is {
                out.push(changed(message, old, Attribute::Presence, was, is));
            }
        }
        let (was, is) = (oneof(old, before), oneof(now, after));
        if was != is {
            out.push(changed(message, old, Attribute::Oneof, &was, &is));
        }
        if old.default_value() != now.default_value() {
            let (was, is) = (old.default_value(), now.default_value());
            out.push(changed(message, old, Attribute::Default, was, is));
        }
        if now.name() == old.name() && now.json_name() != old.json_name() {
            let (was, is) = (old.json_name(), now.json_name());
            out.push(changed(message, old, Attribute::JsonName, was, is));
        }
    }
}

fn changed(
    message: &str,
    field: &FieldDescriptorProto,
    attribute: Attribute,
    from: &str,
    to: &str,
) -> Break {
    Break::FieldAttributeChanged {
        message: message.to_owned(),
        name: field.name().to_owned(),
        attribute,
        from: from.to_owned(),
        to: to.to_owned(),
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
