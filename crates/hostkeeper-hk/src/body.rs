//! The request body: the request file's object with every field a flag or a secret gives added
//! to it, and the files and stdin the secrets and the request file are read from.
//!
//! The request file is not parsed as JSON, and not written again: the gateway reads it. It is
//! scanned for its top-level keys, to refuse a field a flag sets again, and its text goes into
//! the body as it is, with the flags' fields after it.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use hostkeeper_proto::ctl::{FieldKind, Input, Label};

use crate::Stop;
use crate::args::Given;
use crate::encode::{base64, json_text, trim, without_newline};
use crate::table::json_name;

/// The file at `path`, when it can be read and is not a directory.
pub(crate) fn read_path(path: &[u8]) -> Option<Vec<u8>> {
    let path = Path::new(OsStr::from_bytes(path));
    if path.is_dir() {
        return None;
    }
    std::fs::read(path).ok()
}

/// A text from `bytes` without one trailing newline, or a usage error that `what` holds a NUL.
fn text_of(bytes: &[u8], what: &[u8]) -> Result<Vec<u8>, Stop> {
    if bytes.contains(&0) {
        return Err(Stop::usage([what, b" holds a NUL byte"].concat()));
    }
    Ok(without_newline(bytes).to_vec())
}

/// The text of the file `path` without one trailing newline; `what` names it in an error, and
/// what it holds is never in one.
pub(crate) fn read_file(path: &[u8], what: &[u8]) -> Result<Vec<u8>, Stop> {
    match read_path(path) {
        Some(bytes) => text_of(&bytes, what),
        None => Err(Stop::usage(
            [b"cannot read ", what, b" from ", path].concat(),
        )),
    }
}

/// All of stdin.
fn stdin_bytes(stdin: &mut dyn Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    // A stdin that fails to read is as much as it gave.
    let _ = stdin.read_to_end(&mut bytes);
    bytes
}

/// The text of stdin without one trailing newline; `what` names it in an error.
pub(crate) fn read_stdin(stdin: &mut dyn Read, what: &[u8]) -> Result<Vec<u8>, Stop> {
    text_of(&stdin_bytes(stdin), what)
}

/// What a scan of a request file found.
#[derive(Debug, Default)]
pub(crate) struct Scanned {
    /// The top-level keys, as written.
    pub(crate) keys: Vec<Vec<u8>>,
    /// The text of each key's value, trimmed; empty for a key that has none.
    pub(crate) values: Vec<Vec<u8>>,
    /// What is between the braces, trimmed.
    pub(crate) inner: Vec<u8>,
    /// Whether a key holds an escape.
    pub(crate) key_escaped: bool,
}

impl Scanned {
    /// Sets the text of the value of the last key. A value starts only after a key, so there is
    /// one.
    fn set_value(&mut self, value: &[u8]) {
        let last = self.keys.len() - 1;
        self.values[last] = value.to_vec();
    }
}

/// Whether `text` is one JSON object and what its top level holds. It is a scanner of strings and
/// brackets: it tells the keys of the object, not whether the rest is JSON.
pub(crate) fn scan_object(text: &[u8]) -> Option<Scanned> {
    let mut found = Scanned::default();
    let mut at = text
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .unwrap_or(text.len());
    if text.get(at) != Some(&b'{') {
        return None;
    }
    let open = at;
    let mut depth = 1_u32;
    at += 1;
    let (mut in_string, mut escaped, mut in_key) = (false, false, false);
    // Whether a key may open here, and whether one just closed.
    let (mut expect, mut keyed) = (true, false);
    let mut key = Vec::new();
    let mut value_start: Option<usize> = None;
    let mut close = None;
    while let Some(&c) = text.get(at) {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
                found.key_escaped |= in_key;
            } else if c == b'"' {
                in_string = false;
                if in_key {
                    in_key = false;
                    found.keys.push(std::mem::take(&mut key));
                    found.values.push(Vec::new());
                    keyed = true;
                }
            } else if in_key {
                key.push(c);
            }
        } else {
            match c {
                b'"' => {
                    in_string = true;
                    if depth == 1 && expect {
                        in_key = true;
                        key.clear();
                    }
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(at);
                        break;
                    }
                }
                b':' if depth == 1 => {
                    // A value with no key before it: not an object.
                    if !keyed {
                        return None;
                    }
                    keyed = false;
                    expect = false;
                    value_start = Some(at + 1);
                }
                b',' if depth == 1 => {
                    if let Some(start) = value_start.take() {
                        found.set_value(trim(text.get(start..at).unwrap_or_default()));
                        expect = true;
                    }
                }
                _ => {}
            }
        }
        at += 1;
    }
    let close = close?;
    if let Some(start) = value_start {
        found.set_value(trim(text.get(start..close).unwrap_or_default()));
    }
    if !trim(text.get(close + 1..).unwrap_or_default()).is_empty() {
        return None;
    }
    found.inner = trim(text.get(open + 1..close).unwrap_or_default()).to_vec();
    Some(found)
}

