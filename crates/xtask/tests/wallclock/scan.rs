//! The rule: no source of a shipping crate reads the wall clock, unless the allow-list names its
//! path.
//!
//! A source is read as its code ([`Code`]): comments, strings and test-only items are not code.
//! A path is a read when one of the full paths it may stand for ends with the segments of a row
//! of [`FORMS`] (and, for a row that says so, the tokens after it follow): a call, or a reference
//! such as `unwrap_or_else(SystemTime::now)`, which calls it later. Naming a realtime clock id is
//! a read wherever it is, since it is only ever passed to a clock. A rename of a wall-clock name
//! ([`RENAMED`]) is refused too: the rule does not follow renames, so a renamed name would hide
//! its reads. A `use` of a wall-clock name under its own name is not a read.

use std::fmt;

use crate::code::{Chain, Code, MAX_SOURCES, Overflow, Rename};

/// The crate of test helpers: it never ships, so it is not scanned.
const TESTKIT: &str = "hostkeeper-testkit";

/// The allow-list as the failure message names it, from the repository root.
pub const ALLOWED_FILE: &str = "crates/xtask/tests/wallclock/allowed.txt";

/// One way of reading the wall clock: a path ending with `tail`, followed by the tokens `then`.
pub struct Form {
    pub name: &'static str,
    tail: &'static [&'static str],
    then: &'static [&'static str],
}

const fn form(name: &'static str, tail: &'static [&'static str]) -> Form {
    Form {
        name,
        tail,
        then: &[],
    }
}

/// Every form that obtains the wall-clock time. `Instant` and the monotonic and boot-time clock
/// ids are not here: they are not the wall clock.
pub const FORMS: &[Form] = &[
    // std: the system (wall) clock.
    form("SystemTime::now", &["SystemTime", "now"]),
    // std: `SystemTime::elapsed(&t)` reads the wall clock to measure from `t`.
    form("SystemTime::elapsed", &["SystemTime", "elapsed"]),
    // std: the time since the epoch, read from the wall clock.
    Form {
        name: "UNIX_EPOCH.elapsed",
        tail: &["UNIX_EPOCH"],
        then: &[".", "elapsed"],
    },
    // chrono (`chrono::Utc`, `chrono::offset::Utc`): the wall clock in UTC.
    form("Utc::now", &["Utc", "now"]),
    // chrono: the wall clock in the host's time zone.
    form("Local::now", &["Local", "now"]),
    // time: the wall clock in UTC.
    form("OffsetDateTime::now_utc", &["OffsetDateTime", "now_utc"]),
    // time: the wall clock in the host's time zone.
    form(
        "OffsetDateTime::now_local",
        &["OffsetDateTime", "now_local"],
    ),
    // jiff: the wall clock as an instant.
    form("Timestamp::now", &["Timestamp", "now"]),
    // jiff: the wall clock in the host's time zone.
    form("Zoned::now", &["Zoned", "now"]),
    // libc time(2): seconds since the epoch, from the wall clock.
    form("libc::time", &["libc", "time"]),
    // gettimeofday(2) (libc or nix): the wall clock in microseconds.
    form("gettimeofday", &["gettimeofday"]),
    // libc (or nix) realtime clock id: the wall clock.
    form("CLOCK_REALTIME", &["CLOCK_REALTIME"]),
    // libc (or nix) coarse realtime clock id: the wall clock, less precise.
    form("CLOCK_REALTIME_COARSE", &["CLOCK_REALTIME_COARSE"]),
    // libc (or nix) realtime alarm clock id: the wall clock.
    form("CLOCK_REALTIME_ALARM", &["CLOCK_REALTIME_ALARM"]),
    // libc (or nix) TAI clock id: the wall clock without leap seconds.
    form("CLOCK_TAI", &["CLOCK_TAI"]),
    // rustix realtime clock id (also inside `DynamicClockId::Known`): the wall clock.
    form("ClockId::Realtime", &["ClockId", "Realtime"]),
    // rustix coarse realtime clock id: the wall clock, less precise.
    form("ClockId::RealtimeCoarse", &["ClockId", "RealtimeCoarse"]),
    // rustix realtime alarm clock id: the wall clock.
    form(
        "DynamicClockId::RealtimeAlarm",
        &["DynamicClockId", "RealtimeAlarm"],
    ),
    // rustix TAI clock id: the wall clock without leap seconds.
    form("DynamicClockId::Tai", &["DynamicClockId", "Tai"]),
];

/// The names whose rename is refused. A name that other crates use too has a root: its rename
/// is refused when one of the sources it may stand for (every one the file's imports and globs
/// allow) starts with that crate or with something of this crate (`crate`, `self`, `super`, a
/// module the file declares), which may re-export it. The others are refused from any source.
pub const RENAMED: &[(&str, Option<&str>)] = &[
    ("SystemTime", None),
    ("UNIX_EPOCH", None),
    ("Utc", None),
    ("Local", None),
    ("OffsetDateTime", None),
    ("Timestamp", Some("jiff")),
    ("Zoned", None),
    ("libc", None),
    ("time", Some("libc")),
    ("gettimeofday", None),
    ("ClockId", None),
    ("DynamicClockId", None),
    ("Realtime", None),
    ("RealtimeCoarse", None),
    ("RealtimeAlarm", None),
    ("Tai", Some("rustix")),
    ("CLOCK_REALTIME", None),
    ("CLOCK_REALTIME_COARSE", None),
    ("CLOCK_REALTIME_ALARM", None),
    ("CLOCK_TAI", None),
];

/// A wall-clock read, or a rename of a wall-clock name, found in a source.
#[derive(Debug, PartialEq, Eq)]
pub struct Read {
    pub path: String,
    pub line: usize,
    pub form: String,
    pub kind: Kind,
}

/// What a [`Read`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A read of the wall clock; `form` is the row of [`FORMS`].
    Read,
    /// A rename of a wall-clock name; `form` is the rename as written.
    Renamed,
    /// A rename whose sources do not fit in [`MAX_SOURCES`]; `form` is the new name.
    Unresolved,
}

