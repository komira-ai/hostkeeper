//! The program the package launch test (tests/package_launch.rs) packages as `bin/hostkeeper` and
//! starts through the `ExecStart=` of the committed unit of hostkeeper's own package. A program of
//! the tests, not shipped.
//!
//! It prints the path it was started by and then each argument, one per line, and exits 0 when
//! its first argument is `daemon`, the verb the unit must start, and 64 otherwise.

use std::io::Write;
use std::process::ExitCode;

const NOT_THE_DAEMON: u8 = 64;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut out = std::io::stdout().lock();
    for arg in &args {
        if writeln!(out, "{arg}").is_err() {
            return ExitCode::FAILURE;
        }
    }
    if args.get(1).map(String::as_str) == Some("daemon") {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(NOT_THE_DAEMON)
    }
}
