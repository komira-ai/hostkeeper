//! `release check-linux-pkg --coordinator-staged <dir>`: the coordinator's deb or rpm
//! (`packaging/coordinator/nfpm.yaml`) against what the build job staged for it in `<dir>`:
//!
//! - `/usr/bin/hostkeeper-coordinator`, a regular file of mode 0755 owned by root and group root,
//!   with the bytes of `<dir>/hostkeeper-coordinator` (and, as for every package this verb reads,
//!   a static ELF program);
//! - `/usr/share/hostkeeper-coordinator/ui/`, exactly the regular files of `<dir>/ui/`, each with
//!   its bytes, mode 0644, owned by root and group root, as the image holds the web app;
//! - the unit, the sysusers.d entry, the tmpfiles rules and the template of `serve.toml`, each a
//!   regular file of mode 0644 owned by root and group root ([`CONFIG_FILES`]);
//! - no dependency but `systemd` ([`ALLOWED_DEPENDS`]), whose `systemd-sysusers` and
//!   `systemd-tmpfiles` the post-install script runs.

use std::collections::BTreeMap;
use std::path::Path;

use super::{Installed, REGULAR, TYPE_BITS};
use crate::release::Error;

/// Where the packages install the coordinator, and the staged file it must equal.
pub(crate) const INSTALLED_COORDINATOR: &str = "/usr/bin/hostkeeper-coordinator";
const STAGED_COORDINATOR: &str = "hostkeeper-coordinator";

/// Where they install the operator web app, and the staged directory it must equal.
pub(crate) const INSTALLED_UI: &str = "/usr/share/hostkeeper-coordinator/ui";
const STAGED_UI: &str = "ui";

/// The files of `packaging/coordinator/` the packages install, each 0644 root:root.
pub(crate) const CONFIG_FILES: [&str; 4] = [
    "/usr/lib/systemd/system/hostkeeper-coordinator.service",
    "/usr/lib/sysusers.d/hostkeeper-coordinator.conf",
    "/usr/lib/tmpfiles.d/hostkeeper-coordinator.conf",
    "/usr/share/hostkeeper-coordinator/serve.toml.template",
];

/// The dependencies the coordinator's packages may declare.
pub(crate) const ALLOWED_DEPENDS: [&str; 1] = ["systemd"];

/// The problems of one regular file: its type, mode and owner.
fn file_problems(path: &str, got: &Installed, mode: u32, out: &mut Vec<String>) {
    if got.mode & TYPE_BITS != REGULAR {
        out.push(format!("{path}: not a regular file"));
    }
    if got.mode & 0o7777 != mode {
        out.push(format!(
            "{path}: mode {:o}, not {mode:o}",
            got.mode & 0o7777
        ));
    }
    if (got.owner.as_str(), got.group.as_str()) != ("root", "root") {
        out.push(format!(
            "{path}: owned by {}:{}, not root:root",
            got.owner, got.group
        ));
    }
}

/// The regular files below `dir`, by their path relative to it with `/` between components.
fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
    let what = || format!("read {}", dir.display());
    let mut entries = std::fs::read_dir(dir)
        .and_then(|read| read.collect::<Result<Vec<_>, _>>())
        .map_err(Error::io(what()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let here = format!("{rel}/{name}");
        if path.is_dir() {
            walk(&path, &here, out)?;
        } else {
            let bytes =
                std::fs::read(&path).map_err(Error::io(format!("read {}", path.display())))?;
            out.insert(here, bytes);
        }
    }
    Ok(())
}

/// The problems of `found` against the coordinator's files staged in `staged`, one line each,
/// and the number of files of the web app.
pub(super) fn staged_problems(
    found: &BTreeMap<String, Installed>,
    staged: &Path,
) -> Result<(Vec<String>, usize), Error> {
    let mut out = Vec::new();
    let binary = staged.join(STAGED_COORDINATOR);
    let want = std::fs::read(&binary).map_err(Error::io(format!("read {}", binary.display())))?;
    match found.get(INSTALLED_COORDINATOR) {
        None => out.push(format!("{INSTALLED_COORDINATOR}: not in the package")),
        Some(got) => {
            file_problems(INSTALLED_COORDINATOR, got, 0o755, &mut out);
            if got.data != want {
                out.push(format!(
                    "{INSTALLED_COORDINATOR}: its bytes are not those of {}",
                    binary.display()
                ));
            }
        }
    }
    for path in CONFIG_FILES {
        match found.get(path) {
            None => out.push(format!("{path}: not in the package")),
            Some(got) => file_problems(path, got, 0o644, &mut out),
        }
    }
    let mut ui = BTreeMap::new();
    walk(&staged.join(STAGED_UI), "", &mut ui)?;
    let prefix = format!("{INSTALLED_UI}/");
    let packaged: BTreeMap<&str, &Installed> = found
        .iter()
        .filter(|(path, got)| path.starts_with(&prefix) && got.mode & TYPE_BITS == REGULAR)
        .map(|(path, got)| (path.as_str(), got))
        .collect();
    for (rel, bytes) in &ui {
        let path = format!("{INSTALLED_UI}{rel}");
        match packaged.get(path.as_str()) {
            None => out.push(format!("{path}: not in the package")),
            Some(got) => {
                file_problems(&path, got, 0o644, &mut out);
                if got.data != *bytes {
                    out.push(format!(
                        "{path}: its bytes are not those of the staged {STAGED_UI}{rel}"
                    ));
                }
            }
        }
    }
    for path in packaged.keys() {
        let rel = &path[INSTALLED_UI.len()..];
        if !ui.contains_key(rel) {
            out.push(format!("{path}: not a file of the staged {STAGED_UI}/"));
        }
    }
    Ok((out, ui.len()))
}

/// What a package that passed says about the coordinator's files.
pub(super) fn passed(name: &str, ui_files: usize) -> Vec<String> {
    let mut lines = vec![format!(
        "{name}: {INSTALLED_COORDINATOR} is 755 root:root and the staged bytes"
    )];
    lines.push(format!(
        "{name}: {INSTALLED_UI} holds the {ui_files} staged files, 644 root:root"
    ));
    lines.extend(
        CONFIG_FILES
            .iter()
            .map(|path| format!("{name}: {path} is 644 root:root")),
    );
    lines
}
