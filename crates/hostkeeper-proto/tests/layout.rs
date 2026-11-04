//! The protos' layout and conventions: every file compiles from its text alone and is compiled by
//! the build, sits where its package says, opens with the header every hostkeeper proto carries,
//! gives every enum an UNSPECIFIED zero, and puts `(hostkeeper.secret)` only on text and bytes.
//!
//! Each check is a function that returns what is wrong, run on the real protos (nothing) and on
//! a fixture made to break it (exactly that).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use prost::Message as _;
use prost_reflect::{DescriptorPool, Kind, Value};
use prost_types::{EnumDescriptorProto, EnumValueDescriptorProto, FileDescriptorProto};
use protox::file::{
    ChainFileResolver, File, FileResolver, GoogleFileResolver, IncludeFileResolver,
};

/// The include root of the protos.
fn proto_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("proto")
}

/// Every `.proto` file under `proto/hostkeeper`, named as an import names it.
fn files_on_disk() -> BTreeSet<String> {
    fn walk(dir: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("read the proto directory") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "proto") {
                let name = path.strip_prefix(proto_root()).expect("under proto/");
                out.insert(name.to_str().expect("a UTF-8 name").replace('\\', "/"));
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&proto_root().join("hostkeeper"), &mut out);
    out
}

/// The set `build.rs` compiled, as the crate embeds it.
fn compiled() -> prost_types::FileDescriptorSet {
    prost_types::FileDescriptorSet::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET)
        .expect("the compiled set decodes")
}

/// The files of the set that are hostkeeper's own, not the ones they import.
fn ours(set: &prost_types::FileDescriptorSet) -> Vec<&FileDescriptorProto> {
    set.file
        .iter()
        .filter(|f| f.name().starts_with("hostkeeper/"))
        .collect()
}

/// Catches: a `.proto` file the build does not compile (missing from `build.rs`'s list), one
/// that needs `protoc` or anything but its text to compile, and an embedded set that is not what
/// the text compiles to.
#[test]
fn every_proto_compiles_without_protoc() {
    let on_disk = files_on_disk();
    assert!(
        on_disk.contains("hostkeeper/agent/v1/agent.proto"),
        "{on_disk:?}"
    );
    let built = compiled();
    let built_names: BTreeSet<String> = ours(&built).iter().map(|f| f.name().to_owned()).collect();
    assert_eq!(
        built_names, on_disk,
        "build.rs compiles exactly the files on disk"
    );

    // Compiled again here, from the text, by protox alone, and read back the way the build's set
    // is: the same files, the same descriptors, the same comments.
    let mut compiler = protox::Compiler::new([proto_root()]).expect("the include root");
    compiler.include_imports(true).include_source_info(true);
    compiler
        .open_files(&on_disk)
        .expect("the protos compile from their text");
    let again = prost_types::FileDescriptorSet::decode(&compiler.encode_file_descriptor_set()[..])
        .expect("the set decodes");
    let by_name = |set: &prost_types::FileDescriptorSet| {
        let mut files: Vec<FileDescriptorProto> = ours(set).into_iter().cloned().collect();
        files.sort_by(|a, b| a.name().cmp(b.name()));
        files
    };
    assert_eq!(by_name(&again), by_name(&built));
}

/// What is wrong with where a file sits for its package: the package must be its directory with
/// `/` as `.`, and be `hostkeeper` or `hostkeeper.<area>.v<n>`.
fn package_problem(file: &FileDescriptorProto) -> Option<String> {
    let dir = Path::new(file.name()).parent()?.to_str()?.replace('/', ".");
    if dir != file.package() {
        return Some(format!("{} declares {}", file.name(), file.package()));
    }
    let parts: Vec<&str> = dir.split('.').collect();
    let versioned = match parts.as_slice() {
        ["hostkeeper"] => true,
        ["hostkeeper", area, version] => {
            !area.is_empty()
                && area.bytes().all(|b| b.is_ascii_lowercase())
                && version
                    .strip_prefix('v')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }
        _ => false,
    };
    (!versioned).then(|| format!("{dir} is not hostkeeper.<area>.v<n>"))
}