/// The body of a call, and the text of the hold's value if the request names one.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Body {
    pub(crate) bytes: Vec<u8>,
    pub(crate) hold_text: Vec<u8>,
}

/// The body of a call: the request file's object (`request`: a path, or `-` for stdin) with
/// every field `given` sets added to it, in the table's order. `hold` is the index of the field
/// that holds the call.
pub(crate) fn build(
    given: &Given<'_>,
    request: Option<&[u8]>,
    hold: Option<usize>,
    stdin: &mut dyn Read,
) -> Result<Body, Stop> {
    let mut scanned = Scanned::default();
    if let Some(path) = request {
        let text = if path == b"-" {
            read_stdin(stdin, b"the request")?
        } else {
            read_file(path, b"the request")?
        };
        scanned = scan_object(&text)
            .ok_or_else(|| Stop::usage("the request file is not a JSON object"))?;
        if scanned.key_escaped {
            return Err(Stop::usage(
                "the request file has a key with an escape in it; spell each key plainly",
            ));
        }
    }
    let mut hold_text = Vec::new();
    let mut seen = vec![false; given.fields.len()];
    for (key, value) in scanned.keys.iter().zip(&scanned.values) {
        for (at, field) in given.fields.iter().enumerate() {
            if key.as_slice() != field.name.as_bytes()
                && key.as_slice() != json_name(field.name).as_bytes()
            {
                continue;
            }
            if seen[at] {
                return Err(Stop::usage(format!(
                    "the request file names `{}` twice, by its proto name and its JSON name",
                    field.name
                )));
            }
            seen[at] = true;
            if given.slots[at].set {
                let file = if field.input == Input::SecretFile {
                    "-file"
                } else {
                    ""
                };
                return Err(Stop::usage(format!(
                    "--{}{file} and the request file both set `{}`",
                    field.flag, field.name
                )));
            }
            if hold == Some(at) {
                hold_text.clone_from(value);
            }
        }
    }
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for (at, (field, slot)) in given.fields.iter().zip(&given.slots).enumerate() {
        if !slot.set {
            continue;
        }
        let mut vals = slot.vals.clone();
        if field.input == Input::SecretFile {
            let source = slot.source.as_slice();
            let what = format!("`{}`", field.name);
            vals = if field.kind == FieldKind::Bytes {
                let bytes = if source == b"-" {
                    stdin_bytes(stdin)
                } else {
                    read_path(source).ok_or_else(|| {
                        Stop::usage([b"cannot read ", what.as_bytes(), b" from ", source].concat())
                    })?
                };
                format!("\"{}\"", base64(without_newline(&bytes))).into_bytes()
            } else if source == b"-" {
                json_text(&read_stdin(stdin, what.as_bytes())?)
            } else {
                json_text(&read_file(source, what.as_bytes())?)
            };
        }
        let value = match field.label {
            Label::Repeated => [b"[".as_slice(), &vals, b"]"].concat(),
            Label::Map => [b"{".as_slice(), &vals, b"}"].concat(),
            Label::Singular | Label::Optional | Label::Oneof => vals.clone(),
        };
        parts.push(
            [
                format!("\"{}\":", json_name(field.name)).as_bytes(),
                value.as_slice(),
            ]
            .concat(),
        );
        if hold == Some(at) {
            hold_text = vals;
        }
    }
    let joined = parts.join(&b',');
    let bytes = if scanned.inner.is_empty() || joined.is_empty() {
        [b"{", scanned.inner.as_slice(), &joined, b"}"].concat()
    } else {
        [b"{", scanned.inner.as_slice(), b",", &joined, b"}"].concat()
    };
    Ok(Body { bytes, hold_text })
}
