//! `hello-e2e`: the `bin/hello` of the hello packages `tests/device_mac_e2e.rs` builds. It is
//! hello: the update hook (`update-hook <step>`, or a step as the first word) and every other
//! command line are `hello::main`, unchanged. Only the server differs, in the version it
//! answers `/version` with: hello answers the version it was compiled as (or that followed by
//! `-fail-ready` when started with `--fail-ready`), which is one version for every good package
//! a test builds from one binary, so its own `verify` would refuse every update
//! but one as `wrong_version`. This program answers with the version of the manifest of the
//! version directory it was started from (`<exe>/../../manifest.toml`, after the `current` link
//! is resolved), the version the daemon installed, which is what a release build of hello
//! answers too. A program of the tests, not shipped.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use hello::cli::{Command, parse};
use hello::server::{Answer, serve_forever};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match parse(&args) {
        Ok(Command::Serve { port, fail_ready }) => serve(port, fail_ready),
        _ => hello::main(),
    }
}

/// hello's server on `127.0.0.1:<port>`, answering the packaged version. Ends only when it
/// cannot serve, with exit 1, as hello's does.
fn serve(port: u16, fail_ready: bool) -> ExitCode {
    let version = match packaged_version() {
        Ok(version) => version,
        Err(why) => {
            eprintln!("hello-e2e: {why}");
            return ExitCode::from(1);
        }
    };
    let listener = match hello::bind_loopback(port) {
        Ok(listener) => listener,
        Err(why) => {
            eprintln!("hello: cannot listen on 127.0.0.1:{port}: {why}");
            return ExitCode::from(1);
        }
    };
    eprintln!("hello {version}: serving on 127.0.0.1:{port}");
    let answer = Answer {
        version: &version,
        fail_ready,
    };
    let why = serve_forever(listener, &answer);
    eprintln!("hello: stopped serving: {why}");
    ExitCode::from(1)
}

/// The `version = "<v>"` line of the manifest two directories above the executable, the links
/// on the way resolved.
fn packaged_version() -> Result<String, String> {
    let exe = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|e| format!("cannot find the executable: {e}"))?;
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("{} is not in a version directory", exe.display()))?;
    let manifest = dir.join("manifest.toml");
    let text = fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read {}: {e}", manifest.display()))?;
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("version = \"")?
                .strip_suffix('"')
                .map(str::to_owned)
        })
        .ok_or_else(|| format!("{} has no version line", manifest.display()))
}
