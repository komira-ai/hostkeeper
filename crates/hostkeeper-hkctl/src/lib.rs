//! `hkctl`: the command line of hostkeeper's coordinator, one bash script generated from the
//! command table of `hostkeeper-proto` (`hostkeeper_proto::ctl::COMMANDS`), as
//! `docs/design/two-programs.md` section 2.4 designs it.
//!
//! [`script`] writes the script: the runtime every release carries (`runtime.bash`: the flag
//! parser, the JSON writer, the call through `curl`, the exit codes), then the generated part,
//! one spec of flags and one help text per method, and the version. Nothing here names an RPC:
//! a new RPC in a proto is a new command at the next release. [`markdown`] writes
//! `docs/cli.md` from the same table, and [`scan`] holds a script to bash 3.2 and to the rules
//! that keep a secret out of a file, an argument list and a child's environment.
//!
//! What hkctl takes from the table: every command of a package other than the host API's
//! (`hostkeeper.agent.v<n>`), which the gateway never serves and whose caller is the daemon
//! ([`is_served`]); the flags the table gives each field, and the request file for the rest.
//! What it adds: the global flags of [`GLOBALS`], two of which ([`OWN_FLAGS`]) no request field
//! may take, and the hold of a request field named [`HOLD_FIELD`], which lengthens the call's
//! deadline by its value.

mod doc;
pub mod scan;

use std::fmt::Write as _;

use hostkeeper_proto::ctl::{Command, Field, FieldKind, Input, Label};

pub use doc::markdown;

/// The program's name: the first word of `--version`, and the stem of its release asset.
pub const PROGRAM: &str = "hkctl";

/// The runtime every script carries before its generated part.
const RUNTIME: &str = include_str!("runtime.bash");

/// A request field of this name, an integer given as a flag or in the request file, holds the
/// call on the coordinator for that many seconds (`Releases.WaitConverged`): `curl --max-time` is
/// its value plus `--timeout-secs`.
pub(crate) const HOLD_FIELD: &str = "wait_secs";

/// The flags hkctl defines beyond those the command table keeps for itself (the generator's
/// `RESERVED_FLAGS`, which no field takes): no field may take these either. `--version` is
/// hkctl's own only before the service, so a field may be named `version`.
pub(crate) const OWN_FLAGS: [&str; 2] = ["github-oidc-audience", "response-file"];

/// The default of `--timeout-secs`.
pub(crate) const DEFAULT_TIMEOUT_SECS: &str = "30";

/// A flag hkctl defines itself: its name, the name of its value (empty for a switch) and its help.
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
        help: "PEM certificates of the CA the coordinator's certificate must chain to, given to \
               curl as `--cacert` in place of its default bundle (a curl built with a default CA \
               directory also reads that directory). Needed for a call.",
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
        help: "Print the help of hkctl, a service or a method.",
    },
    Global {
        flag: "version",
        value: "",
        help: "Print `hkctl <version>+<commit>`; only before the service, so a method's own \
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
        "anything else: no answer (curl failed), an answer that is not the gateway's, or any \
         other code",
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

/// How hkctl turns a request field into input, said once in `--help` and `docs/cli.md`.
pub(crate) const RULES: &str = "\
Every command is one method: `hkctl <service> <method>`, both in kebab case as the protos name \
them (`Releases.SetDesired` is `releases set-desired`), called as one POST of JSON to the \
coordinator's gateway with curl. hkctl needs bash 3.2 or later and curl 7.55 or later, and no jq.

A request field is a flag named after it (`node_id` is `--node-id`): a scalar takes one value; a \
bool takes `true` or `false`, so that an absent flag and `false` differ; an enum takes a value's \
name without its prefix, in lower case, never `unspecified`; a repeated field repeats its flag; a \
`map<string, string>` repeats `--<field> key=value`. A flag takes its value as `--flag value` or \
`--flag=value`; a value that starts with `--` only as `--flag=value`.

A message, a oneof, `bytes` and any other map come only through `--request <file|->`: a JSON \
object, `-` for stdin, which the flags add to. A field named both by a flag and by the request \
file is a usage error.

A secret field (`(hostkeeper.secret)`) is never a flag: it is read with `--<field>-file <path>` \
(one trailing newline dropped) or `--<field>-stdin`, and never printed. At most one input reads \
stdin. No token or secret is ever written to a file, put in an argument list or in a child's \
environment: curl reads each through a pipe.

The answer, the gateway's JSON, goes to stdout or to `--response-file`; diagnostics go to \
stderr. A refusal prints `refused: <reason>` when its reason is a token of `[a-z0-9_]{1,64}`. \
A call refused `invalid_request` makes one more call, with no token, to the coordinator's \
`GET /v1/version?at_least=<this hkctl's release>`: when that says the coordinator is older (`409`, \
or `404` from a coordinator before the route), hkctl says so on stderr, with the coordinator's \
version when it gave one, and the reason it prints is `coordinator_older`; any other answer, or \
none, leaves `invalid_request`.";

/// Why a command table cannot be written as a script.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmitError {
    /// A request field whose flag is one of [`OWN_FLAGS`].
    #[error("hkctl {command}: the flag --{flag} is one of hkctl's own")]
    FlagClash { command: String, flag: String },
    /// A version stamp with a character a script's quoted line may not hold.
    #[error("the version {0:?} is not one of letters, digits, `.`, `+` and `-`")]
    Stamp(String),
}

