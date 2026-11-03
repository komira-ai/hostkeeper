//! The compatibility rule: one fixture pair per rule, each refused with exactly the breaks it
//! makes, the additions accepted, and this crate's protos held to the committed baseline.

use prost::Message as _;
use prost_types::FileDescriptorSet;
use protox::file::{File, FileResolver};

use super::{Break, breaks};

/// Fixture sources, by file name.
struct Sources(Vec<(String, String)>);

impl FileResolver for Sources {
    fn open_file(&self, name: &str) -> Result<File, protox::Error> {
        let source = self
            .0
            .iter()
            .find(|(n, _)| n == name)
            .map_or("", |(_, s)| s.as_str());
        File::from_source(name, source)
    }
}

/// The descriptor set the fixture files compile to.
fn compile(files: &[(&str, &str)]) -> FileDescriptorSet {
    let owned = files
        .iter()
        .map(|(n, s)| ((*n).to_owned(), (*s).to_owned()))
        .collect();
    let mut compiler = protox::Compiler::with_file_resolver(Sources(owned));
    compiler
        .open_files(files.iter().map(|(name, _)| *name))
        .expect("the fixture compiles");
    compiler.file_descriptor_set()
}

/// How `new` breaks `old`, two versions of one file.
fn diff(old: &str, new: &str) -> Vec<Break> {
    breaks(&compile(&[("a.proto", old)]), &compile(&[("a.proto", new)]))
}

/// `OLD` with `from` replaced by `to`; fails when `from` is not in it, so a fixture cannot go
/// stale and compare `OLD` with itself.
fn edit(from: &str, to: &str) -> String {
    assert!(OLD.contains(from), "{from:?} is not in the fixture");
    OLD.replace(from, to)
}

/// The released shape every fixture changes one thing of.
const OLD: &str = r#"
syntax = "proto3";
package a.v1;

service S {
  rpc Get(GetRequest) returns (GetResponse);
  rpc Put(PutRequest) returns (PutResponse);
}

message GetRequest { string name = 1; }
message GetResponse {
  string value = 1;
  repeated string tags = 2;
  Kind kind = 3;
}
message PutRequest { string name = 1; }
message PutResponse {}

enum Kind {
  KIND_UNSPECIFIED = 0;
  KIND_A = 1;
  KIND_B = 2;
}

message Note {
  message Inner { int32 n = 1; }
  enum Tone { TONE_UNSPECIFIED = 0; }
  string text = 1;
}

enum Spare { SPARE_UNSPECIFIED = 0; }
"#;

const GET_RESPONSE: &str = ".a.v1.GetResponse";
const SERVICE: &str = ".a.v1.S";

fn s(text: &str) -> String {
    text.to_owned()
}

/// Catches: a field dropped from a released message, whose number an old reader still decodes.
#[test]
fn compat_refuses_removed_field() {
    assert_eq!(
        diff(OLD, &edit("  repeated string tags = 2;\n", "")),
        [Break::FieldRemoved {
            message: s(GET_RESPONSE),
            number: 2,
            name: s("tags"),
        }]
    );
}

/// Catches: a field moved to another number, which an old reader decodes as unknown.
#[test]
fn compat_refuses_renumbered_field() {
    assert_eq!(
        diff(OLD, &edit("tags = 2;", "tags = 4;")),
        [Break::FieldRenumbered {
            message: s(GET_RESPONSE),
            name: s("tags"),
            from: 2,
            to: 4,
        }]
    );
}

/// Catches: a field whose scalar type changed, and one whose enum became a message.
#[test]
fn compat_refuses_changed_field_type() {
    let new = edit("string value = 1;", "bytes value = 1;").replace("Kind kind", "Note kind");
    assert_eq!(
        diff(OLD, &new),
        [
            Break::FieldTypeChanged {
                message: s(GET_RESPONSE),
                name: s("value"),
                from: s("TYPE_STRING"),
                to: s("TYPE_BYTES"),
            },
            Break::FieldTypeChanged {
                message: s(GET_RESPONSE),
                name: s("kind"),
                from: s(".a.v1.Kind"),
                to: s(".a.v1.Note"),
            },
        ]
    );
}

