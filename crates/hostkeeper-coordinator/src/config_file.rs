//! `serve --config <file>`: the flags of `serve` read from a TOML file, as the packaged unit
//! starts the coordinator (`packaging/coordinator/hostkeeper-coordinator.service`).
//!
//! The file is the flags themselves, not a second schema: each key is the long name of a flag of
//! `serve` without its `--`, and its value says what follows the flag. A string or an integer is
//! the flag's value (`heartbeat-listen = "192.0.2.10:8443"`, `live-rollouts = 2`); `true` is a
//! flag that takes no value (`ca-init = true`), and `false` leaves it out; an array of strings or
//! integers gives the flag once per element, in order (`server-name = ["a.example", "b.example"]`).
//! Any other value (a float, a date, a table, an empty array, an array holding another kind) is
//! refused, naming the key, and so is a key that is not a flag's name or that is `config`.
//!
//! Before the arguments are parsed, `--config <file>` (or `--config=<file>`) after `serve` is
//! replaced, where it stands, by the flags the file holds ([`expand`]), and the arguments are
//! then parsed once, by the same parser as any command line. So every rule of the flags holds for
//! the file's too: an unknown key is an unknown flag, a value is checked as the flag's own, and a
//! flag the file gives and the command line gives again is refused as a flag given twice. The
//! coordinator still reads no environment variable: the file is named by a flag, and what it
//! holds are flags.

use std::ffi::{OsStr, OsString};
use std::path::Path;

/// The flag that names the file.
pub(crate) const FLAG: &str = "--config";

/// The largest file read, in bytes: a file of flags is a few hundred bytes.
pub(crate) const MAX_BYTES: u64 = 64 * 1024;

/// Why `--config` could not be expanded: the file could not be read, or does not hold flags.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ConfigFileError {
    /// The flag was given more than once.
    #[error("{FLAG} is given twice: the flags of serve come from one file")]
    Twice,
    /// The file could not be read.
    #[error("{FLAG} {path}: {why}")]
    Unreadable {
        /// The file as `--config` named it.
        path: String,
        /// What reading it said.
        why: String,
    },
    /// The file is not TOML, or holds a key or a value that is no flag of `serve`.
    #[error("{FLAG} {path}: {why}")]
    Invalid {
        /// The file as `--config` named it.
        path: String,
        /// The first key or line that is wrong, and why.
        why: String,
    },
}

/// `args` with `--config <file>` after `serve` replaced by the flags of the file, which `read`
/// gives the text of. Arguments that are not `serve`'s, and a `--config` with no value after it
/// (which the parser then refuses), are given back unchanged.
pub(crate) fn expand(
    args: Vec<OsString>,
    read: impl Fn(&Path) -> std::io::Result<String>,
) -> Result<Vec<OsString>, ConfigFileError> {
    if args.get(1).map(OsString::as_os_str) != Some(OsStr::new("serve")) {
        return Ok(args);
    }
    let mut at = None;
    for (i, arg) in args.iter().enumerate().skip(2) {
        if let Some(path) = named(arg, args.get(i + 1)) {
            if at.is_some() {
                return Err(ConfigFileError::Twice);
            }
            at = Some((i, path));
        }
    }
    let Some((i, (path, width))) = at else {
        return Ok(args);
    };
    let shown = path.to_string_lossy().into_owned();
    let text = read(Path::new(&path)).map_err(|e| ConfigFileError::Unreadable {
        path: shown.clone(),
        why: e.to_string(),
    })?;
    let flags = flags(&text).map_err(|why| ConfigFileError::Invalid { path: shown, why })?;
    let mut out = args[..i].to_vec();
    out.extend(flags);
    out.extend_from_slice(&args[i + width..]);
    Ok(out)
}

/// The file `arg` names when it is `--config=<file>`, or `--config` followed by `next`, with the
/// number of arguments it takes; `None` for any other argument, and for `--config` last.
fn named(arg: &OsStr, next: Option<&OsString>) -> Option<(OsString, usize)> {
    let text = arg.to_str()?;
    if text == FLAG {
        return next.map(|path| (path.clone(), 2));
    }
    let path = text.strip_prefix(FLAG)?.strip_prefix('=')?;
    Some((OsString::from(path), 1))
}

/// Reads the file at `path`, refusing one that is not a regular file or is larger than
/// [`MAX_BYTES`].
pub(crate) fn read_file(path: &Path) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    if meta.len() > MAX_BYTES {
        return Err(std::io::Error::other(format!(
            "{} bytes, more than the {MAX_BYTES} a file of flags may hold",
            meta.len()
        )));
    }
    std::fs::read_to_string(path)
}

/// The arguments the TOML `text` stands for, as the module says, keys in the order of their
/// names; or why it does not hold flags.
pub(crate) fn flags(text: &str) -> Result<Vec<OsString>, String> {
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| format!("not TOML: {}", e.message()))?;
    let mut out = Vec::new();
    for (key, value) in &table {
        if !is_flag_name(key) {
            return Err(format!(
                "{key:?} is not the name of a flag: lower-case letters, digits and '-', starting with a letter"
            ));
        }
        if key == "config" {
            return Err("config: the file cannot name another file of flags".to_owned());
        }
        let flag = OsString::from(format!("--{key}"));
        match value {
            toml::Value::Boolean(true) => out.push(flag),
            toml::Value::Boolean(false) => {}
            toml::Value::Array(items) if items.is_empty() => {
                return Err(format!(
                    "{key}: an empty array gives no flag; leave the key out"
                ));
            }
            toml::Value::Array(items) => {
                for item in items {
                    let one = scalar(item).ok_or_else(|| {
                        format!("{key}: each element must be a string or an integer")
                    })?;
                    out.extend([flag.clone(), one]);
                }
            }
            other => {
                let one = scalar(other).ok_or_else(|| {
                    format!(
                        "{key}: a {}, not a string, an integer, a boolean or an array of strings or integers",
                        other.type_str()
                    )
                })?;
                out.extend([flag, one]);
            }
        }
    }
    Ok(out)
}

/// The value of a string or an integer, as the argument after its flag.
fn scalar(value: &toml::Value) -> Option<OsString> {
    match value {
        toml::Value::String(s) => Some(OsString::from(s)),
        toml::Value::Integer(n) => Some(OsString::from(n.to_string())),
        _ => None,
    }
}

/// A long flag's name without its `--`: a lower-case letter, then lower-case letters, digits and
/// `-`.
fn is_flag_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
#[path = "config_file_tests.rs"]
mod tests;
