//! What `hk` reads from the command table and fixes itself: which commands it serves, its global
//! flags, the exit code of each code the gateway names, and how a field becomes a flag.

use hostkeeper_proto::ctl::{Command, Field, FieldKind, Input, Label};

/// A request field of this name, an integer given as a flag or in the request file, holds the
/// call on the coordinator for that many seconds (`Releases.WaitConverged`): the call's deadline
/// is its value plus `--timeout-secs`.
pub(crate) const HOLD_FIELD: &str = "wait_secs";

/// The default of `--timeout-secs`.
pub(crate) const DEFAULT_TIMEOUT_SECS: u32 = 30;

/// A flag `hk` defines itself: its name, the name of its value (empty for a switch) and its help.
pub(crate) struct Global {
    pub(crate) flag: &'static str,
    pub(crate) value: &'static str,
    pub(crate) help: &'static str,
}

/// The global flags, in the order `--help` lists them.
pub(crate) const GLOBALS: [Global; 9] = [
    Global {
        flag: "endpoint",
        value: "URL",
        help: "The coordinator's private listener, as https://<host>[:<port>]. Needed for a call.",
    },
    Global {
        flag: "ca-file",
        value: "PATH",
        help: "PEM certificates of the CA the coordinator's certificate must chain to; no other \
               root is trusted for the coordinator. Needed for a call.",
    },
    Global {
        flag: "token-file",
        value: "PATH",
        help: "Read the bearer token (an operator token) from this file, which only its owner may \
               read; one trailing newline is dropped. The token is never an argument, an \
               environment variable or a line of output.",
    },
    Global {
        flag: "token-stdin",
        value: "",
        help: "Read the bearer token from stdin instead.",
    },
    Global {
        flag: "github-oidc-audience",
        value: "AUDIENCE",
        help: "In a GitHub Actions job with id-token: write: ask the runner for a fresh ID token \
               for this audience and call with it; `::add-mask::<token>` is printed on stdout \
               first.",
    },
    Global {
        flag: "response-file",
        value: "PATH",
        help: "Write the answer to this file instead of stdout.",
    },
    Global {
        flag: "timeout-secs",
        value: "SECONDS",
        help: "How long to wait to connect, and for the answer, 1 to 999999; a method that holds \
               the call (a `wait_secs` field) adds its hold.",
    },
    Global {
        flag: "help",
        value: "",
        help: "Print the help of hk, a service or a method.",
    },
    Global {
        flag: "version",
        value: "",
        help: "Print `hk <version>+<commit>`; only before the service, so a method's own \
               `--version` is its field.",
    },
];

/// The exit code of each code the gateway's error body names (`{"code":"<NAME>",...}`). `OK` is
/// never an error body's: one that says `OK`, like any name not here, is not the gateway's.
pub(crate) const EXIT_CODES: [(&str, u8); 17] = [
    ("OK", 0),
    ("CANCELLED", 1),
    ("UNKNOWN", 1),
    ("INVALID_ARGUMENT", 2),
    ("DEADLINE_EXCEEDED", 1),
    ("NOT_FOUND", 1),
    ("ALREADY_EXISTS", 1),
    ("PERMISSION_DENIED", 3),
    ("RESOURCE_EXHAUSTED", 1),
    ("FAILED_PRECONDITION", 2),
    ("ABORTED", 1),
    ("OUT_OF_RANGE", 1),
    ("UNIMPLEMENTED", 1),
    ("INTERNAL", 1),
    ("UNAVAILABLE", 1),
    ("DATA_LOSS", 1),
    ("UNAUTHENTICATED", 3),
];

/// Each exit code with what it means, for `--help` and `docs/cli.md`.
pub(crate) const MEANINGS: [(u8, &str); 4] = [
    (0, "the call succeeded"),
    (
        1,
        "anything else: no answer, an answer that is not the gateway's, or any other code",
    ),
    (
        2,
        "a usage error, or the coordinator refused the request: INVALID_ARGUMENT or \
         FAILED_PRECONDITION",
    ),
    (
        3,
        "the coordinator refused the caller: UNAUTHENTICATED or PERMISSION_DENIED",
    ),
];

/// Whether `hk` has a command for `command`: every package but the host API's,
/// `hostkeeper.agent.v<n>`.
pub fn is_served(command: &Command) -> bool {
    !command
        .package
        .split('.')
        .take(2)
        .eq(["hostkeeper", "agent"])
}

