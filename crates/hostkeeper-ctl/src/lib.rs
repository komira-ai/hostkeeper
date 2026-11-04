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
//! tokio runtime of a gRPC call is started inside the call.
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
    let matches = match tree::build(commands).try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(e) if e.use_stderr() => {
            let _ = write!(io.stderr, "{e}");
            return exit::USAGE;
        }
        Err(e) => {
            // `--help`: the help is the output.
            return match write!(io.stdout, "{e}") {
                Ok(()) => exit::SUCCESS,
                Err(_) => exit::FAILURE,
            };
        }
    };
    let found = commands.iter().find_map(|c| {
        matches
            .subcommand_matches(c.service_command)
            .and_then(|s| s.subcommand_matches(c.method_command))
            .map(|leaf| (c, leaf))
    });
    let result = match found {
        Some((command, leaf)) => execute(
            command,
            leaf,
            descriptors,
            connect,
            &mut *io.stdin,
            &mut *io.stdout,
            &mut *io.stderr,
        ),
        None => Err(Failure::Usage(
            "name a service and one of its RPCs".to_owned(),
        )),
    };
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

/// The bearer token, from `--token-file` or `--token-stdin`, as `authorization` metadata.
fn metadata(leaf: &ArgMatches, stdin: &mut dyn Read) -> Result<MetadataMap, Failure> {
    let mut metadata = MetadataMap::new();
    let token = if let Some(path) = leaf.get_one::<PathBuf>("token-file") {
        request::read_input(Some(path), stdin, "the token")?
    } else if leaf.get_flag("token-stdin") {
        request::read_input(None, stdin, "the token")?
    } else {
        return Ok(metadata);
    };
    let usable = !token.is_empty() && token.iter().all(|b| b.is_ascii_graphic());
    let value = format!("Bearer {}", String::from_utf8_lossy(&token));
    match MetadataValue::try_from(value.as_str()) {
        Ok(mut value) if usable => {
            value.set_sensitive(true);
            metadata.insert("authorization", value);
            Ok(metadata)
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
    let metadata = metadata(leaf, stdin)?;
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
        .map_err(Failure::Status)?;
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
mod tests;
#[cfg(test)]
mod testutil;
