//! `hostkeeper ctl`: the command line generated from hostkeeper's protobuf services.
//!
//! `hostkeeper_proto::ctl::COMMANDS`, which `hostkeeper-proto`'s build script writes from the
//! descriptors, lists every RPC; this crate turns that table into a clap command tree
//! (`hostkeeper ctl <service> <rpc> [flags]`), turns the parsed flags and the request file into
//! a dynamic message, calls the RPC through a [`CtlTransport`], and prints the response. Nothing
//! here names an RPC: a new RPC in a proto is a new command at the next build.
//!
//! [`run`] is the whole program: arguments in, an exit code out ([`exit`]). It reads the network
//! only to call: `--help` and every usage error are answered before a transport is made, and the
//! tokio runtime of a gRPC call is started inside the call. [`parse_with`] is that first read
//! alone: whether a command line names an RPC with flags it takes, with nothing called, so that
//! a test can check the command lines a document shows.
//!
//! Transports ([`transport`]): gRPC over TLS ([`transport::Tonic`]), the HTTP/JSON gateway of
//! the coordinator's private listener ([`transport::Gateway`]), and an in-memory one that calls a
//! tonic service in-process with an explicit peer identity, for tests ([`transport::InMemory`]).

pub mod codes;
pub mod doc;
pub mod exit;
mod json;
mod output;
mod request;
mod secret;
pub mod transport;
mod tree;

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::ArgMatches;
use hostkeeper_proto::ctl::{COMMANDS, Command};
use prost_reflect::{DescriptorPool, DynamicMessage};
use tonic::Status;
use tonic::metadata::{MetadataMap, MetadataValue};

pub use transport::{Connect, CtlTransport, Endpoint, PeerIdentity, Target, TransportKind};

/// The streams `ctl` reads and writes.
pub struct Io<'a> {
    pub stdin: &'a mut dyn Read,
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
}

/// Why a call did not succeed, and so what `ctl` prints and exits with.
#[derive(Debug)]
enum Failure {
    /// The arguments, or a file they name, cannot make a call: exit 2.
    Usage(String),
    /// The server, or the transport, answered with a status.
    Status(Status),
    /// Anything else: exit 1.
    Local(String),
}

/// Runs `hostkeeper ctl` with `args`, of which the first is the program's own name, and returns
/// the exit code. `connect` makes the transport of the call from the global flags.
pub fn run<I, T>(args: I, connect: &dyn Connect, io: Io<'_>) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    run_with(
        COMMANDS,
        hostkeeper_proto::FILE_DESCRIPTOR_SET,
        args,
        connect,
        io,
    )
}

/// [`run`], with the command table and the encoded descriptor set it was generated from given.
pub fn run_with<I, T>(
    commands: &[Command],
    descriptors: &[u8],
    args: I,
    connect: &dyn Connect,
    io: Io<'_>,
) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let (command, leaf) = match read_args(commands, args) {
        Ok(ReadArgs::Call(command, leaf)) => (command, leaf),
        Ok(ReadArgs::Help(help)) => {
            // `--help`: the help is the output.
            return match write!(io.stdout, "{help}") {
                Ok(()) => exit::SUCCESS,
                Err(_) => exit::FAILURE,
            };
        }
        Err(usage) => {
            let _ = write!(io.stderr, "{}", usage.0);
            return exit::USAGE;
        }
    };
    let result = execute(
        command,
        &leaf,
        descriptors,
        connect,
        &mut *io.stdin,
        &mut *io.stdout,
        &mut *io.stderr,
    );
    match result {
        Ok(()) => exit::SUCCESS,
        Err(Failure::Usage(message)) => {
            let _ = writeln!(io.stderr, "error: {message}");
            exit::USAGE
        }
        Err(Failure::Local(message)) => {
            let _ = writeln!(io.stderr, "error: {message}");
            exit::FAILURE
        }
        Err(Failure::Status(status)) => {
            let code = exit::code(status.code());
            let message = escape(status.message());
            if code == exit::FAILURE {
                let _ = writeln!(
                    io.stderr,
                    "error: {}: {message}",
                    codes::name(status.code())
                );
            } else {
                let _ = writeln!(io.stderr, "refused: {message}");
            }
            code
        }
    }
}

