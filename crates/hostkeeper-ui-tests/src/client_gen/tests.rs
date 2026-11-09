//! The generator of the client, run on fixture protos and hand-written command tables: how each
//! kind of field is declared, where each type goes, the order, a new RPC, every refusal; and on
//! hostkeeper's own table and descriptors, where it must succeed.

use std::path::Path;

use hostkeeper_proto::ctl::Command;
use prost_reflect::DescriptorPool;
use protox::file::{
    ChainFileResolver, File, FileResolver, GoogleFileResolver, IncludeFileResolver,
};

use super::*;

/// Fixture sources by name, in front of the real protos and the Google ones.
struct Sources(Vec<(&'static str, &'static str)>);

impl FileResolver for Sources {
    fn open_file(&self, name: &str) -> Result<File, protox::Error> {
        match self.0.iter().find(|(n, _)| *n == name) {
            Some((_, text)) => File::from_source(name, text),
            None => Err(protox::Error::file_not_found(name)),
        }
    }
}

/// The pool of the fixture files, compiled as the build compiles the protos.
fn pool_of(files: Vec<(&'static str, &'static str)>) -> DescriptorPool {
    let names: Vec<&str> = files.iter().map(|(n, _)| *n).collect();
    let mut chain = ChainFileResolver::new();
    chain.add(Sources(files.clone()));
    chain.add(IncludeFileResolver::new(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../hostkeeper-proto/proto"),
    ));
    chain.add(GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(chain);
    compiler.include_imports(true).include_source_info(true);
    compiler.open_files(names).expect("the fixtures compile");
    pool(&compiler.encode_file_descriptor_set()).expect("the set decodes")
}

/// A command of the operator package, as the table writes one.
fn command(
    service: &'static str,
    method: &'static str,
    words: (&'static str, &'static str),
    messages: (&'static str, &'static str),
) -> Command {
    Command {
        package: PACKAGE,
        service,
        method,
        path: "",
        service_command: words.0,
        method_command: words.1,
        service_doc: "",
        doc: "",
        request: messages.0,
        response: messages.1,
        fields: &[],
    }
}

const OTHER: &str = r#"syntax = "proto3";
package hostkeeper.other.v1;
message Far { string x = 1; }
"#;

const FIXTURE: &str = r#"syntax = "proto3";
package hostkeeper.operator.v1;
import "other.proto";

service Things {
  rpc GetThing(GetThingRequest) returns (GetThingResponse);
  rpc SetLabels(Empty) returns (Empty);
}

message Empty {}

message GetThingRequest {
  double d = 1;
  float f = 2;
  int32 i32 = 3;
  sint32 s32 = 4;
  sfixed32 sf32 = 5;
  uint32 u32 = 6;
  fixed32 f32 = 7;
  int64 i64 = 8;
  sint64 s64 = 9;
  sfixed64 sf64 = 10;
  uint64 u64 = 11;
  fixed64 f64 = 12;
  bool b = 13;
  string node_id = 14;
  bytes raw = 15;
  optional string maybe = 16;
  repeated string names = 17;
  repeated double ratios = 18;
  repeated Kind kinds = 19;
  map<string, string> labels = 20;
  map<string, Kind> kinds_by_name = 21;
  map<string, Inner> inner_by_name = 22;
  map<string, double> ratio_by_name = 23;
  Kind kind = 24;
  oneof pick {
    string chosen = 25;
    Inner chosen_inner = 26;
  }
  hostkeeper.other.v1.Far far = 27;
  Holder.Nested nested = 28;
  Tree tree = 29;
  // Declared out of number order: the declaration order is kept.
  string first = 31;
  string second = 30;
  // A second type of a holder nothing needs: its namespace is declared once.
  Holder.Also also = 32;

  message Inner {
    Deep deep = 1;
    enum Deep {
      DEEP_UNSPECIFIED = 0;
      DEEP_ONE = 1;
    }
  }
}

enum Kind {
  KIND_UNSPECIFIED = 0;
  KIND_A = 1;
}

enum LongEnumNameForWrapping {
  LONG_ENUM_NAME_FOR_WRAPPING_UNSPECIFIED = 0;
  LONG_ENUM_NAME_FOR_WRAPPING_FIRST = 1;
}

message Holder {
  string unused = 1;
  message Nested { string x = 1; }
  message Also {}
}

message Tree { repeated Tree children = 1; }

message GetThingResponse { LongEnumNameForWrapping state = 1; }
"#;

fn fixture() -> DescriptorPool {
    pool_of(vec![("other.proto", OTHER), ("fixture.proto", FIXTURE)])
}

fn things() -> Vec<Command> {
    vec![
        command(
            "Things",
            "GetThing",
            ("things", "get-thing"),
            (
                "hostkeeper.operator.v1.GetThingRequest",
                "hostkeeper.operator.v1.GetThingResponse",
            ),
        ),
        command(
            "Things",
            "SetLabels",
            ("things", "set-labels"),
            (
                "hostkeeper.operator.v1.Empty",
                "hostkeeper.operator.v1.Empty",
            ),
        ),
    ]
}

/// The header both files open with.
fn header() -> String {
    HEADER.to_owned()
}

/// Catches a field declared with another type than proto3 JSON writes, under its proto name
/// rather than its JSON name, required, or out of the order the message declares; a type
/// declared outside its package's or its message's namespace, a nested type whose holder is
/// declared though nothing needs it, a holder's namespace declared twice for two of its types,
/// a map entry declared as a type, a repeated union without
/// its parentheses, a long enum on one line, and a type reached twice declared twice.
#[test]
fn every_kind_of_field_is_declared_as_proto3_json_writes_it() {
    let client = emit(&things(), &fixture()).expect("the fixture generates");
    let expected = format!(
        "{}{}",
        header(),
        r#"// The types of `client.js`, in proto3 JSON; each is described in `crates/hostkeeper-proto/proto`.

/** Sends `request` to `method` (`<package>.<Service>/<Method>`) and resolves to the reply. */
export type Call = (method: string, request: object) => Promise<object>;

/** The operator API: one object per service, one function per method. */
export interface Client {
  readonly things: {
    getThing(request: hostkeeper_operator_v1.GetThingRequest): Promise<hostkeeper_operator_v1.GetThingResponse>;
    setLabels(request: hostkeeper_operator_v1.Empty): Promise<hostkeeper_operator_v1.Empty>;
  };
}

export declare function createClient(call: Call): Client;

export namespace hostkeeper_operator_v1 {
  export interface Empty {
  }
  export interface GetThingRequest {
    d?: number | "NaN" | "Infinity" | "-Infinity";
    f?: number | "NaN" | "Infinity" | "-Infinity";
    i32?: number;
    s32?: number;
    sf32?: number;
    u32?: number;
    f32?: number;
    i64?: string;
    s64?: string;
    sf64?: string;
    u64?: string;
    f64?: string;
    b?: boolean;
    nodeId?: string;
    raw?: string;
    maybe?: string;
    names?: string[];
    ratios?: (number | "NaN" | "Infinity" | "-Infinity")[];
    kinds?: hostkeeper_operator_v1.Kind[];
    labels?: { [key: string]: string };
    kindsByName?: { [key: string]: hostkeeper_operator_v1.Kind };
    innerByName?: { [key: string]: hostkeeper_operator_v1.GetThingRequest.Inner };
    ratioByName?: { [key: string]: number | "NaN" | "Infinity" | "-Infinity" };
    kind?: hostkeeper_operator_v1.Kind;
    chosen?: string;
    chosenInner?: hostkeeper_operator_v1.GetThingRequest.Inner;
    far?: hostkeeper_other_v1.Far;
    nested?: hostkeeper_operator_v1.Holder.Nested;
    tree?: hostkeeper_operator_v1.Tree;
    first?: string;
    second?: string;
    also?: hostkeeper_operator_v1.Holder.Also;
  }
  export namespace GetThingRequest {
    export interface Inner {
      deep?: hostkeeper_operator_v1.GetThingRequest.Inner.Deep;
    }
    export namespace Inner {
      export type Deep = "DEEP_UNSPECIFIED" | "DEEP_ONE";
    }
  }
  export interface GetThingResponse {
    state?: hostkeeper_operator_v1.LongEnumNameForWrapping;
  }
  export namespace Holder {
    export interface Also {
    }
    export interface Nested {
      x?: string;
    }
  }
  export type Kind = "KIND_UNSPECIFIED" | "KIND_A";
  export type LongEnumNameForWrapping =
    | "LONG_ENUM_NAME_FOR_WRAPPING_UNSPECIFIED"
    | "LONG_ENUM_NAME_FOR_WRAPPING_FIRST";
  export interface Tree {
    children?: hostkeeper_operator_v1.Tree[];
  }
}

export namespace hostkeeper_other_v1 {
  export interface Far {
    x?: string;
  }
}
"#
    );
    assert_eq!(client.dts, expected);
}

/// Catches a function under another service or name, posting another method, or out of the
/// table's order, and a service object left open to change.
#[test]
fn each_command_of_the_operator_package_is_one_frozen_function() {
    let client = emit(&things(), &fixture()).expect("the fixture generates");
    let body = client
        .js
        .split_once("export function createClient(call) {\n")
        .expect("createClient")
        .1;
    assert!(client.js.starts_with(&header()), "{}", client.js);
    assert_eq!(
        body,
        "  return Object.freeze({\n    things: Object.freeze({\n      \
         getThing: (request) => call(\"hostkeeper.operator.v1.Things/GetThing\", request),\n      \
         setLabels: (request) => call(\"hostkeeper.operator.v1.Things/SetLabels\", request),\n    \
         }),\n  });\n}\n"
    );
}

/// Catches a function made for a service of another package (the host API, the admin socket),
/// whose types would be declared too, and services merged or split wrongly.
#[test]
fn only_the_operator_package_is_in_the_client_and_services_keep_the_table_order() {
    let mut commands = vec![Command {
        package: "hostkeeper.agent.v1",
        service: "Coordinator",
        method: "Heartbeat",
        service_command: "coordinator",
        method_command: "heartbeat",
        request: "hostkeeper.agent.v1.Missing",
        response: "hostkeeper.agent.v1.Missing",
        ..things()[0]
    }];
    commands.push(command(
        "Alpha",
        "Get",
        ("alpha", "get"),
        (
            "hostkeeper.operator.v1.Empty",
            "hostkeeper.operator.v1.Empty",
        ),
    ));
    commands.extend(things());
    let found = functions(&commands);
    let calls: Vec<String> = found.iter().map(Function::call).collect();
    assert_eq!(calls, ["alpha.get", "things.getThing", "things.setLabels"]);
    assert_eq!(found[1].method, "hostkeeper.operator.v1.Things/GetThing");
    let client = emit(&commands, &fixture()).expect("the agent command is left out");
    assert!(!client.js.contains("coordinator"), "{}", client.js);
    assert!(!client.dts.contains("agent"), "{}", client.dts);
    assert!(
        client.js.contains("    }),\n    things: Object.freeze({\n"),
        "{}",
        client.js
    );
    assert!(
        client.dts.contains("  };\n  readonly things: {\n"),
        "{}",
        client.dts
    );
}

/// Catches a new RPC that needs a hand edit to reach the client: one more command is one more
/// function and its types, and nothing else changes.
#[test]
fn a_new_rpc_appears_with_no_edit() {
    let before = emit(&things()[..1], &fixture()).expect("one command");
    let after = emit(&things(), &fixture()).expect("two commands");
    assert!(!before.js.contains("setLabels"), "{}", before.js);
    assert!(!before.dts.contains("interface Empty"), "{}", before.dts);
    assert!(
        after.js.contains("setLabels: (request) => call("),
        "{}",
        after.js
    );
    assert!(
        after
            .dts
            .contains("setLabels(request: hostkeeper_operator_v1.Empty)"),
        "{}",
        after.dts
    );
    assert!(
        after.dts.contains("export interface Empty {"),
        "{}",
        after.dts
    );
}

/// Catches an empty table that makes a client that does not parse.
#[test]
fn a_table_with_no_operator_command_makes_an_empty_client() {
    let client = emit(&[], &fixture()).expect("nothing to generate");
    assert!(
        client.js.ends_with(
            "export function createClient(call) {\n  return Object.freeze({\n  });\n}\n"
        ),
        "{}",
        client.js
    );
    assert!(
        client
            .dts
            .ends_with("export interface Client {\n}\n\nexport declare function createClient(call: Call): Client;\n"),
        "{}",
        client.dts
    );
}

/// Catches a command whose message the descriptors do not hold, generated as an undeclared
/// type; a well-known type described as a plain message; a type with no package, or two
/// packages merged into one namespace; and descriptors that do not decode taken as empty. Each
/// refusal says what and where.
#[test]
fn what_cannot_be_described_is_refused_by_name() {
    let missing = vec![command(
        "Things",
        "GetThing",
        ("things", "get-thing"),
        (
            "hostkeeper.operator.v1.GetThingRequest",
            "hostkeeper.operator.v1.Gone",
        ),
    )];
    let error = emit(&missing, &fixture()).expect_err("the response is not declared");
    assert_eq!(
        error.to_string(),
        "hostkeeper.operator.v1.Things/GetThing: hostkeeper.operator.v1.Gone is not a message of \
         the descriptors"
    );

    let wkt = pool_of(vec![(
        "wkt.proto",
        "syntax = \"proto3\";\npackage hostkeeper.operator.v1;\n\
         import \"google/protobuf/timestamp.proto\";\n\
         message Stamp { google.protobuf.Timestamp at = 1; }\n",
    )]);
    let at = vec![command(
        "Clock",
        "Now",
        ("clock", "now"),
        (
            "hostkeeper.operator.v1.Stamp",
            "hostkeeper.operator.v1.Stamp",
        ),
    )];
    let error = emit(&at, &wkt).expect_err("a timestamp is a well-known type");
    assert_eq!(
        error.to_string(),
        "google.protobuf.Timestamp is a well-known type, whose JSON the client does not describe"
    );

    let loose = pool_of(vec![
        (
            "nopkg.proto",
            "syntax = \"proto3\";\nmessage Loose { string x = 1; }\n",
        ),
        (
            "holds.proto",
            "syntax = \"proto3\";\npackage hostkeeper.operator.v1;\nimport \"nopkg.proto\";\n\
             message Holds { .Loose loose = 1; }\n",
        ),
    ]);
    let holds = vec![command(
        "Holder",
        "Get",
        ("holder", "get"),
        (
            "hostkeeper.operator.v1.Holds",
            "hostkeeper.operator.v1.Holds",
        ),
    )];
    let error = emit(&holds, &loose).expect_err("a type with no package");
    assert_eq!(
        error.to_string(),
        "the package \"\" has no namespace of its own in the declarations"
    );

    let clash = pool_of(vec![
        (
            "xy_z.proto",
            "syntax = \"proto3\";\npackage x_y.z;\nmessage A {}\n",
        ),
        (
            "x_yz.proto",
            "syntax = \"proto3\";\npackage x.y_z;\nmessage B {}\n",
        ),
        (
            "both.proto",
            "syntax = \"proto3\";\npackage hostkeeper.operator.v1;\n\
             import \"xy_z.proto\";\nimport \"x_yz.proto\";\n\
             message Both { x_y.z.A a = 1; x.y_z.B b = 2; }\n",
        ),
    ]);
    let both = vec![command(
        "Holder",
        "Get",
        ("holder", "get"),
        ("hostkeeper.operator.v1.Both", "hostkeeper.operator.v1.Both"),
    )];
    let error = emit(&both, &clash).expect_err("two packages, one namespace");
    assert_eq!(
        error.to_string(),
        "the package \"x_y.z\" has no namespace of its own in the declarations"
    );

    let error = pool(b"\x0a\x03not").expect_err("not a descriptor set");
    assert!(
        error
            .to_string()
            .starts_with("the descriptor set does not decode: "),
        "{error}"
    );
}

#[test]
fn command_words_become_javascript_names() {
    for (kebab, name) in [
        ("set-desired", "setDesired"),
        ("list", "list"),
        ("clear-halt", "clearHalt"),
        ("a-b-c", "aBC"),
        ("", ""),
        ("x--y", "xY"),
    ] {
        assert_eq!(camel(kebab), name, "{kebab}");
    }
}

/// Catches hostkeeper's own protos turned into a client that misses an operator method or
/// adds one, or that cannot be generated.
#[test]
fn hostkeeper_s_own_table_makes_one_function_per_operator_rpc() {
    let client = current().expect("the real descriptors generate");
    let found = functions(hostkeeper_proto::ctl::COMMANDS);
    let operator: Vec<&Command> = hostkeeper_proto::ctl::COMMANDS
        .iter()
        .filter(|c| c.package == PACKAGE)
        .collect();
    assert_eq!(found.len(), operator.len());
    assert!(found.len() > 30, "{found:?}");
    for (function, command) in found.iter().zip(operator) {
        assert_eq!(format!("/{}", function.method), command.path);
        assert!(
            client.js.contains(&format!(
                "{}: (request) => call(\"{}\", request)",
                function.name, function.method
            )),
            "{function:?}"
        );
    }
    assert!(
        client
            .dts
            .contains("export namespace hostkeeper_agent_v1 {")
    );
}
