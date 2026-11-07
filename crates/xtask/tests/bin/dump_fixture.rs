//! The `docker` the database backup test (tests/coordinator_deploy.rs) runs the committed backup
//! unit's command with. A program of the tests, not shipped.
//!
//! It answers as `docker compose ... exec -T postgres pg_dump ...` does, from files beside the
//! path it was started by: with neither, it prints `a dump by:` and then each of its arguments,
//! one per line, and exits 0; with `fail` there, it prints the start of a dump, says why on its
//! standard error and exits 3; with `empty` there, it prints nothing and exits 0.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

const FAILED: u8 = 3;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let beside = |name: &str| {
        args.first()
            .map(PathBuf::from)
            .and_then(|path| path.parent().map(|dir| dir.join(name)))
            .is_some_and(|path| path.exists())
    };
    let mut out = std::io::stdout().lock();
    if beside("empty") {
        return ExitCode::SUCCESS;
    }
    if beside("fail") {
        let _ = out.write_all(b"PGDMP cut short");
        eprintln!("pg_dump: error: connection to server failed");
        return ExitCode::from(FAILED);
    }
    let mut text = String::from("a dump by:\n");
    for arg in &args[1..] {
        text.push_str(arg);
        text.push('\n');
    }
    if out.write_all(text.as_bytes()).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
