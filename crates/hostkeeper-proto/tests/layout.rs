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
    let mut problems = Vec::new();
    for message in pool.all_messages() {
        for field in message.fields() {
            let marked = field.options().get_extension(&secret).as_ref() == &Value::Bool(true);
            if marked && !matches!(field.kind(), Kind::String | Kind::Bytes) {
                problems.push(format!(
                    "{} is {}",
                    field.full_name(),
                    kind_name(&field.kind())
                ));
            }
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

fn fixture_pool(name: &'static str, text: &'static str) -> DescriptorPool {
    let mut chain = ChainFileResolver::new();
    chain.add(Fixture(name, text));
    chain.add(IncludeFileResolver::new(proto_root()));
    chain.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(chain);
    compiler.open_file(name).expect("the fixture compiles");
    compiler.descriptor_pool()
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
import "hostkeeper/options.proto";
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
}
