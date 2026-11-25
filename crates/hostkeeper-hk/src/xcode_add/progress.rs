//! The progress lines of `hk xcode add`: how far the hashing and the upload of an archive are.
//!
//! On a terminal, one line is rewritten in place (`\r`, the text, and the control sequence that
//! clears the rest of the line) each time the whole percent changes, and a newline ends it. Otherwise
//! one line `hk: <text>` is written each time a tenth of the archive is passed, so a log stays
//! short. The text is `<what> <done> of <total> <unit> (<percent>%)` and, when the clock has moved
//! since the start, `, <rate>/s`; the unit is the one the total is in (B, KB, MB or GB, powers of
//! 1000), so `4.10 of 11.92 GB`. The clock is [`crate::xcode_push::Mac::clock`], which a test
//! replaces; nothing here waits or judges a time.

use std::io::Write;
use std::time::Duration;

use crate::PROGRAM;

/// The units of a size, each 1000 times the one before.
const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];

/// The index in [`UNITS`] of the unit a size of `bytes` is told in.
fn unit_of(bytes: u64) -> usize {
    let mut unit = 0;
    let mut rest = bytes;
    while rest >= 1000 && unit + 1 < UNITS.len() {
        rest /= 1000;
        unit += 1;
    }
    unit
}

/// `bytes` in the unit at `unit`: whole bytes, or two decimals.
fn scaled(bytes: u64, unit: usize) -> String {
    if unit == 0 {
        return bytes.to_string();
    }
    let divisor = (0..unit).fold(1.0_f64, |acc, _| acc * 1000.0);
    format!("{:.2}", bytes as f64 / divisor)
}

/// `bytes` with its unit, as a size is said on its own: `11.92 GB`.
pub(crate) fn size_text(bytes: u64) -> String {
    let unit = unit_of(bytes);
    format!(
        "{} {}",
        scaled(bytes, unit),
        UNITS.get(unit).unwrap_or(&"B")
    )
}

/// A running progress line.
pub(crate) struct Progress {
    what: &'static str,
    total: u64,
    terminal: bool,
    clock: fn() -> Duration,
    start: Duration,
    /// The last whole percent drawn on a terminal, or the last whole tenth written otherwise.
    last: u64,
    /// Whether a line is drawn on the terminal and not yet ended.
    drawn: bool,
}

impl Progress {
    /// The progress of `what` (`hashing`, `uploading`) over `total` bytes, started now by `clock`.
    pub(crate) fn new(
        what: &'static str,
        total: u64,
        terminal: bool,
        clock: fn() -> Duration,
    ) -> Progress {
        Progress {
            what,
            total,
            terminal,
            clock,
            start: clock(),
            last: 0,
            drawn: false,
        }
    }

    /// The whole percent of `done` bytes: 100 for an empty archive.
    fn percent(&self, done: u64) -> u64 {
        if self.total == 0 {
            return 100;
        }
        let percent = u128::from(done.min(self.total)) * 100 / u128::from(self.total);
        u64::try_from(percent).unwrap_or(100)
    }

    /// The text of the line at `done` bytes.
    fn text(&self, done: u64) -> String {
        let unit = unit_of(self.total);
        let done = done.min(self.total);
        let name = UNITS.get(unit).unwrap_or(&"B");
        let mut text = format!(
            "{} {} of {} {name} ({}%)",
            self.what,
            scaled(done, unit),
            scaled(self.total, unit),
            self.percent(done)
        );
        let elapsed = (self.clock)().saturating_sub(self.start).as_secs_f64();
        if elapsed > 0.0 {
            let per_second = done as f64 / elapsed;
            // Whole bytes a second: the unit is that of the rate, not of the total.
            let rate = size_text(per_second as u64);
            text.push_str(&format!(", {rate}/s"));
        }
        text
    }

    /// Says that `done` bytes are through: a line when the percent (on a terminal) or the tenth
    /// (otherwise) changed since the last one.
    pub(crate) fn at(&mut self, done: u64, out: &mut dyn Write) {
        let percent = self.percent(done);
        // A stderr that cannot be written to does not stop the run.
        if self.terminal {
            if self.drawn && percent == self.last {
                return;
            }
            self.last = percent;
            self.drawn = true;
            let _ = write!(out, "\r{}\x1b[K", self.text(done));
        } else {
            let tenth = percent / 10;
            if tenth <= self.last {
                return;
            }
            self.last = tenth;
            let _ = writeln!(out, "{PROGRAM}: {}", self.text(done));
        }
    }

    /// Ends the line drawn on a terminal, so that what follows starts on a line of its own.
    pub(crate) fn finish(&mut self, out: &mut dyn Write) {
        if self.drawn {
            let _ = writeln!(out);
            self.drawn = false;
        }
    }
}