fn file(name: &str, package: &str) -> FileDescriptorProto {
    FileDescriptorProto {
        name: Some(name.to_owned()),
        package: Some(package.to_owned()),
        ..FileDescriptorProto::default()
    }
}

/// Catches: a file whose package is not its directory, or a package outside
/// `hostkeeper.<area>.v<n>`.
#[test]
fn package_matches_path() {
    let built = compiled();
    let files = ours(&built);
    assert_eq!(files.len(), files_on_disk().len());
    for f in files {
        assert_eq!(package_problem(f), None);
    }

    assert_eq!(
        package_problem(&file("hostkeeper/agent/v1/x.proto", "hostkeeper.agent.v2")),
        Some("hostkeeper/agent/v1/x.proto declares hostkeeper.agent.v2".to_owned())
    );
    assert_eq!(
        package_problem(&file("hostkeeper/agent/x.proto", "hostkeeper.agent")),
        Some("hostkeeper.agent is not hostkeeper.<area>.v<n>".to_owned())
    );
    assert_eq!(
        package_problem(&file("hostkeeper/Agent/v1/x.proto", "hostkeeper.Agent.v1")),
        Some("hostkeeper.Agent.v1 is not hostkeeper.<area>.v<n>".to_owned())
    );
    assert_eq!(
        package_problem(&file(
            "hostkeeper/agent/beta/x.proto",
            "hostkeeper.agent.beta"
        )),
        Some("hostkeeper.agent.beta is not hostkeeper.<area>.v<n>".to_owned())
    );
    assert_eq!(
        package_problem(&file("hostkeeper/mdm/v2/x.proto", "hostkeeper.mdm.v2")),
        None
    );
}

/// The sections every header names, each at the start of a comment line.
const SECTIONS: [&str; 5] = [
    "Served by:",
    "Authentication:",
    "Refusals:",
    "Units:",
    "Extending:",
];

/// What a file's header lacks: its first line must name `package`, and each section must open a
/// line of the comment the file starts with.
fn header_gaps(text: &str, package: &str) -> Vec<String> {
    let header: Vec<&str> = text
        .lines()
        .take_while(|l| l.starts_with("//"))
        .map(|l| l.trim_start_matches('/').trim_start())
        .collect();
    let mut gaps = Vec::new();
    let names_package = header.first().is_some_and(|first| {
        first
            .strip_prefix(package)
            .is_some_and(|rest| rest.starts_with([':', ' ']))
    });
    if !names_package {
        gaps.push(format!("a first line naming {package}"));
    }
    for section in SECTIONS {
        if !header.iter().any(|l| l.starts_with(section)) {
            gaps.push(section.to_owned());
        }
    }
    gaps
}

/// Catches: a file missing its header, or any one section of it.
#[test]
fn every_file_has_the_header_sections() {
    let built = compiled();
    for f in ours(&built) {
        let text = std::fs::read_to_string(proto_root().join(f.name())).expect("read the file");
        assert_eq!(
            header_gaps(&text, f.package()),
            Vec::<String>::new(),
            "{}",
            f.name()
        );
    }

    let whole = "// a.v1: what it is.\n//\n// Served by: x.\n//\n// Authentication: x.\n//\n\
                 // Refusals: x.\n//\n// Units: x.\n//\n// Extending: x.\nsyntax = \"proto3\";\n";
    assert_eq!(header_gaps(whole, "a.v1"), Vec::<String>::new());
    let no_units = whole.replace("// Units: x.\n", "");
    assert_eq!(header_gaps(&no_units, "a.v1"), ["Units:"]);
    // A section named only after the header has ended does not count.
    let late = format!("{no_units}// Units: x.\n");
    assert_eq!(header_gaps(&late, "a.v1"), ["Units:"]);
    assert_eq!(header_gaps(whole, "a.v10"), ["a first line naming a.v10"]);
    let mut all = vec!["a first line naming a.v1".to_owned()];
    all.extend(SECTIONS.map(str::to_owned));
    assert_eq!(header_gaps("syntax = \"proto3\";\n", "a.v1"), all);
}

