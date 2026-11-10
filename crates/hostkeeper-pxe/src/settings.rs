//! The settings file of `pxe` (`/etc/pxe/pxe.toml` on a host): the host's choices, in a root-owned
//! file the unit names with `serve --settings` and the package names with its param `settings`.
//!
//! The file is TOML with at most two tables and nothing else at the top level:
//!
//! - `[serve]` (required): the flags of `pxe serve`, by name without the `--`, in kebab case
//!   (`mode`, `address`, `token-file`, `daemon-config`, `user-id`, `reinstall`, ...). A string or
//!   an integer is the flag's value; an array of strings or integers gives the flag once per
//!   element. Any other value is refused, naming the key.
//! - `[prepare]` (optional): the flags of the `prepare` step's download: `platform` (an array),
//!   `ca-bundle`, `gh`, `trusted-root` and `download-timeout-secs`, written the same way.
//!
//! Each table becomes the arguments `--<key> <value>`, which the command's own parser reads
//! after the arguments of the command line (so an unknown key, a missing flag, a bad value and a
//! flag given twice are that parser's refusals). The file holds paths only: the operator token
//! is the file `token-file` names, never a value here. The file is read with
//! [`hostkeeper_config::trust::read_trusted`]: owned by the trusted user and not writable by
//! group or others.

use std::path::Path;

use hostkeeper_config::trust::read_trusted;

/// The arguments the two tables expand to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// The arguments of `[serve]`.
    pub(crate) serve: Vec<String>,
    /// The arguments of `[prepare]`; none when the table is absent.
    pub(crate) prepare: Vec<String>,
}

/// Reads and expands the file at `path`, which `owner` must own.
///
/// # Errors
/// The line that says why, beginning with the path.
pub(crate) fn read(path: &Path, owner: u32) -> Result<Settings, String> {
    let shown = |why: &dyn std::fmt::Display| format!("{}: {why}", path.display());
    let bytes = read_trusted(path, owner).map_err(|e| shown(&e))?;
    let text = String::from_utf8(bytes).map_err(|_| shown(&"not UTF-8"))?;
    parse(&text).map_err(|e| shown(&e))
}

/// Expands the text of a settings file.
///
/// # Errors
/// Why the text is not a settings file.
pub(crate) fn parse(text: &str) -> Result<Settings, String> {
    let mut table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| e.message().to_owned())?;
    if let Some(key) = table
        .keys()
        .find(|k| !matches!(k.as_str(), "serve" | "prepare"))
    {
        return Err(format!(
            "key {key:?} is not a table of this file: only [serve] and [prepare]"
        ));
    }
    let serve = table.remove("serve").ok_or("there is no [serve] table")?;
    let prepare = table.remove("prepare");
    Ok(Settings {
        serve: expand("serve", serve)?,
        prepare: prepare
            .map(|value| expand("prepare", value))
            .transpose()?
            .unwrap_or_default(),
    })
}

/// The arguments of the table `value` named `name`.
fn expand(name: &str, value: toml::Value) -> Result<Vec<String>, String> {
    let toml::Value::Table(table) = value else {
        return Err(format!("{name:?} must be a table"));
    };
    let mut args = Vec::new();
    for (key, value) in table {
        let values = match value {
            toml::Value::Array(elements) => elements,
            single => vec![single],
        };
        for element in values {
            let text = match element {
                toml::Value::String(text) => text,
                toml::Value::Integer(number) => number.to_string(),
                _ => {
                    return Err(format!(
                        "[{name}] {key:?}: a value is a string or an integer, or an array of them"
                    ));
                }
            };
            args.push(format!("--{key}"));
            args.push(text);
        }
    }
    Ok(args)
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
