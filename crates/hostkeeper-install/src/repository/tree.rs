//! The checks of a repository package's extracted tree at `stage` (packages-native.md section
//! 10.2, packages-repositories.md section 3.4), and what its `run` units ask to be enabled by.
//!
//! - **The path rule.** A service's package holds files only under `/usr/bin/`,
//!   `/usr/lib/<package>/`, `/usr/share/<package>/`, `/usr/share/doc/<package>/`,
//!   `/etc/<service>/`, and plain unit files named for the service in `/usr/lib/systemd/system/`
//!   or `/lib/systemd/system/` (`<service>.<kind>`, `<service>-….<kind>` or `<service>@….<kind>`,
//!   of the kinds `service`, `socket` and `timer`, `run` units or not); the directories above
//!   those are allowed as directories only. Anything else (an enablement link, a drop-in
//!   directory, a unit of another name, a link in a unit directory, a cron file, a `sudoers.d`
//!   file, a device) is refused.
//! - **The units.** Every `run` unit is a regular file of the tree in one of the two unit
//!   directories.
//! - **The hook** is the program `usr/lib/<package>/hostkeeper-hook`, when the package has one:
//!   a regular executable file reached through no link. Without one the service gets the
//!   built-in steps, and a manifest that names a `boot` step or `params` (which only a hook
//!   reads) is refused.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The directories a package's unit files may be in, relative to the root.
pub const UNIT_DIRS: [&str; 2] = ["usr/lib/systemd/system", "lib/systemd/system"];

/// The hook's file name, in `usr/lib/<package>/`.
pub const HOOK: &str = "hostkeeper-hook";

/// The hook's path in the tree of `package`.
pub fn hook_path(package: &str) -> String {
    format!("usr/lib/{package}/{HOOK}")
}

/// Why a tree is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    #[error("the package installs /{0}, outside the paths a service's package may use")]
    Outside(String),
    #[error("the package installs /{0}, which is neither a file, a directory nor a link")]
    Special(String),
    #[error("the run unit {0} is not a unit file of the package")]
    NoUnit(String),
    #[error("the hook /{0} is {1}")]
    Hook(String, &'static str),
    #[error("the manifest names the boot step {0:?}, and the package has no hook")]
    BootWithoutHook(String),
    #[error("the manifest has params, which only a hook reads, and the package has no hook")]
    ParamsWithoutHook,
    #[error("the unit {unit} is wanted by {target:?}, not a unit name")]
    Target { unit: String, target: String },
    #[error("the tree cannot be read: {0}")]
    Io(String),
}

/// What a tree's checks are given.
#[derive(Clone, Copy, Debug)]
pub struct Rules<'a> {
    pub service: &'a str,
    pub package: &'a str,
    pub run: &'a [String],
    pub boot: Option<&'a str>,
    pub params: bool,
}

/// The kind of one entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Dir,
    File,
    Link,
    Other,
}

/// Every entry under `tree`, as relative paths with `/`, and its kind; links are not followed.
fn entries(tree: &Path) -> io::Result<Vec<(String, Kind)>> {
    let mut out = Vec::new();
    let mut todo = vec![PathBuf::new()];
    while let Some(rel) = todo.pop() {
        for entry in fs::read_dir(tree.join(&rel))? {
            let entry = entry?;
            let path = rel.join(entry.file_name());
            let kind = entry.file_type()?;
            let kind = match (kind.is_symlink(), kind.is_dir(), kind.is_file()) {
                (true, _, _) => Kind::Link,
                (_, true, _) => Kind::Dir,
                (_, _, true) => Kind::File,
                _ => Kind::Other,
            };
            if kind == Kind::Dir {
                todo.push(path.clone());
            }
            out.push((path.to_string_lossy().into_owned(), kind));
        }
    }
    out.sort();
    Ok(out)
}

