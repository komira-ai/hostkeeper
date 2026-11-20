//! The help texts: of `hk`, of a service, of a method, and of the ssh commands. All of it is
//! written from the command table and says `hk`, whatever name the program is called by.

use std::fmt::Write as _;

use hostkeeper_proto::ctl::{Command, Input};

use crate::PROGRAM;
use crate::table::{GLOBALS, MEANINGS, field_flags, line, shape};

/// How `hk` turns a request field into input, said once in `--help` and `docs/cli.md`.
pub(crate) const RULES: &str = "\
Every command is one method: `hk <service> <method>`, both in kebab case as the protos name \
them (`Releases.SetDesired` is `releases set-desired`), called as one POST of JSON to the \
coordinator's gateway over HTTPS. hk is one program: it needs no bash, no curl and no jq, and \
runs no other program for a gateway call. `hkctl` is the old name of hk, which still answers to \
it: a program called `hkctl` behaves as `hk` does and puts `hkctl:` before its diagnostics.

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
stdin. No token or secret is ever written to a file or put in an argument list or in the \
environment of a program hk runs: it is read into memory and sent in the request.

The answer, the gateway's JSON, goes to stdout or to `--response-file`; diagnostics go to \
stderr. A refusal prints `refused: <reason>` when its reason is a token of `[a-z0-9_]{1,64}`. \
A call refused `invalid_request` makes one more call, with no token, to the coordinator's \
`GET /v1/version?at_least=<this hk's release>`: when that says the coordinator is older (`409`, \
or `404` from a coordinator before the route), hk says so on stderr, with the coordinator's \
version when it gave one, and the reason it prints is `coordinator_older`; any other answer, or \
none, leaves `invalid_request`.

Four commands are not gateway calls: the SSH tunnel is built into hk. `hk ssh <host-id> \
[--identity FILE] [--reason TEXT] [-- ssh args]` gets a one-host SSH certificate and runs ssh to \
the host through the coordinator's tunnel (a word after `ssh` that is not a method of the \
service `ssh` is a host id). `hk ssh-proxy <host-id>` is the byte stream for `ProxyCommand hk \
--endpoint URL --ca-file PATH --token-file PATH ssh-proxy %h` in `~/.ssh/config`. `hk \
ssh-revoke --operator NAME | --serial N | --host <host-id>` revokes certificates. `hk \
ssh-status <host-id>` shows the host keys pinned for a host and the certificate held. They need \
--endpoint, --ca-file and --token-file, take no other credential, keep the key, the \
certificates and the pins in `$HOME/.hostkeeper/ssh`, and read the token from its file. `hk \
ssh-known-host` is run by ssh itself, as the `KnownHostsCommand` of `hk ssh`, and not by hand.";

/// The top-level help; `stamp` is the version, `<version>+<12 hex of the commit>` for a release.
pub(crate) fn top_help(commands: &[&Command], stamp: &str) -> String {
    let mut text = format!(
        "{PROGRAM} {stamp}: call one method of hostkeeper's coordinator through its JSON gateway.\n\n\
         Usage: {PROGRAM} [global flags] <service> <method> [flags]\n\n{RULES}\n\nGlobal flags:\n"
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
    let _ = write!(
        text,
        "\nRun '{PROGRAM} <service> --help' for its methods, and '{PROGRAM} <service> <method> \
         --help' for a method's flags."
    );
    text
}

/// The help of the service of `first`, whose methods are `methods` (`first` is the first).
pub(crate) fn service_help(first: &Command, methods: &[&Command]) -> String {
    let mut text = format!(
        "{PROGRAM} {}: the service `{}` of the package `{}`. {}\n\n\
         Usage: {PROGRAM} [global flags] {} <method> [flags]\n\nMethods:\n",
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
pub(crate) fn command_help(command: &Command) -> String {
    let mut text = format!(
        "{PROGRAM} {} {}: {}\n\n{}\n\nUsage: {PROGRAM} [global flags] {} {} [flags]\n\nFlags:\n  \
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
    let _ = write!(
        text,
        "\nRun '{PROGRAM} --help' for the global flags and the exit codes."
    );
    text
}

/// The help of the commands the SSH tunnel is built into.
pub(crate) const SSH_HELP: &str = "\
hk ssh <host-id> [flags] [-- ssh args]
    Gets a one-host SSH certificate from the coordinator, for the key of --identity, the ssh
    agent's first key, or a key kept in $HOME/.hostkeeper/ssh, and runs ssh to the host through
    the coordinator's tunnel. Flags of ssh: --identity FILE, --reason TEXT.
hk ssh-proxy <host-id>
    The byte stream to the host's port 22, for ssh's ProxyCommand:
    ProxyCommand hk --endpoint URL --ca-file PATH --token-file PATH ssh-proxy %h
hk ssh-revoke --operator NAME | --serial N | --host <host-id>
    Revokes certificates: an operator's, one by serial, or the one held for a host.
hk ssh-status <host-id>
    The host keys pinned for the host, and the certificate held for it.
hk ssh-known-host <pins-file> %H %t %K %f
    Run by ssh itself, as the KnownHostsCommand of hk ssh: says whether a host key is pinned.

These take the global flags --endpoint, --ca-file, --token-file and --timeout-secs, all but the
last required, and are built into hk. The token is read from the file; --token-stdin,
--github-oidc-audience and --response-file are not taken.";
