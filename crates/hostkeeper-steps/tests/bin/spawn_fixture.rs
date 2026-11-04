//! The program the spawner tests run in place of `systemd-run` and `systemctl`
//! (tests/spawn.rs), so the real pipe reading runs with no program of the host. A program of the
//! tests, not shipped.
//!
//! As `systemctl` (first argument `stop`): prints its arguments; exits `5` for a unit whose name
//! has `unloaded` in it, `4` with a complaint for one with `refused`, `0` otherwise.
//!
//! As `systemd-run`: reads the file after `--params` and does what its one line says:
//! `report <code>` prints two log lines and a JSON report, a warning to standard error, and
//! exits with the code; `flood <stdout bytes> <stderr bytes> <code>` writes that many bytes with
//! no line feed, all of standard error first, then all of standard output; `line <len> <code>`
//! prints a log line and then a last line of `len` bytes; `environ` prints the environment as
//! `K=V` lines; `received` prints its arguments, one per line, then a line `stdin:` and what it
//! read from its input. Anything else exits 64.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;

const USAGE: u8 = 64;

/// The prefix of the variables the coverage runtime of an instrumented build sets in this
/// process itself when it starts (`__LLVM_PROFILE_RT_INIT_ONCE`): nothing the spawner passed.
const COVERAGE_RUNTIME: &[u8] = b"__LLVM_PROFILE_RT_";

#[expect(
    clippy::disallowed_methods,
    reason = "the spawner test reads what environment reached the fixture; a program of the tests"
)]
fn environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| !name.as_bytes().starts_with(COVERAGE_RUNTIME))
        .collect()
}

/// `len` bytes of `a` to `z` over and over, from `a`: the content tells where a kept tail began.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| b'a' + (i % 26) as u8).collect()
}

fn number<T: std::str::FromStr>(word: Option<&str>) -> Option<T> {
    word?.parse().ok()
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "stop") {
        return systemctl(&args);
    }
    let params = args
        .iter()
        .position(|a| a == "--params")
        .and_then(|at| args.get(at + 1));
    let Some(line) = params.and_then(|file| std::fs::read_to_string(file).ok()) else {
        return usage("no params file");
    };
    let words: Vec<&str> = line.split_whitespace().collect();
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    match words.as_slice() {
        ["report", code] => {
            let _ = out.write_all(b"preparing\nstill preparing\n{\"reason\":\"all_good\"}\n");
            let _ = err.write_all(b"a warning\n");
            exit(number(Some(*code)))
        }
        ["flood", stdout, stderr, code] => {
            let (Some(stdout), Some(stderr)) = (number(Some(*stdout)), number(Some(*stderr)))
            else {
                return usage("flood needs two sizes");
            };
            let _ = err.write_all(&pattern(stderr));
            let _ = out.write_all(&pattern(stdout));
            exit(number(Some(*code)))
        }
        ["line", len, code] => {
            let Some(len) = number::<usize>(Some(*len)) else {
                return usage("line needs a length");
            };
            let mut last = b"{\"pad\":\"".to_vec();
            last.resize(len - 2, b'x');
            last.extend_from_slice(b"\"}\n");
            let _ = out.write_all(b"starting\n");
            let _ = out.write_all(&last);
            exit(number(Some(*code)))
        }
        ["received"] => {
            for arg in &args {
                let _ = out.write_all(arg.as_bytes());
                let _ = out.write_all(b"\n");
            }
            let mut input = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut input);
            let _ = out.write_all(b"stdin:");
            let _ = out.write_all(&input);
            ExitCode::SUCCESS
        }
        ["environ"] => {
            for (key, value) in environment() {
                let _ = out.write_all(key.as_bytes());
                let _ = out.write_all(b"=");
                let _ = out.write_all(value.as_bytes());
                let _ = out.write_all(b"\n");
            }
            ExitCode::SUCCESS
        }
        _ => usage("unknown mode"),
    }
}

fn exit(code: Option<u8>) -> ExitCode {
    code.map_or_else(|| usage("needs an exit code"), ExitCode::from)
}

fn systemctl(args: &[OsString]) -> ExitCode {
    let words: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
    let _ = std::io::stdout().write_all(&words.join(&b' '));
    let unit = args.last().map(|a| a.to_string_lossy().into_owned());
    let unit = unit.unwrap_or_default();
    if unit.contains("unloaded") {
        let _ = writeln!(std::io::stderr(), "Unit {unit}.service not loaded.");
        return ExitCode::from(5);
    }
    if unit.contains("refused") {
        let _ = writeln!(
            std::io::stderr(),
            "Failed to stop {unit}.service: Access denied"
        );
        return ExitCode::from(4);
    }
    ExitCode::SUCCESS
}

fn usage(message: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "spawn fixture: {message}");
    ExitCode::from(USAGE)
}
