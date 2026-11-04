//! The clap command tree of `hostkeeper ctl`, built at run time from a command table: one
//! subcommand per service, one per RPC under it, and the flags of each request field. Nothing
//! here names an RPC; the table does.

use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, Command as Cli, value_parser};
use hostkeeper_proto::ctl::{Command, Field, FieldKind, Input, Label};

use crate::exit::MEANINGS;

/// A flag `ctl` defines itself: its name, the name of its value (empty for a switch) and its help.
pub struct Global {
    pub flag: &'static str,
    pub value: &'static str,
    pub help: &'static str,
}

/// The flags of every call, in the order `--help` lists them. Their names are
/// `hostkeeper_proto::ctl::RESERVED_FLAGS`, with `request` and `help`, which no field may take.
pub const GLOBALS: &[Global] = &[
    Global {
        flag: "endpoint",
        value: "URL",
        help: "The coordinator's listener, as https://<host>[:<port>]: the private listener for \
               the operator services, the heartbeat listener for `coordinator`. Needed for a call.",
    },
    Global {
        flag: "ca-file",
        value: "PATH",
        help: "PEM certificates of the CA the server's certificate must chain to; no other root \
               is trusted. Needed for a call.",
    },
    Global {
        flag: "token-file",
        value: "PATH",
        help: "Read the bearer token (an operator token, or CI's OIDC token) from this file; one \
               trailing newline is dropped. The token is never a flag value nor an environment \
               variable, and is never printed.",
    },
    Global {
        flag: "token-stdin",
        value: "",
        help: "Read the bearer token from stdin instead.",
    },
    Global {
        flag: "client-cert-file",
        value: "PATH",
        help: "A PEM client certificate, for the heartbeat listener's mutual TLS; with \
               --client-key-file.",
    },
    Global {
        flag: "client-key-file",
        value: "PATH",
        help: "The PEM private key of --client-cert-file.",
    },
    Global {
        flag: "transport",
        value: "grpc|gateway",
        help: "gRPC, or the HTTP/JSON gateway of the private listener (POST /<package>.<Service>/<Method>).",
    },
    Global {
        flag: "output",
        value: "json|text",
        help: "json: the response as proto3 canonical JSON. text: `field: value` lines, nested \
               values as indented JSON.",
    },
    Global {
        flag: "timeout-secs",
        value: "SECONDS",
        help: "How long to wait to connect, and for the server; at least 1.",
    },
];

/// The default of `--transport`, `--output` and `--timeout-secs`.
pub const DEFAULT_TRANSPORT: &str = "grpc";
pub const DEFAULT_OUTPUT: &str = "text";
pub const DEFAULT_TIMEOUT_SECS: &str = "30";

/// How `ctl` turns a request field into input, said once in the root's `--help`.
pub const RULES: &str = "\
Every command is one RPC: `hostkeeper ctl <service> <rpc>`, both in kebab case as the protos \
name them (`Releases.SetDesired` is `releases set-desired`).

A request field is a flag named after it (`node_id` is `--node-id`): a scalar takes one value; a \
bool takes `true` or `false`, so that an absent flag and `false` differ; an enum takes a value's \
name without its prefix, in lower case, never `unspecified`; a repeated field repeats its flag; a \
`map<string, string>` repeats `--<field> key=value`.

A message, a oneof, `bytes` and any other map come only through `--request <file|->`: proto3 \
canonical JSON, `-` for stdin. A field named both by a flag and by the request file is a usage \
error.

A secret field (`(hostkeeper.secret)`) is never a flag: it is read with `--<field>-file <path>` \
(one trailing newline dropped) or `--<field>-stdin`, and never printed. At most one input reads \
stdin.

The response goes to stdout; diagnostics go to stderr. A refusal prints `refused: <token>`.";

/// The exit codes, for the root's `--help`.
fn exit_help() -> String {
    let mut text = String::from("Exit codes:\n");
    for (code, meaning) in MEANINGS {
        text.push_str(&format!("  {code}  {meaning}\n"));
    }
    text
}

/// What a field's value is called in `--help`.
pub fn value_name(field: &Field) -> String {
    match (field.label, field.kind) {
        (Label::Map, _) => "KEY=VALUE".to_owned(),
        (_, FieldKind::Bool) => "true|false".to_owned(),
        (_, FieldKind::Enum) => "VALUE".to_owned(),
        (_, kind) => format!("{kind:?}").to_ascii_uppercase(),
    }
}