/// Whether hkctl has a command for `command`: every package but the host API's,
/// `hostkeeper.agent.v<n>`.
pub fn is_served(command: &Command) -> bool {
    !command
        .package
        .split('.')
        .take(2)
        .eq(["hostkeeper", "agent"])
}

/// A field's name as proto3 JSON spells it: `node_id` is `nodeId`.
fn json_name(name: &str) -> String {
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

/// `text` as one single-quoted bash word.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// `text` on one line: its words joined by one space.
fn line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The runtime's code of a field's kind.
fn kind_code(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::String => "string",
        FieldKind::Bool => "bool",
        FieldKind::Enum => "enum",
        FieldKind::Int32 | FieldKind::Sint32 | FieldKind::Sfixed32 => "i32",
        FieldKind::Uint32 | FieldKind::Fixed32 => "u32",
        FieldKind::Int64 | FieldKind::Sint64 | FieldKind::Sfixed64 => "i64",
        FieldKind::Uint64 | FieldKind::Fixed64 => "u64",
        FieldKind::Float | FieldKind::Double => "float",
        FieldKind::Bytes => "bytes",
        FieldKind::Message => "message",
    }
}

/// The runtime's code of a field's label.
fn label_code(label: Label) -> &'static str {
    match label {
        Label::Singular => "one",
        Label::Optional => "opt",
        Label::Repeated => "many",
        Label::Map => "map",
        Label::Oneof => "oneof",
    }
}

/// Whether a field holds the call: [`HOLD_FIELD`], an integer flag.
fn is_hold(field: &Field) -> bool {
    field.name == HOLD_FIELD
        && field.input == Input::Flag
        && matches!(kind_code(field.kind), "i32" | "u32" | "i64" | "u64")
}

