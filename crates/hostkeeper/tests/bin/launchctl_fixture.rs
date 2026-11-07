//! The `launchctl` the pkg's `postinstall` test (tests/postinstall.rs) puts in the script's
//! place. A program of the tests, not shipped.
//!
//! It appends its arguments, joined by spaces, as one line to `launchctl.calls` in the directory
//! it is run from. When the file `launchctl.<first argument>.exit` is there, it prints
//! `launchctl fixture: <first argument> failed` on standard error and exits with the code the
//! file holds (1 when the file holds no number); otherwise it exits 0.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

/// The directory of this program: the files it reads and writes are beside it.
fn here() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_default()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = here();
    let calls = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("launchctl.calls"));
    let logged = calls.and_then(|mut file| writeln!(file, "{}", args.join(" ")));
    if logged.is_err() {
        return ExitCode::from(70);
    }
    let first = args.first().cloned().unwrap_or_default();
    match std::fs::read_to_string(dir.join(format!("launchctl.{first}.exit"))) {
        Ok(code) => {
            eprintln!("launchctl fixture: {first} failed");
            ExitCode::from(code.trim().parse().unwrap_or(1))
        }
        Err(_) => ExitCode::SUCCESS,
    }
}
