//! The generator of the command table, run on fixture protos: the words of a name, how each
//! kind of field becomes input, the comments, the order, and every refusal; and on the real
//! descriptors, where it must succeed.

use std::path::Path;

use prost_reflect::DescriptorPool;
use protox::file::{
    ChainFileResolver, File, FileResolver, GoogleFileResolver, IncludeFileResolver,
};

use super::{GenError, RESERVED_FLAGS, generate, kebab, value_prefix, words};

/// Fixture sources by name, in front of the real protos (for `hostkeeper/options.proto`) and the
/// Google ones.
struct Sources(Vec<(&'static str, String)>);

impl FileResolver for Sources {
    fn open_file(&self, name: &str) -> Result<File, protox::Error> {
        match self.0.iter().find(|(n, _)| *n == name) {
            Some((_, text)) => File::from_source(name, text),
            None => Err(protox::Error::file_not_found(name)),
        }
    }
}

/// The pool of one fixture file, compiled with its comments, as the build compiles the protos.
fn pool(text: &str) -> DescriptorPool {
    let mut chain = ChainFileResolver::new();
    chain.add(Sources(vec![("fixture.proto", text.to_owned())]));
    chain.add(IncludeFileResolver::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("proto"),
    ));
    chain.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(chain);
    compiler.include_imports(true).include_source_info(true);
    compiler
        .open_file("fixture.proto")
        .expect("the fixture compiles");
    DescriptorPool::decode(&compiler.encode_file_descriptor_set()[..]).expect("the set decodes")
}

/// The fixture header: proto3, hostkeeper's options, a hostkeeper package.
fn fixture(body: &str) -> String {
    format!(
        "syntax = \"proto3\";\npackage hostkeeper.fixture.v1;\n\
         import \"hostkeeper/options.proto\";\n{body}"
    )
}

/// The generated text of one fixture, which must succeed.
fn table(body: &str) -> String {
    generate(&pool(&fixture(body))).expect("the fixture generates")
}

/// The one line of the table that holds the field entry of `name`.
fn field_line<'a>(table: &'a str, name: &str) -> &'a str {
    let needle = format!("Field {{ name: {name:?},");
    table
        .lines()
        .find(|l| l.contains(&needle))
        .unwrap_or_else(|| panic!("no field {name} in\n{table}"))
}

/// Catches a name split at the wrong place: an acronym cut into letters, a digit taken as a
/// boundary, an underscore kept, or a leading or doubled underscore making an empty word.
#[test]
fn names_split_into_words_as_commands_and_flags_spell_them() {
    let cases: &[(&str, &[&str])] = &[
        ("SetDesired", &["set", "desired"]),
        ("List", &["list"]),
        ("GetTLSConfig", &["get", "tls", "config"]),
        ("ABC", &["abc"]),
        ("Ab2Cd", &["ab2", "cd"]),
        ("node_id", &["node", "id"]),
        ("_leading__double_", &["leading", "double"]),
        ("", &[]),
    ];
    for (name, expected) in cases {
        assert_eq!(words(name), *expected, "{name}");
    }
    assert_eq!(kebab("SetMinVersion"), "set-min-version");
    assert_eq!(kebab("if_generation"), "if-generation");
    assert_eq!(value_prefix("ConvergenceState"), "CONVERGENCE_STATE_");
}

