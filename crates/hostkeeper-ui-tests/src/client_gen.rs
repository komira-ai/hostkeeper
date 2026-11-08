//! The generator of the web app's API client, `ui/gen/client.js`, and its TypeScript
//! declarations, `ui/gen/client.d.ts`.
//!
//! It reads the command table of `hostkeeper-proto` (`hostkeeper_proto::ctl::COMMANDS`, one
//! entry per unary RPC, which `hostkeeper ctl` reads too) for the functions, and the descriptor
//! set the table was generated from for the types of their requests and responses. A new RPC of
//! [`PACKAGE`] is a new function at the next run, with no hand edit. Both files are checked in:
//! the app is static files with no build step, and a test of this crate (`tests/client.rs`)
//! fails when they differ from what [`current`] makes.
//!
//! - `client.js` exports `createClient(call)`: one frozen object per service, in the table's
//!   order, with one function per method, `releases.setDesired(request)` being
//!   `call("hostkeeper.operator.v1.Releases/SetDesired", request)`. `ui/api.js` gives it the
//!   `call` that sends the request and keeps the token rules.
//! - `client.d.ts` declares `createClient`, the `Client` it returns and every message and enum
//!   a request or a response reaches, as proto3 JSON writes them: each field by its JSON name and
//!   optional (a default is not written), a 64-bit integer as a string, `bytes` as base64 text,
//!   a float or a double as a number or one of the three names JSON gives the values it has no
//!   number for, an enum as the names of its values, a map as an object. Each type is declared
//!   in its package's namespace, the package's name with `_` for `.`, a nested one inside its
//!   message's namespace, so `hostkeeper_agent_v1.ServiceBeat` is the type of that message and no
//!   two types share a name. (With dots, a reference to the operator package's `Actor` would end
//!   in a top-level domain, and read as a host name to the repository's hygiene lint.) The comments of the protos are not copied: what a method does and what a field holds
//!   is said in its `.proto` file, and the declarations stay short enough to read.
//!
//! What it refuses, so that a test fails rather than the client misdescribing a call:
//!
//! - a request or response the descriptors do not hold ([`GenError::UnknownMessage`]);
//! - a type that reaches a `google.protobuf.*` message, whose JSON has forms of its own
//!   ([`GenError::WellKnownType`]); the command table refuses them too;
//! - a type of a file with no package, or of a package whose namespace another package's takes
//!   (`a_b.c` and `a.b_c`), which the declarations could not keep apart
//!   ([`GenError::Namespace`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use hostkeeper_proto::ctl::Command;
use prost_reflect::{DescriptorPool, EnumDescriptor, FieldDescriptor, Kind, MessageDescriptor};

/// The package whose services the client calls: the operator API.
pub const PACKAGE: &str = "hostkeeper.operator.v1";

/// The longest line of an enum's declaration on one line; a longer one has a line per value.
const WIDTH: usize = 100;

/// What the header of both files says about where they come from.
const HEADER: &str = "\
// Generated from the protobuf descriptors of hostkeeper-proto by hostkeeper-ui-tests
// (`client_gen`). Do not edit: `cargo test -p hostkeeper-ui-tests --test client` fails when
// this file differs from what the descriptors make, and writes the current text beside its test
// binary.
";

/// One function of the client: `<service>.<name>(request)` calls `method`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    /// The service's object: `releases`.
    pub service: String,
    /// The function's name in it: `setDesired`.
    pub name: String,
    /// The method, as the gateway's path names it without its leading `/`:
    /// `hostkeeper.operator.v1.Releases/SetDesired`.
    pub method: String,
}

impl Function {
    /// How a page calls it, after `api.`: `releases.setDesired`.
    pub fn call(&self) -> String {
        format!("{}.{}", self.service, self.name)
    }
}

/// The two generated files' texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    /// `ui/gen/client.js`.
    pub js: String,
    /// `ui/gen/client.d.ts`.
    pub dts: String,
}

