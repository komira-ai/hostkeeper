//! The `env` the install-check tests (tests/channels_check_apt_cli.rs) put on the `PATH` of `xtask
//! channels check-apt`, which starts each program of the checked system as `env
//! DEBIAN_FRONTEND=noninteractive <program> <args>`. A program of the tests, not shipped; it
//! reaches no network and changes nothing outside the scratch directory it is given.
//!
//! It answers from `apt/` beside its own file, and appends each command line (the program and
//! its arguments, separated by a tab) to `apt/calls` there:
//!
//! - `dpkg --print-architecture` prints `apt/machine`;
//! - `apt-get update` and `apt-get install ...` succeed and do nothing;
//! - `curl ... --output <file> <url>` copies `apt/keyring` to `<file>`, making its directory;
//! - `dpkg-query --show --showformat=${Version} <package>` prints `apt/version`;
//! - `getent passwd <user>` succeeds;
//! - `<absolute path> --version`, the installed binary, prints `apt/says`.
//!
//! Anything else, and a first argument other than `DEBIAN_FRONTEND=noninteractive`, exits 2.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

/// The value after `flag` in `args`.
fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).map(String::as_str)
}

/// Prints the file `name` of `data`.
fn print(data: &Path, name: &str) -> ExitCode {
    let Ok(bytes) = std::fs::read(data.join(name)) else {
        eprintln!("env: no {name} beside the fixture");
        return ExitCode::from(2);
    };
    if std::io::stdout().write_all(&bytes).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn curl(data: &Path, args: &[String]) -> ExitCode {
    let Some(output) = value(args, "--output") else {
        return ExitCode::from(2);
    };
    let output = Path::new(output);
    if let Some(dir) = output.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return ExitCode::FAILURE;
    }
    if std::fs::copy(data.join("keyring"), output).is_err() {
        eprintln!("curl: (22) The requested URL returned error: 404");
        return ExitCode::from(22);
    }
    ExitCode::SUCCESS
}

fn answer(data: &Path, program: &str, args: &[String]) -> ExitCode {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match (program, words.as_slice()) {
        ("dpkg", ["--print-architecture"]) => print(data, "machine"),
        ("apt-get", ["update"] | ["install", ..]) | ("getent", ["passwd", _]) => ExitCode::SUCCESS,
        ("curl", _) => curl(data, args),
        ("dpkg-query", ["--show", "--showformat=${Version}", _]) => print(data, "version"),
        (binary, ["--version"]) if binary.starts_with('/') => print(data, "says"),
        _ => ExitCode::from(2),
    }
}

fn main() -> ExitCode {
    let Ok(exe) = std::env::current_exe() else {
        return ExitCode::FAILURE;
    };
    let Some(data) = exe.parent().map(|bin| bin.join("apt")) else {
        return ExitCode::FAILURE;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let logged = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data.join("calls"))
        .and_then(|mut calls| writeln!(calls, "{}", args.join("\t")));
    if logged.is_err() {
        return ExitCode::FAILURE;
    }
    match args.as_slice() {
        [frontend, program, rest @ ..] if frontend == "DEBIAN_FRONTEND=noninteractive" => {
            answer(&data, program, rest)
        }
        _ => ExitCode::from(2),
    }
}