/// Catches a kind or label written wrongly into the table, or a field made a flag that must
/// come through the request file (a message, a oneof member, bytes, a map that is not
/// string to string) and the reverse.
#[test]
fn every_kind_and_label_is_written_with_its_input() {
    let t = table(
        "enum Colour { COLOUR_UNSPECIFIED = 0; COLOUR_RED = 1; COLOUR_DARK_BLUE = 2; }\n\
         message Inner { string x = 1; }\n\
         message Req {\n\
           double d = 1; float f = 2; int32 i32 = 3; int64 i64 = 4; uint32 u32 = 5;\n\
           uint64 u64 = 6; sint32 s32 = 7; sint64 s64 = 8; fixed32 f32 = 9; fixed64 f64 = 10;\n\
           sfixed32 sf32 = 11; sfixed64 sf64 = 12; bool b = 13; string s = 14; bytes raw = 15;\n\
           Colour colour = 16; Inner inner = 17; optional bool maybe = 18;\n\
           repeated string names = 19; repeated Colour colours = 20; map<string, string> labels = 21;\n\
           map<string, uint64> counts = 22; oneof pick { string one = 23; Inner two = 24; }\n\
           repeated Inner inners = 25;\n\
         }\n\
         service Things { rpc Exec(Req) returns (Inner); }\n",
    );
    let expect = [
        ("d", "FieldKind::Double", "Label::Singular", "Input::Flag"),
        ("f", "FieldKind::Float", "Label::Singular", "Input::Flag"),
        ("i32", "FieldKind::Int32", "Label::Singular", "Input::Flag"),
        ("i64", "FieldKind::Int64", "Label::Singular", "Input::Flag"),
        ("u32", "FieldKind::Uint32", "Label::Singular", "Input::Flag"),
        ("u64", "FieldKind::Uint64", "Label::Singular", "Input::Flag"),
        ("s32", "FieldKind::Sint32", "Label::Singular", "Input::Flag"),
        ("s64", "FieldKind::Sint64", "Label::Singular", "Input::Flag"),
        (
            "f32",
            "FieldKind::Fixed32",
            "Label::Singular",
            "Input::Flag",
        ),
        (
            "f64",
            "FieldKind::Fixed64",
            "Label::Singular",
            "Input::Flag",
        ),
        (
            "sf32",
            "FieldKind::Sfixed32",
            "Label::Singular",
            "Input::Flag",
        ),
        (
            "sf64",
            "FieldKind::Sfixed64",
            "Label::Singular",
            "Input::Flag",
        ),
        ("b", "FieldKind::Bool", "Label::Singular", "Input::Flag"),
        ("s", "FieldKind::String", "Label::Singular", "Input::Flag"),
        (
            "raw",
            "FieldKind::Bytes",
            "Label::Singular",
            "Input::RequestFile",
        ),
        (
            "colour",
            "FieldKind::Enum",
            "Label::Singular",
            "Input::Flag",
        ),
        (
            "inner",
            "FieldKind::Message",
            "Label::Singular",
            "Input::RequestFile",
        ),
        ("maybe", "FieldKind::Bool", "Label::Optional", "Input::Flag"),
        (
            "names",
            "FieldKind::String",
            "Label::Repeated",
            "Input::Flag",
        ),
        (
            "colours",
            "FieldKind::Enum",
            "Label::Repeated",
            "Input::Flag",
        ),
        ("labels", "FieldKind::Message", "Label::Map", "Input::Flag"),
        (
            "counts",
            "FieldKind::Message",
            "Label::Map",
            "Input::RequestFile",
        ),
        (
            "one",
            "FieldKind::String",
            "Label::Oneof",
            "Input::RequestFile",
        ),
        (
            "two",
            "FieldKind::Message",
            "Label::Oneof",
            "Input::RequestFile",
        ),
        (
            "inners",
            "FieldKind::Message",
            "Label::Repeated",
            "Input::RequestFile",
        ),
    ];
    for (name, kind, label, input) in expect {
        let line = field_line(&t, name);
        for part in [kind, label, input, "secret: false"] {
            assert!(line.contains(part), "{name}: {part} not in {line}");
        }
    }
    // An enum flag lists its values without the prefix, in lower case, and never UNSPECIFIED;
    // an enum that is not a flag (none here) or any other field lists none.
    let colour = field_line(&t, "colour");
    assert!(
        colour.contains(
            "enum_values: &[EnumValue { flag: \"red\", name: \"COLOUR_RED\", number: 1 }, \
             EnumValue { flag: \"dark_blue\", name: \"COLOUR_DARK_BLUE\", number: 2 }, ]"
        ),
        "{colour}"
    );
    assert!(!colour.contains("UNSPECIFIED"), "{colour}");
    assert!(field_line(&t, "s").contains("enum_values: &[]"));
    assert!(colour.contains("type_name: \"hostkeeper.fixture.v1.Colour\""));
    assert!(field_line(&t, "inner").contains("type_name: \"hostkeeper.fixture.v1.Inner\""));
    assert!(field_line(&t, "s").contains("type_name: \"\""));
    // Fields are written as the message declares them.
    let at = |name: &str| t.find(&format!("Field {{ name: {name:?},")).expect(name);
    assert!(at("d") < at("f") && at("labels") < at("counts") && at("two") < at("inners"));
}

