//! `hostkeeper mac-config [--managed <file>] [--config <file>]`: the daemon config of a Mac,
//! written from the managed preferences its MDM installed (docs/design/hostkeeper-mdm.md,
//! section 4). The start of a Mac's daemon runs it when the config is not there ([`at_start`]);
//! the pkg's `postinstall` does not run it.
//!
//! 1. A config that is there as a regular file is kept: nothing is written and the managed
//!    preferences are not read. It is the operator's, or an earlier run's. Anything else at the
//!    config path (a link, a directory) is refused and left as it is
//!    ([`MacConfigError::NotAFile`]): the daemon reads its config only from a regular file, not
//!    through a link, so loading its job then would start a daemon that cannot start.
//! 2. Otherwise the managed file is read as a file root owns that only its owner can write
//!    ([`hostkeeper_config::trust::read_trusted`], checked on the open file). A managed file that
//!    is not there is the install's failure ([`MacConfigError::Neither`]); one that is there but
//!    not trusted is refused, naming why.
//! 3. It is parsed as a property list, binary or XML, whose top level must be a dictionary. The
//!    dictionary is rendered as TOML: a dictionary is a table, a string, a boolean, a real and an
//!    array are their TOML forms, and an integer is one that fits `i64`. A date, a data value
//!    or a uid has no form in the config and is refused, naming its dotted key.
//! 4. The text is parsed by the daemon's own loader ([`DaemonConfig::parse`]), so a missing key,
//!    an unknown key or a bad value is refused with the loader's reason, as it is in a
//!    hand-written file.
//! 5. The file is written without ever replacing one: a new temporary file in the config's
//!    directory (made mode 0755 when missing), mode 0600 and synced, is hard-linked to the config
//!    path, which fails when the config appeared meanwhile (that file is judged as in 1), and the
//!    temporary file is removed, with no copy left behind on any failure.
//!
//! The verb exits 0 when the config exists afterwards, written or kept, and [`EXIT_FAILED`]
//! with the error on standard error otherwise.
//!
//! The start of a Mac's daemon runs the same steps on its `--config` before it reads it
//! ([`at_start`], docs/design/two-programs.md, section 2.1), saying nothing for a config kept: a
//! config that is not there is written from the managed preferences, and one that cannot be is
//! the start's error ([`crate::start::StartError::MacConfig`], [`crate::start::EXIT_CONFIG`]).

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Cursor, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hostkeeper_config::daemon::{ConfigError, DaemonConfig};
use hostkeeper_config::trust::{TrustError, read_trusted};
use plist::Value;

use crate::start::EXIT_FAILED;

/// The comment the written file starts with.
pub const HEADER: &str =
    "# Written by hostkeeper mac-config from the managed preferences of dev.hostkeeper.config.\n";

/// What the verb did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Configured {
    /// The config was written from the managed preferences.
    Written,
    /// A config was there already (or appeared while this one was made), and is unchanged.
    Kept,
}

