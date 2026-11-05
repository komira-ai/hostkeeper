//! hello: the smallest real service a hostkeeper package carries.
//!
//! It is an HTTP server on loopback (`/readyz`, `/version`) and the update hook that installs,
//! updates and rolls it back through two alternating slots (`a` and `b`) of one unit template.
//! Everything outside the standard library is a dev-dependency: the contract crate the tests
//! check the hook's output against, the scratch helper and the manifest loader.
//!
//! The program is [`run`] over its ports ([`ports::Host`]): the real host runs `systemctl` or
//! `launchctl` and probes over a socket, the tests hand in fakes. [`main`] only wires the real
//! host to the process's arguments and streams.

use std::ffi::OsString;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::process::ExitCode;

pub mod cli;
pub mod hook;
pub mod params;
pub mod ports;
pub mod record;
pub mod report;
pub mod server;

use cli::Command;
use ports::{Host, HttpProbe, ProcessRunner};

/// The version this binary reports: the release version `cargo xtask release set-version` wrote.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit status of a command line the program cannot parse.
const EXIT_USAGE: i32 = 2;
/// Exit status of a server that could not start or stopped.
const EXIT_SERVER: i32 = 1;

/// Binds `127.0.0.1:<port>`: the listener of the real server. `0` asks the system for a free port.
pub fn bind_loopback(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

/// The whole program over its ports. `args` exclude the program name (`argv[0]`). The real
/// program is [`run_with`] over [`bind_loopback`].
pub fn run(
    args: &[OsString],
    host: &Host<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    run_with(args, host, out, err, &bind_loopback)
}

/// [`run`] with the way the server gets its listener handed in, so that a test serves on a
/// listener it already holds instead of a port it would have to guess.
pub fn run_with(
    args: &[OsString],
    host: &Host<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    listen: &dyn Fn(u16) -> std::io::Result<TcpListener>,
) -> i32 {
    match cli::parse(args) {
        Err(why) => {
            let _ = writeln!(err, "hello: {why}");
            EXIT_USAGE
        }
        Ok(Command::Version) => {
            let _ = writeln!(out, "hello {VERSION}");
            0
        }
        Ok(Command::Hook(rest)) => hook::run(&rest, host, out, err),
        Ok(Command::Serve { port, fail_ready }) => match listen(port) {
            Err(why) => {
                let _ = writeln!(err, "hello: cannot listen on 127.0.0.1:{port}: {why}");
                EXIT_SERVER
            }
            Ok(listener) => {
                let _ = writeln!(err, "hello {VERSION}: serving on 127.0.0.1:{port}");
                let answer = server::Answer {
                    version: VERSION,
                    fail_ready,
                };
                let why = server::serve_forever(listener, &answer);
                let _ = writeln!(err, "hello: stopped serving: {why}");
                EXIT_SERVER
            }
        },
    }
}

/// The program: [`run`] over the real host, the process's arguments and its streams.
pub fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let host = Host {
        runner: &ProcessRunner,
        probe: &HttpProbe,
    };
    let code = run(
        &args,
        &host,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    // The codes are 0, 1, 2 and 75: all fit a byte.
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