/// Catches: a singular field made explicit-presence, and a repeated field made singular.
#[test]
fn compat_refuses_changed_field_label() {
    let new = edit("string value = 1;", "optional string value = 1;")
        .replace("repeated string tags", "string tags");
    assert_eq!(
        diff(OLD, &new),
        [
            Break::FieldLabelChanged {
                message: s(GET_RESPONSE),
                name: s("value"),
                from: s("LABEL_OPTIONAL"),
                to: s("LABEL_OPTIONAL (explicit presence)"),
            },
            Break::FieldLabelChanged {
                message: s(GET_RESPONSE),
                name: s("tags"),
                from: s("LABEL_REPEATED"),
                to: s("LABEL_OPTIONAL"),
            },
        ]
    );
}

/// Catches: a field renamed at its number, which changes its JSON name.
#[test]
fn compat_refuses_renamed_field() {
    assert_eq!(
        diff(OLD, &edit("string value = 1;", "string label = 1;")),
        [Break::FieldRenamed {
            message: s(GET_RESPONSE),
            number: 1,
            from: s("value"),
            to: s("label"),
        }]
    );
}

/// Catches: a released name given to a new number while its old number holds another field.
#[test]
fn compat_refuses_reused_name_at_other_number() {
    let new = edit(
        "string value = 1;",
        "string label = 1;\n  string value = 5;",
    );
    assert_eq!(
        diff(OLD, &new),
        [
            Break::FieldRenamed {
                message: s(GET_RESPONSE),
                number: 1,
                from: s("value"),
                to: s("label"),
            },
            Break::NameReused {
                message: s(GET_RESPONSE),
                name: s("value"),
                from: 1,
                to: 5,
            },
        ]
    );
}

/// Catches: a new required field, in a released message and in a new one; a required field that
/// was released already is no new break, and neither is a new optional one.
#[test]
fn compat_refuses_new_required_field() {
    let old = "syntax = \"proto2\";\npackage c.v1;\n\
               message M { required string id = 1; optional string note = 2; }\n";
    let new = "syntax = \"proto2\";\npackage c.v1;\n\
               message M { required string id = 1; optional string note = 2;\n\
                           required string more = 3; optional string extra = 4; }\n\
               message N { required int32 n = 1; }\n";
    assert_eq!(
        diff(old, new),
        [
            Break::RequiredAdded {
                message: s(".c.v1.M"),
                name: s("more"),
            },
            Break::RequiredAdded {
                message: s(".c.v1.N"),
                name: s("n"),
            },
        ]
    );
}

/// Catches: a released message dropped, with the message and enum nested in it.
#[test]
fn compat_refuses_removed_message() {
    let note = "message Note {\n  message Inner { int32 n = 1; }\n  \
                enum Tone { TONE_UNSPECIFIED = 0; }\n  string text = 1;\n}\n";
    assert_eq!(
        diff(OLD, &edit(note, "")),
        [
            Break::MessageRemoved {
                message: s(".a.v1.Note"),
            },
            Break::MessageRemoved {
                message: s(".a.v1.Note.Inner"),
            },
            Break::EnumRemoved {
                name: s(".a.v1.Note.Tone"),
            },
        ]
    );
}

/// Catches: a released enum dropped.
#[test]
fn compat_refuses_removed_enum() {
    assert_eq!(
        diff(OLD, &edit("enum Spare { SPARE_UNSPECIFIED = 0; }", "")),
        [Break::EnumRemoved {
            name: s(".a.v1.Spare"),
        }]
    );
}

/// Catches: an enum value dropped, which an old writer still sends.
#[test]
fn compat_refuses_removed_enum_value() {
    assert_eq!(
        diff(OLD, &edit("  KIND_B = 2;\n", "")),
        [Break::EnumValueRemoved {
            name: s(".a.v1.Kind"),
            number: 2,
            value: s("KIND_B"),
        }]
    );
}

