//! The hook's log. The host's step launcher (`hostkeeper_steps::SystemdLauncher`) runs a step
//! with `systemd-run --pipe`, so what the hook writes reaches the daemon and not the journal of
//! the step's unit; the launcher keeps the last `hostkeeper_steps::RING_BYTES` of each stream
//! and hands them here after every run. The standard error of the hook is its log: each line
//! that is not blank goes to the daemon's standard error (the daemon's journal) under the name of
//! the step's unit. Standard output is not logged: its last line is the step's report, which the
//! step record keeps.

use std::io::{self, Write};

use hostkeeper_steps::Captured;

/// The line put before a hook's log when the launcher dropped the beginning of it.
pub const DROPPED: &str = "(the beginning of the hook's standard error was dropped)";

/// The lines of the daemon's log for one run of a step in `unit`: each line of the kept standard
/// error that is not blank, as `hostkeeper daemon: <unit>: <line>` (a carriage return before the
/// line feed dropped, bytes that are not UTF-8 replaced), after a [`DROPPED`] line when the
/// launcher kept only the end of it. No lines when the hook wrote nothing but blank lines.
pub fn hook_log_lines(unit: &str, captured: &Captured) -> Vec<String> {
    let bytes = captured.stderr.bytes();
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("hostkeeper daemon: {unit}: {line}"))
        .collect();
    if captured.stderr.cut() && !lines.is_empty() {
        lines.insert(0, format!("hostkeeper daemon: {unit}: {DROPPED}"));
    }
    lines
}

/// Writes [`hook_log_lines`] to `out`, one line each, ending in a line feed.
pub fn write_hook_log(out: &mut dyn Write, unit: &str, captured: &Captured) -> io::Result<()> {
    for line in hook_log_lines(unit, captured) {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Writes [`hook_log_lines`] to the daemon's standard error: the `hostkeeper_steps::StepLog` of
/// the host's launcher. A standard error that cannot be written loses the lines; the step is not
/// affected.
pub fn log_hook_output(unit: &str, captured: &Captured) {
    drop(write_hook_log(&mut io::stderr().lock(), unit, captured));
}
