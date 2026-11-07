//! The program the tests of `RealHost::run` (tests/fs_real.rs) run in place of a program of the
//! host, so they need none. A program of the tests, not shipped.
//!
//! `echo ARGS...` prints its arguments joined by one space and a line feed; `environ` prints
//! the environment it was given as `K=V` lines; `exit CODE` exits with the code; `abort` ends
//! by the signal `SIGABRT`. Anything else exits 64.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;

/// The prefix of the variables the coverage runtime of an instrumented build sets in this
/// process itself when it starts (`__LLVM_PROFILE_RT_INIT_ONCE`): nothing the runner passed.
const COVERAGE_RUNTIME: &[u8] = b"__LLVM_PROFILE_RT_";

#[expect(
    clippy::disallowed_methods,
    reason = "the run test reads what environment reached the fixture; a program of the tests"
)]
fn environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| !name.as_bytes().starts_with(COVERAGE_RUNTIME))
        .collect()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["echo", rest @ ..] => {
            let _ = writeln!(out, "{}", rest.join(" "));
            ExitCode::SUCCESS
        }
        ["environ"] => {
            for (name, value) in environment() {
                let _ = out.write_all(name.as_bytes());
                let _ = out.write_all(b"=");
                let _ = out.write_all(value.as_bytes());
                let _ = out.write_all(b"\n");
            }
            ExitCode::SUCCESS
        }
        ["exit", code] => ExitCode::from(code.parse::<u8>().unwrap_or(64)),
        ["abort"] => std::process::abort(),
        _ => ExitCode::from(64),
    }
}