/// Why the client cannot be generated.
#[derive(Debug, thiserror::Error)]
pub enum GenError {
    /// The descriptor set does not decode.
    #[error("the descriptor set does not decode: {0}")]
    Descriptors(#[from] prost_reflect::DescriptorError),
    /// A command's request or response is not a message of the descriptors.
    #[error("{method}: {message} is not a message of the descriptors")]
    UnknownMessage { method: String, message: String },
    /// A request or a response reaches a well-known type.
    #[error("{message} is a well-known type, whose JSON the client does not describe")]
    WellKnownType { message: String },
    /// A reached type's package has no name, or its namespace is another package's.
    #[error("the package {package:?} has no namespace of its own in the declarations")]
    Namespace { package: String },
}

/// A package's namespace in the declarations: its name with `_` for `.`.
fn namespace(package: &str) -> String {
    package.replace('.', "_")
}

/// How the declarations name the type `full_name` of `package`: the package's namespace, then
/// the type's name within the package (`hostkeeper_operator_v1.Host.State`).
fn qualified(full_name: &str, package: &str) -> String {
    let local = full_name.get(package.len() + 1..).unwrap_or_default();
    format!("{}.{local}", namespace(package))
}

/// A kebab-case command word as a JavaScript name: `set-desired` is `setDesired`.
fn camel(kebab: &str) -> String {
    let mut out = String::new();
    for (i, word) in kebab.split('-').enumerate() {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            if i == 0 {
                out.push(first);
            } else {
                out.push(first.to_ascii_uppercase());
            }
            out.extend(chars);
        }
    }
    out
}

/// The commands of [`PACKAGE`], in the table's order.
fn operator(commands: &[Command]) -> impl Iterator<Item = &Command> {
    commands.iter().filter(|c| c.package == PACKAGE)
}

/// Every function of the client of `commands`, in the table's order: services by command name,
/// methods as their service declares them.
pub fn functions(commands: &[Command]) -> Vec<Function> {
    operator(commands)
        .map(|c| Function {
            service: camel(c.service_command),
            name: camel(c.method_command),
            method: format!("{}.{}/{}", c.package, c.service, c.method),
        })
        .collect()
}

/// The descriptor pool of an encoded descriptor set.
///
/// # Errors
/// [`GenError::Descriptors`]: the bytes are not a descriptor set.
pub fn pool(encoded: &[u8]) -> Result<DescriptorPool, GenError> {
    Ok(DescriptorPool::decode(encoded)?)
}

/// The client of hostkeeper's own command table and descriptors: what `ui/gen/` must hold.
///
/// # Errors
/// [`GenError`]: the descriptors do not decode, or a type cannot be described.
pub fn current() -> Result<Client, GenError> {
    emit(
        hostkeeper_proto::ctl::COMMANDS,
        &pool(hostkeeper_proto::FILE_DESCRIPTOR_SET)?,
    )
}

/// A message or an enum the declarations name.
#[derive(Clone)]
enum Type {
    Message(MessageDescriptor),
    Enum(EnumDescriptor),
}

impl Type {
    fn full_name(&self) -> &str {
        match self {
            Type::Message(m) => m.full_name(),
            Type::Enum(e) => e.full_name(),
        }
    }

    fn name(&self) -> &str {
        match self {
            Type::Message(m) => m.name(),
            Type::Enum(e) => e.name(),
        }
    }

    /// The message it is declared in, if it is nested.
    fn parent(&self) -> Option<MessageDescriptor> {
        match self {
            Type::Message(m) => m.parent_message(),
            Type::Enum(e) => e.parent_message(),
        }
    }

    fn package(&self) -> &str {
        match self {
            Type::Message(m) => m.package_name(),
            Type::Enum(e) => e.package_name(),
        }
    }

    /// How the declarations name it.
    fn qualified(&self) -> String {
        qualified(self.full_name(), self.package())
    }
}

