//! Running the Mac's programs for `hk xcode verify`: each to its end, what it did kept apart as
//! its exit and its two streams.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

use crate::xcode_push::command;
use crate::xcode_record::Output;

/// Runs `program` with `args` and no stdin, in `dir` when it is given: its [`Output`]. A program
/// that cannot be started has no exit and says why on stderr.
pub(super) fn tool(program: &Path, args: &[&dyn AsRef<OsStr>], dir: Option<&Path>) -> Output {
    let mut run = command(program);
    for arg in args {
        run.arg(arg.as_ref());
    }
    if let Some(dir) = dir {
        run.current_dir(dir);
    }
    run.stdin(Stdio::null());
    match run.output() {
        Ok(out) => Output {
            exit: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(error) => Output {
            exit: None,
            stdout: String::new(),
            stderr: format!("cannot run {}: {error}", program.display()),
        },
    }
}

/// How `output`'s exit reads in a failure: `exited 3`, or that it did not exit with a code.
pub(super) fn exit_text(output: &Output) -> String {
    match output.exit {
        Some(code) => format!("exited {code}"),
        None => "did not exit with a code".to_owned(),
    }
}
