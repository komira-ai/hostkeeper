//! The update hook: `hello update-hook <step> [<artifact>] [--params <file>]`.
//!
//! The contract is the one of the `hostkeeper-hook` crate. Exit 0 is success, 1 failure, 75
//! deferred (with `retry_after_secs`), 2 a command line this program cannot read (76, halt, is
//! never used: nothing here is beyond repair). The last line of standard output is always one
//! JSON object ([`crate::report::Report`]) and nothing is printed after it; log text goes to
//! standard error.
//!
//! Two slots, `a` and `b`, run the same unit template one at a time on one port. A slot runs
//! only while its marker `<record_dir>/slots/<slot>` exists, so a stopped slot is not started
//! again at boot or by the unit's restart. An update starts the idle slot (the daemon has
//! already pointed `current` at the new version, so the slot runs it) and stops the serving
//! one: a restart, with no hand-off.

use std::ffi::OsString;
use std::io::Write;

use crate::ports::Host;
use crate::record::Record;
use crate::report::{Failure, Report};

mod args;
mod settings;
mod steps;

pub use args::{Call, Step};
pub use settings::{Settings, Units};
pub use steps::{DEB_PROGRAM, PKG_PROGRAM, staged_program};

/// Exit status of a command line the hook cannot read.
pub const EXIT_USAGE: i32 = 2;

/// Runs one step. `args` start at the step (after `update-hook`). Returns the exit status; the
/// report line has been written to `out` by then, and the log text to `err`.
pub fn run(args: &[OsString], host: &Host<'_>, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let (code, report) = match execute(args, host, err) {
        Ok(report) => (0, report),
        Err(failure) => (failure.code, failure.report),
    };
    if let Some(detail) = &report.detail {
        let _ = writeln!(err, "hello update-hook: {detail}");
    }
    let _ = writeln!(out, "{}", report.line());
    code
}

fn execute(args: &[OsString], host: &Host<'_>, err: &mut dyn Write) -> Result<Report, Failure> {
    let call = Call::parse(args).map_err(|why| Failure {
        code: EXIT_USAGE,
        report: Report::environment().with_detail(why),
    })?;
    let file = call
        .params
        .clone()
        .ok_or_else(|| Failure::environment("the step needs --params <file>"))?;
    let text = std::fs::read_to_string(&file).map_err(|why| {
        Failure::environment(format!(
            "cannot read the params file {}: {why}",
            file.display()
        ))
    })?;
    let table = crate::params::parse(&text).map_err(|why| {
        Failure::environment(format!(
            "the params file {} is malformed: {why}",
            file.display()
        ))
    })?;
    let settings = Settings::from_table(&table)?;
    let record = Record::open(&settings.record_dir)?;
    let mut steps = steps::Steps {
        host,
        settings: &settings,
        record: &record,
        err,
    };
    steps.run(call.step, call.artifact)
}