/// Catches a secret field made a flag, a secret given flags that are not `-file` and `-stdin`,
/// and a secret that is repeated or in a oneof given those flags instead of the request file.
#[test]
fn a_secret_is_read_from_a_file_or_stdin_and_never_a_flag() {
    let t = table(
        "message Req {\n\
           string password = 1 [(hostkeeper.secret) = true];\n\
           bytes key = 2 [(hostkeeper.secret) = true];\n\
           optional string pin = 3 [(hostkeeper.secret) = true];\n\
           repeated string many = 4 [(hostkeeper.secret) = true];\n\
           oneof o { string either = 5 [(hostkeeper.secret) = true]; }\n\
           string plain = 6 [(hostkeeper.secret) = false];\n\
         }\n\
         message Resp {}\n\
         service S { rpc Exec(Req) returns (Resp); }\n",
    );
    for (name, input) in [
        ("password", "Input::SecretFile"),
        ("key", "Input::SecretFile"),
        ("pin", "Input::SecretFile"),
        ("many", "Input::RequestFile"),
        ("either", "Input::RequestFile"),
    ] {
        let line = field_line(&t, name);
        assert!(
            line.contains(input) && line.contains("secret: true"),
            "{line}"
        );
    }
    let plain = field_line(&t, "plain");
    assert!(
        plain.contains("Input::Flag") && plain.contains("secret: false"),
        "{plain}"
    );
}

/// Catches the generator accepting `(hostkeeper.secret)` on a field that is not text, which the
/// command line could neither read from a file as given nor keep out of its output.
#[test]
fn secret_option_on_a_non_string_or_bytes_field_fails_the_build() {
    for (decl, field) in [
        ("uint64 pin = 1 [(hostkeeper.secret) = true];", "pin"),
        ("Inner again = 1 [(hostkeeper.secret) = true];", "again"),
        (
            "map<string, string> m = 1 [(hostkeeper.secret) = true];",
            "m",
        ),
    ] {
        let text = fixture(&format!(
            "message Inner {{ string x = 1; }}\nmessage Req {{ {decl} }}\n\
             service S {{ rpc Exec(Req) returns (Inner); }}\n"
        ));
        assert_eq!(
            generate(&pool(&text)),
            Err(GenError::SecretNotText {
                field: format!("hostkeeper.fixture.v1.Req.{field}"),
            }),
            "{decl}"
        );
    }
    // In a response, nested, the same.
    let nested = fixture(
        "message Deep { int32 code = 1 [(hostkeeper.secret) = true]; }\n\
         message Resp { Deep deep = 1; }\nmessage Req {}\n\
         service S { rpc Exec(Req) returns (Resp); }\n",
    );
    let refused = generate(&pool(&nested)).expect_err("a secret int");
    assert_eq!(
        refused.to_string(),
        "hostkeeper.fixture.v1.Deep.code: (hostkeeper.secret) is allowed only on a string or \
         bytes field"
    );
}

/// Catches a streaming RPC written into the table, which a command line could not call.
#[test]
fn a_streaming_rpc_fails_the_build() {
    for rpc in [
        "rpc Up(stream M) returns (M);",
        "rpc Down(M) returns (stream M);",
    ] {
        let text = fixture(&format!("message M {{}}\nservice S {{ {rpc} }}\n"));
        let refused = generate(&pool(&text)).expect_err(rpc);
        assert!(matches!(refused, GenError::Streaming { .. }), "{refused:?}");
    }
    let refused = generate(&pool(&fixture(
        "message M {}\nservice S { rpc Both(stream M) returns (stream M); }\n",
    )))
    .expect_err("both");
    assert_eq!(
        refused.to_string(),
        "hostkeeper.fixture.v1.S.Both: the command table holds unary calls only; this RPC streams"
    );
}

/// Catches a well-known type reached by a request or a response, whose canonical JSON the
/// output would write wrongly; a message reached twice, or through itself, is checked once.
#[test]
fn a_well_known_type_fails_the_build_and_recursion_is_checked_once() {
    let text = "syntax = \"proto3\";\npackage hostkeeper.fixture.v1;\n\
                import \"google/protobuf/timestamp.proto\";\n\
                message Req {}\nmessage Resp { google.protobuf.Timestamp at = 1; }\n\
                service S { rpc Exec(Req) returns (Resp); }\n";
    let refused = generate(&pool(text)).expect_err("a timestamp");
    assert_eq!(
        refused,
        GenError::WellKnownType {
            method: "hostkeeper.fixture.v1.S.Exec".to_owned(),
            message: "google.protobuf.Timestamp".to_owned(),
        }
    );
    assert_eq!(
        refused.to_string(),
        "hostkeeper.fixture.v1.S.Exec: reaches google.protobuf.Timestamp, a well-known type the command table \
         does not write"
    );
    let t = table(
        "message Tree { repeated Tree children = 1; Tree first = 2; }\n\
         service S { rpc Exec(Tree) returns (Tree); }\n",
    );
    assert!(t.contains("request: \"hostkeeper.fixture.v1.Tree\""), "{t}");
}

