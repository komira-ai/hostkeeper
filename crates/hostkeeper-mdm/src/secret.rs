//! The kit's secret files: `KEY=VALUE` lines in 0600 files of root's under `MDM_ETC`
//! (`install` makes the API keys and the SCEP challenge; the operator makes `dns.env`). systemd
//! reads these files as `EnvironmentFile=` for the services and the cert unit, so they are read
//! here by the rules of systemd's `env-file.c`: a later assignment of a key replaces an earlier
//! one, quotes and backslashes are taken as systemd takes them, and a comment is a line whose
//! first non-blank character is `#` or `;`. A construct whose systemd reading is not certain
//! (a quote left open, text after a closing quote, a backslash inside single quotes, a
//! continued line that starts with white space, a backslash ending the file, a line starting
//! with `=`) and bytes that are not UTF-8 are refused by [`read`]; [`has_value`] and
//! [`holds_other_lines`] answer for such a file in the way that leaves it alone: no value, other
//! lines held. A value read is a [`Secret`], which is never printed.

use std::fmt;

use hostkeeper_mdm_step::Host;

use crate::MdmError;

/// A value from a secret file. It has no `Display`, and its `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The value, for the one place it goes.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// One logical line of an environment file that is not blank or a comment.
enum Entry {
    /// `KEY=VALUE`, the key with its surrounding white space dropped.
    Assign { key: String, value: String },
    /// A line without `=`, which systemd ignores.
    Stray,
}

/// A construct refused, and the line (from 1) where its logical line starts.
struct Refusal {
    line: usize,
    why: &'static str,
}

/// systemd's `WHITESPACE`.
const fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// systemd's `NEWLINE`.
const fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r')
}

/// Where the reader is in a line, after systemd's `parse_env_file_internal`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    PreKey,
    Key,
    PreValue,
    Value,
    ValueEscape,
    Single,
    Double,
    DoubleEscape,
    AfterQuote,
    Comment,
    CommentEscape,
}

/// The entries of an environment file, in order, or the construct refused.
fn parse(text: &str) -> Result<Vec<Entry>, Refusal> {
    let mut entries = Vec::new();
    let mut state = State::PreKey;
    let mut line = 1;
    // The line where the logical line being read started.
    let mut start = 1;
    let mut key = String::new();
    // The length of `key` or `value` up to the last character that is not trailing white space.
    let mut key_end = 0;
    let mut value = String::new();
    let mut value_end = 0;
    // The last character was a backslash-newline, so the next one must not be white space.
    let mut continued = false;
    let refuse = |line: usize, why: &'static str| Refusal { line, why };
    for c in text.chars() {
        if continued {
            if is_space(c) {
                return Err(refuse(start, "a continued line starts with white space"));
            }
            continued = false;
        }
        match state {
            State::PreKey if matches!(c, '#' | ';') => state = State::Comment,
            State::PreKey if is_space(c) => {}
            State::PreKey if c == '=' => return Err(refuse(line, "a line starts with =")),
            State::PreKey => {
                start = line;
                key.clear();
                key.push(c);
                key_end = key.len();
                state = State::Key;
            }
            State::Key if is_newline(c) => {
                entries.push(Entry::Stray);
                state = State::PreKey;
            }
            State::Key if c == '=' => {
                key.truncate(key_end);
                value.clear();
                value_end = 0;
                state = State::PreValue;
            }
            State::Key => {
                key.push(c);
                if !is_space(c) {
                    key_end = key.len();
                }
            }
            State::PreValue if is_newline(c) => {
                entries.push(Entry::Assign {
                    key: key.clone(),
                    value: String::new(),
                });
                state = State::PreKey;
            }
            State::PreValue if c == '\'' => state = State::Single,
            State::PreValue if c == '"' => state = State::Double,
            State::PreValue if c == '\\' => state = State::ValueEscape,
            State::PreValue if is_space(c) => {}
            State::PreValue => {
                value.push(c);
                value_end = value.len();
                state = State::Value;
            }
            State::Value if is_newline(c) => {
                value.truncate(value_end);
                entries.push(Entry::Assign {
                    key: key.clone(),
                    value: value.clone(),
                });
                state = State::PreKey;
            }
            State::Value if c == '\\' => state = State::ValueEscape,
            State::Value => {
                value.push(c);
                if !is_space(c) {
                    value_end = value.len();
                }
            }
            State::ValueEscape if is_newline(c) => {
                continued = true;
                state = State::Value;
            }
            State::ValueEscape => {
                value.push(c);
                value_end = value.len();
                state = State::Value;
            }
            State::Single if c == '\'' => state = State::AfterQuote,
            State::Single if c == '\\' => {
                return Err(refuse(start, "a backslash in single quotes"));
            }
            State::Single => value.push(c),
            State::Double if c == '"' => state = State::AfterQuote,
            State::Double if c == '\\' => state = State::DoubleEscape,
            State::Double => value.push(c),
            State::DoubleEscape if is_newline(c) => {
                continued = true;
                state = State::Double;
            }
            State::DoubleEscape => {
                if !matches!(c, '"' | '\\' | '`' | '$') {
                    value.push('\\');
                }
                value.push(c);
                state = State::Double;
            }
            State::AfterQuote if is_newline(c) => {
                entries.push(Entry::Assign {
                    key: key.clone(),
                    value: value.clone(),
                });
                state = State::PreKey;
            }
            State::AfterQuote if is_space(c) => {}
            State::AfterQuote => return Err(refuse(start, "text follows a closing quote")),
            State::Comment if is_newline(c) => state = State::PreKey,
            State::Comment if c == '\\' => state = State::CommentEscape,
            State::Comment => {}
            State::CommentEscape => state = State::Comment,
        }
        if c == '\n' {
            line += 1;
        }
    }
    finish(state, continued, start, key, value, value_end, &mut entries)?;
    Ok(entries)
}

