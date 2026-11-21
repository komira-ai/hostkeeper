//! The `buck2` the farm-build tests (tests/farm_build_cli.rs) hand `cargo xtask farm-build` with
//! `--buck2`. A program of the tests, not shipped; it builds nothing and reaches no network.
//!
//! It appends its arguments, joined by spaces, as one line to `calls` beside its own file, then
//! answers from files there:
//!
//! - `audit ...` prints `audit.stdout` (`{}`, buck2's answer for a key that is not set, when there
//!   is none) and `audit.stderr` (on standard error), and exits with the number in `audit.status`
//!   (0 when there is none);
//! - `build ...` prints `build.stdout` and `build.stderr` the same way, and exits with the number in
//!   `build.status` (0 when there is none);
//! - `kill` exits 0.
//!
//! Anything else exits 2.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The directory of this program's file, where its answers are.
fn here() -> PathBuf {
    let exe = std::env::current_exe().expect("the fixture's own path");
    exe.parent().expect("the fixture's directory").to_owned()
}

/// The text of `name` beside the fixture, or `default`.
fn read(dir: &Path, name: &str, default: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_else(|_| default.to_owned())
}

/// Prints the answer of `step` and returns its exit status.
fn answer(dir: &Path, step: &str, stdout_default: &str) -> ExitCode {
    print!("{}", read(dir, &format!("{step}.stdout"), stdout_default));
    eprint!("{}", read(dir, &format!("{step}.stderr"), ""));
    let status: u8 = read(dir, &format!("{step}.status"), "0")
        .trim()
        .parse()
        .expect("a status of 0 to 255");
    ExitCode::from(status)
}

fn main() -> ExitCode {
    let dir = here();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut calls = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("calls"))
        .expect("the calls file");
    writeln!(calls, "{}", args.join(" ")).expect("record the call");
    match args.first().map(String::as_str) {
        Some("audit") => answer(&dir, "audit", "{}"),
        Some("build") => answer(&dir, "build", ""),
        Some("kill") => ExitCode::SUCCESS,
        _ => ExitCode::from(2),
    }
}