/// The exit code of the gateway code `name`, or `None` for a name that is not an error's.
pub(crate) fn exit_of(name: &[u8]) -> Option<u8> {
    EXIT_CODES
        .iter()
        .find(|(known, code)| known.as_bytes() == name && *code != 0)
        .map(|(_, code)| *code)
}

/// A field's name as proto3 JSON spells it: `node_id` is `nodeId`.
pub(crate) fn json_name(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// How a flag's text becomes a JSON value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Enc {
    String,
    Bool,
    Enum,
    I32,
    U32,
    I64,
    U64,
    Float,
}

/// The encoding of a field's kind. A kind that is no flag's (`bytes`, a message) is read as a
/// 64-bit unsigned integer, as the script's runtime read it.
pub(crate) fn enc_of(kind: FieldKind) -> Enc {
    match kind {
        FieldKind::String => Enc::String,
        FieldKind::Bool => Enc::Bool,
        FieldKind::Enum => Enc::Enum,
        FieldKind::Int32 | FieldKind::Sint32 | FieldKind::Sfixed32 => Enc::I32,
        FieldKind::Uint32 | FieldKind::Fixed32 => Enc::U32,
        FieldKind::Int64 | FieldKind::Sint64 | FieldKind::Sfixed64 => Enc::I64,
        FieldKind::Float | FieldKind::Double => Enc::Float,
        FieldKind::Uint64 | FieldKind::Fixed64 | FieldKind::Bytes | FieldKind::Message => Enc::U64,
    }
}

/// Whether a field holds the call: [`HOLD_FIELD`], an integer flag.
pub(crate) fn is_hold(field: &Field) -> bool {
    field.name == HOLD_FIELD
        && field.input == Input::Flag
        && matches!(
            field.kind,
            FieldKind::Int32
                | FieldKind::Sint32
                | FieldKind::Sfixed32
                | FieldKind::Uint32
                | FieldKind::Fixed32
                | FieldKind::Int64
                | FieldKind::Sint64
                | FieldKind::Sfixed64
                | FieldKind::Uint64
                | FieldKind::Fixed64
        )
}

/// What a field's value is called in help.
pub(crate) fn value_name(field: &Field) -> String {
    match (field.label, field.kind) {
        (Label::Map, _) => "KEY=VALUE".to_owned(),
        (_, FieldKind::Bool) => "true|false".to_owned(),
        (_, FieldKind::Enum) => "VALUE".to_owned(),
        (_, kind) => format!("{kind:?}").to_ascii_uppercase(),
    }
}

/// A field's type, for the fields that come only through the request file.
pub(crate) fn shape(field: &Field) -> String {
    let base = if field.type_name.is_empty() {
        format!("{:?}", field.kind).to_ascii_lowercase()
    } else {
        field.type_name.to_owned()
    };
    match field.label {
        Label::Map => "map".to_owned(),
        Label::Repeated => format!("repeated {base}"),
        Label::Oneof => format!("{base}, in a oneof"),
        Label::Singular | Label::Optional => base,
    }
}

/// `text` on one line: its words joined by one space.
pub(crate) fn line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The flags of one field, each as `(flag with its value, help)`; none for a field that comes
/// only through the request file.
pub(crate) fn field_flags(field: &Field) -> Vec<(String, String)> {
    match field.input {
        Input::RequestFile => Vec::new(),
        Input::SecretFile => vec![
            (
                format!("--{}-file PATH", field.flag),
                line(&format!(
                    "Read the secret `{}` from this file; one trailing newline is dropped. {}",
                    field.name, field.doc
                )),
            ),
            (
                format!("--{}-stdin", field.flag),
                format!("Read the secret `{}` from stdin.", field.name),
            ),
        ],
        Input::Flag => {
            let mut help = line(field.doc);
            if !field.enum_values.is_empty() {
                let values: Vec<&str> = field.enum_values.iter().map(|v| v.flag).collect();
                help = line(&format!("{help} One of: {}.", values.join(", ")));
            }
            if matches!(field.label, Label::Repeated | Label::Map) {
                help = line(&format!("{help} Repeat the flag for more."));
            }
            vec![(format!("--{} {}", field.flag, value_name(field)), help)]
        }
    }
}