/// Every message and enum the declarations need, by full name: those `message` reaches through
/// its fields (a map's entry is no type of its own, its value is), and `message` itself.
fn reach(message: &MessageDescriptor, out: &mut BTreeMap<String, Type>) -> Result<(), GenError> {
    if message.full_name().starts_with("google.protobuf.") {
        return Err(GenError::WellKnownType {
            message: message.full_name().to_owned(),
        });
    }
    if !message.is_map_entry()
        && out
            .insert(
                message.full_name().to_owned(),
                Type::Message(message.clone()),
            )
            .is_some()
    {
        return Ok(());
    }
    for field in message.fields() {
        match field.kind() {
            Kind::Message(inner) => reach(&inner, out)?,
            Kind::Enum(e) => {
                out.insert(e.full_name().to_owned(), Type::Enum(e));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The TypeScript type of one value of `kind`, as proto3 JSON writes it.
fn value_type(kind: &Kind) -> String {
    match kind {
        Kind::Double | Kind::Float => "number | \"NaN\" | \"Infinity\" | \"-Infinity\"".to_owned(),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 | Kind::Uint32 | Kind::Fixed32 => {
            "number".to_owned()
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 | Kind::Uint64 | Kind::Fixed64 => {
            "string".to_owned()
        }
        Kind::Bool => "boolean".to_owned(),
        Kind::String | Kind::Bytes => "string".to_owned(),
        Kind::Message(m) => qualified(m.full_name(), m.package_name()),
        Kind::Enum(e) => qualified(e.full_name(), e.package_name()),
    }
}

/// The TypeScript type of a field: a value, an array of them, or an object of a map's values.
fn field_type(field: &FieldDescriptor) -> String {
    let kind = field.kind();
    if field.is_map() {
        let entry = kind
            .as_message()
            .map(MessageDescriptor::map_entry_value_field);
        let value = entry.map_or_else(String::new, |v| value_type(&v.kind()));
        format!("{{ [key: string]: {value} }}")
    } else if field.is_list() {
        let value = value_type(&kind);
        if value.contains('|') {
            format!("({value})[]")
        } else {
            format!("{value}[]")
        }
    } else {
        value_type(&kind)
    }
}

/// Appends the declaration of `ty` at `indent`: an interface of its fields, as the message
/// declares them, or a union of its values' names.
fn declare(out: &mut String, indent: &str, ty: &Type) {
    match ty {
        Type::Message(m) => {
            let _ = writeln!(out, "{indent}export interface {} {{", m.name());
            let declared: Vec<FieldDescriptor> = m
                .descriptor_proto()
                .field
                .iter()
                .filter_map(|proto| m.get_field_by_name(proto.name()))
                .collect();
            let inner = format!("{indent}  ");
            for field in declared {
                let _ = writeln!(
                    out,
                    "{inner}{}?: {};",
                    field.json_name(),
                    field_type(&field)
                );
            }
            let _ = writeln!(out, "{indent}}}");
        }
        Type::Enum(e) => {
            let names: Vec<String> = e.values().map(|v| format!("\"{}\"", v.name())).collect();
            let line = format!("{indent}export type {} = {};", e.name(), names.join(" | "));
            if line.len() <= WIDTH {
                let _ = writeln!(out, "{line}");
            } else {
                let _ = writeln!(out, "{indent}export type {} =", e.name());
                let _ = writeln!(
                    out,
                    "{indent}  | {};",
                    names.join(&format!("\n{indent}  | "))
                );
            }
        }
    }
}

/// The scope a type is declared in: its message's full name, or its package's.
fn scope_of(ty: &Type) -> String {
    ty.parent()
        .map_or_else(|| ty.package().to_owned(), |p| p.full_name().to_owned())
}

/// Where the declarations go: the types each scope declares, by name.
struct Scopes<'a> {
    /// The types the declarations need.
    needed: &'a BTreeMap<String, Type>,
    /// By scope: the needed types declared in it, and the messages that hold one.
    members: BTreeMap<String, Vec<Type>>,
}

/// Appends the members of `scope` at `indent`: each type declared in it that the declarations
/// need, by name, and after a message the namespace of its nested types, when one is needed. A
/// message that is not needed but holds one is only that namespace.
fn members(out: &mut String, indent: &str, scope: &str, scopes: &Scopes<'_>) {
    let inner = format!("{indent}  ");
    for ty in scopes.members.get(scope).into_iter().flatten() {
        if scopes.needed.contains_key(ty.full_name()) {
            declare(out, indent, ty);
        }
        if scopes.members.contains_key(ty.full_name()) {
            let _ = writeln!(out, "{indent}export namespace {} {{", ty.name());
            members(out, &inner, ty.full_name(), scopes);
            let _ = writeln!(out, "{indent}}}");
        }
    }
}

/// The client of `commands`, with the types of `pool`.
///
/// # Errors
/// [`GenError`]: a command's message is not in `pool`, or reaches a well-known type.
pub fn emit(commands: &[Command], pool: &DescriptorPool) -> Result<Client, GenError> {
    let mut types = BTreeMap::new();
    for command in operator(commands) {
        for name in [command.request, command.response] {
            let message =
                pool.get_message_by_name(name)
                    .ok_or_else(|| GenError::UnknownMessage {
                        method: format!(
                            "{}.{}/{}",
                            command.package, command.service, command.method
                        ),
                        message: name.to_owned(),
                    })?;
            reach(&message, &mut types)?;
        }
    }
    let mut namespaces: BTreeMap<String, &str> = BTreeMap::new();
    for ty in types.values() {
        let package = ty.package();
        let taken = namespaces.insert(namespace(package), package);
        if package.is_empty() || taken.is_some_and(|other| other != package) {
            return Err(GenError::Namespace {
                package: package.to_owned(),
            });
        }
    }
    Ok(Client {
        js: js(commands),
        dts: dts(commands, &types),
    })
}

/// The text of `client.js`.
fn js(commands: &[Command]) -> String {
    let mut out = String::from(HEADER);
    out.push_str(
        "//\n\
         // `createClient(call)` returns one object per service of the operator API, each with one\n\
         // function per method: `releases.setDesired(request)` is\n\
         // `call(\"hostkeeper.operator.v1.Releases/SetDesired\", request)`, and returns what `call`\n\
         // returns. `ui/api.js` gives it the `call` that sends the request. The types of every\n\
         // request and reply are in `client.d.ts`.\n\n\
         export function createClient(call) {\n  return Object.freeze({\n",
    );
    let mut services: Vec<(String, Vec<Function>)> = Vec::new();
    for function in functions(commands) {
        match services.last_mut() {
            Some((name, list)) if *name == function.service => list.push(function),
            _ => services.push((function.service.clone(), vec![function])),
        }
    }
    for (service, list) in services {
        let _ = writeln!(out, "    {service}: Object.freeze({{");
        for function in list {
            let _ = writeln!(
                out,
                "      {}: (request) => call(\"{}\", request),",
                function.name, function.method
            );
        }
        out.push_str("    }),\n");
    }
    out.push_str("  });\n}\n");
    out
}

/// The text of `client.d.ts`.
fn dts(commands: &[Command], types: &BTreeMap<String, Type>) -> String {
    let mut out = String::from(HEADER);
    out.push_str(
        "//\n\
         // The types of `client.js`: each request and reply as proto3 JSON writes it. What each\n\
         // method does, and what each field holds, is said beside it in its `.proto` file under\n\
         // `crates/hostkeeper-proto/proto`.\n\n\
         /** Sends `request` to `method` (`<package>.<Service>/<Method>`) and resolves to the reply. */\n\
         export type Call = (method: string, request: object) => Promise<object>;\n\n\
         /** The operator API: one object per service, one function per method. */\n\
         export interface Client {\n",
    );
    let name = |full: &str| types.get(full).map_or_else(String::new, Type::qualified);
    let mut open: Option<&str> = None;
    for command in operator(commands) {
        if open != Some(command.service_command) {
            if open.is_some() {
                out.push_str("  };\n");
            }
            let _ = writeln!(out, "  readonly {}: {{", camel(command.service_command));
            open = Some(command.service_command);
        }
        let _ = writeln!(
            out,
            "    {}(request: {}): Promise<{}>;",
            camel(command.method_command),
            name(command.request),
            name(command.response)
        );
    }
    if open.is_some() {
        out.push_str("  };\n");
    }
    out.push_str("}\n\nexport declare function createClient(call: Call): Client;\n");

    // Every needed type and every message that holds one, by the scope it is declared in.
    let mut scopes = Scopes {
        needed: types,
        members: BTreeMap::new(),
    };
    let mut placed = BTreeSet::new();
    for ty in types.values() {
        let mut current = ty.clone();
        loop {
            if !placed.insert(current.full_name().to_owned()) {
                break;
            }
            scopes
                .members
                .entry(scope_of(&current))
                .or_default()
                .push(current.clone());
            match current.parent() {
                Some(parent) if !types.contains_key(parent.full_name()) => {
                    current = Type::Message(parent);
                }
                _ => break,
            }
        }
    }
    for list in scopes.members.values_mut() {
        list.sort_by(|a, b| a.name().cmp(b.name()));
    }
    let packages: BTreeSet<&str> = types.values().map(Type::package).collect();
    for package in packages {
        let _ = writeln!(out, "\nexport namespace {} {{", namespace(package));
        members(&mut out, "  ", package, &scopes);
        out.push_str("}\n");
    }
    out
}

#[cfg(test)]
mod tests;
