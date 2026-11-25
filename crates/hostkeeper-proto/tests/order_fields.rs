//! The rule that no field of an order, at any depth, can carry a command, a path, a URL or a
//! script (`hostkeeper/agent/v1/agent.proto`, `Order`): only a verb's typed arguments, the picked
//! entry of a release, and the typed arguments of `maintain`. Run on the real protos (nothing
//! wrong) and on fixtures made to break it (exactly that).

use std::collections::BTreeSet;

use prost_reflect::{DescriptorPool, Kind};

/// A fixture file alone, resolved by its text.
struct One(&'static str);

impl protox::file::FileResolver for One {
    fn open_file(&self, name: &str) -> Result<protox::file::File, protox::Error> {
        protox::file::File::from_source(name, self.0)
    }
}

/// The descriptors of the fixture file `name` holding `text`.
fn fixture_pool(name: &'static str, text: &'static str) -> DescriptorPool {
    let mut compiler = protox::Compiler::with_file_resolver(One(text));
    compiler.open_file(name).expect("the fixture compiles");
    compiler.descriptor_pool()
}

/// A field's type, as a problem names it.
fn kind_name(kind: &Kind) -> String {
    match kind {
        Kind::Message(m) => format!("message {}", m.full_name()),
        Kind::Enum(e) => format!("enum {}", e.full_name()),
        scalar => format!("{scalar:?}"),
    }
}

/// The only fields an order may hold at any depth besides its verbs, each with its one type.
const ORDER_FIELDS: [(&str, Kind); 7] = [
    ("order_id", Kind::String),
    ("try", Kind::Uint64),
    ("service", Kind::String),
    ("version", Kind::String),
    ("package_digest", Kind::String),
    ("index_digest", Kind::String),
    ("halt_generation", Kind::Uint64),
];

/// The one message a verb may carry: the entry of a release picked for the host
/// (`docs/design/packages.md`, section 3.3), by its field name and its type. Its fields are the
/// release's, which the crate `hostkeeper-release` and the daemon check before anything runs, so
/// this check does not go into it.
const ORDER_ENTRY: (&str, &str) = ("target", "hostkeeper.release.v1.Target");

/// What a field of [`MAINTAIN_ARGUMENTS`] must be: a scalar of its type, or the enum or message
/// of its full name.
enum Typed {
    Scalar(Kind),
    Named(&'static str),
}

impl Typed {
    fn fits(&self, kind: &Kind) -> bool {
        match (self, kind) {
            (Typed::Scalar(want), kind) => kind == want,
            (Typed::Named(name), Kind::Enum(e)) => e.full_name() == *name,
            (Typed::Named(name), Kind::Message(m)) => m.full_name() == *name,
            (Typed::Named(_), _) => false,
        }
    }
}

/// The typed arguments of the `maintain` verb and of the Xcode entries it names, beyond
/// `ORDER_FIELDS`: each by its message, its name, its one type and whether it repeats. None can
/// carry a command, a path, a URL or a script: an id, enums, durations, versions, a size, a
/// digest and a switch (an Xcode is named by its version, build, size and SHA-256, and fetched from the
/// coordinator's store).
const MAINTAIN_ARGUMENTS: [(&str, &str, Typed, bool); 15] = [
    (MAINTAIN, "run_id", Typed::Scalar(Kind::String), false),
    (
        MAINTAIN,
        "action",
        Typed::Named("hostkeeper.agent.v1.MaintainAction"),
        false,
    ),
    (
        MAINTAIN,
        "kinds",
        Typed::Named("hostkeeper.agent.v1.MaintenanceKind"),
        true,
    ),
    (
        MAINTAIN,
        "linux_scope",
        Typed::Named("hostkeeper.agent.v1.LinuxScope"),
        false,
    ),
    (
        MAINTAIN,
        "drain_deadline_secs",
        Typed::Scalar(Kind::Uint64),
        false,
    ),
    (
        MAINTAIN,
        "macos_path",
        Typed::Named("hostkeeper.agent.v1.MacosPath"),
        false,
    ),
    (MAINTAIN, "xcodes", Typed::Named(XCODE_ENTRY), true),
    (MAINTAIN, "xcode_remove", Typed::Scalar(Kind::String), true),
    (
        MAINTAIN,
        "xcode_default",
        Typed::Scalar(Kind::String),
        false,
    ),
    (MAINTAIN, "return_secs", Typed::Scalar(Kind::Uint64), false),
    (MAINTAIN, "macos_major", Typed::Scalar(Kind::Bool), false),
    (XCODE_ENTRY, "version", Typed::Scalar(Kind::String), false),
    (XCODE_ENTRY, "build", Typed::Scalar(Kind::String), false),
    (XCODE_ENTRY, "size", Typed::Scalar(Kind::Uint64), false),
    (XCODE_ENTRY, "sha256", Typed::Scalar(Kind::String), false),
];

/// The `maintain` verb's message.
const MAINTAIN: &str = "hostkeeper.agent.v1.Maintain";
/// The message of an Xcode a `maintain` names.
const XCODE_ENTRY: &str = "hostkeeper.agent.v1.XcodeEntry";

/// Whether `field` of `message` is one of [`MAINTAIN_ARGUMENTS`], of its type and its label.
fn maintain_argument(message: &str, field: &prost_reflect::FieldDescriptor) -> bool {
    MAINTAIN_ARGUMENTS
        .iter()
        .any(|(owner, name, typed, repeated)| {
            *owner == message
                && *name == field.name()
                && typed.fits(&field.kind())
                && *repeated == field.is_list()
        })
}

/// What in an order could carry more than a verb and its typed arguments: every field of the
/// order, and of every message under it, must be one of `ORDER_FIELDS` with its type, singular,
/// a message field of the order's `verb` oneof, in a verb the singular [`ORDER_ENTRY`], or one of
/// [`MAINTAIN_ARGUMENTS`] (whose Xcode entries are checked in turn).
fn order_field_problems(order: &prost_reflect::MessageDescriptor) -> Vec<String> {
    let mut problems = Vec::new();
    let mut todo = vec![order.clone()];
    let mut seen = BTreeSet::new();
    while let Some(message) = todo.pop() {
        if !seen.insert(message.full_name().to_owned()) {
            continue;
        }
        for field in message.fields() {
            let verb = field.containing_oneof().is_some_and(|o| o.name() == "verb")
                && message.full_name() == order.full_name();
            if let (true, Kind::Message(inner)) = (verb, field.kind()) {
                todo.push(inner);
                continue;
            }
            let in_verb = message.full_name() != order.full_name();
            if in_verb && maintain_argument(message.full_name(), &field) {
                if let Kind::Message(inner) = field.kind() {
                    todo.push(inner);
                }
                continue;
            }
            let entry = matches!(field.kind(), Kind::Message(inner)
                if (field.name(), inner.full_name()) == ORDER_ENTRY);
            if in_verb && entry && !field.is_list() {
                continue;
            }
            let allowed = ORDER_FIELDS
                .iter()
                .any(|(name, kind)| field.name() == *name && field.kind() == *kind);
            if !allowed || field.is_list() {
                problems.push(format!(
                    "{} is {}{}, not an order field",
                    field.full_name(),
                    if field.is_list() { "repeated " } else { "" },
                    kind_name(&field.kind())
                ));
            }
        }
    }
    problems.sort();
    problems
}

/// Catches: an order field that could name a command, a path, a URL or a script, or any field
/// beyond a verb's typed arguments and the picked entry of a release, at any depth of the order;
/// an entry repeated, outside a verb, or of another message.
#[test]
fn no_order_field_can_carry_a_command_path_or_url() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    let order = pool
        .get_message_by_name("hostkeeper.agent.v1.Order")
        .expect("the order");
    assert_eq!(order_field_problems(&order), Vec::<String>::new());
    assert!(
        order
            .fields()
            .filter(|f| f.containing_oneof().is_some())
            .count()
            >= 7,
        "the verbs are checked"
    );

    let fixture = fixture_pool(
        "o.proto",
        r#"syntax = "proto3";
package o.v1;
message Order {
  string order_id = 1;
  string try = 2;
  oneof verb { Update update = 3; }
  Extra extra = 4;
}
message Update {
  string service = 1;
  string command = 2;
  repeated string version = 3;
  Extra target = 4;
}
message Extra { string url = 1; }
"#,
    );
    let order = fixture
        .get_message_by_name("o.v1.Order")
        .expect("the order");
    assert_eq!(
        order_field_problems(&order),
        [
            "o.v1.Order.extra is message o.v1.Extra, not an order field",
            "o.v1.Order.try is string, not an order field",
            "o.v1.Update.command is string, not an order field",
            "o.v1.Update.target is message o.v1.Extra, not an order field",
            "o.v1.Update.version is repeated string, not an order field",
        ]
    );

    // The entry is let through in a verb, singular, and nowhere else; what is inside it is not
    // gone into.
    let entries = fixture_pool(
        "e.proto",
        r#"syntax = "proto3";
package hostkeeper.release.v1;
message Order {
  string order_id = 1;
  oneof verb { Update update = 3; Force force = 4; }
  Target target = 5;
}
message Update { string service = 1; Target target = 2; }
message Force { repeated Target target = 1; Other other = 2; }
message Other { Target target = 1; }
message Target { string command = 1; }
"#,
    );
    let order = entries
        .get_message_by_name("hostkeeper.release.v1.Order")
        .expect("the order");
    assert_eq!(
        order_field_problems(&order),
        [
            "hostkeeper.release.v1.Force.other is message hostkeeper.release.v1.Other, not an order \
             field",
            "hostkeeper.release.v1.Force.target is repeated message hostkeeper.release.v1.Target, \
             not an order field",
            "hostkeeper.release.v1.Order.target is message hostkeeper.release.v1.Target, not an \
             order field",
        ]
    );

    // The arguments of `maintain` are let through by name, type and label, in that verb and its
    // Xcode entries only: a field beside them, one of another type or label, and the same
    // message in another package are not.
    let maintain = fixture_pool(
        "m.proto",
        r#"syntax = "proto3";
package hostkeeper.agent.v1;
message Order {
  string order_id = 1;
  oneof verb { Maintain maintain = 11; }
}
message Maintain {
  string run_id = 1;
  uint32 action = 2;
  repeated XcodeEntry xcodes = 7;
  string xcode_remove = 8;
  uint64 return_secs = 10;
  string path = 12;
}
message XcodeEntry { string version = 1; uint64 size = 3; string url = 5; }
"#,
    );
    let order = maintain
        .get_message_by_name("hostkeeper.agent.v1.Order")
        .expect("the order");
    assert_eq!(
        order_field_problems(&order),
        [
            "hostkeeper.agent.v1.Maintain.action is uint32, not an order field",
            "hostkeeper.agent.v1.Maintain.path is string, not an order field",
            "hostkeeper.agent.v1.Maintain.xcode_remove is string, not an order field",
            "hostkeeper.agent.v1.XcodeEntry.url is string, not an order field",
        ]
    );
    let elsewhere = fixture_pool(
        "n.proto",
        r#"syntax = "proto3";
package n.v1;
message Order {
  string order_id = 1;
  oneof verb { Maintain maintain = 11; }
}
message Maintain { string run_id = 1; uint64 return_secs = 10; }
"#,
    );
    let order = elsewhere
        .get_message_by_name("n.v1.Order")
        .expect("the order");
    assert_eq!(
        order_field_problems(&order),
        [
            "n.v1.Maintain.return_secs is uint64, not an order field",
            "n.v1.Maintain.run_id is string, not an order field",
        ]
    );
}
