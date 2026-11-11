//! `docs/cli.md`, the reference of hkctl, written from a command table; its test fails when the
//! checked-in file differs from what the table makes.

use std::fmt::Write as _;

use hostkeeper_proto::ctl::{Command, Input};

use crate::{
    DEFAULT_TIMEOUT_SECS, EXIT_CODES, GLOBALS, MEANINGS, PROGRAM, RULES, field_flags, is_served,
    shape,
};

/// A text as one cell of a Markdown table: one line, `|` escaped.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// `docs/cli.md` for `commands`: the rules, the global flags, the exit codes and every command
/// [`is_served`] keeps, with its flags.
pub fn markdown(commands: &[Command]) -> String {
    let mut out = String::from(
        "# hkctl\n\n\
         <!-- Generated from the protos: `cargo test -p hostkeeper-hkctl --test docs` fails when \
         this file differs, and writes the current text beside its test binary. Do not edit by \
         hand. -->\n\n\
         `hkctl` is the command line of hostkeeper's coordinator: one bash script, generated from \
         the protobuf services of `hostkeeper-proto`, that calls one method of the coordinator's \
         JSON gateway ([gateway.md](gateway.md)) per command. It is released as the asset \
         `hkctl-<version>`, attested like every other asset \
         ([deploy/verify-a-release.md](deploy/verify-a-release.md)); `hkctl --version` prints \
         `hkctl <version>+<first 12 hex digits of the commit>`. Every command, flag and help text \
         below is generated from the descriptors; the host API (`hostkeeper.agent.v<n>`), whose \
         caller is the daemon, is not here.\n\n## Rules\n\n",
    );
    out.push_str(RULES);
    out.push_str(
        "\n\n## Global flags\n\n| flag | value | default | meaning |\n|---|---|---|---|\n",
    );
    for g in &GLOBALS {
        let default = if g.flag == "timeout-secs" {
            DEFAULT_TIMEOUT_SECS
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "| `--{}` | {} | {default} | {} |",
            g.flag,
            cell(g.value),
            cell(g.help)
        );
    }
    out.push_str("\n## Exit codes\n\n| code | meaning | gateway codes |\n|---|---|---|\n");
    for (code, meaning) in MEANINGS {
        let names: Vec<&str> = EXIT_CODES
            .iter()
            .filter(|(name, exit)| *exit == code && *name != "OK")
            .map(|(name, _)| *name)
            .collect();
        let _ = writeln!(out, "| {code} | {} | {} |", cell(meaning), names.join(", "));
    }
    out.push_str("\n## Commands\n");
    let mut service = "";
    for command in commands.iter().filter(|c| is_served(c)) {
        if command.service_command != service {
            service = command.service_command;
            let _ = write!(
                out,
                "\n### `{service}`\n\nThe service `{}` of the package `{}`. {}\n",
                command.service,
                command.package,
                cell(command.service_doc)
            );
        }
        let _ = write!(
            out,
            "\n#### `{PROGRAM} {} {}`\n\n`{}`\n\n{}\n",
            command.service_command, command.method_command, command.path, command.doc
        );
        let flags: Vec<String> = command
            .fields
            .iter()
            .flat_map(field_flags)
            .map(|(flag, help)| {
                let (name, value) = flag.split_once(' ').unwrap_or((flag.as_str(), ""));
                format!("| `{name}` | {} | {} |", cell(value), cell(&help))
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
            .map(|f| format!("`{}` ({})", f.name, shape(f)))
            .collect();
        if !file_only.is_empty() {
            let _ = writeln!(out, "\nThrough `--request` only: {}.", file_only.join(", "));
        }
    }
    // No line ends in a space, which an editor would strip.
    out.lines().map(|l| format!("{}\n", l.trim_end())).collect()
}
