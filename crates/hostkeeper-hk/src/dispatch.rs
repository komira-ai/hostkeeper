//! A command line at its three levels: `hk [global flags] <service> [global flags] <method>
//! [flags]`. Global flags are taken at each; the service and the method are words of the command
//! table; `--help` at each level prints that level's help; and the commands that are not
//! gateway calls (`ssh`, `ssh-proxy`, `ssh-revoke`, `ssh-status`, `ssh-known-host`) are handed
//! to the ssh module after the checks of their global flags, and `xcode-push`, `settings` and
//! `tailnet-wait` to their modules. The verbs that use `--endpoint`, `--ca-file` and `--token-file`
//! take what they lack from the settings file first ([`crate::settings::fill`]).

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;

use hostkeeper_proto::ctl::Command;

use crate::args::{Given, Globals, flag_of, value_of};
use crate::call::{call, is_endpoint};
use crate::help::{SSH_HELP, command_help, service_help, top_help};
use crate::ssh::{self, Coordinator, Verb};
use crate::table::is_served;
use crate::{Flow, Stop, World};

/// Writes `text` and a newline to stdout, for a help or a version: a call that succeeded.
fn print(w: &mut World<'_>, text: &str) -> Flow<()> {
    // An output that cannot be written to is the caller's to see; the answer was asked for.
    let _ = writeln!(w.stdout, "{text}");
    let _ = w.stdout.flush();
    Ok(())
}

/// Writes `text` and a newline to stderr and ends with exit code 2: what a command line with
/// nothing after its prefix gets.
fn print_error(w: &mut World<'_>, text: &str) -> Flow<()> {
    let _ = writeln!(w.stderr, "{text}");
    Err(Stop::Exit(2))
}

/// The served commands of the service `service`, in the table's order.
fn methods_of<'t>(table: &'t [Command], service: &[u8]) -> Vec<&'t Command> {
    table
        .iter()
        .filter(|c| is_served(c) && c.service_command.as_bytes() == service)
        .collect()
}

/// Runs a command line (the arguments after the program's name).
pub(crate) fn dispatch(w: &mut World<'_>, argv: &[Vec<u8>]) -> Flow<()> {
    let mut globals = Globals::default();
    let mut at = 0;
    while let Some(arg) = argv.get(at) {
        match arg.as_slice() {
            b"--help" | b"-h" => {
                let served: Vec<&Command> = w.table.iter().filter(|c| is_served(c)).collect();
                let text = top_help(&served, w.stamp);
                return print(w, &text);
            }
            b"--version" => {
                let text = format!("{} {}", crate::PROGRAM, w.stamp);
                return print(w, &text);
            }
            flag if flag.starts_with(b"-") => at += take_global(&mut globals, argv.get(at..))?,
            _ => break,
        }
    }
    let Some(service) = argv.get(at) else {
        let served: Vec<&Command> = w.table.iter().filter(|c| is_served(c)).collect();
        let text = top_help(&served, w.stamp);
        return print_error(w, &text);
    };
    at += 1;
    let tail = argv.get(at..).unwrap_or_default();
    match service.as_slice() {
        b"ssh-proxy" | b"ssh-revoke" | b"ssh-status" => return ssh_main(w, globals, service, tail),
        b"xcode-push" => return crate::xcode_push::xcode_push(w, globals, tail),
        b"settings" => return crate::settings::settings(w, globals, tail),
        b"tailnet-wait" => return crate::tailnet_wait::tailnet_wait(w, globals, tail),
        b"ssh-known-host" => {
            if tail.is_empty() {
                return Err(Stop::usage("ssh-known-host needs <pins-file> %H %t %K %f"));
            }
            let args: Vec<OsString> = tail.iter().cloned().map(OsString::from_vec).collect();
            return exit_with(ssh::known_host(&args, w.stdout));
        }
        _ => {}
    }
    let methods = methods_of(w.table, service);
    let Some(&first) = methods.first() else {
        return Err(Stop::usage(
            [
                b"no service ".as_slice(),
                service,
                b"; '",
                crate::PROGRAM.as_bytes(),
                b" --help' lists them",
            ]
            .concat(),
        ));
    };
    let mut at = 0;
    while let Some(arg) = tail.get(at) {
        match arg.as_slice() {
            b"--help" | b"-h" => return print(w, &service_help(first, &methods)),
            flag if flag.starts_with(b"-") => at += take_global(&mut globals, tail.get(at..))?,
            _ => break,
        }
    }
    let Some(method) = tail.get(at) else {
        return print_error(w, &service_help(first, &methods));
    };
    let after = tail.get(at + 1..).unwrap_or_default();
    let found = methods
        .iter()
        .find(|c| c.method_command.as_bytes() == method.as_slice());
    let Some(command) = found else {
        if service == b"ssh" {
            // Not a method of the service `ssh`: a host id.
            return ssh_main(w, globals, service, tail.get(at..).unwrap_or_default());
        }
        return Err(Stop::usage(
            [
                b"the service ".as_slice(),
                service,
                b" has no method ",
                method,
                b"; '",
                crate::PROGRAM.as_bytes(),
                b" ",
                service,
                b" --help' lists them",
            ]
            .concat(),
        ));
    };
    run_method(w, globals, command, after)
}

