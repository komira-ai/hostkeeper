//! The reference of `hostkeeper ctl`, written from a command table: `docs/ctl.md`, whose test
//! fails when it differs from what the table makes, and the command-tree lines a golden file
//! holds so that no flag name or exit code changes unnoticed.

use hostkeeper_proto::ctl::{Command, Input, Label};

use crate::codes;
use crate::exit;
use crate::tree::{self, GLOBALS};

/// A text as one cell of a Markdown table: one line, `|` escaped.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// `docs/ctl.md`.
pub fn markdown(commands: &[Command]) -> String {
    let mut out = String::from(
        "# hostkeeper ctl\n\n\
         <!-- Generated from the protos: `cargo test -p hostkeeper-ctl --test docs` fails when this \
         file differs, and writes the current text beside its test binary. Do not edit by hand. -->\n\n\
         `hostkeeper ctl` calls one RPC of the coordinator per command. Every command, flag and \
         help text below is generated from the protobuf descriptors of `hostkeeper-proto`; the \
         services of a host's admin socket (packages `hostkeeper.local.v<n>`) are not here.\n\n\
         ## Rules\n\n",
    );
    out.push_str(tree::RULES);
    out.push_str(
        "\n\n## Global flags\n\n| flag | value | default | meaning |\n|---|---|---|---|\n",
    );
    for g in GLOBALS {
        let default = match g.flag {
            "transport" => tree::DEFAULT_TRANSPORT,
            "output" => tree::DEFAULT_OUTPUT,
            "timeout-secs" => tree::DEFAULT_TIMEOUT_SECS,
            _ => "",
        };
        out.push_str(&format!(
            "| `--{}` | {} | {} | {} |\n",
            g.flag,
            cell(g.value),
            default,
            cell(g.help)
        ));
    }
    out.push_str("\n## Exit codes\n\n| code | meaning | gRPC codes |\n|---|---|---|\n");
    for (code, meaning) in exit::MEANINGS {
        let grpc: Vec<&str> = codes::ALL
            .iter()
            .filter(|(c, _)| exit::code(*c) == code)
            .map(|(_, name)| *name)
            .collect();
        out.push_str(&format!(
            "| {code} | {} | {} |\n",
            cell(meaning),
            grpc.join(", ")
        ));
    }
    out.push_str("\n## Commands\n");
    let mut service = "";
    for command in commands {
        if command.service_command != service {
            service = command.service_command;
            out.push_str(&format!(
                "\n### `{service}`\n\nThe service `{}` of the package `{}`. {}\n",
                command.service, command.package, command.service_doc
            ));
        }
        out.push_str(&format!(
            "\n#### `hostkeeper ctl {} {}`\n\n`{}`\n\n{}\n",
            command.service_command, command.method_command, command.path, command.doc
        ));
        let flags: Vec<String> = command
            .fields
            .iter()
            .flat_map(|f| {
                let repeat = matches!(f.label, Label::Repeated | Label::Map);
                match f.input {
                    Input::Flag => vec![format!(
                        "| `--{}` | {}{} | {} |",
                        f.flag,
                        cell(&tree::value_name(f)),
                        if repeat { ", repeated" } else { "" },
                        cell(f.doc)
                    )],
                    Input::SecretFile => vec![
                        format!(
                            "| `--{}-file` | PATH | secret `{}`, from a file. {} |",
                            f.flag,
                            f.name,
                            cell(f.doc)
                        ),
                        format!(
                            "| `--{}-stdin` | | secret `{}`, from stdin |",
                            f.flag, f.name
                        ),
                    ],
                    Input::RequestFile => Vec::new(),
                }
            })
            .collect();
        if !flags.is_empty() {
            out.push_str("\n| flag | value | meaning |\n|---|---|---|\n");
            out.push_str(&flags.join("\n"));
            out.push('\n');
        }
        let file_only: Vec<String> = command
            .fields
            .iter()
            .filter(|f| f.input == Input::RequestFile)
            .map(|f| format!("`{}` ({})", f.name, tree::shape(f)))
            .collect();
        if !file_only.is_empty() {
            out.push_str(&format!(
                "\nThrough `--request` only: {}.\n",
                file_only.join(", ")
            ));
        }
    }
    // No line ends in a space, which an editor would strip.
    out.lines().map(|l| format!("{}\n", l.trim_end())).collect()
}

/// One line per name a caller or a script depends on: each command, each flag with its value,
/// each field that comes only through the request file, each global flag, and the exit code of
/// each gRPC code. A golden file of these lines may grow; a line it holds never changes.
pub fn tree_lines(commands: &[Command]) -> Vec<String> {
    let mut lines: Vec<String> = GLOBALS
        .iter()
        .map(|g| {
            format!("global --{} {}", g.flag, g.value)
                .trim_end()
                .to_owned()
        })
        .collect();
    for (code, name) in codes::ALL {
        lines.push(format!("exit {name} {}", exit::code(code)));
    }
    for c in commands {
        let at = format!("{} {}", c.service_command, c.method_command);
        lines.push(format!("command {at} {}", c.path));
        for f in c.fields {
            lines.push(match f.input {
                Input::Flag => format!("flag {at} --{} {}", f.flag, tree::value_name(f)),
                Input::SecretFile => format!("secret {at} --{0}-file --{0}-stdin", f.flag),
                Input::RequestFile => format!("request-file {at} {}", f.name),
            });
            for value in f.enum_values {
                lines.push(format!("value {at} --{} {}", f.flag, value.flag));
            }
        }
    }
    lines
}
