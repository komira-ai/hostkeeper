//! The program the spawner tests run in place of `systemd-run`. Its first argument is the mode:
//! `echo <args...>` prints the rest joined by one space, `environ` prints the environment as
//! `K=V` lines, `stdin` copies its input to its output, `exit <n>` exits with `n`, `complain
//! <text>` writes the text to standard error and exits 2; anything else exits 64.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;

const USAGE: u8 = 64;

#[expect(
    clippy::disallowed_methods,
    reason = "the spawner test reads what environment reached the fixture; a program of the tests"
)]
fn environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| !name.as_bytes().starts_with(COVERAGE_RUNTIME))
        .collect()
}

/// The prefix of the variables the coverage runtime of an instrumented build sets in this
/// process itself when it starts (`__LLVM_PROFILE_RT_INIT_ONCE`): nothing the spawner passed.
const COVERAGE_RUNTIME: &[u8] = b"__LLVM_PROFILE_RT_";

fn joined(args: &[OsString]) -> Vec<u8> {
    let words: Vec<&[u8]> = args.iter().map(|arg| arg.as_bytes()).collect();
    words.join(&b' ')
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    let mut out = std::io::stdout();
    match args.first().and_then(|mode| mode.to_str()) {
        Some("echo") => {
            let _ = out.write_all(&joined(rest));
            let _ = out.write_all(b"\n");
            ExitCode::SUCCESS
        }
        Some("environ") => {
            for (key, value) in environment() {
                let _ = out.write_all(key.as_bytes());
                let _ = out.write_all(b"=");
                let _ = out.write_all(value.as_bytes());
                let _ = out.write_all(b"\n");
            }
            ExitCode::SUCCESS
        }
        Some("stdin") => {
            let mut input = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut input);
            let _ = out.write_all(&input);
            ExitCode::SUCCESS
        }
        Some("exit") => {
            let code = rest
                .first()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse::<u8>().ok());
            match code {
                Some(code) => ExitCode::from(code),
                None => usage("exit needs a number"),
            }
        }
        Some("complain") => {
            let mut err = std::io::stderr();
            let _ = err.write_all(&joined(rest));
            let _ = err.write_all(b"\n");
            ExitCode::from(2)
        }
        _ => usage("unknown mode"),
    }
}

fn usage(message: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "spawn fixture: {message}");
    ExitCode::from(USAGE)
}
