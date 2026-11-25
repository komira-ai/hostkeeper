//! The help texts: of `hk`, of a service, of a method, of the ssh commands, of `settings`, of `xcode-push`, of `xcode verify` and of `tailnet-wait`. All of it is
//! written from the command table and says `hk`, whatever name the program is called by.

use std::fmt::Write as _;

use hostkeeper_proto::ctl::{Command, Input};

use crate::table::{GLOBALS, MEANINGS, field_flags, line, shape};
use crate::{PROGRAM, hand};

/// How `hk` turns a request field into input, said once in `--help` and `docs/cli.md`.
pub(crate) const RULES: &str = "\
Every command is one method: `hk <service> <method>`, both in kebab case as the protos name \
them (`Releases.SetDesired` is `releases set-desired`), called as one POST of JSON to the \
coordinator's gateway over HTTPS. hk is one program: it needs no bash, no curl and no jq, and \
runs no other program for a gateway call. It puts `hk:` before its diagnostics, whatever file \
name it is run as.

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
ssh-known-host` is run by ssh itself, as the `KnownHostsCommand` of `hk ssh`, and not by hand.

`hk xcode-push <version> | <archive.xip>` is not a gateway call either: on macOS only, it brings \
an Xcode release into the coordinator's Xcode store. Given a version, it downloads the archive \
with `xcodes download <version>` in the operator's own session (a two-factor prompt is answered \
at the terminal); given a file, it takes the file. Apple's signature must be on it \
(`pkgutil --check-signature`: exit 0, `signed Apple Software`, a chain ending at Apple Root CA), \
its version and build are read from the app `xip --expand` makes, and it is uploaded unmodified \
and streamed, `PUT /v1/xcode/<sha256>?version=<version>&build=<build>`; a step that fails \
uploads nothing. It needs --endpoint, --ca-file and --token-file, and takes no other \
credential; `hk xcode-push --help` says each step.

`hk xcode verify [--apple-root FILE] <archive.xip>` is a method of the service `xcode` that is \
not a gateway call: on macOS only, it runs every check of the Mac proof's operator steps on an \
Xcode archive (the programs, the archive's kind, Apple's signature and published root, a damaged \
copy and a file that is no archive, the table of contents, and the expansion with its version \
and build), writes the record of what it found to `$HOME/.hostkeeper/xcode/<sha256>.json` and \
to stdout, and exits 0 when its verdict is pass and 1 otherwise. It asks no coordinator; \
`hk xcode verify --help` says each check.

`hk settings init --endpoint URL --ca-file PATH --token-file PATH` writes the settings file \
`$HOME/.hostkeeper/hk.toml`, which gives `--endpoint`, `--ca-file` and `--token-file` their \
default for every command that takes them (a call, the ssh commands and `xcode-push`; not \
`tailnet-wait`, which takes no token). A flag on the command line wins over the file; a flag \
that neither gives is missing, as without a file. The file is a TOML table of the strings \
`endpoint`, `ca_file` and `token_file`: an address and two paths, never a token. A key of \
another name is ignored, with `hk: <path>: unknown key <name> ignored` on stderr. hk refuses a \
file, or a `$HOME/.hostkeeper`, that is a link, that another user owns or that its group or \
others may write, naming the path; `--token-stdin` and `--github-oidc-audience` leave the \
file's token file unused. `hk settings --help` says what init does.

`hk tailnet-wait [--attempts N] [--interval-secs S]` is not a gateway call either: run by a CI \
job right after it joined the tailnet, it asks the coordinator's `GET /v1/version`, with no \
token, up to N times (6 by default) with S seconds between two tries (10 by default), until \
any HTTP answer comes; when none does, it runs `tailscale down`, then `tailscale up`, and asks \
as many times again. It ends 0 at the first answer, with one line saying which try it was, and \
1 when neither round had one, saying how each ended. It needs --endpoint and --ca-file, takes \
--timeout-secs for each try, and takes no token; `hk tailnet-wait --help` says each step.";

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
    let service = first.service_command;
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
    for m in hand::of(service) {
        let _ = writeln!(text, "  {}\n      {}", m.method, line(m.doc));
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
last required (or given by the settings file), and are built into hk. The token is read from the file; --token-stdin,
--github-oidc-audience and --response-file are not taken.";

/// The help of `hk tailnet-wait`.
pub(crate) const TAILNET_WAIT_HELP: &str = "\
hk tailnet-wait [--attempts N] [--interval-secs S]
    Waits, right after a CI runner joined the tailnet, until the coordinator answers at
    --endpoint, and rejoins the tailnet once when it does not. In order:
    1. Round 1: up to N requests GET <endpoint>/v1/version, with no token, trusting --ca-file
       alone, each limited by --timeout-secs, S seconds apart. Any HTTP status is an answer. A
       request with none prints one line on stderr saying why.
    2. When round 1 had no answer: tailscale down, then tailscale up with no flag (the settings
       of the join), each stopped after 120 seconds. The join must have made this user the
       node's operator (tailscale up --operator=<user>), so neither needs root.
    3. Round 2: as round 1.
    It exits 0 at the first answer, printing one line on stdout that says which attempt of
    which round it was; and 1 when neither round had one, or the rejoin failed, with one line
    that says how each ended.

--attempts N is 1 to 100, 6 by default; --interval-secs S is 0 to 600, 10 by default. It takes
the global flags --endpoint and --ca-file, both required, and --timeout-secs. It takes no token:
--token-file, --token-stdin, --github-oidc-audience and --response-file are not taken.";

/// The help of `hk settings`.
pub(crate) const SETTINGS_HELP: &str = "\
hk settings init --endpoint URL --ca-file PATH --token-file PATH
    Writes $HOME/.hostkeeper/hk.toml, the defaults of --endpoint, --ca-file and --token-file for
    every command that takes them. In order:
    1. The three flags are all required; the two paths must be absolute, since the file keeps
       them. $HOME/.hostkeeper and $HOME/.hostkeeper/xcode are made with mode 0700 when they
       are missing, whatever the umask; one that exists is checked (not a link, the user's own,
       not writable by group or others) and keeps its mode.
    2. A file that is there already is not replaced: hk says so and exits 2.
    3. One Xcode.List call is made with the three values, checked as any call checks them (the
       endpoint, the CA file, a token file only its owner may read). Nothing is written when it
       fails: its refusal or error is said with the path that was not written.
    4. $HOME/.hostkeeper/hk.toml is made with mode 0600, and one line on stdout names it.

The file is read by every command that needs a flag it gives; see `hk --help`. It takes the
global flags --endpoint, --ca-file, --token-file and --timeout-secs. --token-stdin,
--github-oidc-audience and --response-file are not taken.";

/// The help of `hk xcode-push`.
pub(crate) const XCODE_PUSH_HELP: &str = "\
hk xcode-push <version> | <archive.xip>
    Brings an Xcode release into the coordinator's Xcode store, from the operator's own Mac
    signed in to the Apple developer account; macOS only. In order, and nothing is uploaded when
    a step fails:
    1. Given a version (27.2): xcodes download <version> --directory
       $HOME/Downloads/hk-xcode-push/<version>, in the operator's own session, with this
       terminal (a two-factor prompt is answered here); the directory must then hold one .xip.
       Given a path: that file.
    2. pkgutil --check-signature <archive> must exit 0, with the status signed Apple Software
       and a chain ending at Apple Root CA (by its SHA-256 fingerprint).
    3. The archive's SHA-256 and size; then its version (CFBundleShortVersionString) and
       build (ProductBuildVersion), read with plutil from the app that xip --expand makes in
       .hk-xcode-push-expansion beside the archive, which is removed before and afterwards.
       Given a version, the archive's must be it.
    4. PUT /v1/xcode/<sha256>?version=<version>&build=<build>: the archive unmodified and
       streamed. The answer, the archive stored, is printed; a refusal prints
       refused: <code> and exits 3 (401, 403), 2 (other 4xx) or 1. The answer is waited for
       30 minutes at least once the archive is sent.

It takes the global flags --endpoint, --ca-file, --token-file and --timeout-secs, all but the
last required (or given by the settings file). The token is read from its file into memory and sent only as the request's
bearer: never in an argument, the environment of a program it runs, or an output line.
--token-stdin, --github-oidc-audience and --response-file are not taken.";

/// The help of `hk xcode verify`.
pub(crate) const XCODE_VERIFY_HELP: &str = "\
hk xcode verify [--apple-root FILE] <archive.xip>
    Checks an Xcode archive on the operator's own Mac, as the operator's user; macOS only. It
    reads the archive and makes nothing but a scratch directory .hk-xcode-verify-scratch beside
    it (removed afterwards) and its record; it asks no coordinator. In order, ending at the first
    check that fails (the parts after it are null in the record):
    1. The programs: sw_vers and uname -sm; each program's path must be a file (pkgutil, xip,
       plutil, codesign, spctl, xar, sw_vers, uname, cp, df); xcodes version; the first lines
       of xip --help; which of archiveutil, aa and aea exist.
    2. The archive's kind, from its first four bytes: xar! (a .xip) goes on; AA01 (Apple
       Archive) and AEA1 (Apple Encrypted Archive) fail.
    3. pkgutil --check-signature must exit 0, say signed Apple Software and name a chain ending
       at Apple Root CA by its SHA-256 fingerprint. Apple's published root certificate, from
       --apple-root or fetched over HTTPS from https://www.apple.com/appleca/ and trusted by the
       system's roots, must have the SHA-256 fingerprint the program holds; its fingerprint and
       subject are recorded.
    4. A copy-on-write clone of the archive (cp -c) with the byte at its middle changed, and a
       16-byte file that is no archive: pkgutil --check-signature and xip --expand are run on
       each and recorded; xip --expand must exit non-zero on both (pkgutil's answer is only
       recorded). A volume that cannot clone records `not run, the volume cannot clone` for the
       clone and does not fail. A package of the Command Line Tools is checked by pkgutil too,
       when there is one, for the record.
    5. For the record only: xip --list, xar -tf, xar --dump-toc and the Metadata member.
    6. The expansion: xip --expand in the scratch directory, with the seconds it took (recorded,
       never judged); exactly one .app; its version (CFBundleShortVersionString) and build
       (ProductBuildVersion) read with plutil; codesign --verify --deep --strict must exit 0;
       spctl --assess is recorded.
    Before each expansion the volume of the archive must have three times the archive's size
    free (df -Pk), else the check fails with free_space_short.

The verdict is pass, or fail: <the first check that failed>. The record, JSON with the key
\"record\": \"hk-xcode-verify-v1\", is written to $HOME/.hostkeeper/xcode/<sha256>.json (mode 0600)
whatever the verdict, replacing the file that is there, and printed on stdout. The exit is 0 on
pass and 1 otherwise.

--apple-root FILE is Apple's root certificate (DER or PEM). The global flag --timeout-secs limits
the fetch of that certificate; the other global flags are not used.";
