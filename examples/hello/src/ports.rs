//! What the hook runs through, so that its tests never run a real program or open a real
//! connection: running a command, and asking the server a question.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

/// Runs a program.
pub trait Runner {
    /// Runs `argv[0]` with `argv[1..]`: `Ok(exit code)`, or `Err(why it could not run or had no
    /// code)`.
    fn run(&self, argv: &[&str]) -> Result<i32, String>;
}

/// Asks the server a question.
pub trait Probe {
    /// `GET path` from `127.0.0.1:port`: `Ok((status, body))`, or `Err(why)`.
    fn get(&self, port: u16, path: &str) -> Result<(u16, String), String>;
}

/// The two ports together: what the hook is run over.
pub struct Host<'a> {
    pub runner: &'a dyn Runner,
    pub probe: &'a dyn Probe,
}

/// The real [`Runner`]: the program as a child process with an empty environment and no standard
/// input. What it prints goes to this process's standard error (the hook's standard output is the
/// report line and nothing else).
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, argv: &[&str]) -> Result<i32, String> {
        let Some((program, args)) = argv.split_first() else {
            return Err("no program to run".to_owned());
        };
        let output = Command::new(program)
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .output()
            .map_err(|why| format!("cannot run {program}: {why}"))?;
        let mut log = std::io::stderr().lock();
        let _ = log.write_all(&output.stdout);
        let _ = log.write_all(&output.stderr);
        exit_code(program, output.status)
    }
}

/// The exit code of a program that ended, or why it had none (a signal ended it).
fn exit_code(program: &str, status: ExitStatus) -> Result<i32, String> {
    status
        .code()
        .ok_or_else(|| format!("{program} ended without an exit code: {status}"))
}

/// How long the real probe waits to connect and to read: a bound for a wedged server, never a
/// measured quantity.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// The most bytes of an answer the probe keeps.
const PROBE_MAX: u64 = 64 * 1024;

/// The real [`Probe`]: one HTTP/1.1 request over a socket to `127.0.0.1`.
pub struct HttpProbe;

impl Probe for HttpProbe {
    fn get(&self, port: u16, path: &str) -> Result<(u16, String), String> {
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
        let stream = TcpStream::connect_timeout(&address, PROBE_TIMEOUT)
            .map_err(|why| format!("cannot connect to {address}: {why}"))?;
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
        let answer = exchange(stream, request.as_bytes())
            .map_err(|why| format!("cannot talk to {address}: {why}"))?;
        parse_answer(&answer)
    }
}

/// Sends `request` and reads the answer to its end, or to [`PROBE_MAX`] bytes.
fn exchange(mut stream: TcpStream, request: &[u8]) -> io::Result<Vec<u8>> {
    // Neither fails on a connected socket with a non-zero timeout; a failure would surface as
    // the write or the read below.
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    stream.write_all(request)?;
    let mut answer = Vec::new();
    stream.take(PROBE_MAX).read_to_end(&mut answer)?;
    Ok(answer)
}

/// The status and the body of an HTTP response.
fn parse_answer(answer: &[u8]) -> Result<(u16, String), String> {
    let text = String::from_utf8_lossy(answer);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "the answer has no end of head".to_owned())?;
    let status_line = head.lines().next().unwrap_or_default();
    let status = status_line
        .strip_prefix("HTTP/1.")
        .and_then(|rest| rest.split(' ').nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("the answer has no status: {status_line:?}"))?;
    Ok((status, body.to_owned()))
}

#[cfg(test)]
mod tests;