/// `CommitState` as `COMMIT_STATE`.
fn screaming(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.char_indices() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// What is wrong with an enum's zero value: it must exist and be `<ENUM>_UNSPECIFIED`.
fn zero_problem(e: &EnumDescriptorProto) -> Option<String> {
    let want = format!("{}_UNSPECIFIED", screaming(e.name()));
    match e.value.iter().find(|v| v.number() == 0) {
        Some(zero) if zero.name() == want => None,
        Some(zero) => Some(format!("{}: 0 is {}, not {want}", e.name(), zero.name())),
        None => Some(format!("{}: no value 0", e.name())),
    }
}

fn enums_of(file: &FileDescriptorProto) -> Vec<&EnumDescriptorProto> {
    fn nested<'a>(m: &'a prost_types::DescriptorProto, out: &mut Vec<&'a EnumDescriptorProto>) {
        out.extend(&m.enum_type);
        for inner in &m.nested_type {
            nested(inner, out);
        }
    }
    let mut out: Vec<&EnumDescriptorProto> = file.enum_type.iter().collect();
    for m in &file.message_type {
        nested(m, &mut out);
    }
    out
}

fn value(name: &str, number: i32) -> EnumValueDescriptorProto {
    EnumValueDescriptorProto {
        name: Some(name.to_owned()),
        number: Some(number),
        ..EnumValueDescriptorProto::default()
    }
}

/// Catches: an enum whose zero is a real value, so that a field never set, or a value a reader
/// does not know, reads as that value.
#[test]
fn enum_zero_values_are_unspecified() {
    let built = compiled();
    let mut seen = 0;
    for f in ours(&built) {
        for e in enums_of(f) {
            assert_eq!(zero_problem(e), None, "{}", f.name());
            seen += 1;
        }
    }
    assert!(seen >= 10, "only {seen} enums were checked");

    // Enums nested at any depth are found and checked.
    let nested = fixture_set(
        "n.proto",
        "syntax = \"proto3\";\npackage n.v1;\n\
         message Outer {\n  message Inner { enum Deep { DEEP_NONE = 0; } }\n  \
         enum Shallow { SHALLOW_UNSPECIFIED = 0; }\n}\n",
    );
    let problems: Vec<Option<String>> = enums_of(&nested.file[0])
        .into_iter()
        .map(zero_problem)
        .collect();
    assert_eq!(
        problems,
        [
            None,
            Some("Deep: 0 is DEEP_NONE, not DEEP_UNSPECIFIED".to_owned()),
        ]
    );

    let fixture = |values: Vec<EnumValueDescriptorProto>| EnumDescriptorProto {
        name: Some("CommitState".to_owned()),
        value: values,
        ..EnumDescriptorProto::default()
    };
    assert_eq!(
        zero_problem(&fixture(vec![value("COMMIT_STATE_UNSPECIFIED", 0)])),
        None
    );
    assert_eq!(
        zero_problem(&fixture(vec![value("COMMIT_STATE_NONE", 0)])),
        Some("CommitState: 0 is COMMIT_STATE_NONE, not COMMIT_STATE_UNSPECIFIED".to_owned())
    );
    assert_eq!(
        zero_problem(&fixture(vec![value("STATE_UNSPECIFIED", 0)])),
        Some("CommitState: 0 is STATE_UNSPECIFIED, not COMMIT_STATE_UNSPECIFIED".to_owned())
    );
    assert_eq!(
        zero_problem(&fixture(vec![value("COMMIT_STATE_UNSPECIFIED", 1)])),
        Some("CommitState: no value 0".to_owned())
    );
}