/// Catches: an enum value renamed at its number, which changes its JSON spelling.
#[test]
fn compat_refuses_renamed_enum_value() {
    assert_eq!(
        diff(OLD, &edit("KIND_B = 2;", "KIND_C = 2;")),
        [Break::EnumValueRemoved {
            name: s(".a.v1.Kind"),
            number: 2,
            value: s("KIND_B"),
        }]
    );
}

/// Catches: an enum value moved to another number.
#[test]
fn compat_refuses_renumbered_enum_value() {
    assert_eq!(
        diff(OLD, &edit("KIND_B = 2;", "KIND_B = 3;")),
        [Break::EnumValueRenumbered {
            name: s(".a.v1.Kind"),
            value: s("KIND_B"),
            from: 2,
            to: 3,
        }]
    );
}

/// Catches: a released RPC dropped, and one renamed, which is the same to a caller.
#[test]
fn compat_refuses_removed_rpc() {
    let put = "  rpc Put(PutRequest) returns (PutResponse);\n";
    let removed = || Break::RpcRemoved {
        service: s(SERVICE),
        rpc: s("Put"),
    };
    assert_eq!(diff(OLD, &edit(put, "")), [removed()]);
    assert_eq!(diff(OLD, &edit("rpc Put(", "rpc Store(")), [removed()]);
}

/// Catches: an RPC that takes another request message.
#[test]
fn compat_refuses_changed_request_type() {
    assert_eq!(
        diff(OLD, &edit("rpc Get(GetRequest)", "rpc Get(PutRequest)")),
        [Break::RequestTypeChanged {
            service: s(SERVICE),
            rpc: s("Get"),
            from: s(".a.v1.GetRequest"),
            to: s(".a.v1.PutRequest"),
        }]
    );
}

/// Catches: an RPC that answers with another response message.
#[test]
fn compat_refuses_changed_response_type() {
    assert_eq!(
        diff(OLD, &edit("returns (GetResponse)", "returns (PutResponse)")),
        [Break::ResponseTypeChanged {
            service: s(SERVICE),
            rpc: s("Get"),
            from: s(".a.v1.GetResponse"),
            to: s(".a.v1.PutResponse"),
        }]
    );
}

/// Catches: an RPC whose response became a stream, and one whose request did.
#[test]
fn compat_refuses_changed_streaming_mode() {
    let new = edit("returns (GetResponse)", "returns (stream GetResponse)")
        .replace("rpc Put(PutRequest)", "rpc Put(stream PutRequest)");
    assert_eq!(
        diff(OLD, &new),
        [
            Break::StreamingChanged {
                service: s(SERVICE),
                rpc: s("Get"),
            },
            Break::StreamingChanged {
                service: s(SERVICE),
                rpc: s("Put"),
            },
        ]
    );
}

/// Catches: a released service dropped, and one renamed, which is the same to a caller.
#[test]
fn compat_refuses_removed_service() {
    let service = "service S {\n  rpc Get(GetRequest) returns (GetResponse);\n  \
                   rpc Put(PutRequest) returns (PutResponse);\n}\n";
    let removed = || Break::ServiceRemoved {
        service: s(SERVICE),
    };
    assert_eq!(diff(OLD, &edit(service, "")), [removed()]);
    assert_eq!(diff(OLD, &edit("service S {", "service T {")), [removed()]);
}

/// Catches: a file moved to another package, which renames everything in it.
#[test]
fn compat_refuses_changed_package() {
    let messages = [
        "GetRequest",
        "GetResponse",
        "Note",
        "Note.Inner",
        "PutRequest",
        "PutResponse",
    ]
    .map(|m| Break::MessageRemoved {
        message: format!(".a.v1.{m}"),
    });
    let enums = ["Kind", "Note.Tone", "Spare"].map(|e| Break::EnumRemoved {
        name: format!(".a.v1.{e}"),
    });
    let mut want = vec![
        Break::PackageRemoved { package: s("a.v1") },
        Break::PackageChanged {
            file: s("a.proto"),
            from: s("a.v1"),
            to: s("b.v1"),
        },
    ];
    want.extend(messages);
    want.extend(enums);
    want.push(Break::ServiceRemoved {
        service: s(SERVICE),
    });
    assert_eq!(diff(OLD, &edit("package a.v1;", "package b.v1;")), want);
}

