//! The third-party libraries the app carries under `ui/vendor/`, each file pinned by its SHA-256
//! in [`PINS`] (`crates/hostkeeper-ui-tests/vendor.sha256`), so a change to a vendored library is
//! a reviewed change to that file.
//!
//! The pins are `sha256sum` lines run in `ui/`: 64 lower-case hex digits, two spaces and the
//! file's path under `ui/` (`vendor/<library>/<file>`). Blank lines and lines starting with `#`
//! (the library, its version and its licence) are skipped. [`unpinned`] holds every file of
//! [`VENDOR_DIR`] to its pin, and every pin to a file.
//!
//! A vendored file is exempt from the scan's needles and held to [`crate::scan::vendored_code`]
//! instead; see [`crate::scan::scan_file`].

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::scan::Finding;
use crate::sources::UiFile;

/// The directory of `ui/` that holds vendored libraries, as a path prefix.
pub const VENDOR_DIR: &str = "vendor/";

/// Where the pins are, as a finding names the file.
pub const PINS_PATH: &str = "crates/hostkeeper-ui-tests/vendor.sha256";

/// The pins of every vendored file.
pub const PINS: &str = include_str!("../vendor.sha256");

/// Whether the file at `path` (under `ui/`) is part of a vendored library.
pub fn is_vendored(path: &str) -> bool {
    path.starts_with(VENDOR_DIR)
}

/// One pinned file: its path under `ui/`, its SHA-256 in lower-case hex and the 1-based line of
/// the pins it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub path: String,
    pub sha256: String,
    pub line: usize,
}

/// Why the pins do not read.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PinError {
    /// A line that is not `<64 lower-case hex digits>  <path>`.
    #[error(
        "{PINS_PATH}:{line}: not `<SHA-256 in 64 lower-case hex digits>  vendor/<path>`: {text:?}"
    )]
    Malformed { line: usize, text: String },
    /// A path that is not a file below [`VENDOR_DIR`] (outside it, the directory itself, an empty,
    /// `.` or `..` component).
    #[error("{PINS_PATH}:{line}: {path:?} is not a file below {VENDOR_DIR}")]
    Outside { line: usize, path: String },
    /// A path pinned on an earlier line too.
    #[error("{PINS_PATH}:{line}: {path} is pinned twice")]
    Twice { line: usize, path: String },
}

fn is_digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_below_vendor(path: &str) -> bool {
    path.strip_prefix(VENDOR_DIR).is_some_and(|rest| {
        rest.split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
    })
}

/// The pins `text` lists, in its order.
pub fn pins(text: &str) -> Result<Vec<Pin>, PinError> {
    let mut out: Vec<Pin> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let malformed = || PinError::Malformed {
            line,
            text: raw.to_owned(),
        };
        let (sha256, path) = raw.split_once("  ").ok_or_else(malformed)?;
        if !is_digest(sha256) || path.is_empty() || path.contains(char::is_whitespace) {
            return Err(malformed());
        }
        if !is_below_vendor(path) {
            return Err(PinError::Outside {
                line,
                path: path.to_owned(),
            });
        }
        if out.iter().any(|pin| pin.path == path) {
            return Err(PinError::Twice {
                line,
                path: path.to_owned(),
            });
        }
        out.push(Pin {
            path: path.to_owned(),
            sha256: sha256.to_owned(),
            line,
        });
    }
    Ok(out)
}

/// The SHA-256 of `bytes` in lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every vendored file of `files` that no pin names or whose SHA-256 is not its pin's, and every
/// pin that names no file of `files`: one finding each, files first in `files`' order, then the
/// pins no file matched in theirs.
pub fn unpinned(files: &[UiFile], pins: &[Pin]) -> Vec<Finding> {
    let mut out = Vec::new();
    for file in files.iter().filter(|f| is_vendored(f.path)) {
        match pins.iter().find(|pin| pin.path == file.path) {
            None => out.push(Finding {
                path: file.path.to_owned(),
                line: 1,
                what: format!("a vendored file no line of {PINS_PATH} pins"),
            }),
            Some(pin) => {
                let got = sha256_hex(file.text.as_bytes());
                if got != pin.sha256 {
                    out.push(Finding {
                        path: PINS_PATH.to_owned(),
                        line: pin.line,
                        what: format!(
                            "{} has SHA-256 {got}, not the {} pinned",
                            pin.path, pin.sha256
                        ),
                    });
                }
            }
        }
    }
    for pin in pins {
        if !files.iter().any(|f| f.path == pin.path) {
            out.push(Finding {
                path: PINS_PATH.to_owned(),
                line: pin.line,
                what: format!("pins {}, which ui/ does not hold", pin.path),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests;