/// One field's spec, `flag|name|json|input|kind|label|spelling=NAME,...`, which the runtime's
/// `_hk_parse_spec` reads.
fn spec(field: &Field) -> String {
    let input = match field.input {
        Input::Flag => "f",
        Input::SecretFile => "s",
        Input::RequestFile => "r",
    };
    let values: Vec<String> = field
        .enum_values
        .iter()
        .map(|v| format!("{}={}", v.flag, v.name))
        .collect();
    format!(
        "{}|{}|{}|{input}|{}|{}|{}",
        field.flag,
        field.name,
        json_name(field.name),
        kind_code(field.kind),
        label_code(field.label),
        values.join(",")
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

/// The name of the help function of a method.
fn help_fn(command: &Command) -> String {
    format!(
        "_hk_help__{}__{}",
        command.service_command.replace('-', "_"),
        command.method_command.replace('-', "_")
    )
}

/// Appends a function that prints `text`.
fn print_fn(out: &mut String, name: &str, text: &str) {
    let _ = writeln!(out, "{name}() {{\n\tprintf '%s\\n' {}\n}}\n", quoted(text));
}

/// The top-level help.
fn top_help(commands: &[&Command], stamp: &str) -> String {
    let mut text = format!(
        "{PROGRAM} {stamp}: call one method of hostkeeper's coordinator through its JSON gateway.\n\n\
         Usage: hkctl [global flags] <service> <method> [flags]\n\n{RULES}\n\nGlobal flags:\n"
    );
    for g in &GLOBALS {
        let flag = format!("--{} {}", g.flag, g.value);
        let _ = writeln!(text, "  {}\n      {}", flag.trim_end(), line(g.help));
    }
    text.push_str("\nExit codes:\n");
    for (code, meaning) in MEANINGS {
        let _ = writeln!(text, "  {code}  {}", line(meaning));
    }
    text.push_str("\nServices:\n");
    let mut last = "";
    for c in commands {
        if c.service_command != last {
            last = c.service_command;
            let _ = writeln!(
                text,
                "  {}\n      {}",
                c.service_command,
                line(c.service_doc)
            );
        }
    }
    text.push_str(
        "\nRun 'hkctl <service> --help' for its methods, and 'hkctl <service> <method> --help' \
         for a method's flags.",
    );
    text
}

/// The help of one service, from its first command.
fn service_help(first: &Command, methods: &[&Command]) -> String {
    let mut text = format!(
        "hkctl {}: the service `{}` of the package `{}`. {}\n\n\
         Usage: hkctl [global flags] {} <method> [flags]\n\nMethods:\n",
        first.service_command,
        first.service,
        first.package,
        line(first.service_doc),
        first.service_command
    );
    for m in methods {
        let _ = writeln!(text, "  {}\n      {}", m.method_command, line(m.doc));
    }
    text.pop();
    text
}

/// The help of one method.
fn command_help(command: &Command) -> String {
    let mut text = format!(
        "hkctl {} {}: {}\n\n{}\n\nUsage: hkctl [global flags] {} {} [flags]\n\nFlags:\n  \
         --request FILE|-\n      The request as a JSON object, from a file or `-` for stdin; \
         flags add to it.\n",
        command.service_command,
        command.method_command,
        command.path,
        line(command.doc),
        command.service_command,
        command.method_command
    );
    for field in command.fields {
        for (flag, help) in field_flags(field) {
            let _ = writeln!(text, "  {flag}\n      {help}");
        }
    }
    let file_only: Vec<String> = command
        .fields
        .iter()
        .filter(|f| f.input == Input::RequestFile)
        .map(|f| format!("{} ({})", f.name, shape(f)))
        .collect();
    if !file_only.is_empty() {
        let _ = writeln!(text, "\nThrough --request only: {}.", file_only.join(", "));
    }
    text.push_str("\nRun 'hkctl --help' for the global flags and the exit codes.");
    text
}

/// Refuses a field of `command` whose flag is one of [`OWN_FLAGS`].
fn check_flags(command: &Command) -> Result<(), EmitError> {
    for field in command.fields {
        let taken = match field.input {
            Input::Flag => vec![field.flag.to_owned()],
            Input::SecretFile => vec![
                format!("{}-file", field.flag),
                format!("{}-stdin", field.flag),
            ],
            Input::RequestFile => Vec::new(),
        };
        if let Some(flag) = taken.into_iter().find(|f| OWN_FLAGS.contains(&f.as_str())) {
            return Err(EmitError::FlagClash {
                command: format!("{} {}", command.service_command, command.method_command),
                flag,
            });
        }
    }
    Ok(())
}

/// The case arm of `_hk_dispatch` for one method.
fn dispatch_arm(out: &mut String, command: &Command) {
    let words = format!("{} {}", command.service_command, command.method_command);
    let hold = command
        .fields
        .iter()
        .position(is_hold)
        .map_or_else(|| "-1".to_owned(), |i| i.to_string());
    let _ = writeln!(out, "\t{})", quoted(&words));
    let _ = writeln!(out, "\t\t_hk_command={}", quoted(&words));
    let _ = writeln!(out, "\t\t_hk_path={}", quoted(command.path));
    let _ = writeln!(out, "\t\t_hk_helpfn={}", help_fn(command));
    let _ = writeln!(out, "\t\t_hk_hold={hold}");
    out.push_str("\t\t_hk_spec=(");
    for field in command.fields {
        let _ = write!(out, "\n\t\t\t{}", quoted(&spec(field)));
    }
    if !command.fields.is_empty() {
        out.push_str("\n\t\t");
    }
    out.push_str(")\n\t\t;;\n");
}

/// The script of hkctl for `commands`, whose `--version` prints `hkctl <stamp>`
/// (`<version>+<12 hex of the commit>` for a release). Only the commands [`is_served`] keeps are
/// in it, in the table's order.
pub fn script(commands: &[Command], stamp: &str) -> Result<String, EmitError> {
    let stamp_ok = !stamp.is_empty()
        && stamp
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'));
    if !stamp_ok {
        return Err(EmitError::Stamp(stamp.to_owned()));
    }
    let served: Vec<&Command> = commands.iter().filter(|c| is_served(c)).collect();
    for command in &served {
        check_flags(command)?;
    }

    let mut out = String::from(
        "#!/usr/bin/env bash\n\
         # hkctl: the command line of hostkeeper's coordinator. Generated from its protobuf \
         services by\n\
         # `cargo xtask release hkctl` (crates/hostkeeper-hkctl); do not edit. `hkctl --help` \
         says how to\n\
         # use it, and docs/cli.md lists every command.\n\n",
    );
    out.push_str(RUNTIME);
    out.push_str("\n# ---- Generated from the command table.\n\n");
    let _ = writeln!(
        out,
        "_hk_version={}\n",
        quoted(&format!("{PROGRAM} {stamp}"))
    );

    // `_hk_exit_of CODE` returns the exit code of a gateway code, 0 for a name that is none.
    out.push_str("_hk_exit_of() {\n\tcase $1 in\n");
    for exit in 1..=3u8 {
        let names: Vec<&str> = EXIT_CODES
            .iter()
            .filter(|(_, e)| *e == exit)
            .map(|(name, _)| *name)
            .collect();
        let _ = writeln!(out, "\t{}) return {exit} ;;", names.join(" | "));
    }
    out.push_str("\tesac\n\treturn 0\n}\n\n");

    print_fn(&mut out, "_hk_help", &top_help(&served, stamp));

    // `_hk_help_service SERVICE` prints the help of a service; returns 1 for no service.
    out.push_str("_hk_help_service() {\n\tcase $1 in\n");
    let mut at = 0;
    while at < served.len() {
        let service = served[at].service_command;
        let methods: Vec<&Command> = served[at..]
            .iter()
            .take_while(|c| c.service_command == service)
            .copied()
            .collect();
        let _ = writeln!(
            out,
            "\t{})\n\t\tprintf '%s\\n' {}\n\t\t;;",
            quoted(service),
            quoted(&service_help(served[at], &methods))
        );
        at += methods.len();
    }
    out.push_str("\t*) return 1 ;;\n\tesac\n}\n\n");

    for command in &served {
        print_fn(&mut out, &help_fn(command), &command_help(command));
    }

    // `_hk_dispatch SERVICE METHOD` chooses a method's spec; returns 1 for no method.
    out.push_str("_hk_dispatch() {\n\tcase \"$1 $2\" in\n");
    for command in &served {
        dispatch_arm(&mut out, command);
    }
    out.push_str("\t*) return 1 ;;\n\tesac\n}\n\n_hk_main \"$@\"\n");
    Ok(out)
}

#[cfg(test)]
mod tests;