/// Whether `path` is `dir` or below it.
fn within(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Whether the entry `path` of `kind` keeps the path rule.
fn allowed(path: &str, kind: Kind, rules: &Rules<'_>) -> bool {
    let own = [
        "usr/bin".to_owned(),
        format!("usr/lib/{}", rules.package),
        format!("usr/share/{}", rules.package),
        format!("usr/share/doc/{}", rules.package),
        format!("etc/{}", rules.service),
    ];
    if own.iter().any(|dir| within(path, dir)) {
        return true;
    }
    let unit = UNIT_DIRS.iter().find_map(|dir| {
        path.strip_prefix(dir)
            .and_then(|rest| rest.strip_prefix('/'))
    });
    match (kind, unit) {
        (Kind::File, Some(name)) => is_service_unit(rules.service, name),
        (Kind::Dir, None) => own
            .iter()
            .map(String::as_str)
            .chain(UNIT_DIRS)
            .any(|dir| within(dir, path)),
        _ => false,
    }
}

/// The kinds of unit a service's package may install.
pub const UNIT_KINDS: [&str; 3] = ["service", "socket", "timer"];

/// Whether `name` is a plain unit file named for `service`: `<service>.<kind>`,
/// `<service>-<rest>.<kind>` or `<service>@<rest>.<kind>`, `<rest>` a unit name's bytes, of a
/// kind of [`UNIT_KINDS`].
pub fn is_service_unit(service: &str, name: &str) -> bool {
    let Some((stem, kind)) = name.rsplit_once('.') else {
        return false;
    };
    let rest = match stem.strip_prefix(service) {
        Some("") => Some(""),
        Some(rest) => rest.strip_prefix(['-', '@']),
        None => None,
    };
    UNIT_KINDS.contains(&kind)
        & rest.is_some_and(|rest| {
            rest.bytes()
                .all(|b| b.is_ascii_alphanumeric() | b":_.@-".contains(&b))
        })
        & !name.contains('/')
}

/// The checks of `tree` under `rules`; `Ok(true)` when the package has a hook.
pub fn check(tree: &Path, rules: &Rules<'_>) -> Result<bool, TreeError> {
    let io = |e: io::Error| TreeError::Io(e.to_string());
    for (path, kind) in entries(tree).map_err(io)? {
        if kind == Kind::Other {
            return Err(TreeError::Special(path));
        }
        if !allowed(&path, kind, rules) {
            return Err(TreeError::Outside(path));
        }
    }
    for unit in rules.run {
        unit_file(tree, unit).ok_or_else(|| TreeError::NoUnit(unit.clone()))?;
    }
    let hook = hook(tree, rules.package)?;
    match (hook, rules.boot, rules.params) {
        (false, Some(boot), _) => Err(TreeError::BootWithoutHook(boot.to_owned())),
        (false, None, true) => Err(TreeError::ParamsWithoutHook),
        _ => Ok(hook),
    }
}

/// The unit file of `unit` in `tree`, relative to it: a regular file of one of [`UNIT_DIRS`].
pub fn unit_file(tree: &Path, unit: &str) -> Option<String> {
    UNIT_DIRS
        .iter()
        .map(|dir| format!("{dir}/{unit}"))
        .find(|rel| {
            fs::symlink_metadata(tree.join(rel)).is_ok_and(|meta| meta.file_type().is_file())
        })
}

/// Whether the package has a hook, refusing one that is not a regular executable file reached
/// through no link.
fn hook(tree: &Path, package: &str) -> Result<bool, TreeError> {
    let rel = hook_path(package);
    let refused = |why| Err(TreeError::Hook(rel.clone(), why));
    let mut dir = tree.to_path_buf();
    for part in ["usr", "lib", package] {
        dir.push(part);
        match fs::symlink_metadata(&dir) {
            Err(_) => return Ok(false),
            Ok(meta) if meta.file_type().is_symlink() => return refused("reached through a link"),
            Ok(meta) if !meta.is_dir() => return Ok(false),
            Ok(_) => {}
        }
    }
    match fs::symlink_metadata(dir.join(HOOK)) {
        Err(_) => Ok(false),
        Ok(meta) if meta.file_type().is_symlink() => refused("a link"),
        Ok(meta) if !meta.is_file() => refused("not a regular file"),
        Ok(meta) if meta.permissions().mode() & 0o111 == 0 => refused("not executable"),
        Ok(_) => Ok(true),
    }
}

/// The links that enable a unit: one per target of `WantedBy=` (`<target>.wants/`) and
/// `RequiredBy=` (`<target>.requires/`) in its `[Install]` section, as
/// `<target>.<wants|requires>/<unit>` relative to the service manager's directory. An empty
/// assignment drops the targets before it, as systemd reads it.
pub fn install_links(unit: &str, text: &str) -> Result<BTreeSet<String>, TreeError> {
    let mut wants: Vec<String> = Vec::new();
    let mut requires: Vec<String> = Vec::new();
    let mut in_install = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_install = line == "[Install]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_install) else {
            continue;
        };
        let list = match key.trim() {
            "WantedBy" => &mut wants,
            "RequiredBy" => &mut requires,
            _ => continue,
        };
        match value.trim() {
            "" => list.clear(),
            value => list.extend(value.split_whitespace().map(str::to_owned)),
        }
    }
    let mut links = BTreeSet::new();
    for (list, kind) in [(wants, "wants"), (requires, "requires")] {
        for target in list {
            if !is_unit_name(&target) {
                return Err(TreeError::Target {
                    unit: unit.to_owned(),
                    target,
                });
            }
            links.insert(format!("{target}.{kind}/{unit}"));
        }
    }
    Ok(links)
}

/// Whether `name` is a unit's name: `[A-Za-z0-9:_.@-]` with a dot, at most 255 bytes.
fn is_unit_name(name: &str) -> bool {
    (1..=255).contains(&name.len())
        & name.contains('.')
        & name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | b":_.@-".contains(&b))
}