impl fmt::Display for Read {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (path, line, form) = (&self.path, self.line, &self.form);
        match self.kind {
            Kind::Unresolved => {
                return write!(
                    f,
                    "{path}:{line}: cannot resolve `{form}`: more than {MAX_SOURCES} import \
                     sources; split the glob imports or allow-list the path"
                );
            }
            Kind::Renamed => write!(
                f,
                "{path}:{line}: wall-clock name renamed, {form}: the lint does not follow a \
                 rename, so use the name itself; "
            )?,
            Kind::Read => write!(f, "{path}:{line}: wall-clock read {form}: ")?,
        }
        write!(
            f,
            "decisions read hostkeeper_steps::Clock; if this is certificate, OIDC or nonce \
             validity, add the path to {ALLOWED_FILE} with a reason"
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
    chain.paths.iter().any(|p| ends_with(p, form.tail)) && code.followed_by(chain, form.then)
}

/// Whether `source` is a wall-clock name (see [`RENAMED`]).
fn is_wall_clock_source(source: &[String], code: &Code) -> bool {
    let (Some(first), Some(last)) = (source.first(), source.last()) else {
        return false;
    };
    RENAMED
        .iter()
        .any(|(name, root)| last == name && root.is_none_or(|r| first == r || code.is_local(first)))
}

/// What `rename` is to the rule, if anything: a rename of a wall-clock name, when its source as
/// written or one of the sources it may stand for ([`Code::sources`]) is one, so an ambiguous
/// source is refused; or unresolved, when those sources overflow. Only a rename whose last
/// segment is a name of [`RENAMED`] is followed: following keeps the last segment (an alias of a
/// wall-clock name is refused where it is written), so no other rename can name a wall clock.
fn judge(rename: &Rename, code: &Code) -> Option<Kind> {
    if is_wall_clock_source(&rename.path, code) {
        return Some(Kind::Renamed);
    }
    let last = rename.path.last()?;
    if !RENAMED.iter().any(|(name, _)| last == name) {
        return None;
    }
    match code.sources(&rename.path) {
        Err(Overflow) => Some(Kind::Unresolved),
        Ok(sources) => sources
            .iter()
            .any(|source| is_wall_clock_source(source, code))
            .then_some(Kind::Renamed),
    }
}

/// Every wall-clock read and wall-clock rename in the source `text` of the file at `path`, by
/// line.
pub fn reads(path: &str, text: &str) -> Vec<Read> {
    let code = Code::of(text);
    let mut out = Vec::new();
    for chain in code.chains() {
        if let Some(form) = FORMS.iter().find(|f| matches(f, chain, &code)) {
            out.push(Read {
                path: path.to_owned(),
                line: code.line(chain),
                form: form.name.to_owned(),
                kind: Kind::Read,
            });
        }
    }
    for rename in code.renames() {
        let Some(kind) = judge(rename, &code) else {
            continue;
        };
        let form = match kind {
            Kind::Unresolved => rename.alias.clone(),
            Kind::Read | Kind::Renamed => rename.spelling.clone(),
        };
        out.push(Read {
            path: path.to_owned(),
            line: rename.line,
            form,
            kind,
        });
    }
    out.sort_by_key(|read| read.line);
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