/// What a command line of `hostkeeper ctl` asks for, as [`parse_with`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// `--help` of `ctl`, of a service or of an RPC: `ctl` prints the help and calls nothing.
    Help,
    /// The RPC the line calls.
    Call(&'a Command),
}

/// Why a command line of `hostkeeper ctl` cannot make a call: clap's message, as `ctl` prints
/// it on stderr before it exits 2, with every credential typed on the line replaced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(String);

/// Reads a command line of `hostkeeper ctl` against the command tree of `commands`, as
/// [`run_with`] reads it before anything else, and stops there: it reads no file, no stdin and
/// no network, and needs no `--endpoint`, `--ca-file` or token, which only a call reads. clap
/// checks the commands, the flags and the values of a bool, an enum, `--transport`, `--output`
/// and `--timeout-secs`; the text of a number flag is read, and refused, only when the request
/// is built. `args` are as [`run`] takes them, the first the program's own name.
pub fn parse_with<'a, I, T>(commands: &'a [Command], args: I) -> Result<Parsed<'a>, UsageError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    match read_args(commands, args)? {
        ReadArgs::Help(_) => Ok(Parsed::Help),
        ReadArgs::Call(command, _) => Ok(Parsed::Call(command)),
    }
}

/// What the arguments ask for once clap has read them.
enum ReadArgs<'a> {
    /// `--help`: the help text.
    Help(String),
    /// The RPC called, and the matches of its subcommand.
    Call(&'a Command, ArgMatches),
}

/// The arguments read against the tree of `commands`, or the usage error to print.
fn read_args<'a, I, T>(commands: &'a [Command], args: I) -> Result<ReadArgs<'a>, UsageError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = tree::build(commands);
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    // clap repeats what it cannot parse in its error; a credential typed in the wrong place must
    // not be repeated with it.
    let typed: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let credentials = secret::argument_credentials(&typed, &secret::known_flags(&cli));
    let matches = match cli.try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(e) if e.use_stderr() => {
            return Err(UsageError(secret::scrub(&e.to_string(), &credentials)));
        }
        Err(e) => return Ok(ReadArgs::Help(e.to_string())),
    };
    // clap requires a service and an RPC, so a leaf is always found; the error is never made.
    commands
        .iter()
        .find_map(|c| {
            matches
                .subcommand_matches(c.service_command)
                .and_then(|s| s.subcommand_matches(c.method_command))
                .map(|leaf| ReadArgs::Call(c, leaf.clone()))
        })
        .ok_or_else(|| UsageError("error: name a service and one of its RPCs\n".to_owned()))
}

/// A server's text with every control character escaped, so that it cannot act on a terminal.
fn escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// The global flags of the call, read once a command is known to be called.
fn target(leaf: &ArgMatches) -> Result<Target, Failure> {
    let endpoint = leaf
        .get_one::<String>("endpoint")
        .ok_or_else(|| Failure::Usage("--endpoint is needed to call an RPC".to_owned()))?;
    let endpoint = Endpoint::parse(endpoint).map_err(|e| Failure::Usage(e.to_string()))?;
    let ca_file = leaf
        .get_one::<PathBuf>("ca-file")
        .cloned()
        .ok_or_else(|| Failure::Usage("--ca-file is needed to call an RPC".to_owned()))?;
    let client_identity = leaf
        .get_one::<PathBuf>("client-cert-file")
        .cloned()
        .zip(leaf.get_one::<PathBuf>("client-key-file").cloned());
    let transport = match leaf.get_one::<String>("transport").map(String::as_str) {
        Some("gateway") => TransportKind::Gateway,
        _ => TransportKind::Grpc,
    };
    let timeout = Duration::from_secs(leaf.get_one::<u64>("timeout-secs").copied().unwrap_or(30));
    Ok(Target {
        endpoint,
        transport,
        ca_file,
        client_identity,
        timeout,
    })
}

