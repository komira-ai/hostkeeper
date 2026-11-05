//! `release set-version`: writes a version into the `version = ` line of the `[workspace.package]`
//! table of the root `Cargo.toml` and into the version line of every workspace member's block of
//! `Cargo.lock`, so a `--locked` build accepts it.
//!
//! Both files are edited by lines, so every other byte is kept. A table header is a line whose
//! first non-blank character is `[`; the edit assumes no line of an array inside the edited
//! tables starts with `[`. Both new texts are computed before either file is written.

use std::collections::BTreeSet;
use std::path::Path;

use super::Error;
use super::contract::Version;

/// Sets the workspace at `root` to `version`; one line saying what was set.
pub(crate) fn set_version(root: &Path, version: &Version) -> Result<Vec<String>, Error> {
    let manifest_path = root.join("Cargo.toml");
    let lock_path = root.join("Cargo.lock");
    let manifest = read(&manifest_path)?;
    let lock = read(&lock_path)?;
    let names = member_names(root, &manifest)?;
    let new_manifest = edit_manifest(&manifest, version)?;
    let new_lock = edit_lock(&lock, &names, version)?;
    write(&manifest_path, &new_manifest)?;
    write(&lock_path, &new_lock)?;
    Ok(vec![format!(
        "Cargo.toml and Cargo.lock: the workspace and its {} members are {version}",
        names.len()
    )])
}

fn read(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path).map_err(Error::io(format!("read {}", path.display())))
}

fn write(path: &Path, text: &str) -> Result<(), Error> {
    std::fs::write(path, text).map_err(Error::io(format!("write {}", path.display())))
}

/// The package names of the workspace members, each checked to inherit the workspace version.
fn member_names(root: &Path, manifest: &str) -> Result<BTreeSet<String>, Error> {
    let table = manifest
        .parse::<toml::Table>()
        .map_err(|e| Error::Failed(format!("Cargo.toml: {e}")))?;
    let members = table
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| Error::Failed("Cargo.toml has no [workspace] members array".to_owned()))?;
    let entries = members
        .iter()
        .map(|m| {
            m.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::Failed(format!("Cargo.toml: the member {m} is not a path")))
        })
        .collect::<Result<Vec<String>, Error>>()?;
    let mut names = BTreeSet::new();
    for dir in member_dirs(root, &entries)? {
        let shown = format!("{dir}/Cargo.toml");
        let text = read(&root.join(&shown))?;
        let package = text
            .parse::<toml::Table>()
            .map_err(|e| Error::Failed(format!("{shown}: {e}")))?
            .get("package")
            .cloned();
        let inherits = package
            .as_ref()
            .and_then(|p| p.get("version"))
            .and_then(|v| v.get("workspace"))
            .and_then(toml::Value::as_bool);
        if inherits != Some(true) {
            return Err(Error::Failed(format!(
                "{shown} does not inherit the workspace version: its [package] needs version.workspace = true"
            )));
        }
        let name = package
            .as_ref()
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| Error::Failed(format!("{shown} has no package name")))?;
        names.insert(name.to_owned());
    }
    Ok(names)
}

/// The member directories (from the root, sorted) the `members` entries name: `<dir>/*` is every
/// subdirectory of `<dir>` holding a `Cargo.toml`, any other entry is a directory.
fn member_dirs(root: &Path, entries: &[String]) -> Result<Vec<String>, Error> {
    let mut dirs = BTreeSet::new();
    for entry in entries {
        match entry.strip_suffix("/*") {
            Some(parent) => {
                let path = root.join(parent);
                let what = format!("read {}", path.display());
                for item in std::fs::read_dir(&path).map_err(Error::io(&what))? {
                    let item = item.map_err(Error::io(&what))?;
                    if item.path().join("Cargo.toml").is_file() {
                        dirs.insert(format!("{parent}/{}", item.file_name().to_string_lossy()));
                    }
                }
            }
            None => {
                dirs.insert(entry.clone());
            }
        }
    }
    Ok(dirs.into_iter().collect())
}

/// Whether `line` sets the key `key`: the key, then optional blanks, then `=`.
fn is_key(line: &str, key: &str) -> bool {
    line.trim_start()
        .strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// The version line for `version`, keeping the line break (or its absence) of `old`.
fn version_line(old: &str, version: &Version) -> String {
    format!("version = \"{version}\"{}", &old[old.trim_end().len()..])
}

/// `manifest` with the `version` line of `[workspace.package]` set to `version`.
fn edit_manifest(manifest: &str, version: &Version) -> Result<String, Error> {
    let mut out = String::with_capacity(manifest.len());
    let mut in_table = false;
    let mut done = false;
    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim();
        let header = trimmed.starts_with('[');
        in_table = (in_table & !header) | (trimmed == "[workspace.package]");
        if in_table & !header & !done & is_key(line, "version") {
            out.push_str(&version_line(line, version));
            done = true;
        } else {
            out.push_str(line);
        }
    }
    done.then_some(out).ok_or_else(|| {
        Error::Failed("Cargo.toml has no version line in [workspace.package]".to_owned())
    })
}

/// `lock` with the version line of every workspace member's block set to `version`. A block
/// with a `source = ` line is a registry or git package and stays as it is.
fn edit_lock(lock: &str, members: &BTreeSet<String>, version: &Version) -> Result<String, Error> {
    let mut lines: Vec<String> = lock.split_inclusive('\n').map(str::to_owned).collect();
    let mut seen = BTreeSet::new();
    let mut start = None;
    for end in 0..=lines.len() {
        let at_header = lines.get(end).is_none_or(|l| l.starts_with('['));
        let closing = if at_header { start.take() } else { None };
        if let Some(first) = closing {
            edit_block(&mut lines[first..end], members, version, &mut seen)?;
        }
        if lines
            .get(end)
            .is_some_and(|l| l.trim_end() == "[[package]]")
        {
            start = Some(end);
        }
    }
    if let Some(missing) = members.iter().find(|m| !seen.contains(*m)) {
        return Err(Error::Failed(format!("{missing} is not in Cargo.lock")));
    }
    Ok(lines.concat())
}

/// Sets the version line of the `[[package]]` block `block` when it is a workspace member's.
fn edit_block(
    block: &mut [String],
    members: &BTreeSet<String>,
    version: &Version,
    seen: &mut BTreeSet<String>,
) -> Result<(), Error> {
    let name = block
        .iter()
        .find_map(|l| {
            l.strip_prefix("name = \"")
                .and_then(|rest| rest.trim_end().strip_suffix('"'))
        })
        .map(str::to_owned);
    let sourced = block.iter().any(|l| l.starts_with("source = "));
    let Some(name) = name.filter(|n| members.contains(n) & !sourced) else {
        return Ok(());
    };
    let line = block
        .iter_mut()
        .find(|l| is_key(l, "version"))
        .ok_or_else(|| Error::Failed(format!("Cargo.lock: the block of {name} has no version")))?;
    *line = version_line(line, version);
    seen.insert(name);
    Ok(())
}

#[cfg(test)]
mod tests;