/// A field's type, for the list of fields that come only through the request file.
pub fn shape(field: &Field) -> String {
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

/// The flags of one field: none when it comes only through the request file.
fn field_args(field: &Field) -> Vec<Arg> {
    match field.input {
        Input::RequestFile => Vec::new(),
        Input::SecretFile => vec![
            Arg::new(format!("{}-file", field.flag))
                .long(format!("{}-file", field.flag))
                .value_name("PATH")
                .value_parser(value_parser!(std::path::PathBuf))
                .conflicts_with(format!("{}-stdin", field.flag))
                .help(format!(
                    "Read `{}` from this file; one trailing newline is dropped. A secret: never a \
                     flag value, never printed. {}",
                    field.name, field.doc
                )),
            Arg::new(format!("{}-stdin", field.flag))
                .long(format!("{}-stdin", field.flag))
                .action(ArgAction::SetTrue)
                .help(format!("Read `{}` from stdin.", field.name)),
        ],
        Input::Flag => {
            let mut arg = Arg::new(field.flag)
                .long(field.flag)
                .value_name(value_name(field))
                .help(field.doc);
            if matches!(field.label, Label::Repeated | Label::Map) {
                arg = arg.action(ArgAction::Append);
            }
            match field.kind {
                FieldKind::Bool => arg.value_parser(["true", "false"]),
                FieldKind::String | FieldKind::Bytes | FieldKind::Message => arg,
                FieldKind::Enum => {
                    let names: Vec<&'static str> =
                        field.enum_values.iter().map(|v| v.flag).collect();
                    arg.value_parser(PossibleValuesParser::new(names))
                }
                // A number: `-1` is a value, not a flag; the parse says what is wrong with it.
                _ => arg.allow_negative_numbers(true),
            }
        }
    }
}

/// The subcommand of one RPC.
fn rpc(command: &Command) -> Cli {
    let file_only: Vec<String> = command
        .fields
        .iter()
        .filter(|f| f.input == Input::RequestFile)
        .map(|f| format!("  {} ({})", f.name, shape(f)))
        .collect();
    let mut cli = Cli::new(command.method_command)
        .about(command.doc)
        .arg(
            Arg::new("request")
                .long("request")
                .value_name("FILE|-")
                .help("The request as proto3 canonical JSON, from a file or `-` for stdin; flags add to it."),
        )
        .args(command.fields.iter().flat_map(field_args));
    if !file_only.is_empty() {
        cli = cli.after_help(format!("Through --request only:\n{}", file_only.join("\n")));
    }
    cli
}

/// The global flags of `ctl`.
fn globals() -> Vec<Arg> {
    GLOBALS
        .iter()
        .map(|g| {
            let arg = Arg::new(g.flag).long(g.flag).global(true).help(g.help);
            match g.flag {
                "token-stdin" => arg.action(ArgAction::SetTrue).conflicts_with("token-file"),
                "client-cert-file" => arg
                    .value_name(g.value)
                    .value_parser(value_parser!(std::path::PathBuf))
                    .requires("client-key-file"),
                "client-key-file" => arg
                    .value_name(g.value)
                    .value_parser(value_parser!(std::path::PathBuf))
                    .requires("client-cert-file"),
                "ca-file" | "token-file" => arg
                    .value_name(g.value)
                    .value_parser(value_parser!(std::path::PathBuf)),
                "transport" => arg
                    .value_name(g.value)
                    .value_parser(["grpc", "gateway"])
                    .default_value(DEFAULT_TRANSPORT),
                "output" => arg
                    .value_name(g.value)
                    .value_parser(["json", "text"])
                    .default_value(DEFAULT_OUTPUT),
                "timeout-secs" => arg
                    .value_name(g.value)
                    .value_parser(value_parser!(u64).range(1..))
                    .default_value(DEFAULT_TIMEOUT_SECS),
                _ => arg.value_name(g.value),
            }
        })
        .collect()
}

/// The whole tree: `ctl`, a subcommand per service in the table's order, an RPC under each.
pub fn build(commands: &[Command]) -> Cli {
    let mut services: Vec<Cli> = Vec::new();
    for command in commands {
        let position = services
            .iter()
            .position(|s| s.get_name() == command.service_command);
        let index = position.unwrap_or_else(|| {
            services.push(
                Cli::new(command.service_command)
                    .about(command.service_doc)
                    .subcommand_required(true)
                    .arg_required_else_help(true),
            );
            services.len() - 1
        });
        let service = std::mem::replace(&mut services[index], Cli::new(""));
        services[index] = service.subcommand(rpc(command));
    }
    Cli::new("ctl")
        .bin_name("hostkeeper ctl")
        .about(
            "Call one RPC of hostkeeper's coordinator; every command is generated from the protos.",
        )
        .long_about(RULES)
        .after_help(exit_help())
        .subcommand_required(true)
        .arg_required_else_help(true)
        .args(globals())
        .subcommands(services)
}