/// Takes the global flag that starts `args`: how many arguments it took, or the usage error that
/// the flag is none.
fn take_global(globals: &mut Globals, args: Option<&[Vec<u8>]>) -> Flow<usize> {
    let args = args.unwrap_or_default();
    match globals.take(args)? {
        Some(took) => Ok(took),
        None => Err(Stop::usage(
            [
                b"unknown flag ".as_slice(),
                args.first().map(|a| flag_of(a)).unwrap_or_default(),
            ]
            .concat(),
        )),
    }
}

/// An exit code as the end of the run.
fn exit_with(code: u8) -> Flow<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Stop::Exit(code))
    }
}

/// The flags of the method `command`, then its call.
fn run_method(
    w: &mut World<'_>,
    mut globals: Globals,
    command: &Command,
    args: &[Vec<u8>],
) -> Flow<()> {
    let mut given = Given::new(command.fields);
    let mut request: Option<Vec<u8>> = None;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        let flag = flag_of(arg);
        let rest = args.get(at..).unwrap_or_default();
        match flag {
            b"--help" | b"-h" => return print(w, &command_help(command)),
            b"--request" => {
                let (value, took) = value_of(rest)?;
                if request.is_some() {
                    return Err(Stop::usage("--request is given twice"));
                }
                request = Some(value.to_vec());
                if value == b"-" {
                    globals.claim_stdin(flag)?;
                }
                at += took;
                continue;
            }
            flag if !flag.starts_with(b"--") => {
                return Err(Stop::usage(format!(
                    "{} {} takes no argument; it has flags only",
                    command.service_command, command.method_command
                )));
            }
            _ => {}
        }
        if let Some(took) = given.take(&mut globals, rest)? {
            at += took;
        } else if let Some(took) = globals.take(rest)? {
            at += took;
        } else {
            return Err(Stop::usage(
                [
                    format!(
                        "{} {} has no flag ",
                        command.service_command, command.method_command
                    )
                    .as_bytes(),
                    flag,
                ]
                .concat(),
            ));
        }
    }
    crate::settings::fill(w, &mut globals)?;
    call(w, &globals, command, &given, request.as_deref())
}

/// An ssh command: the global flags among `args` (until a `--`) are taken here and the rest goes
/// to the ssh module, which parses the flags of its own.
fn ssh_main(w: &mut World<'_>, mut globals: Globals, word: &[u8], args: &[Vec<u8>]) -> Flow<()> {
    let mut rest: Vec<&[u8]> = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        match arg.as_slice() {
            b"--" => break,
            b"--help" | b"-h" => return print(w, SSH_HELP),
            _ => {}
        }
        if let Some(took) = globals.take(args.get(at..).unwrap_or_default())? {
            at += took;
        } else {
            rest.push(arg);
            at += 1;
        }
    }
    let name = crate::PROGRAM;
    let sub = String::from_utf8_lossy(word).into_owned();
    if globals.token_stdin || globals.audience.is_some() || globals.response_file.is_some() {
        return Err(Stop::usage(format!(
            "{name} {sub} takes --token-file only: --token-stdin, --github-oidc-audience and \
             --response-file are not for it"
        )));
    }
    crate::settings::fill(w, &mut globals)?;
    let (Some(endpoint), Some(ca_file), Some(token_file)) =
        (&globals.endpoint, &globals.ca_file, &globals.token_file)
    else {
        return Err(Stop::usage(format!(
            "{name} {sub} needs --endpoint, --ca-file and --token-file"
        )));
    };
    if !is_endpoint(endpoint) {
        return Err(Stop::usage("--endpoint takes https://<host>[:<port>]"));
    }
    let home = match &w.home {
        Some(home) if !home.as_os_str().is_empty() => home.clone(),
        _ => {
            return Err(Stop::fail(format!(
                "{name} {sub} keeps its key and certificates under $HOME, which is not set"
            )));
        }
    };
    let verb = match word {
        b"ssh-proxy" => Verb::Proxy,
        b"ssh-revoke" => Verb::Revoke,
        b"ssh-status" => Verb::Status,
        _ => Verb::Ssh,
    };
    let coordinator = Coordinator {
        endpoint: String::from_utf8_lossy(endpoint.strip_suffix(b"/").unwrap_or(endpoint))
            .into_owned(),
        ca_file: PathBuf::from(OsString::from_vec(ca_file.clone())),
        token_file: PathBuf::from(OsString::from_vec(token_file.clone())),
        timeout_secs: u64::from(globals.timeout_secs()),
    };
    let tail = args.get(at..).unwrap_or_default();
    let forwarded: Vec<OsString> = rest
        .into_iter()
        .chain(tail.iter().map(Vec::as_slice))
        .map(|arg| OsString::from_vec(arg.to_vec()))
        .collect();
    exit_with(
        w.ssh
            .launch(verb, coordinator, forwarded, name, &home, w.stderr),
    )
}