/// What is wrong with `(hostkeeper.secret)` in a pool: it must be declared once, as a `bool`
/// option of fields, and be set only on `string` and `bytes` fields.
fn secret_problems(pool: &DescriptorPool) -> Result<Vec<String>, String> {
    let secret = pool
        .get_extension_by_name("hostkeeper.secret")
        .ok_or("(hostkeeper.secret) is not declared")?;
    if secret.containing_message().full_name() != "google.protobuf.FieldOptions"
        || secret.kind() != Kind::Bool
    {
        return Err("(hostkeeper.secret) is not a bool option of fields".to_owned());
    }
    // Every field: those of messages, then extensions, which carry field options too.
    let fields = pool.all_messages().flat_map(|m| {
        m.fields()
            .map(|f| (f.full_name().to_owned(), f.kind(), f.options()))
            .collect::<Vec<_>>()
    });
    let extensions = pool
        .all_extensions()
        .map(|e| (e.full_name().to_owned(), e.kind(), e.options()));
    let mut problems = Vec::new();
    for (name, kind, options) in fields.chain(extensions) {
        let marked = options.get_extension(&secret).as_ref() == &Value::Bool(true);
        if marked && !matches!(kind, Kind::String | Kind::Bytes) {
            problems.push(format!("{name} is {}", kind_name(&kind)));
        }
    }
    Ok(problems)
}

/// A field's type as a problem names it.
fn kind_name(kind: &Kind) -> String {
    match kind {
        Kind::Message(m) => format!("message {}", m.full_name()),
        Kind::Enum(e) => format!("enum {}", e.full_name()),
        scalar => format!("{scalar:?}"),
    }
}

/// Fixture sources by name, in front of the real protos and the Google ones.
struct Fixture(&'static str, &'static str);

impl FileResolver for Fixture {
    fn open_file(&self, name: &str) -> Result<File, protox::Error> {
        if name == self.0 {
            File::from_source(name, self.1)
        } else {
            Err(protox::Error::file_not_found(name))
        }
    }
}

fn fixture_compiler(name: &'static str, text: &'static str) -> protox::Compiler {
    let mut chain = ChainFileResolver::new();
    chain.add(Fixture(name, text));
    chain.add(IncludeFileResolver::new(proto_root()));
    chain.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(chain);
    compiler.open_file(name).expect("the fixture compiles");
    compiler
}

fn fixture_pool(name: &'static str, text: &'static str) -> DescriptorPool {
    fixture_compiler(name, text).descriptor_pool()
}

/// The fixture file alone, as a descriptor set.
fn fixture_set(name: &'static str, text: &'static str) -> prost_types::FileDescriptorSet {
    fixture_compiler(name, text).file_descriptor_set()
}

