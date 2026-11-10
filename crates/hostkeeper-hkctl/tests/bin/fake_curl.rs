//! The program the integration tests of the emitted script (tests/hkctl_integration.rs) put
//! first on PATH, copied as `curl` and as `find` into a directory of the test. A program of the
//! tests, not shipped.
//!
//! Every run records, under `<its directory>/calls/<n>/` (`n` counting from 0 in the order of the
//! runs): `program` (`curl` or `find`), `argv` (its arguments, each ended by a NUL byte),
//! `environ` (its environment as `NAME=value`, each ended by a NUL byte, without the variables the
//! coverage runtime sets in the process itself), `read-<i>` (what it read from its argument `i`
//! when that argument is `@<path>`), and `stdout` (what the file `<its directory>/stdout` held
//! when it ran: the test sends the script's stdout there, so this shows what was printed before
//! the call).
//!
//! As `find` it then runs the first other `find` on PATH with its arguments. As `curl`:
//! - when `<its directory>/fail` or, for the `n`th run of `curl` (from 1; `find` is not counted),
//!   `fail-<n>` exists, it prints `curl: (7) Failed to connect` on stderr and exits 7;
//! - when `<its directory>/reply-<n>` exists for the `n`th run of `curl` (from 1), or else
//!   `<its directory>/reply`, it answers its content (a status line, a line break, then the
//!   body);
//! - otherwise it sends the request over plain HTTP/1.1 to the host and port of its `--url`
//!   (a loopback listener of the test): `POST` with the `--data-binary` it read, or `GET`, and
//!   every `--header`, read or given; and answers what came back.
//!
//! An answer is the body on stdout, then the `--write-out` text with `\n` a line break and
//! `%{http_code}` the status. It exits 0, or 7 when nothing listens.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The prefix of the variables the coverage runtime of an instrumented build sets in this
/// process itself when it starts: nothing the script passed.
const COVERAGE_RUNTIME: &[u8] = b"__LLVM_PROFILE_RT_";

#[expect(
    clippy::disallowed_methods,
    reason = "the tests read what environment reached curl and find; a program of the tests"
)]
fn environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| !name.as_bytes().starts_with(COVERAGE_RUNTIME))
        .collect()
}

#[expect(
    clippy::disallowed_methods,
    reason = "find is looked for on the PATH the script was given, as a shell would"
)]
fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// Each argument ended by a NUL byte.
fn nul_joined<'a>(items: impl Iterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        out.extend_from_slice(item);
        out.push(0);
    }
    out
}

/// A new directory `calls/<n>` under `dir`, the first `n` not taken.
fn call_dir(dir: &Path) -> PathBuf {
    let calls = dir.join("calls");
    let _ = std::fs::create_dir_all(&calls);
    let mut n = 0;
    loop {
        let at = calls.join(n.to_string());
        if std::fs::create_dir(&at).is_ok() {
            return at;
        }
        n += 1;
    }
}

/// The bytes of the argument after `flag`, when there is one.
fn after<'a>(args: &'a [OsString], flag: &str) -> Option<&'a OsStr> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(OsString::as_os_str)
}

/// The value of an argument: what was read from it when it is `@<path>`, else its bytes.
fn value_of(args: &[OsString], reads: &[Option<Vec<u8>>], index: usize) -> Vec<u8> {
    reads[index]
        .clone()
        .unwrap_or_else(|| args[index].as_bytes().to_vec())
}

/// The answer `body` with `status`, and the `--write-out` text, on stdout.
fn answer(args: &[OsString], status: &str, body: &[u8]) -> ExitCode {
    let mut out = std::io::stdout();
    let _ = out.write_all(body);
    if let Some(format) = after(args, "--write-out") {
        let text = String::from_utf8_lossy(format.as_bytes())
            .replace("\\n", "\n")
            .replace("%{http_code}", status);
        let _ = out.write_all(text.as_bytes());
    }
    ExitCode::SUCCESS
}