/// Catches two RPCs with one command, and a flag that two fields, or a field and a generated command
/// line itself, would both take.
#[test]
fn command_and_flag_clashes_fail_the_build() {
    let clash =
        fixture("message M {}\nservice S { rpc SetX(M) returns (M); rpc Set_X(M) returns (M); }\n");
    let refused = generate(&pool(&clash)).expect_err("a clash");
    assert_eq!(
        refused,
        GenError::CommandClash {
            command: "s set-x".to_owned()
        }
    );
    assert_eq!(refused.to_string(), "two RPCs make the command `s set-x`");

    let cases = [
        ("string endpoint = 1;", "endpoint"),
        ("string request = 1;", "request"),
        (
            "string x_file = 1; string x = 2 [(hostkeeper.secret) = true];",
            "x-file",
        ),
        (
            "string token = 1 [(hostkeeper.secret) = true];",
            "token-file",
        ),
    ];
    for (decl, flag) in cases {
        let text = fixture(&format!(
            "message Req {{ {decl} }}\nmessage M {{}}\nservice S {{ rpc Exec(Req) returns (M); }}\n"
        ));
        let refused = generate(&pool(&text)).expect_err(decl);
        assert_eq!(
            refused,
            GenError::FlagClash {
                method: "hostkeeper.fixture.v1.S.Exec".to_owned(),
                flag: flag.to_owned(),
            },
            "{decl}"
        );
    }
    assert_eq!(
        GenError::FlagClash {
            method: "m".to_owned(),
            flag: "help".to_owned()
        }
        .to_string(),
        "m: the flag --help is taken twice or kept by the command lines"
    );
    // Every flag the command lines keep is refused as a field.
    for flag in RESERVED_FLAGS {
        let field = flag.replace('-', "_");
        let text = fixture(&format!(
            "message Req {{ string {field} = 1; }}\nmessage M {{}}\n\
             service S {{ rpc Exec(Req) returns (M); }}\n"
        ));
        assert!(
            matches!(generate(&pool(&text)), Err(GenError::FlagClash { .. })),
            "{flag}"
        );
    }
}

/// Catches an enum flag whose values cannot be told apart: one spelled as nothing (its name is
/// the prefix alone) or two spelled alike.
#[test]
fn an_enum_value_with_an_empty_or_repeated_spelling_fails_the_build() {
    // proto2, whose compiler does not itself refuse values alike once the prefix is gone.
    for (values, bad) in [
        ("E_UNSPECIFIED = 0; E_ = 1;", "E_"),
        ("E_UNSPECIFIED = 0; E_A = 1; A = 2;", "A"),
    ] {
        let text = format!(
            "syntax = \"proto2\";\npackage hostkeeper.fixture.v1;\n\
             enum E {{ {values} }}\nmessage Req {{ optional E e = 1; }}\n\
             service S {{ rpc Exec(Req) returns (Req); }}\n"
        );
        let refused = generate(&pool(&text)).expect_err(values);
        assert_eq!(
            refused,
            GenError::EnumSpelling {
                value: format!("hostkeeper.fixture.v1.{bad}"),
            }
        );
        assert_eq!(
            refused.to_string(),
            format!("hostkeeper.fixture.v1.{bad}: its flag spelling is empty or another value's")
        );
    }
    // A value without the prefix is spelled from its whole name.
    let t = table(
        "enum E { E_UNSPECIFIED = 0; OTHER_NAME = 1; }\nmessage Req { E e = 1; }\n\
         service S { rpc Exec(Req) returns (Req); }\n",
    );
    assert!(field_line(&t, "e").contains("flag: \"other_name\""), "{t}");
}

/// Catches an unprefixed `UNSPECIFIED` value taken as a flag value, which the rules refuse.
#[test]
fn an_unprefixed_unspecified_value_is_not_a_flag_value() {
    let t = table(
        "enum Speed { UNSPECIFIED = 0; FAST = 1; SLOW = 2; }\nmessage Req { Speed speed = 1; }\n\
         service S { rpc Exec(Req) returns (Req); }\n",
    );
    assert!(
        field_line(&t, "speed").contains(
            "enum_values: &[EnumValue { flag: \"fast\", name: \"FAST\", number: 1 }, \
             EnumValue { flag: \"slow\", name: \"SLOW\", number: 2 }, ]"
        ),
        "{t}"
    );
    assert!(!field_line(&t, "speed").contains("unspecified"), "{t}");
}