/// Why no config exists after the verb.
#[derive(Debug, thiserror::Error)]
pub enum MacConfigError {
    /// There is no config and no managed preferences file to make one from.
    #[error("neither a managed config ({}) nor a config ({}) is there", managed.display(), config.display())]
    Neither { managed: PathBuf, config: PathBuf },
    /// Something other than a regular file is at the config path; it is left as it is.
    #[error("{} is there but is not a regular file; it is not replaced", path.display())]
    NotAFile { path: PathBuf },
    /// The managed file is there but is not one root trusts, or cannot be read.
    #[error("the managed config {} is refused: {source}", path.display())]
    Refused {
        path: PathBuf,
        #[source]
        source: TrustError,
    },
    /// The managed file is not a property list.
    #[error("the managed config {} is not a property list: {source}", path.display())]
    NotPlist {
        path: PathBuf,
        #[source]
        source: plist::Error,
    },
    /// The top level of the property list is not a dictionary.
    #[error("the managed config {} is not a dictionary at its top level", path.display())]
    NotDictionary { path: PathBuf },
    /// A value has no form in the config.
    #[error("the managed config's {key} is {kind}, which the daemon config cannot hold")]
    Unsupported { key: String, kind: &'static str },
    /// An integer does not fit a signed 64-bit integer.
    #[error("the managed config's {key} does not fit a signed 64-bit integer")]
    IntegerRange { key: String },
    /// The daemon's loader refused the rendered text.
    #[error("the managed config does not make a daemon config: {0}")]
    Invalid(#[from] ConfigError),
    /// The config could not be written.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The verb with the real effects: root is the trusted owner of the managed file. Prints a line
/// on standard error when the config is kept, and the error when there is none.
pub fn main(managed: &Path, config: &Path) -> ExitCode {
    finish(mac_config(managed, config, 0), config)
}

/// The exit status of the verb for `result`, with the line it says on standard error: nothing
/// for a config written, [`kept_line`] for one kept, the error for none.
pub fn finish(result: Result<Configured, MacConfigError>, config: &Path) -> ExitCode {
    match result {
        Ok(Configured::Written) => ExitCode::SUCCESS,
        Ok(Configured::Kept) => {
            eprintln!("{}", kept_line(config));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hostkeeper mac-config: {error}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

/// The start's step: [`mac_config`] of `managed` into `config`, the daemon config the start reads
/// next, with `trusted_uid` the owner the managed file must have; `Ok` when the config is there
/// afterwards, written or kept.
pub fn at_start(managed: &Path, config: &Path, trusted_uid: u32) -> Result<(), MacConfigError> {
    mac_config(managed, config, trusted_uid).map(drop)
}

/// The line said when the config is kept.
pub fn kept_line(config: &Path) -> String {
    format!(
        "hostkeeper mac-config: {} exists and is kept; the managed config is not read",
        config.display()
    )
}

/// Writes `config` from `managed`, as the module says; `trusted_uid` is the owner the managed
/// file must have.
pub fn mac_config(
    managed: &Path,
    config: &Path,
    trusted_uid: u32,
) -> Result<Configured, MacConfigError> {
    mac_config_with(managed, config, trusted_uid, &|| {})
}

/// [`mac_config`], with `between` run after the temporary file is written and before it is linked
/// to the config path, where a config can appear.
pub fn mac_config_with(
    managed: &Path,
    config: &Path,
    trusted_uid: u32,
    between: &dyn Fn(),
) -> Result<Configured, MacConfigError> {
    if let Some(existing) = existing(config) {
        return existing;
    }
    let bytes = read_trusted(managed, trusted_uid).map_err(|source| match source {
        TrustError::Io(ref io) if io.kind() == io::ErrorKind::NotFound => MacConfigError::Neither {
            managed: managed.to_path_buf(),
            config: config.to_path_buf(),
        },
        source => MacConfigError::Refused {
            path: managed.to_path_buf(),
            source,
        },
    })?;
    let text = render(managed, &bytes)?;
    DaemonConfig::parse(&text)?;
    let written = write_new(config, &format!("{HEADER}{text}"), between).map_err(|source| {
        MacConfigError::Io {
            path: config.to_path_buf(),
            source,
        }
    })?;
    match written {
        Configured::Written => Ok(Configured::Written),
        Configured::Kept => existing(config).unwrap_or(Ok(Configured::Kept)),
    }
}

/// What is at `config` already, judged as the module's step 1 says: `None` when nothing is
/// there, [`Configured::Kept`] for a regular file (not followed through a link), and
/// [`MacConfigError::NotAFile`] for anything else.
fn existing(config: &Path) -> Option<Result<Configured, MacConfigError>> {
    let meta = fs::symlink_metadata(config).ok()?;
    Some(if meta.file_type().is_file() {
        Ok(Configured::Kept)
    } else {
        Err(MacConfigError::NotAFile {
            path: config.to_path_buf(),
        })
    })
}

/// The property list in `bytes`, as the text of a TOML document.
fn render(managed: &Path, bytes: &[u8]) -> Result<String, MacConfigError> {
    let value =
        Value::from_reader(Cursor::new(bytes)).map_err(|source| MacConfigError::NotPlist {
            path: managed.to_path_buf(),
            source,
        })?;
    match value {
        Value::Dictionary(dict) => {
            let table = table(&dict, "")?;
            Ok(table.to_string())
        }
        _ => Err(MacConfigError::NotDictionary {
            path: managed.to_path_buf(),
        }),
    }
}

/// `key` and `name` joined as a dotted key.
fn dotted(key: &str, name: &str) -> String {
    match key.is_empty() {
        true => name.to_string(),
        false => format!("{key}.{name}"),
    }
}

/// A dictionary as a TOML table; `key` is the dotted key of the dictionary, empty at the top.
fn table(dict: &plist::Dictionary, key: &str) -> Result<toml::Table, MacConfigError> {
    dict.iter()
        .map(|(name, value)| Ok((name.clone(), convert(value, &dotted(key, name))?)))
        .collect()
}

/// A property list value as a TOML value; `key` names it in an error.
fn convert(value: &Value, key: &str) -> Result<toml::Value, MacConfigError> {
    let unsupported = |kind| MacConfigError::Unsupported {
        key: key.to_string(),
        kind,
    };
    match value {
        Value::Dictionary(dict) => table(dict, key).map(toml::Value::Table),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(at, item)| convert(item, &format!("{key}[{at}]")))
            .collect::<Result<Vec<_>, _>>()
            .map(toml::Value::Array),
        Value::String(text) => Ok(toml::Value::String(text.clone())),
        Value::Boolean(flag) => Ok(toml::Value::Boolean(*flag)),
        Value::Real(real) => Ok(toml::Value::Float(*real)),
        Value::Integer(int) => {
            int.as_signed()
                .map(toml::Value::Integer)
                .ok_or_else(|| MacConfigError::IntegerRange {
                    key: key.to_string(),
                })
        }
        Value::Date(_) => Err(unsupported("a date")),
        Value::Data(_) => Err(unsupported("data")),
        _ => Err(unsupported("a uid")),
    }
}

/// The name of the temporary file, in the config's directory.
fn temporary_name() -> String {
    format!(".hostkeeper-mac-config.{}.tmp", std::process::id())
}

/// Removes the file at its path when dropped, so that no failure leaves it behind.
struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Writes `text` as `config`, mode 0600, without replacing a file: the module's step 5.
fn write_new(config: &Path, text: &str, between: &dyn Fn()) -> io::Result<Configured> {
    let parent = config
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(parent)?;
    let path = parent.join(temporary_name());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let temporary = Temporary(path);
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    between();
    let linked = match fs::hard_link(&temporary.0, config) {
        Ok(()) => Configured::Written,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Configured::Kept,
        Err(error) => return Err(error),
    };
    drop(temporary);
    File::open(parent)?.sync_all()?;
    Ok(linked)
}