/// Catches: a check that refuses what the rule allows: a field, an RPC, a service, an enum
/// value, a message, an enum and a package added at once.
#[test]
fn compat_accepts_added_field_rpc_service_and_enum_value() {
    let new = edit("string name = 1; }", "string name = 1; string extra = 9; }")
        .replace(
            "  rpc Put(",
            "  rpc List(GetRequest) returns (stream GetResponse);\n  rpc Put(",
        )
        .replace("KIND_B = 2;", "KIND_B = 2;\n  KIND_C = 3;")
        + "service U { rpc Ping(PutRequest) returns (PutResponse); }\n\
           message Added { optional string maybe = 1; }\n\
           enum Fresh { FRESH_UNSPECIFIED = 0; }\n";
    let old = compile(&[("a.proto", OLD)]);
    let other = "syntax = \"proto3\";\npackage z.v1;\nmessage Z { string z = 1; }\n";
    let more = compile(&[("a.proto", new.as_str()), ("z.proto", other)]);
    assert_eq!(breaks(&old, &more), Vec::<Break>::new());
}

/// Catches: a check that ties a name to its file: a message that moves to another file of its
/// package keeps its full name, so no caller sees a change.
#[test]
fn compat_accepts_a_message_moved_to_another_file_of_its_package() {
    let text = "syntax = \"proto3\";\npackage m.v1;\nmessage M { string s = 1; }\n";
    let old = compile(&[("first.proto", text)]);
    let new = compile(&[("second.proto", text)]);
    assert_eq!(breaks(&old, &new), Vec::<Break>::new());
}

/// The descriptor set this crate compiles, as `build.rs` wrote it.
fn current() -> FileDescriptorSet {
    FileDescriptorSet::decode(crate::FILE_DESCRIPTOR_SET).expect("the compiled set decodes")
}

/// The set the last release committed.
fn baseline() -> FileDescriptorSet {
    let bytes = include_bytes!("../../proto/compat/baseline.binpb");
    FileDescriptorSet::decode(&bytes[..]).expect("the baseline decodes")
}

/// Catches: a change to `proto/` that removes, renumbers, renames or retypes anything the last
/// release shipped. Also catches an empty or foreign baseline, which would let every change
/// through.
#[test]
fn the_protos_keep_every_rule_against_the_baseline() {
    let baseline = baseline();
    for package in [
        "hostkeeper",
        "hostkeeper.agent.v1",
        "hostkeeper.operator.v1",
        "hostkeeper.local.v1",
    ] {
        assert!(
            baseline.file.iter().any(|f| f.package() == package),
            "the baseline declares no {package}"
        );
    }
    assert_eq!(breaks(&baseline, &current()), Vec::<Break>::new());
}

/// Catches: a baseline check that passes whatever the current set holds: the real set with one
/// field taken out is refused, naming that field.
#[test]
fn the_baseline_check_refuses_the_real_set_with_a_field_taken_out() {
    let mut set = current();
    let agent = set
        .file
        .iter_mut()
        .find(|f| f.package() == "hostkeeper.agent.v1")
        .expect("the agent file");
    let beat = agent
        .message_type
        .iter_mut()
        .find(|m| m.name() == "HeartbeatRequest")
        .expect("the heartbeat");
    beat.field.retain(|f| f.name() != "platform");
    assert_eq!(
        breaks(&baseline(), &set),
        [Break::FieldRemoved {
            message: s(".hostkeeper.agent.v1.HeartbeatRequest"),
            number: 1,
            name: s("platform"),
        }]
    );
}