/// Catches a local service given commands, a local service missing from the list, a service
/// outside hostkeeper's packages written into the table, and services or RPCs out of order.
#[test]
fn local_services_are_listed_and_others_ordered_by_command() {
    let text = "syntax = \"proto3\";\npackage hostkeeper.local.v9;\n\
                message M {}\nservice Admin { rpc Status(M) returns (M); }\n";
    let mut chain = ChainFileResolver::new();
    chain.add(Sources(vec![
        ("local.proto", text.to_owned()),
        (
            "other.proto",
            "syntax = \"proto3\";\npackage other.v1;\nmessage M {}\n\
             service Foreign { rpc Exec(M) returns (M); }\n"
                .to_owned(),
        ),
        (
            "ours.proto",
            fixture(
                "message M {}\n// Zed does zed things.\nservice Zed { rpc B(M) returns (M); \
                 rpc A(M) returns (M); }\nservice Alpha { rpc Only(M) returns (M); }\n",
            ),
        ),
    ]));
    chain.add(IncludeFileResolver::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("proto"),
    ));
    chain.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(chain);
    compiler.include_imports(true).include_source_info(true);
    compiler
        .open_files(["local.proto", "other.proto", "ours.proto"])
        .expect("the fixtures compile");
    let pool = DescriptorPool::decode(&compiler.encode_file_descriptor_set()[..]).expect("decode");
    let t = generate(&pool).expect("generates");
    assert!(
        t.contains("pub static HAND_WRITTEN_LOCAL: &[&str] = &[\"hostkeeper.local.v9.Admin\", ];"),
        "{t}"
    );
    assert!(!t.contains("/hostkeeper.local.v9.Admin/Status"), "{t}");
    assert!(!t.contains("Foreign"), "{t}");
    let order: Vec<&str> = t
        .lines()
        .filter_map(|l| l.trim().strip_prefix("path: "))
        .collect();
    assert_eq!(
        order,
        [
            "\"/hostkeeper.fixture.v1.Alpha/Only\",",
            "\"/hostkeeper.fixture.v1.Zed/B\",",
            "\"/hostkeeper.fixture.v1.Zed/A\",",
        ]
    );
    assert!(t.contains("service_doc: \"Zed does zed things.\""), "{t}");
    let reserved: Vec<String> = RESERVED_FLAGS.iter().map(|f| format!("{f:?}, ")).collect();
    assert!(
        t.contains(&format!(
            "pub static RESERVED_FLAGS: &[&str] = &[{}];",
            reserved.concat()
        )),
        "{t}"
    );
}

/// Catches comments lost or mangled on their way to `--help`: the leading space of each line
/// taken off, lines kept, a field with no comment given an empty one; and a pool without
/// hostkeeper's options read as having no secret.
#[test]
fn comments_become_docs_and_a_pool_without_options_has_no_secret() {
    let text = "syntax = \"proto3\";\npackage hostkeeper.fixture.v1;\n\
                // Things.\nservice Things {\n  // Do it.\n  //\n  //   Indented.\n\
                rpc Exec(Req) returns (Req);\n}\n\
                message Req {\n  // The name.\n  string name = 1;\n  string bare = 2;\n}\n";
    let t = generate(&pool(text)).expect("generates");
    assert!(t.contains("doc: \"Do it.\\n\\n  Indented.\""), "{t}");
    assert!(field_line(&t, "name").contains("doc: \"The name.\""));
    assert!(field_line(&t, "bare").contains("doc: \"\""));
    assert!(field_line(&t, "name").contains("secret: false"));
    assert!(t.contains("method_command: \"exec\""), "{t}");
    assert!(t.contains("response: \"hostkeeper.fixture.v1.Req\""), "{t}");
    assert!(t.contains("package: \"hostkeeper.fixture.v1\""), "{t}");
}

/// Catches the real protos failing the generator, which would fail the build of every crate.
#[test]
fn the_real_protos_generate() {
    let pool = DescriptorPool::decode(crate::FILE_DESCRIPTOR_SET).expect("decode");
    let t = generate(&pool).expect("the real protos generate");
    assert!(
        t.contains("path: \"/hostkeeper.operator.v1.Releases/SetDesired\""),
        "{t}"
    );
    assert!(t.contains("\"hostkeeper.local.v1.Admin\""), "{t}");
}
