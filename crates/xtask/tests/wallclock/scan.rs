//! The rule: no source of a shipping crate reads the wall clock, unless the allow-list names its
//! path.
//!
//! A source is read as its code ([`Code`]): comments and strings are not code, so a form named in
//! a comment or a string is not a read. A path such as `std::time::SystemTime::now` is a read when
//! its last segments are those of a row of [`FORMS`], with its first segment replaced by what a
//! `use` of the file brought in under that name (so `use std::time::SystemTime as T; T::now()` is
//! caught). A `use` itself is never a read, and neither is an item behind `#[cfg(test)]`.

use std::fmt;

use crate::code::{Chain, Code};

/// The crate of test helpers: it never ships, so it is not scanned.
const TESTKIT: &str = "hostkeeper-testkit";

/// The allow-list as the failure message names it, from the repository root.
pub const ALLOWED_FILE: &str = "crates/xtask/tests/wallclock/allowed.txt";

/// One way of reading the wall clock: a path whose last segments are `path` and, when `arg` is
/// not empty, a call of it whose arguments hold a path whose last segments are `arg`.
pub struct Form {
    pub name: &'static str,
    path: &'static [&'static str],
    arg: &'static [&'static str],
}

const fn form(
    name: &'static str,
    path: &'static [&'static str],
    arg: &'static [&'static str],
) -> Form {
    Form { name, path, arg }
}

/// Every form that obtains the wall-clock time. `Instant::now` and the monotonic and boot-time
/// clock ids are not here: they are not the wall clock.
pub const FORMS: &[Form] = &[
    // std: the system (wall) clock, also behind `.elapsed()` and `duration_since(` on its value.
    form("SystemTime::now", &["SystemTime", "now"], &[]),
    // chrono (`chrono::Utc`, `chrono::offset::Utc`): the wall clock in UTC.
    form("Utc::now", &["Utc", "now"], &[]),
    // chrono: the wall clock in the host's time zone.
    form("Local::now", &["Local", "now"], &[]),
    // time: the wall clock in UTC.
    form(
        "OffsetDateTime::now_utc",
        &["OffsetDateTime", "now_utc"],
        &[],
    ),
    // time: the wall clock in the host's time zone.
    form(
        "OffsetDateTime::now_local",
        &["OffsetDateTime", "now_local"],
        &[],
    ),
    // jiff: the wall clock as an instant.
    form("Timestamp::now", &["Timestamp", "now"], &[]),
    // jiff: the wall clock in the host's time zone.
    form("Zoned::now", &["Zoned", "now"], &[]),
    // libc time(2): seconds since the epoch, from the wall clock.
    form("libc::time", &["libc", "time"], &[]),
    // libc gettimeofday(2): the wall clock in microseconds.
    form("libc::gettimeofday", &["libc", "gettimeofday"], &[]),
    // libc (or nix) clock_gettime(2) on the realtime clock: the wall clock.
    form(
        "clock_gettime(CLOCK_REALTIME)",
        &["clock_gettime"],
        &["CLOCK_REALTIME"],
    ),
    // libc (or nix) clock_gettime(2) on the coarse realtime clock: the wall clock, less precise.
    form(
        "clock_gettime(CLOCK_REALTIME_COARSE)",
        &["clock_gettime"],
        &["CLOCK_REALTIME_COARSE"],
    ),
    // rustix clock_gettime on the realtime clock: the wall clock.
    form(
        "clock_gettime(ClockId::Realtime)",
        &["clock_gettime"],
        &["ClockId", "Realtime"],
    ),
    // rustix clock_gettime on the coarse realtime clock: the wall clock, less precise.
    form(
        "clock_gettime(ClockId::RealtimeCoarse)",
        &["clock_gettime"],
        &["ClockId", "RealtimeCoarse"],
    ),
    // rustix clock_gettime_dynamic on the realtime clock (`DynamicClockId::Known`).
    form(
        "clock_gettime_dynamic(ClockId::Realtime)",
        &["clock_gettime_dynamic"],
        &["ClockId", "Realtime"],
    ),
    // rustix clock_gettime_dynamic on the coarse realtime clock.
    form(
        "clock_gettime_dynamic(ClockId::RealtimeCoarse)",
        &["clock_gettime_dynamic"],
        &["ClockId", "RealtimeCoarse"],
    ),
];

/// A wall-clock read found in a source.
#[derive(Debug, PartialEq, Eq)]
pub struct Read {
    pub path: String,
    pub line: usize,
    pub form: &'static str,
}

impl fmt::Display for Read {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: wall-clock read {}: decisions read hostkeeper_steps::Clock; if this is \
             certificate, OIDC or nonce validity, add the path to {ALLOWED_FILE} with a reason",
            self.path, self.line, self.form
        )
    }
}