/// Catches: the option missing or redeclared as something else, and a secret marked on a field
/// the command line could not read from a file (a number, a message).
#[test]
fn secret_option_is_declared_and_only_on_string_or_bytes_fields() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");
    assert_eq!(secret_problems(&pool), Ok(Vec::new()));
    let secret = pool
        .get_extension_by_name("hostkeeper.secret")
        .expect("declared");
    assert_eq!(secret.number(), 50000);

    let marked = fixture_pool(
        "f.proto",
        r#"syntax = "proto3";
package f.v1;
import "google/protobuf/descriptor.proto";
import "hostkeeper/options.proto";
extend google.protobuf.FieldOptions {
  int32 tag = 50099 [(hostkeeper.secret) = true];
  string note = 50098 [(hostkeeper.secret) = true];
}
message Login {
  string token = 1 [(hostkeeper.secret) = true];
  bytes key = 2 [(hostkeeper.secret) = true];
  uint64 pin = 3 [(hostkeeper.secret) = true];
  Login again = 4 [(hostkeeper.secret) = true];
  string user = 5 [(hostkeeper.secret) = false];
  uint32 tries = 6;
}
"#,
    );
    assert_eq!(
        secret_problems(&marked),
        Ok(vec![
            "f.v1.Login.pin is uint64".to_owned(),
            "f.v1.Login.again is message f.v1.Login".to_owned(),
            "f.v1.tag is int32".to_owned(),
        ])
    );

    let undeclared = fixture_pool(
        "g.proto",
        "syntax = \"proto3\";\npackage g.v1;\nmessage M { string s = 1; }\n",
    );
    assert_eq!(
        secret_problems(&undeclared),
        Err("(hostkeeper.secret) is not declared".to_owned())
    );

    let wrong = fixture_pool(
        "hostkeeper/options.proto",
        r#"syntax = "proto3";
package hostkeeper;
import "google/protobuf/descriptor.proto";
extend google.protobuf.MessageOptions { bool secret = 50000; }
"#,
    );
    assert_eq!(
        secret_problems(&wrong),
        Err("(hostkeeper.secret) is not a bool option of fields".to_owned())
    );

    let not_bool = fixture_pool(
        "hostkeeper/options.proto",
        r#"syntax = "proto3";
package hostkeeper;
import "google/protobuf/descriptor.proto";
extend google.protobuf.FieldOptions { string secret = 50000; }
"#,
    );
    assert_eq!(
        secret_problems(&not_bool),
        Err("(hostkeeper.secret) is not a bool option of fields".to_owned())
    );
}

/// The number a header gives in its sentence "A heartbeat lists at most <n> services.", read
/// across the header's line breaks.
fn header_service_limit(text: &str) -> Option<usize> {
    let header: Vec<&str> = text
        .lines()
        .take_while(|l| l.starts_with("//"))
        .map(|l| l.trim_start_matches('/').trim())
        .collect();
    let joined = header.join(" ");
    let rest = joined.split("A heartbeat lists at most ").nth(1)?;
    let (number, after) = rest.split_once(' ')?;
    after
        .starts_with("services.")
        .then(|| number.parse().ok())?
}

/// Catches: a service-count limit that the crate and the heartbeat's header give differently, as
/// the import and an earlier schema once did (16 against 17).
#[test]
fn the_service_limit_is_the_headers() {
    let text = std::fs::read_to_string(proto_root().join("hostkeeper/agent/v1/agent.proto"))
        .expect("read the file");
    assert_eq!(
        header_service_limit(&text),
        Some(hostkeeper_proto::MAX_SERVICES_PER_HEARTBEAT)
    );
    assert_eq!(hostkeeper_proto::MAX_SERVICES_PER_HEARTBEAT, 16);

    assert_eq!(
        header_service_limit("// A heartbeat lists at most 17\n// services.\nsyntax\n"),
        Some(17)
    );
    assert_eq!(
        header_service_limit("// A heartbeat lists at most many services.\n"),
        None
    );
    assert_eq!(header_service_limit("// No limit is given.\n"), None);
    assert_eq!(
        header_service_limit("syntax\n// A heartbeat lists at most 16 services.\n"),
        None
    );
}

/// The only fields an order may hold at any depth besides its verbs, each with its one type.
const ORDER_FIELDS: [(&str, Kind); 6] = [
    ("order_id", Kind::String),
    ("try", Kind::Uint64),
    ("service", Kind::String),
    ("version", Kind::String),
    ("package_digest", Kind::String),
    ("halt_generation", Kind::Uint64),
];

/// What in an order could carry more than a verb and its typed arguments: every field of the
/// order, and of every message under it, must be one of `ORDER_FIELDS` with its type, singular,
/// or a message field of the order's `verb` oneof.
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
/// beyond a verb's typed arguments, at any depth of the order.
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
            "o.v1.Update.version is repeated string, not an order field",
        ]
    );
}
