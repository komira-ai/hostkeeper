//! The `launchctl` the pkg's `postinstall` test (tests/postinstall.rs) puts in the script's
//! place. A program of the tests, not shipped.
//!
//! It appends its arguments, joined by spaces, as one line to `launchctl.calls` in the directory
//! it is run from. When the file `launchctl.<first argument>.exit` is there, it prints
//! `launchctl fixture: <first argument> failed` on standard error and exits with the code the
//! file holds (1 when the file holds no number); otherwise it exits 0.
//!
//! It plays launchd's memory of the job too. The file `launchctl.loaded` is there while the job is
//! loaded: `print` then writes a description of the job on standard output and standard error and
//! exits 0, and without the file it says the job is not found and exits 113, as `launchctl print`
//! does; a `bootstrap` that succeeds creates the file.

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
    let loaded = dir.join("launchctl.loaded");
    match std::fs::read_to_string(dir.join(format!("launchctl.{first}.exit"))) {
        Ok(code) => {
            eprintln!("launchctl fixture: {first} failed");
            ExitCode::from(code.trim().parse().unwrap_or(1))
        }
        Err(_) if first == "print" => print(&loaded),
        Err(_) if first == "bootstrap" && std::fs::write(&loaded, "").is_err() => {
            ExitCode::from(70)
        }
        Err(_) => ExitCode::SUCCESS,
    }
}

/// `launchctl print` of the job: its description when `loaded` is there, "not found" (113) when
/// it is not.
fn print(loaded: &std::path::Path) -> ExitCode {
    if loaded.exists() {
        println!("system/job = {{ state = running }}");
        eprintln!("launchctl fixture: print noise");
        ExitCode::SUCCESS
    } else {
        eprintln!("Bad request.\nCould not find service in domain for system");
        ExitCode::from(113)
    }
}