/// Sends the request the arguments describe to the URL's host and port over plain HTTP/1.1.
fn forward(args: &[OsString], reads: &[Option<Vec<u8>>]) -> ExitCode {
    let url = after(args, "--url")
        .map(|u| String::from_utf8_lossy(u.as_bytes()).into_owned())
        .unwrap_or_default();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(&url);
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let mut headers = Vec::new();
    let mut body = None;
    for (i, arg) in args.iter().enumerate() {
        if i + 1 < args.len() && arg == "--header" {
            let text = value_of(args, reads, i + 1);
            for line in String::from_utf8_lossy(&text).lines() {
                headers.push(line.trim_end_matches('\r').to_owned());
            }
        }
        if i + 1 < args.len() && arg == "--data-binary" {
            body = Some(value_of(args, reads, i + 1));
        }
    }
    let Ok(mut stream) = TcpStream::connect(authority) else {
        eprintln!("curl: (7) Failed to connect");
        return ExitCode::from(7);
    };
    let method = if body.is_some() { "POST" } else { "GET" };
    let body = body.unwrap_or_default();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\n");
    for header in headers {
        request.push_str(&header);
        request.push_str("\r\n");
    }
    request.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let mut bytes = request.into_bytes();
    bytes.extend_from_slice(&body);
    let _ = stream.write_all(&bytes);
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(response.len());
    let head = String::from_utf8_lossy(&response[..split]).into_owned();
    let status = head.split(' ').nth(1).unwrap_or("000").to_owned();
    let body = response.get(split + 4..).unwrap_or_default();
    answer(args, &status, body)
}

/// Runs the first `find` on PATH outside `dir` with `args`.
fn real_find(dir: &Path, args: &[OsString]) -> ExitCode {
    let found = path_dirs()
        .into_iter()
        .filter(|d| d != dir)
        .map(|d| d.join("find"))
        .find(|p| p.is_file());
    let Some(find) = found else {
        eprintln!("find: no find on PATH");
        return ExitCode::from(127);
    };
    match Command::new(find).args(args).status() {
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(_) => ExitCode::from(126),
    }
}

fn main() -> ExitCode {
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let program = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    let record = call_dir(&dir);
    let reads: Vec<Option<Vec<u8>>> = args
        .iter()
        .map(|a| {
            let path = a.as_bytes().strip_prefix(b"@")?;
            std::fs::read(OsStr::from_bytes(path)).ok()
        })
        .collect();
    let _ = std::fs::write(record.join("program"), &program);
    let _ = std::fs::write(
        record.join("argv"),
        nul_joined(args.iter().map(|a| a.as_bytes())),
    );
    let environ: Vec<Vec<u8>> = environment()
        .into_iter()
        .map(|(name, value)| {
            let mut entry = name.into_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            entry
        })
        .collect();
    let _ = std::fs::write(
        record.join("environ"),
        nul_joined(environ.iter().map(Vec::as_slice)),
    );
    for (i, read) in reads.iter().enumerate() {
        if let Some(bytes) = read {
            let _ = std::fs::write(record.join(format!("read-{i}")), bytes);
        }
    }
    if let Ok(printed) = std::fs::read(dir.join("stdout")) {
        let _ = std::fs::write(record.join("stdout"), printed);
    }

    if program == "find" {
        return real_find(&dir, &args);
    }
    // This run's place among the runs of curl, from 1: the recorded runs of curl, itself included.
    let nth = std::fs::read_dir(dir.join("calls"))
        .map(|calls| {
            calls
                .filter_map(Result::ok)
                .filter(|c| std::fs::read(c.path().join("program")).is_ok_and(|p| p == b"curl"))
                .count()
        })
        .unwrap_or(1);
    if dir.join("fail").exists() || dir.join(format!("fail-{nth}")).exists() {
        eprintln!("curl: (7) Failed to connect");
        return ExitCode::from(7);
    }
    let reply = std::fs::read(dir.join(format!("reply-{nth}")))
        .or_else(|_| std::fs::read(dir.join("reply")));
    if let Ok(reply) = reply {
        let split = reply
            .iter()
            .position(|b| *b == b'\n')
            .unwrap_or(reply.len());
        let status = String::from_utf8_lossy(&reply[..split]).into_owned();
        let body = reply.get(split + 1..).unwrap_or_default();
        return answer(&args, &status, body);
    }
    forward(&args, &reads)
}