/// The end of the file in `state`: the last line's entry, or the construct refused.
fn finish(
    state: State,
    continued: bool,
    start: usize,
    key: String,
    mut value: String,
    value_end: usize,
    entries: &mut Vec<Entry>,
) -> Result<(), Refusal> {
    let refuse = |why: &'static str| Refusal { line: start, why };
    if continued || matches!(state, State::ValueEscape | State::DoubleEscape) {
        return Err(refuse("a backslash ends the file"));
    }
    match state {
        State::Key => entries.push(Entry::Stray),
        State::Value => {
            value.truncate(value_end);
            entries.push(Entry::Assign { key, value });
        }
        State::PreValue | State::AfterQuote => entries.push(Entry::Assign { key, value }),
        State::Single | State::Double => return Err(refuse("a quote is not closed")),
        _ => {}
    }
    Ok(())
}

/// The last assignment of `key`: what systemd gives the service.
fn last_value<'a>(entries: &'a [Entry], key: &str) -> Option<&'a str> {
    entries.iter().rev().find_map(|e| match e {
        Entry::Assign { key: k, value } if k == key => Some(value.as_str()),
        _ => None,
    })
}

/// The entries of `bytes`, or `None` when `bytes` cannot be read with certainty.
fn entries_of(bytes: &[u8]) -> Option<Vec<Entry>> {
    parse(std::str::from_utf8(bytes).ok()?).ok()
}

/// Refuse a secret file that is not mode 0600 or not root's.
pub fn check_mode(path: &str, mode: u32, uid: u32) -> Result<(), MdmError> {
    match (mode, uid) {
        (0o600, 0) => Ok(()),
        (mode, uid) => Err(MdmError::SecretMode {
            path: path.to_owned(),
            mode,
            uid,
        }),
    }
}

/// The last assignment of `key` in `bytes`, as systemd reads an environment file, is not empty.
/// A file that cannot be read with certainty has no value.
pub fn has_value(bytes: &[u8], key: &str) -> bool {
    entries_of(bytes).is_some_and(|e| last_value(&e, key).is_some_and(|v| !v.is_empty()))
}

/// Whether `bytes` assigns a key other than `key` or holds a line that is neither blank, a
/// comment nor an assignment. A file that cannot be read with certainty holds other lines.
pub fn holds_other_lines(bytes: &[u8], key: &str) -> bool {
    entries_of(bytes).is_none_or(|entries| {
        entries.iter().any(|e| match e {
            Entry::Assign { key: k, .. } => k != key,
            Entry::Stray => true,
        })
    })
}

/// The value of `key` in the secret file at `path`: refused unless the file is a 0600 file of
/// root's, UTF-8 text without a construct of uncertain reading, whose last assignment of `key`
/// has a value without control characters.
pub fn read(host: &dyn Host, path: &str, key: &str) -> Result<Secret, MdmError> {
    let missing = || MdmError::NoSecret {
        path: path.to_owned(),
        key: key.to_owned(),
    };
    let meta = host.stat(path)?.ok_or_else(missing)?;
    check_mode(path, meta.mode, meta.uid)?;
    let bytes = host.read(path)?.ok_or_else(missing)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| MdmError::SecretEncoding {
        path: path.to_owned(),
    })?;
    let entries = parse(text).map_err(|r| MdmError::SecretSyntax {
        path: path.to_owned(),
        line: r.line,
        why: r.why,
    })?;
    let value = last_value(&entries, key)
        .filter(|v| !v.is_empty())
        .ok_or_else(missing)?;
    match value.chars().any(char::is_control) {
        true => Err(MdmError::SecretValue {
            path: path.to_owned(),
            key: key.to_owned(),
        }),
        false => Ok(Secret(value.to_owned())),
    }
}