/// The bearer token, from `--token-file` or `--token-stdin`, as `authorization` metadata, and
/// the token's text (none when there is no token).
fn metadata(
    leaf: &ArgMatches,
    stdin: &mut dyn Read,
) -> Result<(MetadataMap, Option<String>), Failure> {
    let mut metadata = MetadataMap::new();
    let token = if let Some(path) = leaf.get_one::<PathBuf>("token-file") {
        request::read_input(Some(path), stdin, "the token")?
    } else if leaf.get_flag("token-stdin") {
        request::read_input(None, stdin, "the token")?
    } else {
        return Ok((metadata, None));
    };
    let usable = !token.is_empty() && token.iter().all(|b| b.is_ascii_graphic());
    let value = format!("Bearer {}", String::from_utf8_lossy(&token));
    match MetadataValue::try_from(value.as_str()) {
        Ok(mut value) if usable => {
            value.set_sensitive(true);
            metadata.insert("authorization", value);
            Ok((metadata, Some(String::from_utf8_lossy(&token).into_owned())))
        }
        _ => Err(Failure::Usage(
            "the token is empty or holds a character that is not visible ASCII".to_owned(),
        )),
    }
}

/// How many of the inputs read stdin.
fn stdin_readers(command: &Command, leaf: &ArgMatches) -> usize {
    let request = leaf.get_one::<String>("request").is_some_and(|r| r == "-");
    let token = leaf.get_flag("token-stdin");
    let secrets = command
        .fields
        .iter()
        .filter(|f| f.input == hostkeeper_proto::ctl::Input::SecretFile)
        .filter(|f| leaf.get_flag(&format!("{}-stdin", f.flag)))
        .count();
    usize::from(request) + usize::from(token) + secrets
}

/// Everything after the arguments parsed: check them, build the request, call, print.
fn execute(
    command: &Command,
    leaf: &ArgMatches,
    descriptors: &[u8],
    connect: &dyn Connect,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<(), Failure> {
    let target = target(leaf)?;
    if stdin_readers(command, leaf) > 1 {
        return Err(Failure::Usage(
            "at most one input may read stdin (--request -, --token-stdin, a secret's -stdin)"
                .to_owned(),
        ));
    }
    let pool = DescriptorPool::decode(descriptors)
        .map_err(|e| Failure::Local(format!("the descriptor set does not decode: {e}")))?;
    let request = request::build(command, leaf, &pool, stdin)?;
    let (metadata, token) = metadata(leaf, stdin)?;
    // What a server's diagnostic must not repeat: the token and every secret of the request.
    let mut credentials = request.secrets();
    credentials.extend(token);
    let response_desc = pool.get_message_by_name(command.response).ok_or_else(|| {
        Failure::Local(format!(
            "the descriptors have no message {}",
            command.response
        ))
    })?;
    let transport = connect
        .connect(&target)
        .map_err(|e| Failure::Usage(e.to_string()))?;
    let reply = transport
        .call(command.path, request.encode(), &metadata)
        .map_err(|status| {
            Failure::Status(Status::new(
                status.code(),
                secret::scrub(status.message(), &credentials),
            ))
        })?;
    let response = DynamicMessage::decode(response_desc, reply)
        .map_err(|e| Failure::Local(format!("malformed reply: {e}")))?;
    let format = match leaf.get_one::<String>("output").map(String::as_str) {
        Some("json") => output::Format::Json,
        _ => output::Format::Text,
    };
    output::write(
        &response,
        format,
        secret::extension(&pool).as_ref(),
        stdout,
        stderr,
    )
    .map_err(|e| Failure::Local(format!("cannot write the response: {e}")))
}

#[cfg(test)]
mod credential_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;
