//! The program the hook test (tests/native_hook.rs) puts in a native package's tree and starts
//! through the package's hook. A program of the tests, not shipped.
//!
//! It prints the path it was started by and then each argument, one per line, and exits 0.

use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut out = std::io::stdout().lock();
    for arg in &args {
        if writeln!(out, "{arg}").is_err() {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