/// Whether the file at `path` (from the repository root) is scanned: a `.rs` file under
/// `crates/<crate>/src/`, unless the crate is the testkit, the file is named `tests.rs`, or it
/// sits in a `tests` directory inside `src/`. Files under a crate's `tests/`, `benches/` and
/// `examples/` are outside `src/` and so are not scanned.
pub fn scanned(path: &str) -> bool {
    let Some((krate, inner)) = path
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    let Some(src) = inner.strip_prefix("src/") else {
        return false;
    };
    let mut segments: Vec<&str> = src.split('/').collect();
    let file = segments.pop().unwrap_or_default();
    krate != TESTKIT && file.ends_with(".rs") && file != "tests.rs" && !segments.contains(&"tests")
}

fn ends_with(segments: &[String], tail: &[&str]) -> bool {
    segments.len() >= tail.len()
        && segments[segments.len() - tail.len()..]
            .iter()
            .zip(tail)
            .all(|(a, b)| a == b)
}

fn matches(form: &Form, chain: &Chain, code: &Code) -> bool {
    ends_with(&chain.segments, form.path)
        && (form.arg.is_empty()
            || code
                .args(chain)
                .iter()
                .any(|c| ends_with(&c.segments, form.arg)))
}

/// Every wall-clock read in the source `text` of the file at `path`.
pub fn reads(path: &str, text: &str) -> Vec<Read> {
    let code = Code::of(text);
    let mut out = Vec::new();
    for chain in code.chains() {
        if let Some(form) = FORMS.iter().find(|f| matches(f, chain, &code)) {
            out.push(Read {
                path: path.to_owned(),
                line: code.line(chain),
                form: form.name,
            });
        }
    }
    out
}

/// An allow-list entry: a file, or a directory when it ends in `/`; with its line.
pub struct Entry {
    pub path: String,
    pub line: usize,
}

impl Entry {
    fn covers(&self, path: &str) -> bool {
        if self.path.ends_with('/') {
            path.starts_with(&self.path)
        } else {
            path == self.path
        }
    }
}

/// What is wrong with the entry `path`, if anything.
fn entry_error(path: &str) -> Option<&'static str> {
    let segments: Vec<&str> = path.strip_suffix('/').unwrap_or(path).split('/').collect();
    if path.starts_with('/') {
        Some("is absolute; give it from the repository root")
    } else if segments.contains(&"..") {
        Some("has a `..` segment")
    } else if path.contains(['\\', ' ', '\t']) || segments.iter().any(|s| s.is_empty() || *s == ".")
    {
        Some("is not a plain relative path (forward slashes, no `.`, empty segment or space)")
    } else {
        None
    }
}

/// The entries of the allow-list `text`, or every line that is wrong in it.
pub fn parse_allowed(text: &str) -> Result<Vec<Entry>, Vec<String>> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut errors = Vec::new();
    let mut reason = false;
    for (index, raw) in text.lines().enumerate() {
        let n = index + 1;
        let line = raw.trim_end();
        if line.len() != raw.len() {
            errors.push(format!("allowed.txt:{n}: trailing whitespace"));
        }
        if let Some(comment) = line.strip_prefix('#') {
            reason = !comment.trim().is_empty();
            continue;
        }
        let had_reason = std::mem::replace(&mut reason, false);
        if line.is_empty() {
            continue;
        }
        if !had_reason {
            errors.push(format!(
                "allowed.txt:{n}: {line} has no `#` line above it saying why it may read the \
                 wall clock"
            ));
        }
        if let Some(error) = entry_error(line) {
            errors.push(format!("allowed.txt:{n}: {line} {error}"));
        } else if let Some(first) = entries.iter().find(|e| e.path == line) {
            errors.push(format!(
                "allowed.txt:{n}: {line} is listed twice (first on line {})",
                first.line
            ));
        } else {
            entries.push(Entry {
                path: line.to_owned(),
                line: n,
            });
        }
    }
    if errors.is_empty() {
        Ok(entries)
    } else {
        Err(errors)
    }
}

/// Checks the scanned `files` (path from the repository root, text) against the allow-list
/// `allowed`: every read outside it is refused, and so is an entry that names no file or whose
/// files read nothing.
pub fn check(files: &[(String, String)], allowed: &str) -> Result<(), Vec<String>> {
    let entries = parse_allowed(allowed)?;
    let mut errors = Vec::new();
    let mut reading: Vec<&str> = Vec::new();
    for (path, text) in files {
        let found = reads(path, text);
        if !found.is_empty() {
            reading.push(path);
        }
        if !entries.iter().any(|e| e.covers(path)) {
            errors.extend(found.iter().map(ToString::to_string));
        }
    }
    for entry in &entries {
        if !files.iter().any(|(path, _)| entry.covers(path)) {
            errors.push(format!(
                "stale allow-list entry: {} matches no file",
                entry.path
            ));
        } else if !reading.iter().any(|path| entry.covers(path)) {
            errors.push(format!(
                "unused allow-list entry: {} has no wall-clock read",
                entry.path
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
